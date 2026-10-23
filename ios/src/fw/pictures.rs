//! The game's UIKit pictures on the phone screen.
//!
//! Games draw their world with OpenGL, but some put UIKit views over it
//! (start buttons, splash and loading pictures) or make whole menus of
//! them. Our layer has no UIKit drawing of its own, so the phone app draws
//! them: up to 20 times a second the main thread walks the game's windows,
//! lists every picture that shows (where, how big, how see-through), and
//! sends the list when it changed. Pictures under a video's view are left
//! out, since it covers them as on iOS.
//!
//! What is shown: picture views (UIImageView and kinds of it), labels'
//! text, and buttons' state pictures and titles (laid out in
//! controls::button_parts). Text is drawn into a PNG at the phone's pixels
//! (graphics::text_picture). A stretchable picture (cap insets) filling
//! its area is sent as nine parts, so its caps keep their size.
//!
//! Places follow UIKit: each view sits at its centre in its parent, turned
//! and scaled by its transform, its bounds shifting what is inside it; so
//! sideways launch pictures in older games come out right. Clipping and
//! background colours are not drawn; picture files are PNGs as UIImage
//! reads them here. Pictures made in memory (or turned photos) are shown
//! through an upright PNG made once.

use crate::fw::geometry::{Point, Rect, Transform, IDENTITY};
use crate::fw::graphics::TextLook;
use crate::fw::uikit::{property_get, view_state};
use crate::objc::runtime::{self, Class, Id};
use std::sync::{Mutex, OnceLock};

/// One picture to draw.
#[derive(Clone, Debug, PartialEq)]
pub struct Picture {
    pub path: String,
    /// Maps the view's own area, (0, 0) to `area`, to screen points.
    pub place: Transform,
    /// The view's size in its own points.
    pub area: (f64, f64),
    pub alpha: f64,
    /// UIViewContentMode: 0 fills the area, 1 fits inside it, 2 fills it
    /// and crops; others put the picture at its own size in the middle.
    pub mode: i64,
    /// The picture's own size in points.
    pub size: (f64, f64),
    /// Text drawn into a file when shown (labels, button titles; see `resolve`).
    pub text: Option<TextLook>,
    /// The screen area it may draw in, in screen points: the bounds of the
    /// views around it that clip to their bounds, else the whole screen.
    pub clip: Rect,
}

/// A picture a view shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Shown {
    pub path: String,
    /// UIViewContentMode, as in `Picture`.
    pub mode: i64,
    /// The picture's own size in points.
    pub size: (f64, f64),
    /// Where in the view, in points from its visible top-left corner.
    pub rect: Rect,
    /// A stretchable picture's caps in points (top, left, bottom, right):
    /// filling its rect, they keep their size and the middle stretches.
    pub caps: Option<[f64; 4]>,
    /// The parts between the caps are repeated at their size, not stretched
    /// (UIImageResizingModeTile).
    pub tile: bool,
    /// Text to show instead of a picture file (`path` is then empty).
    pub text: Option<TextLook>,
    /// How see-through it is drawn, on top of the view's own alpha (dimmed pictures).
    pub alpha: f64,
}

/// A view as far as drawing pictures goes.
#[derive(Default)]
pub struct Node {
    pub frame: Rect,
    pub bounds_origin: Point,
    pub transform: Option<Transform>,
    pub hidden: bool,
    pub alpha: f64,
    /// Its pictures, back to front.
    pub pictures: Vec<Shown>,
    /// It shows a video: what is under it is covered (on iOS also before and
    /// after the video plays, while the view is there).
    pub video: bool,
    /// It shows the GL picture, which is drawn under every UIKit picture:
    /// what is behind it is covered.
    pub gl: bool,
    /// Its background colour (straight red, green, blue, alpha).
    pub background: Option<[f64; 4]>,
    /// It clips what it holds to its bounds (clipsToBounds).
    pub clips: bool,
    pub children: Vec<Node>,
}

/// `first` then `second` (CGAffineTransformConcat order: x' = a x + c y + tx).
fn then(first: Transform, second: Transform) -> Transform {
    Transform {
        a: first.a * second.a + first.b * second.c,
        b: first.a * second.b + first.b * second.d,
        c: first.c * second.a + first.d * second.c,
        d: first.c * second.b + first.d * second.d,
        tx: first.tx * second.a + first.ty * second.c + second.tx,
        ty: first.tx * second.b + first.ty * second.d + second.ty,
    }
}

fn moved(x: f64, y: f64) -> Transform {
    Transform { tx: x, ty: y, ..IDENTITY }
}

/// The pictures under `node` (a window, at the screen's top-left), back to front.
pub fn flatten(node: &Node) -> Vec<Picture> {
    let mut out = Vec::new();
    walk(node, None, 1.0, Rect { origin: Point::default(), size: node.frame.size }, &mut out);
    out
}

/// The screen area (axis-aligned) a rect in some view's own points covers.
fn screen_box(place: Transform, rect: Rect) -> Rect {
    let corners = [(0.0, 0.0), (rect.size.width, 0.0), (0.0, rect.size.height), (rect.size.width, rect.size.height)]
        .map(|(x, y)| (rect.origin.x + x, rect.origin.y + y))
        .map(|(x, y)| (place.a * x + place.c * y + place.tx, place.b * x + place.d * y + place.ty));
    let (left, right) = (corners.iter().map(|c| c.0).fold(f64::INFINITY, f64::min), corners.iter().map(|c| c.0).fold(f64::NEG_INFINITY, f64::max));
    let (top, bottom) = (corners.iter().map(|c| c.1).fold(f64::INFINITY, f64::min), corners.iter().map(|c| c.1).fold(f64::NEG_INFINITY, f64::max));
    Rect::new(left, top, right - left, bottom - top)
}

/// Where two areas meet (no size when they don't).
fn meet(a: Rect, b: Rect) -> Rect {
    let (left, top) = (a.origin.x.max(b.origin.x), a.origin.y.max(b.origin.y));
    let right = (a.origin.x + a.size.width).min(b.origin.x + b.size.width);
    let bottom = (a.origin.y + a.size.height).min(b.origin.y + b.size.height);
    Rect::new(left, top, (right - left).max(0.0), (bottom - top).max(0.0))
}

