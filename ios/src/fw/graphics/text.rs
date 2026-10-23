//! UIKit's NSString drawing (UIStringDrawing): text measured and drawn
//! with real fonts into the current UIKit context.
//!
//! The font file is the game's own when the UIFont's name matches one of
//! its Info.plist UIAppFonts (by family, full or PostScript name, or file
//! name), else the system's sans-serif font (its bold for bold names).
//! Glyph outlines go through the CTM into the context's path filler
//! (shapes.rs), so text is anti-aliased, clipped and transformed like any
//! other shape, upright in UIKit's flipped coordinates, and cut at the
//! rect's edges; the context's text drawing mode picks fill, stroke or
//! both, and its clip modes add the glyphs to the clip. Lines wrap at words (or
//! letters) and stop at the last line that fits; the truncating modes end
//! with "…". With no font file at all, sizes use the same estimate as
//! controls::text_size and nothing is drawn.

use super::*;
use crate::fw::plist::{self, Value};
use crate::fw::{controls, string};
use std::collections::HashMap;
use ttf_parser::{Face, GlyphId};

// UILineBreakMode
const CHARACTER_WRAP: isize = 1;
const CLIP: isize = 2;
const HEAD_TRUNCATION: isize = 3;
const TAIL_TRUNCATION: isize = 4;
const MIDDLE_TRUNCATION: isize = 5;

// UITextAlignment
const CENTER: isize = 1;
const RIGHT: isize = 2;

// ----- font files -----

/// A font file, parsed once and kept (games use a handful).
fn load(path: &str) -> Option<&'static Face<'static>> {
    let bytes: &'static [u8] = Box::leak(std::fs::read(path).ok()?.into_boxed_slice());
    match Face::parse(bytes, 0) {
        Ok(face) => Some(Box::leak(Box::new(face))),
        Err(e) => {
            crate::log!("font {path}: {e}");
            None
        }
    }
}

/// A font name to compare: lower case, letters and digits only, no file
/// extension ("Coming Soon", "ComingSoon" and "comingsoon.ttf" match).
fn simple(name: &str) -> String {
    let name = name.to_lowercase();
    let name = name.strip_suffix(".ttf").or_else(|| name.strip_suffix(".otf")).unwrap_or(&name);
    name.chars().filter(|c| c.is_alphanumeric()).collect()
}

/// The game's own fonts (Info.plist UIAppFonts), each with its names.
fn app_fonts() -> &'static [(Vec<String>, &'static Face<'static>)] {
    static FONTS: OnceLock<Vec<(Vec<String>, &'static Face<'static>)>> = OnceLock::new();
    FONTS.get_or_init(|| {
        let Some(dir) = crate::loader::try_app_dir() else { return Vec::new() };
        let Some(Value::Dict(info)) = std::fs::read(format!("{dir}/Info.plist")).ok().and_then(|bytes| plist::parse(&bytes)) else {
            return Vec::new();
        };
        let Some((_, Value::Array(files))) = info.iter().find(|(key, _)| key == "UIAppFonts") else { return Vec::new() };
        let files = files.iter().filter_map(|file| if let Value::String(file) = file { Some(file) } else { None });
        files
            .filter_map(|file| {
                let face = load(&format!("{dir}/{file}"))?;
                let mut names: Vec<String> = face
                    .names()
                    .into_iter()
                    .filter(|n| matches!(n.name_id, 1 | 4 | 6 | 16)) // family, full, PostScript, typographic family
                    .filter_map(|n| n.to_string())
                    .map(|n| simple(&n))
                    .collect();
                names.push(simple(file.rsplit('/').next().unwrap_or(file)));
                Some((names, face))
            })
            .collect()
    })
}

