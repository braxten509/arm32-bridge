//! The game's UIKit pictures on the phone screen.
//!
//! Games draw their world with OpenGL, but some put plain UIKit picture
//! views (UIImageView and kinds of it) over it: start buttons, splash and
//! loading pictures. Our layer has no UIKit drawing of its own, so the
//! phone app draws them: up to 20 times a second the main thread walks the
//! game's windows, lists every picture view that shows (where, how big, how
//! see-through), and sends the list when it changed. Pictures under a
//! video's view are left out, since it covers them as on iOS.
//!
//! Places follow UIKit: each view sits at its centre in its parent, turned
//! and scaled by its transform, its bounds shifting what is inside it; so
//! sideways launch pictures in older games come out right. Clipping is not
//! drawn; picture files are PNGs as UIImage reads them here.

use crate::fw::geometry::{Point, Rect, Transform, IDENTITY};
use crate::fw::uikit::{property_get, view_state};
use crate::objc::runtime::{self, Id};
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
}

/// A view as far as drawing pictures goes.
#[derive(Default)]
pub struct Node {
    pub frame: Rect,
    pub bounds_origin: Point,
    pub transform: Option<Transform>,
    pub hidden: bool,
    pub alpha: f64,
    /// Its picture: file, mode and size in points.
    pub picture: Option<(String, i64, (f64, f64))>,
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
    if let Some((path, mode, picture_size)) = &node.picture {
        if size.width > 0.0 && size.height > 0.0 {
            let place = then(moved(node.bounds_origin.x, node.bounds_origin.y), own);
            out.push(Picture { path: path.clone(), place, area: (size.width, size.height), alpha, mode: *mode, size: *picture_size });
        }
    }
    for child in &node.children {
        walk(child, Some(own), alpha, out);
    }
}

/// The view tree under `view` as `flatten` reads it (main thread).
fn snapshot(view: Id, image_view_class: usize, depth: usize) -> Node {
    let state = view_state(view);
    let is_image_view = image_view_class != 0 && {
        let rt = runtime::lock();
        rt.is_subclass(rt.class_of(view), image_view_class)
    };
    let picture = if is_image_view {
        let image = property_get(view, "image").unwrap_or(0) as Id;
        // Pictures made in memory have no file the phone app could show.
        crate::fw::uikit::image_info(image).filter(|info| !info.path.is_empty()).map(|info| {
            let mode = property_get(view, "contentMode").unwrap_or(0) as i64;
            (info.path.clone(), mode, (info.pixels.0 as f64 / info.scale, info.pixels.1 as f64 / info.scale))
        })
    } else {
        None
    };
    Node {
        frame: state.frame,
        bounds_origin: state.bounds_origin,
        transform: Some(state.transform),
        hidden: property_get(view, "hidden").unwrap_or(0) != 0,
        alpha: property_get(view, "alpha").map(f64::from_bits).unwrap_or(1.0),
        picture,
        video: crate::fw::media::shows_video(view),
        children: if depth < 64 { state.subviews.iter().map(|&child| snapshot(child, image_view_class, depth + 1)).collect() } else { Vec::new() },
    }
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
    SCREEN.get().is_some()
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
    let Some(screen) = SCREEN.get() else { return };
    static LAST: Mutex<Option<(std::time::Instant, String)>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if last.as_ref().is_some_and(|(at, _)| at.elapsed().as_millis() < 50) {
        return;
    }
    let image_view_class = runtime::class_named("UIImageView");
    let mut pictures = Vec::new();
    for window in crate::fw::uikit::windows() {
        pictures.extend(flatten(&snapshot(window, image_view_class, 0)));
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

    fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { origin: Point { x, y }, size: Size { width: w, height: h } }
    }

    fn view(frame: Rect, picture: Option<&str>, children: Vec<Node>) -> Node {
        Node { frame, alpha: 1.0, picture: picture.map(|p| (p.to_string(), 0, (10.0, 10.0))), children, ..Default::default() }
    }

    fn window(children: Vec<Node>) -> Node {
        Node { alpha: 1.0, frame: rect(0.0, 0.0, 960.0, 540.0), children, ..Default::default() }
    }

    /// Where a point of the picture's area lands on the screen.
    fn at(p: &Picture, x: f64, y: f64) -> (f64, f64) {
        let t = p.place;
        ((t.a * x + t.c * y + t.tx).round(), (t.b * x + t.d * y + t.ty).round())
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