/// `parent`: the parent's own points to screen points (None for the window);
/// `clip`: the screen area the node may draw in.
fn walk(node: &Node, parent: Option<Transform>, parent_alpha: f64, clip: Rect, out: &mut Vec<Picture>) {
    let alpha = parent_alpha * node.alpha;
    if node.hidden || alpha <= 0.01 {
        return;
    }
    let size = node.frame.size;
    // The view's own points (its bounds) to screen points.
    let own = match parent {
        None => moved(-node.bounds_origin.x, -node.bounds_origin.y),
        Some(parent) => {
            let centre = (node.frame.origin.x + size.width / 2.0, node.frame.origin.y + size.height / 2.0);
            let turn = node.transform.unwrap_or(IDENTITY);
            let to_middle = moved(-node.bounds_origin.x - size.width / 2.0, -node.bounds_origin.y - size.height / 2.0);
            then(then(then(to_middle, turn), moved(centre.0, centre.1)), parent)
        }
    };
    if node.video || node.gl {
        out.clear();
    }
    let visible = Rect { origin: node.bounds_origin, size };
    let clip = if node.clips { meet(clip, screen_box(own, visible)) } else { clip };
    // The background colour first, under what the view shows (the GL picture covers a GL view's).
    let background = node.background.filter(|color| color[3] > 0.0 && !node.gl).map(|color| Shown {
        path: color_path(color),
        mode: 0,
        size: (1.0, 1.0),
        rect: Rect { origin: Point::default(), size },
        caps: None,
        tile: false,
        text: None,
        alpha: 1.0,
    });
    for shown in background.iter().chain(&node.pictures) {
        for (path, rect, picture_size, cut) in parts(shown) {
            if rect.size.width > 0.0 && rect.size.height > 0.0 {
                let place = then(moved(node.bounds_origin.x + rect.origin.x, node.bounds_origin.y + rect.origin.y), own);
                // A tile reaching past its stretch is cut off at its end.
                let clip = match cut {
                    Some(area) => meet(clip, screen_box(own, Rect { origin: Point { x: node.bounds_origin.x + area.origin.x, y: node.bounds_origin.y + area.origin.y }, size: area.size })),
                    None => clip,
                };
                let seen = meet(screen_box(place, Rect { origin: Point::default(), size: rect.size }), clip);
                if seen.size.width <= 0.0 || seen.size.height <= 0.0 {
                    continue; // wholly clipped away
                }
                let text = shown.text.clone();
                let alpha = alpha * shown.alpha;
                out.push(Picture { path, place, area: (rect.size.width, rect.size.height), alpha, mode: shown.mode, size: picture_size, text, clip });
            }
        }
    }
    for child in &node.children {
        walk(child, Some(own), alpha, clip, out);
    }
}

/// Most copies of a tiling part one way; more are drawn stretched instead.
const MOST_TILES: f64 = 64.0;

/// What a picture is drawn as: itself in its rect, or, stretchable and
/// filling it (mode 0), its nine parts (rows from the top), each with its
/// rect and its own size. Caps longer than the rect shrink to share it.
/// A tiling picture's parts between the caps are copies at their own size,
/// each with the area it may draw in (the last ones are cut off there).
fn parts(shown: &Shown) -> Vec<(String, Rect, (f64, f64), Option<Rect>)> {
    let caps = shown.caps.filter(|caps| shown.mode == 0 && caps.iter().any(|&cap| cap > 0.0));
    let Some([top, left, bottom, right]) = caps else { return vec![(shown.path.clone(), shown.rect, shown.size, None)] };
    /// Start, length and source length of the pieces of one side's three
    /// parts, with the part each belongs to and the stretch it fills.
    fn side(start: f64, length: f64, source: f64, first: f64, last: f64, tile: bool) -> Vec<(usize, f64, f64, f64, (f64, f64))> {
        let shrink = if first + last > length { length / (first + last) } else { 1.0 };
        let (shown_first, shown_last) = (first * shrink, last * shrink);
        let (middle_start, middle_length, middle_source) = (start + shown_first, length - shown_first - shown_last, (source - first - last).max(0.0));
        let mut pieces = vec![(0, start, shown_first, first, (start, shown_first))];
        let copies = if middle_source > 0.0 { (middle_length / middle_source).ceil() } else { 0.0 };
        if tile && copies >= 1.0 && copies <= MOST_TILES {
            let area = (middle_start, middle_length);
            pieces.extend((0..copies as usize).map(|k| (1, middle_start + k as f64 * middle_source, middle_source, middle_source, area)));
        } else {
            pieces.push((1, middle_start, middle_length, middle_source, (middle_start, middle_length)));
        }
        pieces.push((2, start + length - shown_last, shown_last, last, (start + length - shown_last, shown_last)));
        pieces
    }
    let rect = shown.rect;
    let columns = side(rect.origin.x, rect.size.width, shown.size.0, left, right, shown.tile);
    let rows = side(rect.origin.y, rect.size.height, shown.size.1, top, bottom, shown.tile);
    let mut out = Vec::new();
    for &(row, y, height, source_height, (area_y, area_height)) in &rows {
        for &(column, x, width, source_width, (area_x, area_width)) in &columns {
            if width > 0.0 && height > 0.0 && source_width > 0.0 && source_height > 0.0 {
                let piece = Rect::new(x, y, width, height);
                let area = Rect::new(area_x, area_y, area_width, area_height);
                let cut = (piece != area && (x + width > area_x + area_width || y + height > area_y + area_height)).then_some(area);
                out.push((format!("{}{PART_MARK}{}", shown.path, row * 3 + column), piece, (source_width, source_height), cut));
            }
        }
    }
    out
}

/// The UIKit classes that show something.
struct Classes {
    image_view: Class,
    label: Class,
    button: Class,
    text_field: Class,
    table: Class,
    cell: Class,
    gl_layer: Class,
}

impl Classes {
    fn find() -> Classes {
        Classes {
            image_view: runtime::class_named("UIImageView"),
            label: runtime::class_named("UILabel"),
            button: runtime::class_named("UIButton"),
            text_field: runtime::class_named("UITextField"),
            table: runtime::class_named("UITableView"),
            cell: runtime::class_named("UITableViewCell"),
            gl_layer: runtime::class_named("CAEAGLLayer"),
        }
    }

    fn is(view: Id, class: Class) -> bool {
        let rt = runtime::lock();
        class != 0 && rt.is_subclass(rt.class_of(view), class)
    }
}

/// A UIImage shown in `rect` with a content mode (None for no image).
fn image_shown(image: Id, mode: i64, rect: Rect) -> Option<Shown> {
    let info = crate::fw::uikit::image_info(image)?;
    let size = crate::fw::uikit::image_points(image);
    // The file is found (or made) only for pictures that end up shown; see `resolve`.
    Some(Shown { path: format!("{IMAGE_PREFIX}{image:x}"), mode, size: (size.width, size.height), rect, caps: info.caps, tile: false, text: None, alpha: 1.0 })
}