/// The system's sans-serif font (Android's, then common Linux ones).
fn system_face(bold: bool) -> Option<&'static Face<'static>> {
    const REGULAR: &[&str] = &[
        "/system/fonts/Roboto-Regular.ttf",
        "/system/fonts/DroidSans.ttf",
        "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
    ];
    const BOLD: &[&str] = &[
        "/system/fonts/Roboto-Bold.ttf",
        "/system/fonts/DroidSans-Bold.ttf",
        "/usr/share/fonts/liberation/LiberationSans-Bold.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
        "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        "/usr/share/fonts/noto/NotoSans-Bold.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf",
    ];
    static FACES: [OnceLock<Option<&'static Face<'static>>>; 2] = [OnceLock::new(), OnceLock::new()];
    let regular = || *FACES[0].get_or_init(|| REGULAR.iter().find_map(|path| load(path)));
    if bold {
        FACES[1].get_or_init(|| BOLD.iter().find_map(|path| load(path))).or_else(regular)
    } else {
        regular()
    }
}

/// The font file for a UIFont name (looked up once per name).
fn face_named(name: &str) -> Option<&'static Face<'static>> {
    static CACHE: Mutex<Option<HashMap<String, Option<&'static Face<'static>>>>> = Mutex::new(None);
    if let Some(&found) = CACHE.lock().unwrap().get_or_insert_with(HashMap::new).get(name) {
        return found;
    }
    let wanted = simple(name);
    let found = app_fonts().iter().find(|(names, _)| names.contains(&wanted)).map(|(_, face)| *face).or_else(|| {
        let lower = name.to_lowercase();
        system_face(["bold", "medium", "heavy", "black"].iter().any(|w| lower.contains(w)))
    });
    if found.is_none() {
        crate::log!("font {name}: no font file found; text is measured but not drawn");
    }
    CACHE.lock().unwrap().get_or_insert_with(HashMap::new).insert(name.to_string(), found);
    found
}

// ----- measuring and line breaking -----

/// A UIFont at its size, measuring in points.
struct Font {
    face: Option<&'static Face<'static>>,
    size: f64,
}

impl Font {
    fn of(font: Id) -> Font {
        let (name, size) = controls::font_name_and_size(font);
        Font { face: face_named(&name), size }
    }

    /// Points per font unit.
    fn scale(&self, face: &Face) -> f64 {
        self.size / f64::from(face.units_per_em().max(1))
    }

    fn glyph(face: &Face, c: char) -> GlyphId {
        face.glyph_index(if c == '\t' { ' ' } else { c }).unwrap_or(GlyphId(0))
    }

    fn advance(&self, c: char) -> f64 {
        match self.face {
            Some(face) => f64::from(face.glyph_hor_advance(Font::glyph(face, c)).unwrap_or(0)) * self.scale(face),
            None => self.size * 11.0 / 20.0,
        }
    }

    fn width(&self, text: &[char]) -> f64 {
        text.iter().map(|&c| self.advance(c)).sum()
    }

    fn ascender(&self) -> f64 {
        match self.face {
            Some(face) => f64::from(face.ascender()) * self.scale(face),
            None => self.size * 0.952,
        }
    }

    /// Ascender to descender plus the line gap (UIFont's lineHeight).
    fn line_height(&self) -> f64 {
        match self.face {
            Some(face) => f64::from(i32::from(face.ascender()) - i32::from(face.descender()) + i32::from(face.line_gap())) * self.scale(face),
            None => self.size * 6.0 / 5.0,
        }
    }
}

fn trim_end(line: &[char]) -> &[char] {
    let end = line.iter().rposition(|&c| c != ' ').map_or(0, |i| i + 1);
    &line[..end]
}

/// One paragraph broken into lines no wider than `width` (at least one
/// letter each): at the last space that fits, or anywhere for letter wrap.
fn wrap(font: &Font, text: &[char], width: f64, mode: isize, lines: &mut Vec<Vec<char>>) {
    if !(width.is_finite() && width > 0.0) || font.width(trim_end(text)) <= width {
        lines.push(trim_end(text).to_vec());
        return;
    }
    let mut start = 0;
    while start < text.len() {
        let (mut end, mut used) = (start, 0.0);
        while end < text.len() {
            let advance = font.advance(text[end]);
            // Spaces may hang past the edge; letters may not (but one always fits).
            if text[end] != ' ' && used + advance > width && end > start {
                break;
            }
            used += advance;
            end += 1;
        }
        if end < text.len() && mode != CHARACTER_WRAP {
            if let Some(space) = text[start..end].iter().rposition(|&c| c == ' ') {
                end = start + space + 1;
            }
        }
        lines.push(trim_end(&text[start..end]).to_vec());
        start = end;
        while start < text.len() && text[start] == ' ' {
            start += 1;
        }
    }
}

