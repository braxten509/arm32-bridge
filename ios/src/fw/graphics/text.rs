//! UIKit's NSString drawing (UIStringDrawing): text measured and drawn
//! with real fonts into the current UIKit context.
//!
//! The font file is the game's own when the UIFont's name matches one of
//! its Info.plist UIAppFonts (by family, full or PostScript name, or file
//! name), else the system's sans-serif font (its bold for bold names).
//! A character the chosen file has no glyph for comes from the system's
//! font at the same size, as iOS does, so a game's font without "&" still
//! has one. Glyph outlines go through the CTM into the context's path filler
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
    let found = app_fonts().iter().find(|(names, _)| names.contains(&wanted)).map(|(_, face)| *face).or_else(|| system_face(bold_name(name)));
    if found.is_none() {
        crate::log!("font {name}: no font file found; text is measured but not drawn");
    }
    CACHE.lock().unwrap().get_or_insert_with(HashMap::new).insert(name.to_string(), found);
    found
}

/// Whether a UIFont name asks for a bold face.
fn bold_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    ["bold", "medium", "heavy", "black"].iter().any(|w| lower.contains(w))
}

// ----- measuring and line breaking -----

/// A UIFont at its size, measuring in points.
struct Font {
    face: Option<&'static Face<'static>>,
    size: f64,
    /// The name asks for a bold face: letters the chosen file lacks come
    /// from the system's bold font.
    bold: bool,
}

impl Font {
    fn of(font: Id) -> Font {
        let (name, size) = controls::font_name_and_size(font);
        Font { face: face_named(&name), size, bold: bold_name(&name) }
    }

    /// Points per font unit.
    fn scale(&self, face: &Face) -> f64 {
        self.size / f64::from(face.units_per_em().max(1))
    }

    /// The font and glyph that draw `c`: the chosen face's own, else the
    /// system font's (as iOS fills a game font's missing letter in), else
    /// the chosen face's missing-glyph box. Tabs are spaces.
    fn glyph(&self, c: char) -> Option<(&'static Face<'static>, GlyphId)> {
        let c = if c == '\t' { ' ' } else { c };
        let face = self.face?;
        if let Some(id) = face.glyph_index(c) {
            return Some((face, id));
        }
        if let Some(system) = system_face(self.bold) {
            if let Some(id) = system.glyph_index(c) {
                return Some((system, id));
            }
        }
        Some((face, GlyphId(0)))
    }