/// How see-through a disabled button's pictures are drawn.
const DIMMED: f64 = 0.5;

/// A button's picture as its state draws it: a darker copy when pressed,
/// dimmed when disabled.
fn shaded(image: Id, shade: crate::fw::controls::Shade, rect: Rect) -> Option<Shown> {
    if !shade.darker && shade.darker_when_pressed && image != 0 {
        // A press may last one frame: the darker copy and its files are made now.
        let darker = crate::fw::graphics::darker_image(image);
        match crate::fw::uikit::image_info(darker).and_then(|info| info.caps) {
            Some(_) => drop(crate::fw::graphics::picture_part_file(darker, 0)),
            None => drop(crate::fw::graphics::picture_file(darker)),
        }
    }
    let darker = if shade.darker { crate::fw::graphics::darker_image(image) } else { 0 };
    let mut shown = image_shown(if darker != 0 { darker } else { image }, 0, rect)?;
    if shade.dimmed {
        shown.alpha = DIMMED;
    }
    Some(shown)
}

/// Text drawn over `rect` (None for no text).
fn text_shown(look: Option<TextLook>, rect: Rect) -> Option<Shown> {
    let look = look?;
    Some(Shown { path: String::new(), mode: 0, size: look.size, rect, caps: None, tile: false, text: Some(look), alpha: 1.0 })
}

/// The view tree under `view` as `flatten` reads it (main thread).
fn snapshot(view: Id, classes: &Classes, depth: usize) -> Node {
    if Classes::is(view, classes.table) {
        crate::fw::tables::ensure_rows(view);
    }
    let state = view_state(view);
    let bounds = Rect { origin: Point::default(), size: state.frame.size };
    let mut pictures = Vec::new();
    if Classes::is(view, classes.image_view) {
        let mode = property_get(view, "contentMode").unwrap_or(0) as i64;
        pictures.extend(image_shown(property_get(view, "image").unwrap_or(0) as Id, mode, bounds));
    }
    if Classes::is(view, classes.label) {
        pictures.extend(text_shown(crate::fw::controls::label_look(view), bounds));
    }
    if Classes::is(view, classes.button) {
        let parts = crate::fw::controls::button_parts(view);
        pictures.extend(shaded(parts.background, parts.background_shade, bounds));
        if let Some((image, rect)) = parts.image {
            pictures.extend(shaded(image, parts.image_shade, rect));
        }
        if let Some((look, rect)) = parts.title {
            pictures.extend(text_shown(Some(look), rect));
        }
    }
    let mut background = crate::fw::graphics::color_rgba(property_get(view, "background").unwrap_or(0) as Id);
    if Classes::is(view, classes.text_field) {
        if let Some((look, rect)) = crate::fw::typing::text_field_look(view) {
            pictures.extend(text_shown(Some(look), rect));
        }
        // A field with a bezel or rounded border is white inside.
        if background.is_none() && matches!(property_get(view, "borderStyle"), Some(2 | 3)) {
            background = Some([1.0, 1.0, 1.0, 1.0]);
        }
    }
    if Classes::is(view, classes.cell) {
        background = crate::fw::tables::cell_fill(view).or(background);
    }
    Node {
        frame: state.frame,
        bounds_origin: state.bounds_origin,
        transform: Some(state.transform),
        hidden: property_get(view, "hidden").unwrap_or(0) != 0,
        alpha: property_get(view, "alpha").map(f64::from_bits).unwrap_or(1.0),
        pictures,
        video: crate::fw::media::shows_video(view),
        gl: state.layer != 0 && Classes::is(state.layer, classes.gl_layer),
        background,
        clips: property_get(view, "clips").unwrap_or(0) != 0,
        children: if depth < 64 { state.subviews.iter().map(|&child| snapshot(child, classes, depth + 1)).collect() } else { Vec::new() },
    }
}

/// Stands for a plain colour in a picture path until `resolve`.
const COLOR_PREFIX: &str = "\u{1}color:";

/// The path standing for a colour (straight red, green, blue, alpha).
fn color_path(rgba: [f64; 4]) -> String {
    format!("{COLOR_PREFIX}{},{},{},{}", rgba[0], rgba[1], rgba[2], rgba[3])
}

/// Stands for a UIImage in a snapshot's picture path until `resolve`.
const IMAGE_PREFIX: &str = "\u{1}uiimage:";

/// After a picture's path: one of a stretchable picture's nine parts
/// (0-8, rows from the top), each its own file (see graphics::picture_part_file).
const PART_MARK: &str = "\u{1}part:";

/// Pixels per point on the phone's screen (the screen's scale, or more
/// when the display is denser), so text is drawn sharp.
fn pixels_per_point() -> f64 {
    let (_, high, scale) = crate::fw::uikit::screen_points();
    let display = crate::fw::uikit::display().map_or(0.0, |(w, h)| w.min(h) / high);
    scale.max(display).min(4.0)
}

/// Turns the shown pictures' UIImages and text into files the phone app
/// can draw: the image's own file, or an upright PNG of a picture made in
/// memory or a turned photo, or one of a stretchable picture's parts; text
/// is drawn into a PNG. Pictures that can't be made into a file are left out.
fn resolve(pictures: Vec<Picture>) -> Vec<Picture> {
    // All the screen's text at once, so none of its files is let go.
    let looks: Vec<&crate::fw::graphics::TextLook> = pictures.iter().filter_map(|p| p.text.as_ref()).collect();
    let mut text_files = crate::fw::graphics::text_pictures(&looks, pixels_per_point()).into_iter();
    pictures
        .into_iter()
        .filter_map(|mut p| {
            if p.text.is_some() {
                p.path = text_files.next()??;
                return Some(p);
            }
            if let Some(color) = p.path.strip_prefix(COLOR_PREFIX) {
                let parts: Vec<f64> = color.split(',').filter_map(|part| part.parse().ok()).collect();
                p.path = crate::fw::graphics::color_picture(parts.try_into().ok()?)?;
                return Some(p);
            }
            let Some(id) = p.path.strip_prefix(IMAGE_PREFIX) else { return Some(p) };
            p.path = match id.split_once(PART_MARK) {
                Some((id, part)) => crate::fw::graphics::picture_part_file(usize::from_str_radix(id, 16).ok()?, part.parse().ok()?)?,
                None => crate::fw::graphics::picture_file(usize::from_str_radix(id, 16).ok()?)?,
            };
            Some(p)
        })
        .collect()
}