/// `text` cut to fit `width` with "…" where the mode says.
fn truncate(font: &Font, text: &[char], width: f64, mode: isize) -> Vec<char> {
    if !width.is_finite() || font.width(text) <= width {
        return text.to_vec();
    }
    let mut keep = text.len();
    loop {
        let cut: Vec<char> = match mode {
            HEAD_TRUNCATION => std::iter::once('…').chain(text[text.len() - keep..].iter().copied()).collect(),
            MIDDLE_TRUNCATION => {
                let front = keep.div_ceil(2);
                let back = keep - front;
                text[..front].iter().copied().chain(std::iter::once('…')).chain(text[text.len() - back..].iter().copied()).collect()
            }
            _ => trim_end(&text[..keep]).iter().copied().chain(std::iter::once('…')).collect(),
        };
        if keep == 0 || font.width(&cut) <= width {
            return cut;
        }
        keep -= 1;
    }
}

/// The lines UIKit shows of `text` in `width` (infinite: no wrapping): the
/// first paragraph alone for the single-line methods, else the wrapped
/// lines, at most `max_lines` of them (0: any number).
fn lay_out(font: &Font, text: &str, width: f64, single_line: bool, max_lines: usize, mode: isize) -> Vec<Vec<char>> {
    let paragraphs: Vec<Vec<char>> = text.split('\n').map(|p| p.chars().filter(|&c| c != '\r').collect()).collect();
    let truncating = matches!(mode, HEAD_TRUNCATION | TAIL_TRUNCATION | MIDDLE_TRUNCATION);
    if single_line {
        let first = &paragraphs[0];
        return vec![if truncating { truncate(font, first, width, mode) } else { first.clone() }];
    }
    let mut lines = Vec::new();
    for paragraph in &paragraphs {
        wrap(font, paragraph, width, mode, &mut lines);
    }
    if max_lines > 0 && lines.len() > max_lines {
        // The last line shown ends with "…" when the text goes on.
        let rest = lines[max_lines - 1..].join(&' ');
        lines.truncate(max_lines);
        if truncating {
            lines[max_lines - 1] = truncate(font, &rest, width, TAIL_TRUNCATION);
        }
    }
    lines
}

/// How many lines fit in `height` (0: no limit; always at least one).
fn lines_in(font: &Font, height: f64) -> usize {
    if height.is_finite() && height > 0.0 {
        (((height + 0.001) / font.line_height()).floor() as usize).max(1)
    } else {
        0
    }
}

/// The size UIKit answers: the widest line (no wider than `width`) by the
/// lines' height, rounded up; nothing for an empty string.
fn size_of(font: &Font, text: &str, lines: &[Vec<char>], width: f64) -> Size {
    if text.is_empty() {
        return Size { width: 0.0, height: 0.0 };
    }
    let widest = lines.iter().map(|line| font.width(line)).fold(0.0, f64::max);
    let widest = if width.is_finite() && width > 0.0 { widest.min(width) } else { widest };
    Size { width: widest.ceil(), height: (lines.len() as f64 * font.line_height()).ceil() }
}

// ----- drawing -----

/// Glyph outlines into a device-space path: font units (y up) placed at a
/// pen position on a baseline in UIKit's flipped user space, then the CTM.
struct Pen<'a> {
    outline: &'a mut shapes::Outline,
    ctm: Transform,
    x: f64,
    baseline: f64,
    scale: f64,
    last: (f64, f64),
}