    fn advance(&self, c: char) -> f64 {
        match self.glyph(c) {
            Some((face, id)) => f64::from(face.glyph_hor_advance(id).unwrap_or(0)) * self.scale(face),
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
    fn draw_text(&mut self, font: &Font, lines: &[Vec<char>], rect: Rect, align: isize) {
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
            let scale = font.face.map_or(0.0, |face| font.scale(face));
            let mut pen = Pen { outline: &mut outline, ctm: self.state.ctm, x, baseline, scale, last: (0.0, 0.0) };
            for &c in line {
                if let Some((face, id)) = font.glyph(c) {
                    pen.scale = font.scale(face);
                    face.outline_glyph(id, &mut pen);
                }
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
    if font.face.is_some() {
        if let Some(mut c) = canvas("-[NSString drawInRect:withFont:]", current_context()) {
            c.draw_text(&font, &lines, rect, align);
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

// ----- labels -----

/// What a UILabel shows (or a UIButton's title): its text drawn in its
/// bounds as -[UILabel drawTextInRect:] does.
#[derive(Clone, Debug, PartialEq)]
pub struct TextLook {
    pub text: String,
    /// UIFont name and point size.
    pub font: (String, f64),
    /// Straight red, green, blue, alpha.
    pub color: [f64; 4],
    /// Shadow colour and offset in points (right, down).
    pub shadow: Option<([f64; 4], (f64, f64))>,
    /// NSTextAlignment (left, centre, right; others draw left).
    pub alignment: isize,
    /// NSLineBreakMode.
    pub line_break: isize,
    /// numberOfLines (0: as many as fit).
    pub lines: isize,
    /// The bounds' size in points.
    pub size: (f64, f64),
    /// Set when the font shrinks to fit a single line's width
    /// (adjustsFontSizeToFitWidth): the smallest size it may take.
    pub minimum_font_size: Option<f64>,
}

impl Font {
    fn named(name: &str, size: f64) -> Font {
        Font { face: face_named(name), size, bold: bold_name(name) }
    }
}

/// The font a label draws with (smaller when it shrinks to fit) and the
/// lines it shows: one cut as its line break mode says, or wrapped, no
/// more than numberOfLines or than fit its height.
fn label_lines(look: &TextLook) -> (Font, Vec<Vec<char>>) {
    let (width, height) = look.size;
    let single_line = look.lines == 1;
    let mut font = Font::named(&look.font.0, look.font.1);
    if let (true, Some(minimum)) = (single_line, look.minimum_font_size) {
        let first: Vec<char> = look.text.split('\n').next().unwrap_or("").chars().collect();
        let wide = font.width(&first);
        if wide > width && width > 0.0 {
            // Widths grow with the size; a hair under so it surely fits.
            font.size = (font.size * width / wide * 0.999).max(minimum.max(1.0)).min(font.size);
        }
    }
    let fit = lines_in(&font, height);
    let max_lines = match (look.lines.max(0) as usize, fit) {
        (0, fit) => fit,
        (wanted, 0) => wanted,
        (wanted, fit) => wanted.min(fit),
    };
    let lines = lay_out(&font, &look.text, width, single_line, max_lines, look.line_break);
    (font, lines)
}

/// The size `text` takes in a font (name, points) no wider than `width`, on
/// at most `lines` lines (1: one line cut by `mode`; 0: any number), as
/// UILabel sizes it to fit.
pub fn text_box(text: &str, font: &(String, f64), width: f64, lines: isize, mode: isize) -> (f64, f64) {
    let font = Font::named(&font.0, font.1);
    let laid = lay_out(&font, text, width, lines == 1, lines.max(0) as usize, mode);
    let size = size_of(&font, text, &laid, width);
    (size.width, size.height)
}

/// A look drawn at `scale` pixels per point: width, height and straight
/// RGBA rows from the top. The lines sit in the middle of the bounds up and
/// down, the shadow under the text; nothing is drawn outside the bounds.
fn draw_look(look: &TextLook, scale: f64) -> Option<(usize, usize, Vec<u8>)> {
    let bitmap = look_bitmap(look, scale)?;
    let rgba = bitmap.straight_rgba()?;
    Some((bitmap.width, bitmap.height, rgba))
}

/// The same as a picture (rows from the top) to draw into a context.
pub(super) fn look_bitmap(look: &TextLook, scale: f64) -> Option<Bitmap> {
    let (width, height) = ((look.size.0 * scale).ceil(), (look.size.1 * scale).ceil());
    if !(width >= 1.0 && height >= 1.0 && width * height <= 16e6) {
        return None;
    }
    let (font, lines) = label_lines(look);
    font.face?;
    let mut canvas = Canvas::new(std::ptr::null_mut(), width as usize, height as usize, 8, 0, Space::Rgb, PREMULTIPLIED_LAST).ok()?;
    // UIKit's flip: (0, 0) at the top left, `scale` pixels per point.
    canvas.state.ctm = Transform { a: scale, b: 0.0, c: 0.0, d: -scale, tx: 0.0, ty: height };
    let block = lines.len() as f64 * font.line_height();
    let rect = Rect::new(0.0, (look.size.1 - block) / 2.0, look.size.0, block);
    if let Some((color, (right, down))) = look.shadow {
        canvas.state.fill = color;
        canvas.draw_text(&font, &lines, Rect::new(rect.origin.x + right, rect.origin.y + down, rect.size.width, rect.size.height), look.alignment);
    }
    canvas.state.fill = look.color;
    canvas.draw_text(&font, &lines, rect, look.alignment);
    Some(canvas.snapshot())
}

/// The text picture files made so far, least recently used first.
#[derive(Default)]
struct TextFiles {
    made: Vec<(TextLook, u64, Option<String>)>,
}

impl TextFiles {
    /// The files for one screen's looks (made by `make` the first time);
    /// beyond `keep` files the least recently used ones not on this screen
    /// are deleted.
    fn files(&mut self, looks: &[&TextLook], scale: f64, keep: usize, make: impl Fn(&TextLook, f64) -> Option<String>) -> Vec<Option<String>> {
        let files = looks
            .iter()
            .map(|&look| {
                let entry = match self.made.iter().position(|(seen, seen_scale, _)| seen == look && *seen_scale == scale.to_bits()) {
                    Some(at) => self.made.remove(at),
                    None => (look.clone(), scale.to_bits(), make(look, scale)),
                };
                let file = entry.2.clone();
                self.made.push(entry);
                file
            })
            .collect();
        self.trim(keep, scale, looks);
        files
    }

    /// The files for one screen's looks with the not-yet-made ones drawn by
    /// `draw` (in parallel, a few at a time); beyond `keep` files the least
    /// recently used ones not on this screen are deleted.
    fn files_parallel(&mut self, looks: &[&TextLook], scale: f64, keep: usize, draw: &(dyn Fn(&TextLook, f64) -> Option<String> + Sync)) -> Vec<Option<String>> {
        // One slot per look: an already-made file, or the new look's index.
        enum Slot {
            Have(Option<String>),
            New(usize),
        }
        let mut slots: Vec<Slot> = Vec::with_capacity(looks.len());
        let mut new: Vec<TextLook> = Vec::new();
        for &look in looks {
            // A look shown twice on the screen shares the one being drawn.
            if let Some(at) = new.iter().position(|pending| pending == look) {
                slots.push(Slot::New(at));
                continue;
            }
            match self.made.iter().position(|(seen, seen_scale, _)| seen == look && *seen_scale == scale.to_bits()) {
                Some(at) => {
                    let entry = self.made.remove(at);
                    let file = entry.2.clone();
                    self.made.push(entry);
                    slots.push(Slot::Have(file));
                }
                None => {
                    let at = new.len();
                    new.push(look.clone());
                    self.made.push((look.clone(), scale.to_bits(), None));
                    slots.push(Slot::New(at));
                }
            }
        }
        let drawn = draw_in_parallel(&new, scale, draw);
        // The new looks keep the picture they drew.
        for (look, file) in new.iter().zip(&drawn) {
            if let Some(entry) = self.made.iter_mut().find(|(seen, seen_scale, _)| seen == look && *seen_scale == scale.to_bits()) {
                entry.2 = file.clone();
            }
        }
        let files = slots
            .into_iter()
            .map(|slot| match slot {
                Slot::Have(file) => file,
                Slot::New(at) => drawn[at].clone(),
            })
            .collect();
        self.trim(keep, scale, looks);
        files
    }

    /// Deletes the least recently used files beyond `keep` that are not on
    /// this screen (the entries at the front of `made`).
    fn trim(&mut self, keep: usize, scale: f64, looks: &[&TextLook]) {
        // The screen's own entries were all moved to the end.
        let unused = self
            .made
            .iter()
            .take_while(|(seen, seen_scale, _)| !(*seen_scale == scale.to_bits() && looks.contains(&seen)))
            .count();
        let extra = self.made.len().saturating_sub(keep).min(unused);
        for (_, _, file) in self.made.drain(..extra) {
            if let Some(old) = file {
                let _ = std::fs::remove_file(old);
            }
        }
    }
}

/// Every look drawn, a few at a time on worker threads, in order.
fn draw_in_parallel(looks: &[TextLook], scale: f64, draw: &(dyn Fn(&TextLook, f64) -> Option<String> + Sync)) -> Vec<Option<String>> {
    if looks.is_empty() {
        return Vec::new();
    }
    let workers = looks.len().min(4);
    let chunk = looks.len().div_ceil(workers);
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|worker| {
                let start = (worker * chunk).min(looks.len());
                let end = ((worker + 1) * chunk).min(looks.len());
                scope.spawn(move || looks[start..end].iter().map(|look| draw(look, scale)).collect::<Vec<_>>())
            })
            .collect();
        handles.into_iter().flat_map(|handle| handle.join().unwrap()).collect()
    })
}

/// PNG files showing one screen's `looks` at `scale` pixels per point
/// (fw/pictures.rs), each made once; None for a look there is nothing to
/// draw with.
pub fn text_pictures(looks: &[&TextLook], scale: f64) -> Vec<Option<String>> {
    static MADE: Mutex<TextFiles> = Mutex::new(TextFiles { made: Vec::new() });
    let mut made = MADE.lock().unwrap_or_else(|e| e.into_inner());
    let draw = |look: &TextLook, scale: f64| {
        draw_look(look, scale)
            .and_then(|(width, height, rgba)| encode_png(width, height, &rgba, true))
            .and_then(|bytes| write_png_file(&format!("text \"{}\"", look.text), &bytes))
    };
    made.files_parallel(looks, scale, 256, &draw)
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

    fn look(text: &str, size: (f64, f64)) -> TextLook {
        TextLook {
            text: text.into(),
            font: ("Helvetica".into(), 20.0),
            color: [1.0, 1.0, 1.0, 1.0],
            shadow: None,
            alignment: 0,
            line_break: TAIL_TRUNCATION,
            lines: 1,
            size,
            minimum_font_size: None,
        }
    }

    /// Straight RGBA pixels of a look drawn at a scale.
    fn drawn(look: &TextLook, scale: f64) -> (usize, usize, Vec<u8>) {
        setup();
        draw_look(look, scale).expect("drawn")
    }

    /// The painted pixels' bounds (left, top, right, bottom) for pixels `pick` chooses.
    fn bounds(width: usize, pixels: &[u8], pick: impl Fn(&[u8]) -> bool) -> Option<(usize, usize, usize, usize)> {
        let found: Vec<(usize, usize)> = pixels.chunks(4).enumerate().filter(|(_, p)| pick(p)).map(|(i, _)| (i % width, i / width)).collect();
        let xs = found.iter().map(|p| p.0);
        let ys = found.iter().map(|p| p.1);
        Some((xs.clone().min()?, ys.clone().min()?, xs.max()?, ys.max()?))
    }

    /// UILabel's text: in the middle of its bounds up and down, aligned
    /// across, in its colour, its shadow under it at the offset; at the
    /// screen's scale.
    #[test]
    fn labels_draw_their_text_centred_with_its_colour_and_shadow() {
        let mut centred = look("T", (64.0, 64.0));
        centred.alignment = CENTER;
        let (width, height, pixels) = drawn(&centred, 1.0);
        assert_eq!((width, height), (64, 64));
        let (left, top, right, bottom) = bounds(width, &pixels, |p| p[3] > 0).expect("text drawn");
        assert!(top >= 18 && bottom <= 46, "not in the middle up and down: rows {top}-{bottom}");
        assert!(((left + right) as f64 / 2.0 - 32.0).abs() <= 3.0, "not centred: columns {left}-{right}");
        let solid = pixels.chunks(4).find(|p| p[3] == 255).expect("a solid pixel");
        assert_eq!(&solid[..3], &[255, 255, 255], "white text");
        // The shadow: red, 3 points right and down.
        centred.shadow = Some(([1.0, 0.0, 0.0, 1.0], (3.0, 3.0)));
        let (_, _, pixels) = drawn(&centred, 1.0);
        let white = bounds(width, &pixels, |p| p[3] == 255 && p[1] > 200).unwrap();
        let red = bounds(width, &pixels, |p| p[3] == 255 && p[0] > 200 && p[1] < 50).expect("shadow drawn");
        assert!(red.2.abs_diff(white.2 + 3) <= 1 && red.3.abs_diff(white.3 + 3) <= 1, "shadow {red:?} text {white:?}");
        // Twice the pixels at scale 2.
        let (width, height, _) = drawn(&centred, 2.0);
        assert_eq!((width, height), (128, 128));
    }

    /// Lines: one line is cut ("…"), any number wraps; a label that adjusts
    /// its font to fit draws smaller instead of cutting.
    #[test]
    fn labels_wrap_cut_or_shrink_their_text() {
        let rows = |look: &TextLook| {
            let (width, _, pixels) = drawn(look, 1.0);
            bounds(width, &pixels, |p| p[3] > 0).map(|(_, top, _, bottom)| bottom - top).unwrap()
        };
        let one = look("ab ab ab ab", (40.0, 100.0));
        let mut many = one.clone();
        many.lines = 0;
        assert!(rows(&many) > rows(&one) * 2, "wrapped: {} rows, one line {}", rows(&many), rows(&one));
        let mut shrinking = look("WWWWWWWW", (40.0, 64.0));
        let cut = rows(&shrinking);
        shrinking.minimum_font_size = Some(4.0);
        assert!(rows(&shrinking) * 2 < cut, "shrunk to fit: {} rows, cut {cut}", rows(&shrinking));
    }

    /// Every look on screen keeps its file, however many there are, and is
    /// drawn only once; files no longer shown go when there are too many.
    #[test]
    fn shown_text_keeps_its_picture_files() {
        let folder = std::env::temp_dir().join(format!("text-files-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let drawn = std::cell::Cell::new(0);
        let make = |look: &TextLook, _: f64| {
            drawn.set(drawn.get() + 1);
            let file = folder.join(&look.text).to_string_lossy().into_owned();
            std::fs::write(&file, b"png").ok().map(|()| file)
        };
        let looks: Vec<TextLook> = (0..5).map(|n| look(&format!("label {n}"), (10.0, 10.0))).collect();
        let shown: Vec<&TextLook> = looks.iter().collect();
        let mut files = TextFiles::default();
        let first = files.files(&shown, 2.0, 4, make);
        assert!(first.iter().all(|f| f.as_ref().is_some_and(|f| std::path::Path::new(f).exists())), "a shown label's file was deleted: {first:?}");
        assert_eq!(files.files(&shown, 2.0, 4, make), first, "the same screen again uses the same files");
        assert_eq!(drawn.get(), 5, "each label drawn once");
        // A new screen with one label: the old ones beyond 4 files go, oldest first.
        let other = look("other", (10.0, 10.0));
        files.files(&[&other], 2.0, 4, make);
        let kept: Vec<bool> = first.iter().map(|f| std::path::Path::new(f.as_ref().unwrap()).exists()).collect();
        assert_eq!(kept, [false, false, true, true, true]);
        let _ = std::fs::remove_dir_all(&folder);
    }

    /// The PNG file a label draws (the serial and parallel paths both use it).
    fn label_png(look: &TextLook, scale: f64) -> Option<String> {
        draw_look(look, scale)
            .and_then(|(width, height, rgba)| encode_png(width, height, &rgba, true))
            .and_then(|bytes| write_png_file(&format!("text \"{}\"", look.text), &bytes))
    }

    /// A label drawn once in the serial order and once by the parallel
    /// worker threads comes out byte for byte the same.
    #[test]
    fn parallel_and_serial_text_pictures_match() {
        setup();
        let looks: Vec<TextLook> = (0..5).map(|n| look(&format!("label {n}"), (48.0, 24.0))).collect();
        // The first label is on the screen twice.
        let shown: Vec<&TextLook> = looks.iter().chain(&looks[..1]).collect();
        let mut serial = TextFiles::default();
        let serial_files = serial.files(&shown, 2.0, 256, label_png);
        let mut parallel = TextFiles::default();
        let parallel_files = parallel.files_parallel(&shown, 2.0, 256, &label_png);
        assert_eq!(serial_files.len(), parallel_files.len());
        for (serial, parallel) in serial_files.iter().zip(&parallel_files) {
            match (serial, parallel) {
                (Some(a), Some(b)) => assert_eq!(std::fs::read(a).unwrap(), std::fs::read(b).unwrap(), "the same label drew different pictures"),
                _ => assert_eq!(serial, parallel, "serial and parallel presence differ"),
            }
        }
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

    /// A letter the game's own font has no glyph for is measured with the
    /// system font at the same point size, the way iOS draws it, not as the
    /// game font's missing-glyph box.
    #[test]
    fn a_letter_the_game_font_lacks_is_measured_with_the_system_font() {
        let path = "/usr/share/tessdata/pdf.ttf";
        if !std::path::Path::new(path).is_file() {
            eprintln!("skipped: {path} (a nearly glyphless font) is not installed");
            return;
        }
        let face = load(path).expect("pdf.ttf parses as a font");
        assert!(face.glyph_index('&').is_none(), "pdf.ttf unexpectedly has an &");
        let font = Font { face: Some(face), size: 20.0, bold: false };
        let system = system_face(false).expect("a system font file");
        let id = system.glyph_index('&').expect("the system font's &");
        let expected = f64::from(system.glyph_hor_advance(id).unwrap_or(0)) * font.scale(system);
        assert!(expected > 1.0, "the system font gives & no width");
        let measured = font.width(&['&']);
        let boxed = f64::from(face.glyph_hor_advance(GlyphId(0)).unwrap_or(0)) * font.scale(face);
        assert!((measured - expected).abs() < 1e-9, "& measured {measured}, the system font's {expected} (the game font's box measures {boxed})");
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