/// The phone app's side: draws the list it's given (see `encode`).
pub struct PictureScreen {
    pub show: fn(&str),
}

static SCREEN: OnceLock<PictureScreen> = OnceLock::new();

pub fn set_picture_screen(screen: PictureScreen) {
    let _ = SCREEN.set(screen);
}

/// Whether there is a phone screen to draw pictures on (the main loop then wakes often enough).
pub fn active() -> bool {
    screen().is_some()
}

/// The phone's screen; on a PC run with IOS_FRAMES_DIR, the lists are kept
/// in that folder instead (pictures-NNNNN.txt) to see what it would show.
fn screen() -> Option<&'static PictureScreen> {
    static FILE_SCREEN: OnceLock<Option<PictureScreen>> = OnceLock::new();
    SCREEN.get().or_else(|| {
        FILE_SCREEN.get_or_init(|| std::env::var("IOS_FRAMES_DIR").ok().map(|_| PictureScreen { show: save_list })).as_ref()
    })
}

fn save_list(text: &str) {
    if let Ok(folder) = std::env::var("IOS_FRAMES_DIR") {
        save_list_in(&folder, text);
    }
}

fn save_list_in(folder: &str, text: &str) {
    static NUMBER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let number = NUMBER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    let path = format!("{folder}/pictures-{number:05}.txt");
    let _ = std::fs::create_dir_all(folder);
    if let Err(error) = std::fs::write(&path, text) {
        crate::log!("pictures: list not saved to {path}: {error}");
    }
}

/// The list as the phone app reads it: the screen's size in points, then one
/// line per picture, back to front, fields split by tabs: its place (a, b,
/// c, d, tx, ty), area width and height, alpha, mode, picture width and
/// height, the area it may draw in (screen points: x, y, width, height),
/// file.
pub fn encode(screen: (f64, f64), pictures: &[Picture]) -> String {
    let mut text = format!("{}\t{}\n", screen.0, screen.1);
    for p in pictures {
        let (t, c) = (p.place, p.clip);
        text += &format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            t.a, t.b, t.c, t.d, t.tx, t.ty, p.area.0, p.area.1, p.alpha, p.mode, p.size.0, p.size.1,
            c.origin.x, c.origin.y, c.size.width, c.size.height, p.path
        );
    }
    text
}