impl Pen<'_> {
    fn map(&self, x: f32, y: f32) -> (f64, f64) {
        geometry::apply(self.ctm, self.x + f64::from(x) * self.scale, self.baseline - f64::from(y) * self.scale)
    }

    /// Curves as short lines: about one per 2 device pixels of control polygon.
    fn curve(&mut self, points: &[(f64, f64)], at: impl Fn(f64) -> (f64, f64)) {
        let mut length = 0.0;
        let mut previous = self.last;
        for &p in points {
            length += ((p.0 - previous.0).powi(2) + (p.1 - previous.1).powi(2)).sqrt();
            previous = p;
        }
        let steps = (length / 2.0).ceil().clamp(1.0, 16.0) as usize;
        for i in 1..=steps {
            let p = at(i as f64 / steps as f64);
            self.outline.line_to(p);
        }
        self.last = *points.last().unwrap();
    }
}

impl ttf_parser::OutlineBuilder for Pen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.last = self.map(x, y);
        self.outline.move_to(self.last);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.last = self.map(x, y);
        self.outline.line_to(self.last);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (p0, p1, p2) = (self.last, self.map(x1, y1), self.map(x, y));
        self.curve(&[p1, p2], |t| {
            let u = 1.0 - t;
            (u * u * p0.0 + 2.0 * u * t * p1.0 + t * t * p2.0, u * u * p0.1 + 2.0 * u * t * p1.1 + t * t * p2.1)
        });
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (p0, p1, p2, p3) = (self.last, self.map(x1, y1), self.map(x2, y2), self.map(x, y));
        self.curve(&[p1, p2, p3], |t| {
            let u = 1.0 - t;
            let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            (a * p0.0 + b * p1.0 + c * p2.0 + d * p3.0, a * p0.1 + b * p1.1 + c * p2.1 + d * p3.1)
        });
    }
    fn close(&mut self) {
        self.outline.close();
    }
}

impl Canvas {
    /// Lines of text in `rect` (UIKit's coordinates: its top is the first
    /// line's top), aligned in its width and cut at its edges, filled
    /// and/or stroked as the context's text drawing mode says; the clip
    /// modes then add the glyphs' shapes to the context's clip.
    fn draw_text(&mut self, face: &Face, font: &Font, lines: &[Vec<char>], rect: Rect, align: isize) {
        // CGTextDrawingMode: fill, stroke, fill+stroke, invisible, then the same four adding to the clip.
        let mode = self.state.text_mode;
        let (fill, stroke) = match mode & 3 {
            0 => (true, false),
            1 => (false, true),
            2 => (true, true),
            _ => (false, false),
        };
        let clip = (4..=7).contains(&mode);
        if !fill && !stroke && !clip {
            return;
        }
        let mut outline = shapes::Outline::default();
        for (i, line) in lines.iter().enumerate() {
            let width = font.width(line);
            let room = if rect.size.width.is_finite() { rect.size.width - width } else { 0.0 };
            let x = rect.origin.x
                + match align {
                    CENTER => room / 2.0,
                    RIGHT => room,
                    _ => 0.0,
                };
            let baseline = rect.origin.y + i as f64 * font.line_height() + font.ascender();
            let mut pen = Pen { outline: &mut outline, ctm: self.state.ctm, x, baseline, scale: font.scale(face), last: (0.0, 0.0) };
            for &c in line {
                face.outline_glyph(Font::glyph(face, c), &mut pen);
                pen.x += font.advance(c);
            }
        }
        if fill || stroke {
            // Painted only inside the rect (an unbounded side stays open;
            // the one line always shown may be taller than the rect).
            let saved = self.state.clip.clone();
            let open = |start: f64, length: f64| if length.is_finite() { (start, length) } else { (start - 1e6, 2e6) };
            let (x, width) = open(rect.origin.x, rect.size.width);
            let (y, height) = open(rect.origin.y, rect.size.height.max(lines.len() as f64 * font.line_height()));
            let mut bounds = shapes::Outline::default();
            let ctm = self.state.ctm;
            bounds.rect(Rect::new(x, y, width, height), &|(x, y)| geometry::apply(ctm, x, y));
            self.clip_to(&bounds, false);
            if fill {
                self.fill_outline(&outline, false);
            }
            if stroke {
                self.stroke_outline(&outline);
            }
            self.state.clip = saved;
        }
        if clip {
            self.clip_to(&outline, false);
        }
    }
}

