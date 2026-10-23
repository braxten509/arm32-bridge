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
    /// Text to show instead of a picture file (`path` is then empty).
    pub text: Option<TextLook>,
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
    walk(node, None, 1.0, &mut out);
    out
}

/// `parent`: the parent's own points to screen points (None for the window).
fn walk(node: &Node, parent: Option<Transform>, parent_alpha: f64, out: &mut Vec<Picture>) {
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
    if node.video {
        out.clear();
    }
    for shown in &node.pictures {
        for (path, rect, picture_size) in parts(shown) {
            if rect.size.width > 0.0 && rect.size.height > 0.0 {
                let place = then(moved(node.bounds_origin.x + rect.origin.x, node.bounds_origin.y + rect.origin.y), own);
                let text = shown.text.clone();
                out.push(Picture { path, place, area: (rect.size.width, rect.size.height), alpha, mode: shown.mode, size: picture_size, text });
            }
        }
    }
    for child in &node.children {
        walk(child, Some(own), alpha, out);
    }
}

/// What a picture is drawn as: itself in its rect, or, stretchable and
/// filling it (mode 0), its nine parts (rows from the top), each with its
/// rect and its own size. Caps longer than the rect shrink to share it.
fn parts(shown: &Shown) -> Vec<(String, Rect, (f64, f64))> {
    let caps = shown.caps.filter(|caps| shown.mode == 0 && caps.iter().any(|&cap| cap > 0.0));
    let Some([top, left, bottom, right]) = caps else { return vec![(shown.path.clone(), shown.rect, shown.size)] };
    /// Start, length and source length of the three parts of one side.
    fn side(start: f64, length: f64, source: f64, first: f64, last: f64) -> [(f64, f64, f64); 3] {
        let shrink = if first + last > length { length / (first + last) } else { 1.0 };
        let (shown_first, shown_last) = (first * shrink, last * shrink);
        [
            (start, shown_first, first),
            (start + shown_first, length - shown_first - shown_last, (source - first - last).max(0.0)),
            (start + length - shown_last, shown_last, last),
        ]
    }
    let rect = shown.rect;
    let columns = side(rect.origin.x, rect.size.width, shown.size.0, left, right);
    let rows = side(rect.origin.y, rect.size.height, shown.size.1, top, bottom);
    let mut out = Vec::new();
    for (row, &(y, height, source_height)) in rows.iter().enumerate() {
        for (column, &(x, width, source_width)) in columns.iter().enumerate() {
            if width > 0.0 && height > 0.0 && source_width > 0.0 && source_height > 0.0 {
                out.push((format!("{}{PART_MARK}{}", shown.path, row * 3 + column), Rect::new(x, y, width, height), (source_width, source_height)));
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
}

impl Classes {
    fn find() -> Classes {
        Classes { image_view: runtime::class_named("UIImageView"), label: runtime::class_named("UILabel"), button: runtime::class_named("UIButton") }
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
    Some(Shown { path: format!("{IMAGE_PREFIX}{image:x}"), mode, size: (size.width, size.height), rect, caps: info.caps, text: None })
}

/// Text drawn over `rect` (None for no text).
fn text_shown(look: Option<TextLook>, rect: Rect) -> Option<Shown> {
    let look = look?;
    Some(Shown { path: String::new(), mode: 0, size: look.size, rect, caps: None, text: Some(look) })
}

/// The view tree under `view` as `flatten` reads it (main thread).
fn snapshot(view: Id, classes: &Classes, depth: usize) -> Node {
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
        pictures.extend(image_shown(parts.background, 0, bounds));
        if let Some((image, rect)) = parts.image {
            pictures.extend(image_shown(image, 0, rect));
        }
        if let Some((look, rect)) = parts.title {
            pictures.extend(text_shown(Some(look), rect));
        }
    }
    Node {
        frame: state.frame,
        bounds_origin: state.bounds_origin,
        transform: Some(state.transform),
        hidden: property_get(view, "hidden").unwrap_or(0) != 0,
        alpha: property_get(view, "alpha").map(f64::from_bits).unwrap_or(1.0),
        pictures,
        video: crate::fw::media::shows_video(view),
        children: if depth < 64 { state.subviews.iter().map(|&child| snapshot(child, classes, depth + 1)).collect() } else { Vec::new() },
    }
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
    pictures
        .into_iter()
        .filter_map(|mut p| {
            if let Some(look) = &p.text {
                p.path = crate::fw::graphics::text_picture(look, pixels_per_point())?;
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
/// height, file.
pub fn encode(screen: (f64, f64), pictures: &[Picture]) -> String {
    let mut text = format!("{}\t{}\n", screen.0, screen.1);
    for p in pictures {
        let t = p.place;
        text += &format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            t.a, t.b, t.c, t.d, t.tx, t.ty, p.area.0, p.area.1, p.alpha, p.mode, p.size.0, p.size.1, p.path
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
        let pictures = picture.map(|p| Shown { path: p.to_string(), mode: 0, size: (10.0, 10.0), rect, caps: None, text: None }).into_iter().collect();
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
        button.pictures.push(Shown { path: "b.png".into(), mode: 0, size: (20.0, 10.0), rect: rect(0.0, 0.0, 100.0, 40.0), caps, text: None });
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
        narrow.pictures.push(Shown { path: "b.png".into(), mode: 0, size: (20.0, 10.0), rect: rect(0.0, 0.0, 6.0, 40.0), caps, text: None });
        let widths: Vec<(String, f64)> = flatten(&window(vec![narrow])).into_iter().map(|p| (p.path, p.area.0)).collect();
        assert_eq!(widths.iter().filter(|(path, _)| path.ends_with(&format!("{PART_MARK}1"))).count(), 0, "{widths:?}");
        assert!(widths.iter().all(|&(_, w)| w == 3.0), "{widths:?}");
        // Shown at its own size (centred): whole.
        let mut centred = view(rect(0.0, 0.0, 100.0, 40.0), None, vec![]);
        centred.pictures.push(Shown { path: "b.png".into(), mode: 4, size: (20.0, 10.0), rect: rect(0.0, 0.0, 100.0, 40.0), caps, text: None });
        let paths: Vec<String> = flatten(&window(vec![centred])).into_iter().map(|p| p.path).collect();
        assert_eq!(paths, ["b.png"]);
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
        assert_eq!(text, "960\t540\n2\t0\t0\t2\t95\t95\t20\t10\t1\t0\t10\t10\tgrown.png\n");
    }
}