/// From the main loop: sends the pictures when they changed (at most 20 times a second).
pub fn tick() {
    let Some(screen) = screen() else { return };
    static LAST: Mutex<Option<(std::time::Instant, String)>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if last.as_ref().is_some_and(|(at, _)| at.elapsed().as_millis() < 50) {
        return;
    }
    let classes = Classes::find();
    let mut pictures = Vec::new();
    for window in crate::fw::uikit::windows() {
        pictures.extend(resolve(flatten(&snapshot(window, &classes, 0))));
    }
    let (wide, high, _) = crate::fw::uikit::screen_points();
    let text = encode((wide, high), &pictures);
    let changed = last.as_ref().is_none_or(|(_, sent)| *sent != text);
    if changed && (last.is_some() || !pictures.is_empty()) {
        // The first changes in full (where each picture goes), then only how many.
        static LOGGED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        if LOGGED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 20 {
            crate::log!("pictures: {} shown:\n{}", pictures.len(), text.trim_end());
        } else {
            crate::log!("pictures: {} shown", pictures.len());
        }
        (screen.show)(&text);
    }
    *last = Some((std::time::Instant::now(), text));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fw::geometry::Size;
    use crate::objc::runtime::Sel;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { origin: Point { x, y }, size: Size { width: w, height: h } }
    }

    fn view(frame: Rect, picture: Option<&str>, children: Vec<Node>) -> Node {
        let rect = rect(0.0, 0.0, frame.size.width, frame.size.height);
        let pictures = picture.map(|p| Shown { path: p.to_string(), mode: 0, size: (10.0, 10.0), rect, caps: None, tile: false, text: None, alpha: 1.0 }).into_iter().collect();
        Node { frame, alpha: 1.0, pictures, children, ..Default::default() }
    }

    fn window(children: Vec<Node>) -> Node {
        Node { alpha: 1.0, frame: rect(0.0, 0.0, 960.0, 540.0), children, ..Default::default() }
    }

    /// Where a point of the picture's area lands on the screen.
    fn at(p: &Picture, x: f64, y: f64) -> (f64, f64) {
        let t = p.place;
        ((t.a * x + t.c * y + t.tx).round(), (t.b * x + t.d * y + t.ty).round())
    }

    /// PC runs without a phone screen keep each list in the frames folder,
    /// numbered, to see what the phone would show.
    #[test]
    fn lists_are_kept_in_the_frames_folder_on_the_pc() {
        let folder = std::env::temp_dir().join(format!("gamebridge-pictures-{}", std::process::id()));
        let folder_text = folder.to_string_lossy().into_owned();
        save_list_in(&folder_text, "480\t320\n");
        save_list_in(&folder_text, "480\t320\nsecond\n");
        let mut names: Vec<String> = std::fs::read_dir(&folder).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        names.sort();
        assert_eq!(names.len(), 2);
        assert!(names[0].starts_with("pictures-") && names[0].ends_with(".txt"), "{names:?}");
        assert_eq!(std::fs::read_to_string(folder.join(&names[1])).unwrap(), "480\t320\nsecond\n");
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn positions_add_up_and_hidden_views_are_skipped() {
        let mut hidden = view(rect(0.0, 0.0, 5.0, 5.0), Some("hidden.png"), vec![]);
        hidden.hidden = true;
        let mut faint = view(rect(1.0, 1.0, 5.0, 5.0), Some("faint.png"), vec![]);
        faint.alpha = 0.5;
        let mut panel = view(rect(10.0, 20.0, 300.0, 200.0), Some("panel.png"), vec![view(rect(5.0, 6.0, 50.0, 40.0), Some("button.png"), vec![]), hidden, faint]);
        panel.bounds_origin = Point { x: 1.0, y: 2.0 };
        let pictures = flatten(&window(vec![view(rect(100.0, 100.0, 960.0, 540.0), None, vec![panel])]));
        let names: Vec<&str> = pictures.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(names, ["panel.png", "button.png", "faint.png"]);
        assert_eq!((at(&pictures[0], 0.0, 0.0), pictures[0].area), ((110.0, 120.0), (300.0, 200.0)));
        // Inside the panel, its bounds origin (1, 2) shifts what it holds.
        assert_eq!(at(&pictures[1], 0.0, 0.0), (114.0, 124.0));
        assert_eq!(at(&pictures[1], 50.0, 40.0), (164.0, 164.0));
        assert_eq!(pictures[2].alpha, 0.5);
    }

    #[test]
    fn a_playing_video_covers_what_is_under_it() {
        let mut video = view(rect(0.0, 0.0, 960.0, 540.0), None, vec![]);
        video.video = true;
        let window = window(vec![view(rect(0.0, 0.0, 960.0, 540.0), Some("splash.png"), vec![]), video, view(rect(700.0, 350.0, 200.0, 80.0), Some("start.png"), vec![])]);
        let names: Vec<String> = flatten(&window).into_iter().map(|p| p.path).collect();
        assert_eq!(names, ["start.png"]);
    }

    /// A stretchable picture filling its view comes in nine parts: the
    /// corners keep their size, the edges stretch one way, the middle both.
    /// Too small for its caps, they shrink to fit; other modes show it whole.
    #[test]
    fn stretchable_pictures_keep_their_caps() {
        let mut button = view(rect(10.0, 10.0, 100.0, 40.0), None, vec![]);
        let caps = Some([3.0, 4.0, 3.0, 4.0]);
        button.pictures.push(Shown { path: "b.png".into(), mode: 0, size: (20.0, 10.0), rect: rect(0.0, 0.0, 100.0, 40.0), caps, tile: false, text: None, alpha: 1.0 });
        let pictures = flatten(&window(vec![button]));
        let parts: Vec<(String, (f64, f64), (f64, f64), (f64, f64))> =
            pictures.iter().map(|p| (p.path.clone(), at(p, 0.0, 0.0), p.area, p.size)).collect();
        let part = |n: usize| format!("b.png{PART_MARK}{n}");
        assert_eq!(
            parts,
            [
                (part(0), (10.0, 10.0), (4.0, 3.0), (4.0, 3.0)),
                (part(1), (14.0, 10.0), (92.0, 3.0), (12.0, 3.0)),
                (part(2), (106.0, 10.0), (4.0, 3.0), (4.0, 3.0)),
                (part(3), (10.0, 13.0), (4.0, 34.0), (4.0, 4.0)),
                (part(4), (14.0, 13.0), (92.0, 34.0), (12.0, 4.0)),
                (part(5), (106.0, 13.0), (4.0, 34.0), (4.0, 4.0)),
                (part(6), (10.0, 47.0), (4.0, 3.0), (4.0, 3.0)),
                (part(7), (14.0, 47.0), (92.0, 3.0), (12.0, 3.0)),
                (part(8), (106.0, 47.0), (4.0, 3.0), (4.0, 3.0)),
            ]
        );
        // 6 points wide: the 4-point caps shrink to 3 and there is no middle column.
        let mut narrow = view(rect(0.0, 0.0, 6.0, 40.0), None, vec![]);
        narrow.pictures.push(Shown { path: "b.png".into(), mode: 0, size: (20.0, 10.0), rect: rect(0.0, 0.0, 6.0, 40.0), caps, tile: false, text: None, alpha: 1.0 });
        let widths: Vec<(String, f64)> = flatten(&window(vec![narrow])).into_iter().map(|p| (p.path, p.area.0)).collect();
        assert_eq!(widths.iter().filter(|(path, _)| path.ends_with(&format!("{PART_MARK}1"))).count(), 0, "{widths:?}");
        assert!(widths.iter().all(|&(_, w)| w == 3.0), "{widths:?}");
        // Shown at its own size (centred): whole.
        let mut centred = view(rect(0.0, 0.0, 100.0, 40.0), None, vec![]);
        centred.pictures.push(Shown { path: "b.png".into(), mode: 4, size: (20.0, 10.0), rect: rect(0.0, 0.0, 100.0, 40.0), caps, tile: false, text: None, alpha: 1.0 });
        let paths: Vec<String> = flatten(&window(vec![centred])).into_iter().map(|p| p.path).collect();
        assert_eq!(paths, ["b.png"]);
    }

    /// A tiling picture repeats its edges and middle at their own size
    /// between the caps, the last copy cut off at the cap.
    #[test]
    fn tiling_pictures_repeat_their_middle() {
        let mut button = view(rect(10.0, 10.0, 100.0, 40.0), None, vec![]);
        let caps = Some([3.0, 4.0, 3.0, 4.0]);
        button.pictures.push(Shown { path: "b.png".into(), mode: 0, size: (20.0, 10.0), rect: rect(0.0, 0.0, 100.0, 40.0), caps, tile: true, text: None, alpha: 1.0 });
        let pictures = flatten(&window(vec![button]));
        let part = |n: usize| -> Vec<&Picture> { pictures.iter().filter(|p| p.path == format!("b.png{PART_MARK}{n}")).collect() };
        // The top edge (12 points of picture) across the 92 between the caps: 8 copies.
        let top = part(1);
        let starts: Vec<(f64, f64)> = top.iter().map(|p| at(p, 0.0, 0.0)).collect();
        assert_eq!(starts, (0..8).map(|k| (14.0 + 12.0 * k as f64, 10.0)).collect::<Vec<_>>());
        assert!(top.iter().all(|p| p.area == (12.0, 3.0) && p.size == (12.0, 3.0)), "{top:?}");
        let last = top.last().unwrap();
        assert_eq!(last.clip.origin.x + last.clip.size.width, 106.0, "cut at the right cap");
        // The middle: 8 across, 9 down (4 points of picture in 34).
        assert_eq!(part(4).len(), 72);
        assert_eq!((part(0).len(), part(8).len()), (1, 1), "corners once");
    }

    fn setup() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
    }

    /// A new view of a UIKit class with a frame.
    fn new_view(class: &str, frame: Rect) -> Id {
        let object = runtime::send(runtime::class_named(class), "alloc", &[]);
        let s = runtime::sel("initWithFrame:");
        let init: extern "C" fn(Id, Sel, Rect) -> Id = unsafe { std::mem::transmute(runtime::imp_for(object, s).unwrap()) };
        init(object, s, frame)
    }

    /// A UILabel shows its text: a picture over its bounds drawn from its
    /// text, font, colours, alignment and lines, made into a file at the
    /// screen's pixels when shown. A label with no text shows nothing.
    #[test]
    fn labels_show_their_text() {
        setup();
        let label = new_view("UILabel", rect(10.0, 20.0, 100.0, 30.0));
        runtime::send(label, "setText:", &[crate::fw::string::make("Score")]);
        runtime::send(label, "setTextColor:", &[runtime::send(runtime::class_named("UIColor"), "redColor", &[])]);
        runtime::send(label, "setShadowColor:", &[runtime::send(runtime::class_named("UIColor"), "blackColor", &[])]);
        runtime::send(label, "setTextAlignment:", &[1]);
        runtime::send(label, "setFont:", &[crate::fw::controls::font("Helvetica-Bold", 12.0)]);
        let pictures = flatten(&window(vec![snapshot(label, &Classes::find(), 0)]));
        assert_eq!(pictures.len(), 1, "{pictures:?}");
        assert_eq!((at(&pictures[0], 0.0, 0.0), pictures[0].area), ((10.0, 20.0), (100.0, 30.0)));
        let look = pictures[0].text.clone().expect("the label's text");
        assert_eq!(
            look,
            TextLook {
                text: "Score".into(),
                font: ("Helvetica-Bold".into(), 12.0),
                color: [1.0, 0.0, 0.0, 1.0],
                shadow: Some(([0.0, 0.0, 0.0, 1.0], (0.0, -1.0))),
                alignment: 1,
                line_break: 4,
                lines: 1,
                size: (100.0, 30.0),
                minimum_font_size: None,
            }
        );
        let shown = resolve(pictures);
        let scale = pixels_per_point();
        let made = image::image_dimensions(&shown[0].path).expect("a picture file");
        assert_eq!(made, ((100.0 * scale).ceil() as u32, (30.0 * scale).ceil() as u32));
        runtime::send(label, "setText:", &[0]);
        assert!(snapshot(label, &Classes::find(), 0).pictures.is_empty());
    }

    fn send_struct<T>(object: Id, name: &str, value: T) {
        let s = runtime::sel(name);
        let f: extern "C" fn(Id, Sel, T) = unsafe { std::mem::transmute(runtime::imp_for(object, s).unwrap()) };
        f(object, s, value);
    }

    /// A UIButton shows its state's background picture over its bounds
    /// (stretchable ones keep their caps), then its picture and its title
    /// side by side in the middle of its content (its bounds less the
    /// content insets), each moved by its own insets; the title in the
    /// state's colour and shadow, the title label's font, on one line.
    #[test]
    fn buttons_show_their_state_pictures_and_title() {
        use crate::fw::controls::Insets;
        use crate::fw::uikit::{make_image, ImageInfo};
        setup();
        let picture = |w: u32, h: u32, caps| make_image(ImageInfo { path: format!("/nowhere/{w}x{h}.png"), pixels: (w, h), scale: 1.0, caps, ..Default::default() });
        let (background, pressed, icon) = (picture(20, 20, Some([5.0; 4])), picture(20, 21, None), picture(20, 10, None));
        let color = |name: &str| runtime::send(runtime::class_named("UIColor"), name, &[]);
        let button = new_view("UIButton", rect(0.0, 0.0, 100.0, 40.0));
        runtime::send(button, "setBackgroundImage:forState:", &[background, 0]);
        runtime::send(button, "setBackgroundImage:forState:", &[pressed, 1]);
        runtime::send(button, "setImage:forState:", &[icon, 0]);
        runtime::send(button, "setTitle:forState:", &[crate::fw::string::make("Go"), 0]);
        runtime::send(button, "setTitleColor:forState:", &[color("redColor"), 0]);
        runtime::send(button, "setTitleColor:forState:", &[color("whiteColor"), 1]);
        runtime::send(button, "setTitleShadowColor:forState:", &[color("blackColor"), 0]);
        send_struct(runtime::send(button, "titleLabel", &[]), "setShadowOffset:", Size { width: 1.0, height: 1.0 });
        let shown = || snapshot(button, &Classes::find(), 0).pictures;
        let found = shown();
        assert_eq!(found.len(), 3, "{found:?}");
        assert_eq!((&found[0].path, found[0].rect, found[0].caps), (&format!("{IMAGE_PREFIX}{background:x}"), rect(0.0, 0.0, 100.0, 40.0), Some([5.0; 4])));
        let (image, title) = (&found[1], &found[2]);
        assert_eq!((&image.path, image.rect.size, image.rect.origin.y), (&format!("{IMAGE_PREFIX}{icon:x}"), Size { width: 20.0, height: 10.0 }, 15.0));
        let look = title.text.clone().expect("the title");
        assert_eq!((look.text.as_str(), look.color, look.shadow, look.lines), ("Go", [1.0, 0.0, 0.0, 1.0], Some(([0.0, 0.0, 0.0, 1.0], (1.0, 1.0))), 1));
        assert_eq!(look.size, (title.rect.size.width, title.rect.size.height));
        assert!(title.rect.size.width > 10.0 && title.rect.size.height > 15.0, "{:?}", title.rect);
        // Side by side, together in the middle both ways.
        let middle = |found: &[Shown]| (found[1].rect.origin.x + found[2].rect.origin.x + found[2].rect.size.width) / 2.0;
        assert_eq!(image.rect.origin.x + 20.0, title.rect.origin.x);
        assert!((middle(&found) - 50.0).abs() < 1e-9, "{found:?}");
        assert!((title.rect.origin.y + title.rect.size.height / 2.0 - 20.0).abs() < 1e-9, "{:?}", title.rect);
        // Pressed: its own background (not stretchable) and title colour.
        runtime::send(button, "setHighlighted:", &[1]);
        let found = shown();
        assert_eq!((&found[0].path, found[0].caps), (&format!("{IMAGE_PREFIX}{pressed:x}"), None));
        assert_eq!(found[2].text.as_ref().unwrap().color, [1.0, 1.0, 1.0, 1.0]);
        runtime::send(button, "setHighlighted:", &[0]);
        // Content insets move the content's middle; the title's own move only it (half, centred).
        send_struct(button, "setContentEdgeInsets:", Insets { top: 0.0, left: 20.0, bottom: 0.0, right: 0.0 });
        let found = shown();
        assert!((middle(&found) - 60.0).abs() < 1e-9, "{found:?}");
        send_struct(button, "setTitleEdgeInsets:", Insets { top: 0.0, left: 10.0, bottom: 0.0, right: 0.0 });
        let moved = shown();
        assert_eq!((moved[1].rect, moved[2].rect.origin.x), (found[1].rect, found[2].rect.origin.x + 5.0));
        // Left aligned content starts at the content's left.
        runtime::send(button, "setContentHorizontalAlignment:", &[1]);
        assert_eq!(shown()[1].rect.origin.x, 20.0);
        // No title and no pictures: nothing.
        let plain = new_view("UIButton", rect(0.0, 0.0, 100.0, 40.0));
        assert!(snapshot(plain, &Classes::find(), 0).pictures.is_empty());
    }

    /// A title given only as an attributed string is drawn with its text,
    /// font and colour.
    #[test]
    fn buttons_draw_their_attributed_titles() {
        setup();
        let red = runtime::send(runtime::class_named("UIColor"), "redColor", &[]);
        let font = crate::fw::controls::font("Helvetica-Bold", 20.0);
        let attributes = crate::fw::collections::dictionary_from(&[(crate::fw::string::make("NSFont"), font), (crate::fw::string::make("NSColor"), red)]);
        let class = runtime::class_named("NSAttributedString");
        assert_ne!(class, 0, "no NSAttributedString");
        let title = runtime::send(runtime::send(class, "alloc", &[]), "initWithString:attributes:", &[crate::fw::string::make("Go"), attributes]);
        let button = new_view("UIButton", rect(0.0, 0.0, 100.0, 40.0));
        runtime::send(button, "setAttributedTitle:forState:", &[title, 0]);
        let found = snapshot(button, &Classes::find(), 0).pictures;
        let look = found.iter().find_map(|p| p.text.clone()).expect("the title drawn");
        assert_eq!((look.text.as_str(), look.font, look.color), ("Go", ("Helvetica-Bold".into(), 20.0), [1.0, 0.0, 0.0, 1.0]));
    }

    /// Content alignment Fill (3) stretches the picture over the content
    /// area (less its insets) that way; the other way it keeps its size.
    #[test]
    fn filled_button_content_stretches_the_picture() {
        use crate::fw::uikit::{make_image, ImageInfo};
        setup();
        let icon = make_image(ImageInfo { path: "/nowhere/20x10.png".into(), pixels: (20, 10), scale: 1.0, ..Default::default() });
        let button = new_view("UIButton", rect(0.0, 0.0, 100.0, 40.0));
        runtime::send(button, "setImage:forState:", &[icon, 0]);
        runtime::send(button, "setContentVerticalAlignment:", &[3]);
        let image_rect = || snapshot(button, &Classes::find(), 0).pictures[0].rect;
        assert_eq!(image_rect(), rect(40.0, 0.0, 20.0, 40.0));
        runtime::send(button, "setContentHorizontalAlignment:", &[3]);
        send_struct(button, "setImageEdgeInsets:", crate::fw::controls::Insets { top: 2.0, left: 4.0, bottom: 6.0, right: 8.0 });
        assert_eq!(image_rect(), rect(4.0, 2.0, 88.0, 32.0));
    }

    /// A white picture made in memory (one reference, the caller's).
    fn white_picture(width: usize, height: usize) -> Id {
        use crate::fw::graphics::{Bitmap, Space, PREMULTIPLIED_LAST};
        let bitmap = Bitmap { width, height, bits_per_pixel: 32, bytes_per_row: width * 4, info: PREMULTIPLIED_LAST, space: Space::Rgb, data: vec![255; width * height * 4] };
        let pixels = crate::fw::graphics::make_image(bitmap);
        let image = runtime::send(runtime::send(runtime::class_named("UIImage"), "alloc", &[]), "initWithCGImage:", &[pixels]);
        crate::fw::counted::release(pixels);
        image
    }

    /// The first pixel of a UIImage's picture (red, green, blue, alpha).
    fn first_pixel(image: Id) -> [u8; 4] {
        let mut pixel = [0u8; 4];
        let bytes = crate::fw::graphics::straight_pixels(crate::fw::graphics::image_cg(image)).expect("pixels");
        pixel.copy_from_slice(&bytes[..4]);
        pixel
    }

    /// Pressed, a button's pictures are drawn darker (adjustsImageWhenHighlighted,
    /// unless it has a picture of its own for being pressed); disabled, they
    /// are dimmed (adjustsImageWhenDisabled). The title stays as it is.
    #[test]
    fn pressed_buttons_darken_and_disabled_ones_dim() {
        setup();
        let (background, icon) = (white_picture(4, 4), white_picture(2, 2));
        let button = new_view("UIButton", rect(0.0, 0.0, 100.0, 40.0));
        runtime::send(button, "setBackgroundImage:forState:", &[background, 0]);
        runtime::send(button, "setImage:forState:", &[icon, 0]);
        runtime::send(button, "setTitle:forState:", &[crate::fw::string::make("Go"), 0]);
        let shown = || snapshot(button, &Classes::find(), 0).pictures;
        let image_of = |shown: &Shown| usize::from_str_radix(shown.path.strip_prefix(IMAGE_PREFIX).expect("an image"), 16).unwrap();
        let plain = shown();
        assert_eq!((image_of(&plain[0]), image_of(&plain[1])), (background, icon));
        assert!(plain.iter().all(|p| p.alpha == 1.0));
        runtime::send(button, "setHighlighted:", &[1]);
        let pressed = shown();
        let (dark_background, dark_icon) = (image_of(&pressed[0]), image_of(&pressed[1]));
        assert!(dark_background != background && dark_icon != icon, "darker copies");
        let [r, g, b, a] = first_pixel(dark_background);
        assert!(r < 160 && r > 60 && r == g && g == b && a == 255, "darker: {r} {g} {b} {a}");
        assert_eq!(crate::fw::uikit::image_info(dark_background).unwrap().caps, crate::fw::uikit::image_info(background).unwrap().caps);
        assert_eq!(image_of(&shown()[0]), dark_background, "made once");
        runtime::send(button, "setAdjustsImageWhenHighlighted:", &[0]);
        assert_eq!(image_of(&shown()[0]), background);
        runtime::send(button, "setAdjustsImageWhenHighlighted:", &[1]);
        // Its own pressed picture is shown as it is.
        let own = white_picture(4, 4);
        runtime::send(button, "setBackgroundImage:forState:", &[own, 1]);
        assert_eq!(image_of(&shown()[0]), own);
        runtime::send(button, "setHighlighted:", &[0]);
        // Disabled: pictures dimmed, the title not.
        runtime::send(button, "setEnabled:", &[0]);
        let disabled = shown();
        assert_eq!((image_of(&disabled[0]), image_of(&disabled[1])), (background, icon));
        assert!(disabled[0].alpha < 0.6 && disabled[1].alpha < 0.6 && disabled[2].alpha == 1.0, "{disabled:?}");
        runtime::send(button, "setAdjustsImageWhenDisabled:", &[0]);
        assert!(shown().iter().all(|p| p.alpha == 1.0));
    }

    /// A press is often one frame long: the darker pictures are made while
    /// the button shows normally, so the first pressed frame has them all.
    #[test]
    fn pressed_pictures_are_ready_on_the_first_pressed_frame() {
        setup();
        let background = white_picture(6, 6);
        let button = new_view("UIButton", rect(0.0, 0.0, 100.0, 40.0));
        runtime::send(button, "setBackgroundImage:forState:", &[background, 0]);
        let drawn = || resolve(flatten(&window(vec![snapshot(button, &Classes::find(), 0)]))).len();
        // Shown normally until its picture file is ready (made on its own thread).
        for _ in 0..200 {
            if drawn() == 1 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(drawn(), 1);
        std::thread::sleep(std::time::Duration::from_millis(500));
        runtime::send(button, "setHighlighted:", &[1]);
        assert_eq!(drawn(), 1, "the darker background is ready at once");
    }

    #[test]
    fn turned_and_scaled_views_keep_their_centre() {
        // An upright 320x480 launch picture turned a quarter so it fills a 480x320 screen.
        let mut splash = view(rect(80.0, -80.0, 320.0, 480.0), Some("Default.png"), vec![]);
        splash.transform = Some(Transform { a: 0.0, b: 1.0, c: -1.0, d: 0.0, tx: 0.0, ty: 0.0 });
        let mut grown = view(rect(100.0, 100.0, 20.0, 10.0), Some("grown.png"), vec![]);
        grown.transform = Some(Transform { a: 2.0, b: 0.0, c: 0.0, d: 2.0, tx: 5.0, ty: 0.0 });
        let pictures = flatten(&window(vec![splash, grown]));
        // Its corners land on the screen's corners.
        assert_eq!(at(&pictures[0], 0.0, 0.0), (480.0, 0.0));
        assert_eq!(at(&pictures[0], 320.0, 480.0), (0.0, 320.0));
        assert_eq!(at(&pictures[1], 0.0, 0.0), (95.0, 95.0));
        assert_eq!(at(&pictures[1], 20.0, 10.0), (135.0, 115.0));
        let text = encode((960.0, 540.0), &pictures[1..]);
        assert_eq!(text, "960\t540\n2\t0\t0\t2\t95\t95\t20\t10\t1\t0\t10\t10\t0\t0\t960\t540\tgrown.png\n");
    }

    /// Views with a background colour fill their bounds with it, under what
    /// they show and what they hold; a clear colour shows nothing. A GL view
    /// covers what is behind it (the GL picture is under every UIKit one,
    /// so nothing behind may be drawn over it), not what it holds.
    #[test]
    fn background_colours_fill_views_and_gl_views_cover_what_is_behind() {
        let full = rect(0.0, 0.0, 960.0, 540.0);
        let mut panel = view(rect(10.0, 20.0, 100.0, 50.0), Some("icon.png"), vec![view(rect(0.0, 0.0, 10.0, 10.0), Some("child.png"), vec![])]);
        panel.background = Some([1.0, 0.0, 0.0, 0.5]);
        let mut clear = view(rect(0.0, 0.0, 50.0, 50.0), None, vec![]);
        clear.background = Some([0.0, 0.0, 0.0, 0.0]);
        let pictures = flatten(&window(vec![panel, clear]));
        let paths: Vec<String> = pictures.iter().map(|p| p.path.clone()).collect();
        assert_eq!(paths, [color_path([1.0, 0.0, 0.0, 0.5]), "icon.png".into(), "child.png".into()]);
        assert_eq!((at(&pictures[0], 0.0, 0.0), pictures[0].area, pictures[0].mode), ((10.0, 20.0), (100.0, 50.0), 0));
        // The colour becomes a one-pixel picture of it.
        let made = resolve(vec![pictures[0].clone()]);
        let pixel = image::open(&made[0].path).expect("a colour picture").to_rgba8();
        assert_eq!((pixel.dimensions(), pixel.get_pixel(0, 0).0), ((1, 1), [255, 0, 0, 128]));
        // Behind a GL view: covered.
        let mut behind = view(full, Some("behind.png"), vec![]);
        behind.background = Some([0.0, 0.0, 0.0, 1.0]);
        let mut gl = view(full, None, vec![view(rect(5.0, 5.0, 20.0, 20.0), Some("hud.png"), vec![])]);
        gl.gl = true;
        gl.background = Some([0.0, 0.0, 0.0, 1.0]);
        let front = view(rect(0.0, 0.0, 30.0, 30.0), Some("front.png"), vec![]);
        let paths: Vec<String> = flatten(&window(vec![behind, gl, front])).into_iter().map(|p| p.path).collect();
        assert_eq!(paths, ["hud.png", "front.png"]);
    }

    /// A view that clips to its bounds keeps what it holds inside them: each
    /// picture carries the screen area it may draw in (nested clips meet),
    /// and one wholly outside is left out. Others may use the whole screen.
    #[test]
    fn clipping_views_keep_what_they_hold_inside() {
        let inner = view(rect(20.0, 20.0, 100.0, 100.0), Some("inner.png"), vec![]);
        let mut small = view(rect(30.0, -10.0, 40.0, 40.0), None, vec![view(rect(0.0, 0.0, 100.0, 100.0), Some("deep.png"), vec![])]);
        small.clips = true;
        let away = view(rect(200.0, 200.0, 10.0, 10.0), Some("away.png"), vec![]);
        let mut clipping = view(rect(100.0, 100.0, 50.0, 50.0), Some("box.png"), vec![inner, small, away]);
        clipping.clips = true;
        let free = view(rect(0.0, 0.0, 10.0, 10.0), Some("free.png"), vec![]);
        let pictures = flatten(&window(vec![clipping, free]));
        let clips: Vec<(&str, Rect)> = pictures.iter().map(|p| (p.path.as_str(), p.clip)).collect();
        assert_eq!(
            clips,
            [
                ("box.png", rect(100.0, 100.0, 50.0, 50.0)),
                ("inner.png", rect(100.0, 100.0, 50.0, 50.0)),
                ("deep.png", rect(130.0, 100.0, 20.0, 30.0)),
                ("free.png", rect(0.0, 0.0, 960.0, 540.0)),
            ]
        );
        assert!(encode((960.0, 540.0), &pictures[2..3]).ends_with("\t130\t100\t20\t30\tdeep.png\n"));
    }
}