/// Lays out and draws `text` in `rect` into the current context; the size UIKit answers.
fn draw(text: Id, rect: Rect, font: Id, mode: isize, align: isize, single_line: bool) -> Size {
    let font = Font::of(font);
    let text = string::get(text);
    let lines = lay_out(&font, &text, rect.size.width, single_line, lines_in(&font, rect.size.height), mode);
    if let Some(face) = font.face {
        if let Some(mut c) = canvas("-[NSString drawInRect:withFont:]", current_context()) {
            c.draw_text(face, &font, &lines, rect, align);
        }
    }
    size_of(&font, &text, &lines, rect.size.width)
}

fn measure(text: Id, font: Id, size: Size, mode: isize, single_line: bool) -> Size {
    let font = Font::of(font);
    let text = string::get(text);
    let lines = lay_out(&font, &text, size.width, single_line, lines_in(&font, size.height), mode);
    size_of(&font, &text, &lines, size.width)
}

const ANY: Size = Size { width: f64::INFINITY, height: f64::INFINITY };

extern "C" fn size_with_font(this: Id, _: Sel, font: Id) -> Size {
    measure(this, font, ANY, CLIP, true)
}
extern "C" fn size_constrained(this: Id, _: Sel, font: Id, size: Size) -> Size {
    measure(this, font, size, 0, false)
}
extern "C" fn size_constrained_mode(this: Id, _: Sel, font: Id, size: Size, mode: isize) -> Size {
    measure(this, font, size, mode, false)
}
extern "C" fn size_for_width(this: Id, _: Sel, font: Id, width: f64, mode: isize) -> Size {
    measure(this, font, Size { width, height: f64::INFINITY }, mode, true)
}
extern "C" fn draw_at_point(this: Id, _: Sel, point: Point, font: Id) -> Size {
    draw(this, Rect { origin: point, size: ANY }, font, CLIP, 0, true)
}
extern "C" fn draw_at_point_for_width(this: Id, _: Sel, point: Point, width: f64, font: Id, mode: isize) -> Size {
    draw(this, Rect { origin: point, size: Size { width, height: f64::INFINITY } }, font, mode, 0, true)
}
extern "C" fn draw_in_rect(this: Id, _: Sel, rect: Rect, font: Id) -> Size {
    draw(this, rect, font, 0, 0, false)
}
extern "C" fn draw_in_rect_mode(this: Id, _: Sel, rect: Rect, font: Id, mode: isize) -> Size {
    draw(this, rect, font, mode, 0, false)
}
extern "C" fn draw_in_rect_mode_alignment(this: Id, _: Sel, rect: Rect, font: Id, mode: isize, align: isize) -> Size {
    draw(this, rect, font, mode, align, false)
}

/// The NSString methods this file adds (32-bit games reach them through
/// framework_selectors.tsv, which needs a row for each).
fn selectors() -> [(&'static str, usize); 9] {
    [
        ("sizeWithFont:", size_with_font as usize),
        ("sizeWithFont:constrainedToSize:", size_constrained as usize),
        ("sizeWithFont:constrainedToSize:lineBreakMode:", size_constrained_mode as usize),
        ("sizeWithFont:forWidth:lineBreakMode:", size_for_width as usize),
        ("drawAtPoint:withFont:", draw_at_point as usize),
        ("drawAtPoint:forWidth:withFont:lineBreakMode:", draw_at_point_for_width as usize),
        ("drawInRect:withFont:", draw_in_rect as usize),
        ("drawInRect:withFont:lineBreakMode:", draw_in_rect_mode as usize),
        ("drawInRect:withFont:lineBreakMode:alignment:", draw_in_rect_mode_alignment as usize),
    ]
}

pub fn register() {
    methods(runtime::class_named("NSString"), &selectors());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fw::string;

    const W: usize = 64;
    const H: usize = 64;

    fn setup() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
    }

    fn system_font(size: f64) -> Id {
        let class = runtime::class_named("UIFont");
        let s = runtime::sel("systemFontOfSize:");
        let imp = runtime::imp_for(class, s).expect("+[UIFont systemFontOfSize:]");
        let f: extern "C" fn(Class, Sel, f64) -> Id = unsafe { std::mem::transmute(imp) };
        f(class, s, size)
    }

    /// -[NSString drawInRect:withFont:lineBreakMode:alignment:] into a red-filling
    /// 64x64 bitmap flipped the way cocos2d flips it before UIGraphicsPushContext.
    /// Gives the size it answered and which pixels it painted (rows from the top in memory).
    fn draw(text: &str, rect: Rect, mode: isize, align: isize) -> (Size, Vec<Vec<bool>>) {
        render(text, rect, mode, align, 0, |_| {})
    }

    /// The same with a CGTextDrawingMode, and `after` run on the context
    /// before its pixels are read.
    fn render(text: &str, rect: Rect, mode: isize, align: isize, text_mode: i32, after: impl FnOnce(&mut Canvas)) -> (Size, Vec<Vec<bool>>) {
        setup();
        let context = bitmap_context_create(std::ptr::null_mut(), W, H, 8, 0, space_object(Space::Rgb), PREMULTIPLIED_LAST);
        context_translate_ctm(context, 0.0, H as f64);
        context_scale_ctm(context, 1.0, -1.0);
        context_set_rgb_fill_color(context, 1.0, 0.0, 0.0, 1.0);
        locked(context).unwrap().state.text_mode = text_mode;
        graphics_push_context(context);
        let s = runtime::sel("drawInRect:withFont:lineBreakMode:alignment:");
        let string = string::make(text);
        let imp = runtime::imp_for(string, s).expect("NSString has no drawInRect:withFont:lineBreakMode:alignment:");
        let f: extern "C" fn(Id, Sel, Rect, Id, isize, isize) -> Size = unsafe { std::mem::transmute(imp) };
        let size = f(string, s, rect, system_font(20.0), mode, align);
        graphics_pop_context();
        let mut c = locked(context).unwrap();
        after(&mut c);
        let (bytes_per_row, memory) = (c.bytes_per_row, c.memory().to_vec());
        let painted = (0..H).map(|row| (0..W).map(|x| memory[row * bytes_per_row + x * 4 + 3] > 0).collect()).collect();
        (size, painted)
    }

    fn rows(painted: &[Vec<bool>]) -> Vec<usize> {
        (0..H).filter(|&r| painted[r].iter().any(|&p| p)).collect()
    }

    fn columns(painted: &[Vec<bool>]) -> Vec<usize> {
        (0..W).filter(|&x| painted.iter().any(|row| row[x])).collect()
    }

    #[test]
    fn strings_draw_upright_aligned_and_wrapped_where_uikit_puts_them() {
        let all = Rect::new(0.0, 0.0, W as f64, H as f64);
        // One line at the top, upright: a T's bar (its top painted row) is wider than its stem.
        let (size, painted) = draw("T", all, 0, 0);
        assert!(size.width > 5.0 && size.height >= 20.0 && size.height <= 30.0, "{}x{}", size.width, size.height);
        let used = rows(&painted);
        assert!(!used.is_empty(), "nothing drawn");
        assert!(*used.last().unwrap() < size.height as usize, "drawn below its line: rows {used:?}");
        let count = |r: usize| painted[r].iter().filter(|&&p| p).count();
        assert!(count(used[0]) > count(*used.last().unwrap()) * 2, "T upside down? top {} bottom {}", count(used[0]), count(*used.last().unwrap()));
        assert!(columns(&painted)[0] <= 3, "left aligned starts at the left");
        // Centred and right aligned (UITextAlignmentCenter 1, Right 2).
        let (_, painted) = draw("i", all, 0, 1);
        let x = columns(&painted);
        let middle = (x[0] + x[x.len() - 1]) as f64 / 2.0;
        assert!((middle - W as f64 / 2.0).abs() <= 3.0, "centre {middle}");
        let (_, painted) = draw("i", all, 0, 2);
        let x = columns(&painted);
        assert!(x[x.len() - 1] >= W - 6 && x[0] > W / 2, "right aligned: columns {x:?}");
        // Word wrap: two words too wide for one line make two lines, and
        // sizeWithFont:constrainedToSize:lineBreakMode: answers the same size.
        let (one, _) = draw("ab", all, 0, 0);
        let narrow = Rect::new(0.0, 0.0, one.width + 4.0, H as f64);
        let (two, painted) = draw("ab ab", narrow, 0, 0);
        assert!(two.height >= one.height * 2.0 - 1.0, "one line {} two lines {}", one.height, two.height);
        assert!(*rows(&painted).last().unwrap() as f64 > one.height, "second line drawn");
        let s = runtime::sel("sizeWithFont:constrainedToSize:lineBreakMode:");
        let string = string::make("ab ab");
        let imp = runtime::imp_for(string, s).unwrap();
        let f: extern "C" fn(Id, Sel, Id, Size, isize) -> Size = unsafe { std::mem::transmute(imp) };
        let measured = f(string, s, system_font(20.0), narrow.size, 0);
        assert_eq!((measured.width, measured.height), (two.width, two.height));
    }

    /// 32-bit games reach these methods only through a framework_selectors.tsv row.
    #[test]
    fn every_string_drawing_selector_has_a_32_bit_type_encoding() {
        let table = crate::guest32::objc::selectors::FrameworkSelectors::builtin();
        let missing: Vec<&str> = selectors().iter().map(|&(name, _)| name).filter(|name| table.get(name).is_none()).collect();
        assert!(missing.is_empty(), "no framework_selectors.tsv row for {missing:?}");
    }

    /// A rect one line high wraps first and keeps the first wrapped line
    /// (not the whole paragraph); text never paints outside the rect's width.
    #[test]
    fn rect_text_wraps_before_the_line_limit_and_stays_in_its_width() {
        let (one, _) = draw("one", Rect::new(0.0, 0.0, W as f64, H as f64), 0, 0);
        let short = Rect::new(0.0, 0.0, one.width + 2.0, one.height);
        let (both, _) = draw("one two", short, 0, 0);
        assert_eq!((both.width, both.height), (one.width, one.height), "only \"one\" fits");
        // One letter is always drawn, even wider than the rect: cut at its edge.
        let (_, painted) = draw("W", Rect::new(0.0, 0.0, 6.0, H as f64), 0, 0);
        let x = columns(&painted);
        assert!(!x.is_empty() && *x.last().unwrap() < 6, "painted past the rect: columns {x:?}");
    }

    /// The clip text modes (4-7) add the glyphs to the context's clip;
    /// mode 7 paints nothing itself.
    #[test]
    fn clip_text_modes_clip_to_the_glyphs() {
        let all = Rect::new(0.0, 0.0, W as f64, H as f64);
        let fill_all = |c: &mut Canvas| {
            let mut square = shapes::Outline::default();
            for p in [(0.0, 0.0), (W as f64, 0.0), (W as f64, H as f64), (0.0, H as f64)] {
                square.line_to(p);
            }
            square.close();
            c.fill_outline(&square, false);
        };
        let (_, text) = render("T", all, 0, 0, 7, |_| {});
        assert!(rows(&text).is_empty(), "mode 7 painted");
        let (_, filled) = render("T", all, 0, 0, 7, fill_all);
        let (_, drawn) = draw("T", all, 0, 0);
        let count = |p: &[Vec<bool>]| p.iter().flatten().filter(|&&b| b).count();
        assert!(count(&filled) > 0 && count(&filled) <= count(&drawn) + 8, "fill after mode 7: {} pixels, the T has {}", count(&filled), count(&drawn));
        let (_, filled) = render("T", all, 0, 0, 4, fill_all);
        assert!(count(&filled) <= count(&drawn) + 8, "fill after mode 4: {} pixels", count(&filled));
    }
}
