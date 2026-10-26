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
//! sideways launch pictures in older games come out right. Picture files
//! are PNGs as UIImage reads them here. Pictures made in memory (or turned
//! photos) are shown through an upright PNG made once.
//!
//! Layers draw as Core Animation draws them: the layers a game adds (among
//! a view's subviews where it put them), each with its background (round
//! corners), contents, gradient or shape, sublayers by zPosition, and the
//! border on top. A layer cut to round or turned bounds, masked, or with a
//! shadow is drawn into one picture with what it holds, its shadow under
//! it (fw/graphics/compose.rs); other clips are the screen areas pictures
//! may draw in.

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
    /// On a GROUP_MARK: how the pictures up to its END_MARK are drawn together.
    pub group: Option<Box<Group>>,
}

/// A layer whose pictures are drawn together (fw/graphics/compose.rs):
/// cut to its round corners, through its mask, or with a shadow under them.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    /// The layer's own points to screen points.
    pub place: Transform,
    /// Its bounds in its own points.
    pub bounds: Rect,
    /// What shows is cut to the bounds, their corners rounded by `radius` (masksToBounds).
    pub clips: bool,
    pub radius: f64,
    /// Its mask layer's pictures come after a MASK_MARK: where they are clear, it is too.
    pub masked: bool,
    pub shadow: Option<Shadow>,
}

/// A layer's shadow.
#[derive(Clone, Debug, PartialEq)]
pub struct Shadow {
    /// Straight colour, its alpha times shadowOpacity and the layer's opacity.
    pub color: [f64; 4],
    /// Blur radius in the layer's points.
    pub radius: f64,
    /// Moved by this in the layer's points (iOS: y goes down).
    pub offset: (f64, f64),
    /// The shadowPath's figures in the layer's points; None: the shadow of what the layer shows.
    pub path: Option<Vec<Vec<Point>>>,
}

/// Starts a group of pictures drawn together (see `Group`).
pub const GROUP_MARK: &str = "\u{1}group";
/// In a masked group: the mask's pictures follow.
pub const MASK_MARK: &str = "\u{1}mask";
/// Ends the group.
pub const END_MARK: &str = "\u{1}end";

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
    /// Its layer's anchor point (unit square), what it turns around; None: the middle.
    pub anchor: Option<Point>,
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
    /// It clips what it holds to its bounds (clipsToBounds, masksToBounds).
    pub clips: bool,
    /// Its layer's corner radius: the background and border are rounded, and
    /// so is the clip when it clips.
    pub radius: f64,
    /// Its layer's border (straight colour, width), drawn over everything inside.
    pub border: Option<([f64; 4], f64)>,
    /// Its layer's shadow (the colour's alpha not yet times the opacity).
    pub shadow: Option<Shadow>,
    /// Its layer's mask, in its own points.
    pub mask: Option<Box<Node>>,
    pub children: Vec<Node>,
    /// Moving by Core Animation: where it is in each coming frame after the
    /// first (see `Sampling`); empty when it stays as it is.
    pub motion: Vec<Pose>,
}

/// A view's place in one frame: its frame's origin, transform and alpha.
#[derive(Clone, Copy, Debug)]
pub struct Pose {
    pub origin: Point,
    pub transform: Transform,
    pub alpha: f64,
}

impl Node {
    /// Its place in coming frame `k` (0: now).
    fn pose(&self, k: usize) -> (Point, Option<Transform>, f64) {
        match k.checked_sub(1).and_then(|i| self.motion.get(i).or(self.motion.last())) {
            Some(pose) if k > 0 => (pose.origin, Some(pose.transform), pose.alpha),
            _ => (self.frame.origin, self.transform, self.alpha),
        }
    }
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

/// When the coming frames of moving views are worked out (media times
/// `start + k * step`, k below `count`; frame 0 is now).
#[derive(Clone, Copy, Debug)]
pub struct Sampling {
    pub start: f64,
    pub step: f64,
    pub count: usize,
}

thread_local! {
    static SAMPLING: std::cell::Cell<Option<Sampling>> = const { std::cell::Cell::new(None) };
}

/// Snapshots made by `make` sample animations by `sampling`.
fn with_sampling<T>(sampling: Sampling, make: impl FnOnce() -> T) -> T {
    let before = SAMPLING.with(|s| s.replace(Some(sampling)));
    let made = make();
    SAMPLING.with(|s| s.set(before));
    made
}

/// The pictures of each coming frame (see `Sampling`) of the windows.
fn frames(windows: &[Node], count: usize) -> Vec<Vec<Picture>> {
    (0..count.max(1)).map(|k| windows.iter().flat_map(|w| flatten_at(w, k)).collect()).collect()
}

/// A media time (CACurrentMediaTime: CLOCK_MONOTONIC seconds) in nanoseconds.
fn nanoseconds(t: f64) -> i64 {
    (t * 1e9) as i64
}

/// `encode` with the coming frames after the first, each after a line
/// `@` and its time (CLOCK_MONOTONIC nanoseconds, as the phone's
/// System.nanoTime); the first line also has the first frame's time.
pub fn encode_frames(screen: (f64, f64), frames: &[(f64, Vec<Picture>)]) -> String {
    let Some(((first_time, first), rest)) = frames.split_first() else { return encode(screen, &[]) };
    if rest.is_empty() {
        return encode(screen, first);
    }
    let mut text = format!("{}\t{}\t{}\n", screen.0, screen.1, nanoseconds(*first_time));
    encode_lines(&mut text, first);
    for (time, pictures) in rest {
        text += &format!("@\t{}\n", nanoseconds(*time));
        encode_lines(&mut text, pictures);
    }
    text
}

/// The pictures under `node` (a window, at the screen's top-left), back to front.
pub fn flatten(node: &Node) -> Vec<Picture> {
    let mut out = Vec::new();
    // A window clips nothing (its own clipsToBounds aside), the screen does:
    // on an iPhone 5 a 320x480 window still shows its views below 480. The
    // area reaches over the window too; past the screen the phone cuts it.
    let (wide, high, _) = crate::fw::uikit::screen_points();
    let area = Rect::new(0.0, 0.0, wide.max(node.frame.size.width), high.max(node.frame.size.height));
    walk(node, 0, None, 1.0, area, &mut out);
    out
}

/// `flatten` in coming frame `k` (see `Sampling`).
fn flatten_at(node: &Node, k: usize) -> Vec<Picture> {
    let (wide, high, _) = crate::fw::uikit::screen_points();
    let area = Rect::new(0.0, 0.0, wide.max(node.frame.size.width), high.max(node.frame.size.height));
    let mut out = Vec::new();
    walk(node, k, None, 1.0, area, &mut out);
    out
}

/// The screen area (axis-aligned) a rect in some view's own points covers.
pub(crate) fn screen_box(place: Transform, rect: Rect) -> Rect {
    let corners = [(0.0, 0.0), (rect.size.width, 0.0), (0.0, rect.size.height), (rect.size.width, rect.size.height)]
        .map(|(x, y)| (rect.origin.x + x, rect.origin.y + y))
        .map(|(x, y)| (place.a * x + place.c * y + place.tx, place.b * x + place.d * y + place.ty));
    let (left, right) = (corners.iter().map(|c| c.0).fold(f64::INFINITY, f64::min), corners.iter().map(|c| c.0).fold(f64::NEG_INFINITY, f64::max));
    let (top, bottom) = (corners.iter().map(|c| c.1).fold(f64::INFINITY, f64::min), corners.iter().map(|c| c.1).fold(f64::NEG_INFINITY, f64::max));
    Rect::new(left, top, right - left, bottom - top)
}

/// Where two areas meet (no size when they don't).
pub(crate) fn meet(a: Rect, b: Rect) -> Rect {
    let (left, top) = (a.origin.x.max(b.origin.x), a.origin.y.max(b.origin.y));
    let right = (a.origin.x + a.size.width).min(b.origin.x + b.size.width);
    let bottom = (a.origin.y + a.size.height).min(b.origin.y + b.size.height);
    Rect::new(left, top, (right - left).max(0.0), (bottom - top).max(0.0))
}

/// `parent`: the parent's own points to screen points (None for the window);
/// `clip`: the screen area the node may draw in.
fn walk(node: &Node, k: usize, parent: Option<Transform>, parent_alpha: f64, clip: Rect, out: &mut Vec<Picture>) {
    let (origin, transform, own_alpha) = node.pose(k);
    let alpha = parent_alpha * own_alpha;
    if node.hidden || alpha <= 0.01 {
        return;
    }
    let size = node.frame.size;
    // The view's own points (its bounds) to screen points.
    let own = match parent {
        None => moved(-node.bounds_origin.x, -node.bounds_origin.y),
        Some(parent) => {
            // It turns and scales around its anchor point (the middle unless moved).
            let anchor = node.anchor.unwrap_or(Point { x: 0.5, y: 0.5 });
            let centre = (origin.x + size.width * anchor.x, origin.y + size.height * anchor.y);
            let turn = transform.unwrap_or(IDENTITY);
            let to_middle = moved(-node.bounds_origin.x - size.width * anchor.x, -node.bounds_origin.y - size.height * anchor.y);
            then(then(then(to_middle, turn), moved(centre.0, centre.1)), parent)
        }
    };
    if node.video || node.gl {
        out.clear();
    }
    let visible = Rect { origin: node.bounds_origin, size };
    // Drawn together (after resolve, by fw/graphics/compose.rs) when cut to
    // round or turned bounds, masked, or shadowed.
    let turned = own.b.abs() > 1e-9 || own.c.abs() > 1e-9;
    let group = (node.clips && (node.radius > 0.0 || turned) || node.mask.is_some() || node.shadow.is_some()).then(|| Group {
        place: own,
        bounds: visible,
        clips: node.clips,
        radius: node.radius,
        masked: node.mask.is_some(),
        shadow: node.shadow.clone().map(|shadow| Shadow { color: [shadow.color[0], shadow.color[1], shadow.color[2], shadow.color[3] * alpha], ..shadow }),
    });
    let mark = |path: &str, group: Option<Group>| Picture {
        path: path.into(),
        place: own,
        area: (size.width, size.height),
        alpha,
        mode: 0,
        size: (size.width, size.height),
        text: None,
        clip,
        group: group.map(Box::new),
    };
    let grouped = group.is_some();
    if let Some(group) = group {
        out.push(mark(GROUP_MARK, Some(group)));
    }
    let clip = if node.clips { meet(clip, screen_box(own, visible)) } else { clip };
    let over_bounds = |path: String| Shown { path, mode: 0, size: (1.0, 1.0), rect: Rect { origin: Point::default(), size }, caps: None, tile: false, text: None, alpha: 1.0 };
    // The background colour first, under what the view shows (the GL picture covers a GL view's).
    let background = node.background.filter(|color| color[3] > 0.0 && !node.gl).map(|color| {
        if node.radius > 0.0 {
            over_bounds(box_path((size.width, size.height), node.radius, color, None))
        } else {
            over_bounds(color_path(color))
        }
    });
    let border = node.border.map(|border| over_bounds(box_path((size.width, size.height), node.radius, [0.0; 4], Some(border))));
    let draw = |shown: &Shown, out: &mut Vec<Picture>| {
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
                out.push(Picture { path, place, area: (rect.size.width, rect.size.height), alpha, mode: shown.mode, size: picture_size, text, clip, group: None });
            }
        }
    };
    // Core Animation's order: background, contents, sublayers, the border on top.
    for shown in background.iter().chain(&node.pictures) {
        draw(shown, out);
    }
    for child in &node.children {
        walk(child, k, Some(own), alpha, clip, out);
    }
    if let Some(border) = &border {
        draw(border, out);
    }
    if grouped {
        if let Some(mask) = &node.mask {
            out.push(mark(MASK_MARK, None));
            // The mask counts by its own opacity alone, wherever the layer's clip is.
            walk(mask, k, Some(own), 1.0, Rect::new(-1e9, -1e9, 2e9, 2e9), out);
        }
        out.push(mark(END_MARK, None));
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
    let caps = shown.caps.filter(|caps| shown.mode == 0 && (shown.tile || caps.iter().any(|&cap| cap > 0.0)));
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
    progress: Class,
    gl_layer: Class,
    gradient_layer: Class,
    shape_layer: Class,
    image: Class,
}

impl Classes {
    fn find() -> Classes {
        Classes {
            gradient_layer: runtime::class_named("CAGradientLayer"),
            shape_layer: runtime::class_named("CAShapeLayer"),
            image: runtime::class_named("UIImage"),
            image_view: runtime::class_named("UIImageView"),
            label: runtime::class_named("UILabel"),
            button: runtime::class_named("UIButton"),
            text_field: runtime::class_named("UITextField"),
            table: runtime::class_named("UITableView"),
            cell: runtime::class_named("UITableViewCell"),
            progress: runtime::class_named("UIProgressView"),
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
    Some(Shown { path: format!("{IMAGE_PREFIX}{image:x}"), mode, size: (size.width, size.height), rect, caps: info.caps, tile: info.tile, text: None, alpha: 1.0 })
}

/// How see-through a disabled button's pictures are drawn.
const DIMMED: f64 = 0.5;

/// Starts making the files a picture will be drawn from (its parts when it
/// is stretchable), so it can show the moment it is needed.
fn start_files(image: Id) {
    match crate::fw::uikit::image_info(image).map(|info| info.caps) {
        Some(Some(_)) => drop(crate::fw::graphics::picture_part_file(image, 0)),
        Some(None) => drop(crate::fw::graphics::picture_file(image)),
        None => {}
    }
}

/// A button's picture as its state draws it: a darker copy when pressed,
/// dimmed when disabled.
fn shaded(image: Id, shade: crate::fw::controls::Shade, rect: Rect) -> Option<Shown> {
    if !shade.darker && shade.darker_when_pressed && image != 0 {
        // A press may last one frame: the darker copy and its files are made now.
        start_files(crate::fw::graphics::darker_image(image));
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
        // The pressed state's own pictures, as for the darker copies in `shaded`.
        parts.pressed_own.into_iter().for_each(start_files);
    }
    if Classes::is(view, classes.progress) {
        // The track over the bounds, the part done from the left: each its picture or colour.
        let ((track_image, track_color), (done_image, done_color)) = crate::fw::progress::parts(view);
        let done = Rect { size: crate::fw::geometry::Size { width: bounds.size.width * crate::fw::progress::progress(view), ..bounds.size }, ..bounds };
        for (image, color, plain, rect) in [(track_image, track_color, crate::fw::progress::TRACK, bounds), (done_image, done_color, crate::fw::progress::DONE, done)] {
            if rect.size.width <= 0.0 {
                continue;
            }
            match image_shown(image, 0, rect) {
                Some(shown) => pictures.push(shown),
                None => pictures.push(Shown {
                    path: color_path(crate::fw::graphics::color_rgba(color).unwrap_or(plain)),
                    mode: 0,
                    size: (1.0, 1.0),
                    rect,
                    caps: None,
                    tile: false,
                    text: None,
                    alpha: 1.0,
                }),
            }
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
    if let Some(pieces) = crate::fw::pickers::pieces(view) {
        pictures.extend(pieces.into_iter().map(piece_shown));
    }
    if state.layer != 0 {
        // What its layer shows (contents, a gradient or shape layer's) comes first.
        let mut own = layer_content(state.layer, Rect { origin: state.bounds_origin, size: state.frame.size }, classes);
        own.append(&mut pictures);
        pictures = own;
    }
    let alpha = property_get(view, "alpha").map(f64::from_bits).unwrap_or(1.0);
    let anchor = if state.layer == 0 { Point { x: 0.5, y: 0.5 } } else { crate::fw::layers::anchor(state.layer) };
    let size = state.frame.size;
    let position = Point { x: state.frame.origin.x + size.width * anchor.x, y: state.frame.origin.y + size.height * anchor.y };
    let ((position, transform, alpha), motion) = presented(state.layer, position, state.transform, alpha);
    let children = if depth >= 64 {
        Vec::new()
    } else if state.layer != 0 {
        layer_children(state.layer, view, classes, depth)
    } else {
        in_z_order(state.subviews.iter().map(|&child| (z_of(child), snapshot(child, classes, depth + 1))).collect())
    };
    let mut node = Node {
        frame: Rect { origin: Point { x: position.x - size.width * anchor.x, y: position.y - size.height * anchor.y }, size },
        bounds_origin: state.bounds_origin,
        transform: Some(transform),
        anchor: Some(anchor),
        hidden: property_get(view, "hidden").unwrap_or(0) != 0,
        alpha,
        pictures,
        video: crate::fw::media::shows_video(view),
        gl: state.layer != 0 && Classes::is(state.layer, classes.gl_layer),
        background,
        clips: property_get(view, "clips").unwrap_or(0) != 0,
        children,
        motion: poses(motion, size, anchor),
        ..Default::default()
    };
    if state.layer != 0 {
        dress(&mut node, state.layer, classes, depth);
    }
    node
}

/// Where a layer is, how it is turned and how see-through, with a running
/// Core Animation animation shown over its own values: now, and (while
/// sampling, see `Sampling`) in each coming frame after the first.
fn presented(layer: Id, position: Point, transform: Transform, opacity: f64) -> ((Point, Transform, f64), Vec<(Point, Transform, f64)>) {
    if layer == 0 || !crate::fw::animation::animating(layer) {
        return ((position, transform, opacity), Vec::new());
    }
    let model = crate::fw::animation::Shown { opacity, position, transform };
    let at = |t: f64| {
        let shown = crate::fw::animation::presented_at(layer, model, t);
        (shown.position, shown.transform, shown.opacity)
    };
    match SAMPLING.with(|s| s.get()) {
        Some(s) => (at(s.start), (1..s.count).map(|k| at(s.start + k as f64 * s.step)).collect()),
        None => (at(crate::fw::runloop::media_time()), Vec::new()),
    }
}

/// Layer positions (at the anchor point) as frame origins.
fn poses(motion: Vec<(Point, Transform, f64)>, size: crate::fw::geometry::Size, anchor: Point) -> Vec<Pose> {
    motion
        .into_iter()
        .map(|(position, transform, alpha)| Pose { origin: Point { x: position.x - size.width * anchor.x, y: position.y - size.height * anchor.y }, transform, alpha })
        .collect()
}

/// Layers with a higher zPosition show in front of their siblings (a stable sort keeps the rest in order).
fn in_z_order(mut children: Vec<(f64, Node)>) -> Vec<Node> {
    children.sort_by(|a, b| a.0.total_cmp(&b.0));
    children.into_iter().map(|(_, node)| node).collect()
}

/// What is drawn inside a layer: its view's subviews (`view`: 0 for a layer
/// of its own) and the layers the game added, in order; a replicator
/// layer's, once per copy.
fn layer_children(layer: Id, view: Id, classes: &Classes, depth: usize) -> Vec<Node> {
    use crate::fw::layers::{self, Child};
    if depth >= 64 {
        return Vec::new();
    }
    let nodes = || {
        in_z_order(
            if view != 0 { layers::view_children(view, layer) } else { layers::children(layer) }
                .into_iter()
                .map(|child| match child {
                    Child::View(view) => (z_of(view), snapshot(view, classes, depth + 1)),
                    Child::Layer(sub) => (layers::z_position(sub), layer_node(sub, classes, depth + 1)),
                })
                .collect(),
        )
    };
    if !Classes::is(layer, runtime::class_named("CAReplicatorLayer")) {
        return nodes();
    }
    let (copies, step) = layers::replication(layer);
    // Each copy one instanceTransform further than the last, around the replicator's middle.
    let (bounds, step) = (layers::bounds(layer), layers::affine_of(step));
    let mut turn = IDENTITY;
    (0..copies)
        .map(|_| {
            let copy = Node { frame: bounds, bounds_origin: bounds.origin, transform: Some(turn), alpha: 1.0, children: nodes(), ..Default::default() };
            turn = then(turn, step);
            copy
        })
        .collect()
}

/// A layer of its own (made by the game) as `flatten` reads it.
fn layer_node(layer: Id, classes: &Classes, depth: usize) -> Node {
    use crate::fw::layers;
    let (bounds, anchor) = (layers::bounds(layer), layers::anchor(layer));
    let size = bounds.size;
    let ((position, transform, alpha), motion) = presented(layer, layers::position(layer), layers::affine_of(layers::transform(layer)), layers::opacity(layer));
    let mut node = Node {
        frame: Rect { origin: Point { x: position.x - size.width * anchor.x, y: position.y - size.height * anchor.y }, size },
        bounds_origin: bounds.origin,
        transform: Some(transform),
        anchor: Some(anchor),
        hidden: layers::hidden(layer),
        alpha,
        pictures: layer_content(layer, bounds, classes),
        background: crate::fw::graphics::color_rgba(layers::background(layer)),
        clips: layers::masks(layer),
        children: layer_children(layer, 0, classes, depth),
        motion: poses(motion, size, anchor),
        ..Default::default()
    };
    dress(&mut node, layer, classes, depth);
    node
}

/// A layer's corners, border, shadow and mask.
fn dress(node: &mut Node, layer: Id, classes: &Classes, depth: usize) {
    let looks = crate::fw::layers::looks(layer);
    node.radius = looks.corner_radius;
    node.border = looks.border;
    node.shadow = looks.shadow.map(|(color, radius, offset, path)| Shadow { color, radius, offset, path: (path != 0).then(|| crate::fw::graphics::path_points(path)) });
    if looks.mask != 0 && depth < 64 {
        node.mask = Some(Box::new(layer_node(looks.mask, classes, depth + 1)));
    }
}

/// What a layer itself shows over its background: its contents (placed by
/// its contentsGravity), a gradient layer's colours, a shape layer's path.
/// `bounds`: the layer's bounds.
fn layer_content(layer: Id, bounds: Rect, classes: &Classes) -> Vec<Shown> {
    use crate::fw::layers;
    let size = bounds.size;
    let plain = |path: String, rect: Rect| Shown { path, mode: 0, size: (rect.size.width, rect.size.height), rect, caps: None, tile: false, text: None, alpha: 1.0 };
    let mut out = Vec::new();
    let contents = layers::contents(layer);
    // A UIImage as contents shows (some games set one); a CGImage at contentsScale pixels per point.
    let own = if contents == 0 {
        None
    } else if Classes::is(contents, classes.image) {
        let points = crate::fw::uikit::image_points(contents);
        Some((format!("{IMAGE_PREFIX}{contents:x}"), (points.width, points.height)))
    } else {
        let scale = layers::contents_scale(layer);
        let scale = if scale > 0.0 { scale } else { 1.0 };
        crate::fw::graphics::cg_size(contents).map(|(w, h)| (format!("{CG_IMAGE_PREFIX}{contents:x}"), (w as f64 / scale, h as f64 / scale)))
    };
    if let Some((path, own)) = own {
        out.push(Shown { size: own, ..plain(path, gravity_rect(&layers::contents_gravity(layer), own, size, layers::contents_are_flipped(layer))) });
    }
    let whole = Rect { origin: Point::default(), size };
    if Classes::is(layer, classes.gradient_layer) {
        let stops = layers::gradient_stops(layer);
        if !stops.is_empty() {
            let (start, end) = layers::gradient_points(layer);
            let mut path = format!("{GRADIENT_PREFIX}{},{},{},{},{},{}", size.width, size.height, start.x, start.y, end.x, end.y);
            for ([r, g, b, a], at) in stops {
                path += &format!(";{r},{g},{b},{a},{at}");
            }
            out.push(plain(path, whole));
        }
    }
    if Classes::is(layer, classes.shape_layer) {
        let (path, fill, stroke, width, even_odd) = layers::shape_look(layer);
        let figures = if path == 0 { Vec::new() } else { crate::fw::graphics::path_points(path) };
        let points: Vec<&Point> = figures.iter().flatten().collect();
        let fill = crate::fw::graphics::color_rgba(fill).unwrap_or([0.0; 4]);
        let stroke = crate::fw::graphics::color_rgba(stroke).filter(|_| width > 0.0).unwrap_or([0.0; 4]);
        if !points.is_empty() {
            // The path may reach past the bounds (often a layer with no size draws it all).
            let reach = if stroke[3] > 0.0 { width / 2.0 } else { 0.0 } + 1.0;
            let (x0, x1) = points.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| (lo.min(p.x), hi.max(p.x)));
            let (y0, y1) = points.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| (lo.min(p.y), hi.max(p.y)));
            let area = Rect::new(x0 - reach, y0 - reach, x1 - x0 + 2.0 * reach, y1 - y0 + 2.0 * reach);
            let mut key = format!(
                "{SHAPE_PREFIX}{},{},{},{},{},{},{},{},{},{},{},{},{},{width}",
                area.origin.x, area.origin.y, area.size.width, area.size.height, even_odd as u8, fill[0], fill[1], fill[2], fill[3], stroke[0], stroke[1], stroke[2], stroke[3]
            );
            for figure in &figures {
                key.push(';');
                key += &figure.iter().map(|p| format!("{},{}", p.x, p.y)).collect::<Vec<_>>().join(",");
            }
            out.push(plain(key, Rect { origin: Point { x: area.origin.x - bounds.origin.x, y: area.origin.y - bounds.origin.y }, size: area.size }));
        }
    }
    out
}

/// Where contents of `own` size go in a layer of `size` for a contentsGravity.
/// `flipped`: contentsAreFlipped (an iOS layer whose geometry isn't flipped).
fn gravity_rect(gravity: &str, own: (f64, f64), size: crate::fw::geometry::Size, flipped: bool) -> Rect {
    let ((w, h), (cw, ch)) = ((size.width, size.height), own);
    match gravity {
        "resizeAspect" | "resizeAspectFill" if cw > 0.0 && ch > 0.0 => {
            let k = if gravity == "resizeAspect" { (w / cw).min(h / ch) } else { (w / cw).max(h / ch) };
            Rect::new((w - cw * k) / 2.0, (h - ch * k) / 2.0, cw * k, ch * k)
        }
        "center" | "top" | "bottom" | "left" | "right" | "topLeft" | "topRight" | "bottomLeft" | "bottomRight" => {
            let x = if gravity == "left" || gravity.ends_with("Left") {
                0.0
            } else if gravity == "right" || gravity.ends_with("Right") {
                w - cw
            } else {
                (w - cw) / 2.0
            };
            // Flipped contents (Apple's contentsGravity note) put "top" at the
            // visual bottom and "bottom" at the top; otherwise as named.
            let (top, bottom) = if flipped { (h - ch, 0.0) } else { (0.0, h - ch) };
            let y = if gravity.starts_with("top") {
                top
            } else if gravity.starts_with("bottom") {
                bottom
            } else {
                (h - ch) / 2.0
            };
            Rect::new(x, y, cw, ch)
        }
        _ => Rect::new(0.0, 0.0, w, h),
    }
}

/// A view's layer's zPosition (0 without a layer).
fn z_of(view: Id) -> f64 {
    match view_state(view).layer {
        0 => 0.0,
        layer => crate::fw::layers::z_position(layer),
    }
}

/// A page control's or picker's piece as a picture.
fn piece_shown(piece: crate::fw::pickers::Piece) -> Shown {
    use crate::fw::pickers::Piece;
    let plain = |path: String, rect: Rect| Shown { path, mode: 0, size: (1.0, 1.0), rect, caps: None, tile: false, text: None, alpha: 1.0 };
    match piece {
        Piece::Color(color, rect) => plain(color_path(color), rect),
        Piece::Dot(color, rect) => plain(format!("{DOT_PREFIX}{},{},{},{}", color[0], color[1], color[2], color[3]), rect),
        Piece::Text(look, rect) => Shown { path: String::new(), mode: 0, size: look.size, rect, caps: None, tile: false, text: Some(look), alpha: 1.0 },
    }
}

/// Stands for a round dot of a colour in a picture path until `resolve`.
const DOT_PREFIX: &str = "\u{1}dot:";

/// Stands for a plain colour in a picture path until `resolve`.
const COLOR_PREFIX: &str = "\u{1}color:";

/// The path standing for a colour (straight red, green, blue, alpha).
fn color_path(rgba: [f64; 4]) -> String {
    format!("{COLOR_PREFIX}{},{},{},{}", rgba[0], rgba[1], rgba[2], rgba[3])
}

/// Stands for a CGImage (a layer's contents) until `resolve`.
const CG_IMAGE_PREFIX: &str = "\u{1}cgimage:";

/// Stands for a layer's background or border until `resolve`.
const BOX_PREFIX: &str = "\u{1}box:";

/// Stands for a gradient layer's colours until `resolve`: its size, start
/// and end points, then `;` and each stop's colour and location.
const GRADIENT_PREFIX: &str = "\u{1}gradient:";

/// Stands for a shape layer's path until `resolve`: the area drawn (layer
/// points), even-odd, fill and stroke colours, line width, then `;` and
/// each figure's points.
const SHAPE_PREFIX: &str = "\u{1}shape:";

/// The numbers in a comma-separated list (None if one isn't a number).
fn numbers(text: &str) -> Option<Vec<f64>> {
    text.split(',').map(|n| n.parse().ok()).collect()
}

/// The file for a picture `resolve` draws itself (None: not one of those;
/// Some(None): it can't be made).
fn drawn_file(path: &str, scale: f64) -> Option<Option<String>> {
    use crate::fw::graphics;
    let rgba = |n: &[f64]| [n[0], n[1], n[2], n[3]];
    if let Some(id) = path.strip_prefix(CG_IMAGE_PREFIX) {
        return Some(usize::from_str_radix(id, 16).ok().and_then(graphics::cg_picture));
    }
    if let Some(rest) = path.strip_prefix(BOX_PREFIX) {
        return Some(numbers(rest).filter(|n| n.len() == 12).and_then(|n| graphics::box_picture((n[0], n[1]), n[2], rgba(&n[3..7]), (rgba(&n[7..11]), n[11]), scale)));
    }
    if let Some(rest) = path.strip_prefix(GRADIENT_PREFIX) {
        let mut lists = rest.split(';').map(numbers);
        let made = (|| {
            let head = lists.next()??;
            let stops: Option<Vec<([f64; 4], f64)>> = lists.map(|stop| stop.filter(|s| s.len() == 5).map(|s| (rgba(&s), s[4]))).collect();
            let point = |x: f64, y: f64| Point { x, y };
            (head.len() == 6).then_some(())?;
            graphics::gradient_picture((head[0], head[1]), point(head[2], head[3]), point(head[4], head[5]), &stops?, scale)
        })();
        return Some(made);
    }
    if let Some(rest) = path.strip_prefix(SHAPE_PREFIX) {
        let mut lists = rest.split(';').map(numbers);
        let made = (|| {
            let n = lists.next()??;
            (n.len() == 14).then_some(())?;
            let figures: Option<Vec<Vec<Point>>> = lists.map(|figure| figure.map(|f| f.chunks_exact(2).map(|p| Point { x: p[0], y: p[1] }).collect())).collect();
            let (fill, stroke) = (rgba(&n[5..9]), rgba(&n[9..13]));
            let stroke = (stroke[3] > 0.0 && n[13] > 0.0).then_some((stroke, n[13]));
            graphics::shape_picture(Rect::new(n[0], n[1], n[2], n[3]), &figures?, (fill[3] > 0.0).then_some(fill), n[4] != 0.0, stroke, scale)
        })();
        return Some(made);
    }
    None
}

/// The path standing for a box of `size` points with corners rounded by
/// `radius`: filled with `fill`, and a border of a colour and width inside its edge.
fn box_path(size: (f64, f64), radius: f64, fill: [f64; 4], border: Option<([f64; 4], f64)>) -> String {
    let ([r, g, b, a], width) = border.unwrap_or(([0.0; 4], 0.0));
    format!("{BOX_PREFIX}{},{},{radius},{},{},{},{},{r},{g},{b},{a},{width}", size.0, size.1, fill[0], fill[1], fill[2], fill[3])
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
    let scale = pixels_per_point();
    let mut text_files = crate::fw::graphics::text_pictures(&looks, scale).into_iter();
    pictures
        .into_iter()
        .filter_map(|mut p| {
            if p.text.is_some() {
                p.path = text_files.next()??;
                return Some(p);
            }
            if let Some(file) = drawn_file(&p.path, scale) {
                p.path = file?;
                return Some(p);
            }
            if let Some(color) = p.path.strip_prefix(COLOR_PREFIX) {
                let parts: Vec<f64> = color.split(',').filter_map(|part| part.parse().ok()).collect();
                p.path = crate::fw::graphics::color_picture(parts.try_into().ok()?)?;
                return Some(p);
            }
            if let Some(color) = p.path.strip_prefix(DOT_PREFIX) {
                let parts: Vec<f64> = color.split(',').filter_map(|part| part.parse().ok()).collect();
                p.path = crate::fw::graphics::dot_picture(parts.try_into().ok()?)?;
                p.size = (crate::fw::graphics::DOT_PIXELS as f64, crate::fw::graphics::DOT_PIXELS as f64);
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
    encode_lines(&mut text, pictures);
    text
}

fn encode_lines(text: &mut String, pictures: &[Picture]) {
    for p in pictures {
        let (t, c) = (p.place, p.clip);
        *text += &format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            t.a, t.b, t.c, t.d, t.tx, t.ty, p.area.0, p.area.1, p.alpha, p.mode, p.size.0, p.size.1,
            c.origin.x, c.origin.y, c.size.width, c.size.height, p.path
        );
    }
}

/// Every window's pictures, back to front, with files to draw them from
/// (made for all windows at once, so no window's text files are let go).
#[cfg(test)]
fn screen_pictures(windows: &[Id], classes: &Classes) -> Vec<Picture> {
    crate::fw::graphics::new_screen_check();
    compose_groups(resolve(windows.iter().flat_map(|&window| flatten(&snapshot(window, classes, 0))).collect()))
}

/// Draws each group of pictures together, innermost first (see `Group`),
/// so the list holds only pictures to show.
fn compose_groups(pictures: Vec<Picture>) -> Vec<Picture> {
    let is_mark = |p: &Picture| [GROUP_MARK, MASK_MARK, END_MARK].contains(&p.path.as_str());
    if !pictures.iter().any(is_mark) {
        return pictures;
    }
    let scale = pixels_per_point();
    let mut out: Vec<Picture> = Vec::new();
    let mut starts = Vec::new();
    for p in pictures {
        if p.path == GROUP_MARK {
            starts.push(out.len());
            out.push(p);
        } else if p.path == END_MARK {
            // Inner groups are drawn already: a mark left inside is this group's mask.
            let Some(at) = starts.pop() else { continue };
            let mut content = out.split_off(at + 1);
            let start = out.pop().expect("the group's mark");
            let mask = match content.iter().position(|q| q.path == MASK_MARK) {
                Some(i) => content.split_off(i).split_off(1),
                None => Vec::new(),
            };
            out.extend(crate::fw::graphics::group_pictures(&start, &content, &mask, scale));
        } else {
            out.push(p);
        }
    }
    // Marks of groups a covering view (video, GL) cut short.
    out.retain(|p| !is_mark(p));
    out
}

/// Lines to log once the screen has drawn what they are about: tests tap on
/// them ("screen shown: <class>", "screen drawn after tap at x,y"). Each
/// waits for a draw that changed something and then a screen check that
/// finds nothing new (a table's rows come one draw after the table), so a
/// tap goes to what is really on screen. The bool: a change came since.
static AFTER_DRAW: Mutex<Vec<(String, bool)>> = Mutex::new(Vec::new());

pub fn on_next_draw(line: String) {
    AFTER_DRAW.lock().unwrap_or_else(|e| e.into_inner()).push((line, false));
}

/// Whether the screen changed since it last settled.
static UNSETTLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// After each screen check: a change, or (the first check with nothing new
/// after one) the screen has settled.
fn after_check(changed: bool) {
    let settled = !changed && UNSETTLED.swap(changed, std::sync::atomic::Ordering::Relaxed);
    if changed {
        UNSETTLED.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    announce_drawn(changed);
    if settled {
        crate::fw::touch::screen_settled();
    }
}

fn announce_drawn(changed: bool) {
    let ready: Vec<String> = {
        let mut waiting = AFTER_DRAW.lock().unwrap_or_else(|e| e.into_inner());
        if changed {
            waiting.iter_mut().for_each(|(_, seen)| *seen = true);
            return;
        }
        let (ready, still): (Vec<_>, Vec<_>) = std::mem::take(&mut *waiting).into_iter().partition(|(_, seen)| *seen);
        *waiting = still;
        ready.into_iter().map(|(line, _)| line).collect()
    };
    for line in ready {
        crate::log!("{line}");
    }
}

/// How far ahead coming frames are worked out: moving pictures keep moving
/// this long while the game thread is busy.
const AHEAD: f64 = 0.3;

/// The coming frames to work out now (see `Sampling`): one screen refresh
/// apart on a fixed grid (a moment is sampled the same way every tick),
/// until the running animations end (at most AHEAD); None when none runs.
fn sampling_now(now: f64) -> Option<Sampling> {
    let end = crate::fw::animation::latest_end()?;
    let step = crate::fw::runloop::refresh_seconds();
    let start = (now / step).ceil() * step;
    let count = 1 + ((end.min(now + AHEAD) - start) / step).ceil().max(0.0) as usize;
    (count > 1).then_some(Sampling { start, step, count })
}

/// What was last sent with coming frames: each frame's time and lines.
struct Sent {
    frames: Vec<(f64, String)>,
}

impl Sent {
    /// It still holds at `start` (the same lines then) and reaches `until`.
    fn holds(&self, start: f64, lines: &str, until: f64) -> bool {
        let reaches = self.frames.last().is_some_and(|(t, _)| *t >= until - 1e-6);
        reaches && self.frames.iter().any(|(t, sent)| (t - start).abs() < 1e-6 && sent == lines)
    }
}

/// An animation was added: the next tick goes now, not up to 50 ms later.
static DRAW_SOON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The fade waiting for the next list (seconds, and when it was asked): a
/// view or controller transition asked for a cross-dissolve (`cross_fade`).
static SCREEN_FADE: Mutex<Option<(f64, std::time::Instant)>> = Mutex::new(None);

/// A view or controller transition asked for a cross-dissolve (or a flip
/// or curl, shown as one): the next list fades to the new pictures in that
/// time, as the iPhone dissolves from the old views to the new ones.
pub fn cross_fade(seconds: f64) {
    *SCREEN_FADE.lock().unwrap_or_else(|e| e.into_inner()) = Some((seconds, std::time::Instant::now()));
}

/// Whether a node (or one inside it) shows the GL picture.
fn shows_gl(node: &Node) -> bool {
    !node.hidden && node.alpha > 0.01 && (node.gl || node.children.iter().any(shows_gl))
}

/// The list with its fade line, when a fade is waiting: the new screen comes
/// in over the old one, or, when it shows the GL picture (drawn under every
/// UIKit picture), the old one goes out over it.
fn with_fade(text: String, gl: bool) -> String {
    // The new screen's pictures can take a while to make (a slow phone, a
    // big screen); a transition that changed nothing shown leaves its fade
    // unused, so it is dropped after 2 s rather than given to a later change.
    match SCREEN_FADE.lock().unwrap_or_else(|e| e.into_inner()).take() {
        Some((seconds, asked)) if asked.elapsed().as_secs_f64() < seconds.max(2.0) => fade_line(&text, seconds, gl),
        _ => text,
    }
}

/// `text` with a fade line after its first line: fade `seconds` to it.
fn fade_line(text: &str, seconds: f64, gl: bool) -> String {
    let (first, rest) = text.split_once('\n').unwrap_or((text, ""));
    format!("{first}\n!\tfade\t{}\t{}\n{rest}", (seconds * 1000.0).round() as i64, if gl { "under" } else { "over" })
}

/// When the pictures last changed (frame 0 of a list), for `gap`.
static LAST_CHANGE: Mutex<Option<std::time::Instant>> = Mutex::new(None);

/// How long the main loop waits between ticks: a screen refresh while a
/// finger is down or the pictures changed in the last 100 ms (a card
/// dragged, a view the game moves itself), else 50 ms.
pub fn gap() -> f64 {
    let moving = LAST_CHANGE.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|at| at.elapsed().as_secs_f64() < 0.1);
    if moving || crate::fw::touch::fingers_down() { crate::fw::runloop::refresh_seconds() } else { 0.05 }
}

pub fn draw_soon() {
    DRAW_SOON.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// The windows' pictures in each coming frame (made into files; frames
/// after the first only while their groups are made already), with times.
fn screen_frames(nodes: &[Node], sampling: Sampling) -> Vec<(f64, Vec<Picture>)> {
    let mut out = Vec::new();
    for (k, pictures) in frames(nodes, sampling.count).into_iter().enumerate() {
        let time = sampling.start + k as f64 * sampling.step;
        let resolved = resolve(pictures);
        let composed = if k == 0 { Some(compose_groups(resolved)) } else { crate::fw::graphics::only_made(|| compose_groups(resolved)) };
        match composed {
            Some(pictures) => out.push((time, pictures)),
            None => break,
        }
    }
    out
}

fn lines_of(pictures: &[Picture]) -> String {
    let mut lines = String::new();
    encode_lines(&mut lines, pictures);
    lines
}

/// From the main loop: sends the pictures when they changed (at most 20
/// times a second, or at once after an animation was added). While
/// animations run, the list carries the coming frames too (encode_frames);
/// they are worked out again only when what is shown stops matching them or
/// they run short.
pub fn tick() {
    let Some(screen) = screen() else { return };
    static LAST: Mutex<Option<(std::time::Instant, String)>> = Mutex::new(None);
    static SENT: Mutex<Option<Sent>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let soon = DRAW_SOON.load(std::sync::atomic::Ordering::Relaxed);
    // A little under the gap, so a loop woken on time isn't turned away.
    if !soon && last.as_ref().is_some_and(|(at, _)| at.elapsed().as_secs_f64() < gap() * 0.9) {
        return;
    }
    DRAW_SOON.store(false, std::sync::atomic::Ordering::Relaxed);
    let classes = Classes::find();
    let windows = crate::fw::uikit::windows();
    let (wide, high, _) = crate::fw::uikit::screen_points();
    let now = crate::fw::runloop::media_time();
    let mut sent = SENT.lock().unwrap_or_else(|e| e.into_inner());
    crate::fw::graphics::new_screen_check();
    let sampling = sampling_now(now);
    let nodes: Vec<Node> = match sampling {
        Some(sampling) => with_sampling(sampling, || windows.iter().map(|&w| snapshot(w, &classes, 0)).collect()),
        None => windows.iter().map(|&w| snapshot(w, &classes, 0)).collect(),
    };
    let gl = nodes.iter().any(shows_gl);
    let (pictures, text) = match sampling {
        None => {
            *sent = None;
            let pictures = compose_groups(resolve(frames(&nodes, 1).remove(0)));
            let text = encode((wide, high), &pictures);
            (pictures, text)
        }
        Some(sampling) => {
            let first = compose_groups(resolve(frames(&nodes, 1).remove(0)));
            let lines = lines_of(&first);
            // What was sent still holds while it matches now and reaches the next tick or the end.
            let until = (sampling.start + (sampling.count - 1) as f64 * sampling.step).min(sampling.start + 0.15);
            if sent.as_ref().is_some_and(|s| s.holds(sampling.start, &lines, until)) {
                let text = last.as_ref().map(|(_, text)| text.clone()).unwrap_or_default();
                (first, text)
            } else {
                let timed = screen_frames(&nodes, sampling);
                *sent = Some(Sent { frames: timed.iter().map(|(t, p)| (*t, lines_of(p))).collect() });
                let text = encode_frames((wide, high), &timed);
                (first, text)
            }
        }
    };
    drop(sent);
    let changed = last.as_ref().is_none_or(|(_, sent)| *sent != text);
    let text = if changed { with_fade(text, gl) } else { text };
    // What counts as a change for "settled" (tests tap on a settled screen):
    // the shown pictures, not the coming frames' times; and with an animation
    // that repeats forever, only which pictures show, not where.
    static SHOWN: Mutex<String> = Mutex::new(String::new());
    let shown = if crate::fw::animation::latest_end().is_some_and(f64::is_infinite) {
        pictures.iter().map(|p| p.path.as_str()).collect::<Vec<_>>().join("\n")
    } else {
        lines_of(&pictures)
    };
    let new_screen = std::mem::replace(&mut *SHOWN.lock().unwrap_or_else(|e| e.into_inner()), shown.clone()) != shown;
    if changed {
        *LAST_CHANGE.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());
    }
    if changed && (last.is_some() || !pictures.is_empty()) {
        // The first changes in full (where each picture goes), then only how many.
        static LOGGED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        if LOGGED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 20 {
            crate::log!("pictures: {} shown:\n{}", pictures.len(), encode((wide, high), &pictures).trim_end());
        } else {
            crate::log!("pictures: {} shown", pictures.len());
        }
        (screen.show)(&text);
    }
    *last = Some((std::time::Instant::now(), text));
    drop(last);
    after_check(new_screen);
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
    fn a_line_waiting_for_the_next_draw_comes_once_the_screen_has_changed_and_settled() {
        let mut watch = crate::report::LineWatch::now();
        let seen = |watch: &mut crate::report::LineWatch| watch.wait_for("draw test: the screen after a tap", std::time::Duration::ZERO);
        on_next_draw("draw test: the screen after a tap".to_string());
        announce_drawn(false);
        assert!(!seen(&mut watch), "nothing new was drawn yet");
        announce_drawn(true);
        assert!(!seen(&mut watch), "still drawing (a table's rows come one draw later)");
        announce_drawn(true);
        announce_drawn(false);
        assert!(seen(&mut watch), "logged once a check finds nothing new after the change");
        announce_drawn(true);
        announce_drawn(false);
        assert!(!watch.wait_for("draw test: the screen after a tap", std::time::Duration::ZERO), "logged once");
    }

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

    /// A real tiling UIImage in an image view: repeated at its size, also
    /// with no caps at all (the whole picture tiles).
    #[test]
    fn tiling_images_repeat_in_their_views() {
        use crate::fw::uikit::{make_image, ImageInfo};
        setup();
        let image = make_image(ImageInfo { path: "/nowhere/10x10.png".into(), pixels: (10, 10), scale: 1.0, caps: Some([0.0; 4]), tile: true, ..Default::default() });
        let view = new_view("UIImageView", rect(0.0, 0.0, 30.0, 20.0));
        runtime::send(view, "setImage:", &[image]);
        let pictures = flatten(&window(vec![snapshot(view, &Classes::find(), 0)]));
        let tiles: Vec<((f64, f64), (f64, f64))> = pictures.iter().map(|p| (at(p, 0.0, 0.0), p.area)).collect();
        assert_eq!(tiles, [((0.0, 0.0), (10.0, 10.0)), ((10.0, 0.0), (10.0, 10.0)), ((20.0, 0.0), (10.0, 10.0)), ((0.0, 10.0), (10.0, 10.0)), ((10.0, 10.0), (10.0, 10.0)), ((20.0, 10.0), (10.0, 10.0))]);
    }

    /// Held by tests that make text files: they share one cache, and a test
    /// checking that files stay must not see another test's screen evict them.
    static TEXT_FILES: Mutex<()> = Mutex::new(());

    /// Text on two windows at once keeps every file while both show it,
    /// however many labels there are between them.
    #[test]
    fn text_on_every_window_keeps_its_files() {
        setup();
        let _files = TEXT_FILES.lock().unwrap_or_else(|e| e.into_inner());
        let windows: Vec<Id> = [200, 57]
            .iter()
            .enumerate()
            .map(|(w, &count)| {
                let window = new_view("UIWindow", rect(0.0, 0.0, 568.0, 320.0));
                for n in 0..count {
                    let label = new_view("UILabel", rect((n % 50) as f64 * 10.0, (n / 50) as f64 * 10.0, 8.0, 8.0));
                    runtime::send(label, "setText:", &[crate::fw::string::make(&format!("{w}.{n}"))]);
                    runtime::send(window, "addSubview:", &[label]);
                }
                window
            })
            .collect();
        let shown = screen_pictures(&windows, &Classes::find());
        assert_eq!(shown.len(), 257);
        let gone: Vec<&str> = shown.iter().map(|p| p.path.as_str()).filter(|path| !std::path::Path::new(path).exists()).collect();
        assert!(gone.is_empty(), "{} shown labels lost their files", gone.len());
    }

    fn setup() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
    }

    /// While a view moves by Core Animation, the list holds its coming
    /// frames too, one sampling step apart, each with its time, so the
    /// phone can draw every screen refresh between lists.
    #[test]
    fn transitions_fade_for_the_time_the_game_asks() {
        use crate::fw::uikit::transition_fade;
        const CROSS_DISSOLVE: usize = 5 << 20;
        assert_eq!(transition_fade(0.5, CROSS_DISSOLVE | 1 << 8), Some(0.5), "SpellCraft's screen switch");
        assert_eq!(transition_fade(0.3, 1 << 20), Some(0.3), "a flip fades too");
        assert_eq!(transition_fade(0.5, 0), None, "no transition asked");
        assert_eq!(transition_fade(0.0, CROSS_DISSOLVE), None, "no time");
        let list = "480\t320\nline one\nline two";
        assert_eq!(fade_line(list, 0.5, false), "480\t320\n!\tfade\t500\tover\nline one\nline two");
        assert_eq!(fade_line("480\t320", 0.25, true), "480\t320\n!\tfade\t250\tunder\n", "the old screen fades out over a GL one");
    }

    #[test]
    fn moving_views_send_their_coming_frames() {
        setup();
        let parent = new_view("UIView", rect(0.0, 0.0, 300.0, 300.0));
        // Already at its end, as a game sets it before animating there.
        let moving = new_view("UIView", rect(100.0, 0.0, 10.0, 10.0));
        runtime::send(moving, "setBackgroundColor:", &[runtime::send(runtime::class_named("UIColor"), "redColor", &[])]);
        runtime::send(parent, "addSubview:", &[moving]);
        let animation = runtime::send(runtime::class_named("CABasicAnimation"), "animationWithKeyPath:", &[crate::fw::string::make("position")]);
        runtime::send(animation, "setFromValue:", &[crate::fw::layers::boxed("valueWithCGPoint:", Point { x: 5.0, y: 5.0 })]);
        runtime::send(animation, "setToValue:", &[crate::fw::layers::boxed("valueWithCGPoint:", Point { x: 105.0, y: 5.0 })]);
        send_struct(animation, "setDuration:", 1.0f64);
        let layer = runtime::send(moving, "layer", &[]);
        // Begins at a set time, so a slow machine doesn't move it on before sampling.
        let start = crate::fw::runloop::media_time();
        send_struct(animation, "setBeginTime:", start);
        runtime::send(layer, "addAnimation:forKey:", &[animation, 0]);
        let sampling = Sampling { start, step: 0.25, count: 5 };
        let node = with_sampling(sampling, || snapshot(parent, &Classes::find(), 0));
        let frames = frames(&[window(vec![node])], sampling.count);
        let lefts: Vec<f64> = frames.iter().map(|f| at(&f[0], 0.0, 0.0).0).collect();
        assert_eq!(lefts, [0.0, 25.0, 50.0, 75.0, 100.0], "each frame where the animation has it then");
        let timed: Vec<(f64, Vec<Picture>)> = frames.into_iter().enumerate().map(|(k, f)| (start + k as f64 * 0.25, f)).collect();
        let text = encode_frames((300.0, 300.0), &timed);
        let times: Vec<i64> = text.lines().filter_map(|l| l.strip_prefix("@\t")).map(|t| t.parse().unwrap()).collect();
        assert_eq!(times.len(), 4, "{text}");
        assert!(times.windows(2).all(|w| (w[1] - w[0] - 250_000_000).abs() < 1000), "{times:?}");
        assert_eq!(text.lines().next().unwrap().split('\t').nth(2).map(|t| t.parse::<i64>().unwrap()), Some((start * 1e9) as i64));
        runtime::send(layer, "removeAllAnimations", &[]);
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
        let _files = TEXT_FILES.lock().unwrap_or_else(|e| e.into_inner());
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

    /// A press may last one frame, and picture files are made on their own
    /// thread: the pictures a pressed button would show of its own (not the
    /// normal ones drawn darker) have their files started while it is shown
    /// unpressed, so the first press doesn't show the button without them.
    #[test]
    fn a_shown_buttons_own_pressed_pictures_are_made_beforehand() {
        use crate::fw::uikit::{image_info, make_image, ImageInfo};
        setup();
        let picture = |caps| make_image(ImageInfo { path: "/nowhere/pressed.png".to_string(), pixels: (20, 20), scale: 1.0, orientation: 1, caps, ..Default::default() });
        let started = |image: Id| {
            let info = image_info(image).unwrap();
            info.parts_started.load(std::sync::atomic::Ordering::Relaxed) || info.picture_started.load(std::sync::atomic::Ordering::Relaxed)
        };
        let (normal, pressed_background, pressed_icon) = (picture(Some([5.0; 4])), picture(Some([5.0; 4])), picture(None));
        let button = new_view("UIButton", rect(0.0, 0.0, 100.0, 40.0));
        runtime::send(button, "setBackgroundImage:forState:", &[normal, 0]);
        runtime::send(button, "setBackgroundImage:forState:", &[pressed_background, 1]);
        runtime::send(button, "setImage:forState:", &[pressed_icon, 1]);
        assert!(!started(pressed_background) && !started(pressed_icon));
        snapshot(button, &Classes::find(), 0);
        assert!(started(pressed_background), "the pressed background's parts");
        assert!(started(pressed_icon), "the pressed picture's file");
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

    /// A progress bar shows its track over its bounds and the part done
    /// from the left, in their colours (or pictures); its progress stays
    /// between 0 and 1.
    #[test]
    fn progress_bars_show_how_far_along_they_are() {
        setup();
        assert_ne!(runtime::class_named("UIProgressView"), 0, "no UIProgressView");
        let bar = new_view("UIProgressView", rect(0.0, 0.0, 100.0, 9.0));
        send_struct(bar, "setProgress:", 0.25f32);
        runtime::send(bar, "setProgressTintColor:", &[runtime::send(runtime::class_named("UIColor"), "redColor", &[])]);
        let shown = || snapshot(bar, &Classes::find(), 0).pictures.into_iter().map(|p| (p.path, p.rect)).collect::<Vec<_>>();
        let track = color_path(crate::fw::progress::TRACK);
        assert_eq!(shown(), [(track.clone(), rect(0.0, 0.0, 100.0, 9.0)), (color_path([1.0, 0.0, 0.0, 1.0]), rect(0.0, 0.0, 25.0, 9.0))]);
        send_struct(bar, "setProgress:", 3.0f32);
        assert_eq!(shown()[1].1, rect(0.0, 0.0, 100.0, 9.0));
        send_struct(bar, "setProgress:", 0.0f32);
        assert_eq!(shown(), [(track, rect(0.0, 0.0, 100.0, 9.0))]);
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
        let _files = TEXT_FILES.lock().unwrap_or_else(|e| e.into_inner());
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

    /// A view's layer decides how it is drawn: it turns around its anchor
    /// point, a higher zPosition puts it in front of its siblings, and a
    /// running Core Animation animation shows over its own values.
    #[test]
    fn views_follow_their_layers_anchor_z_order_and_animations() {
        setup();
        let parent = new_view("UIView", rect(0.0, 0.0, 300.0, 300.0));
        let (turned, plain) = (new_view("UIView", rect(100.0, 100.0, 40.0, 20.0)), new_view("UIView", rect(0.0, 0.0, 10.0, 10.0)));
        let color = |name: &str| runtime::send(runtime::class_named("UIColor"), name, &[]);
        runtime::send(turned, "setBackgroundColor:", &[color("redColor")]);
        runtime::send(plain, "setBackgroundColor:", &[color("blueColor")]);
        runtime::send(parent, "addSubview:", &[turned]);
        runtime::send(parent, "addSubview:", &[plain]);
        let layer = runtime::send(turned, "layer", &[]);
        send_struct(layer, "setAnchorPoint:", Point { x: 0.0, y: 0.0 });
        send_struct(turned, "setTransform:", Transform { a: 0.0, b: 1.0, c: -1.0, d: 0.0, tx: 0.0, ty: 0.0 });
        send_struct(layer, "setZPosition:", 1.0f64);
        let animation = runtime::send(runtime::class_named("CABasicAnimation"), "animationWithKeyPath:", &[crate::fw::string::make("opacity")]);
        runtime::send(animation, "setFromValue:", &[crate::fw::collections::number_float(0.25)]);
        runtime::send(animation, "setToValue:", &[crate::fw::collections::number_float(0.25)]);
        send_struct(animation, "setDuration:", 100.0f64);
        runtime::send(runtime::send(plain, "layer", &[]), "addAnimation:forKey:", &[animation, 0]);
        let pictures = flatten(&window(vec![snapshot(parent, &Classes::find(), 0)]));
        let paths: Vec<&str> = pictures.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(paths, [color_path([0.0, 0.0, 1.0, 1.0]), color_path([1.0, 0.0, 0.0, 1.0])], "the higher zPosition in front");
        // Its top-left corner is the anchor: it stays where the position is.
        assert_eq!(at(&pictures[1], 0.0, 0.0), (120.0, 110.0));
        assert_eq!(at(&pictures[1], 40.0, 0.0), (120.0, 150.0));
        assert!((pictures[0].alpha - 0.25).abs() < 1e-6, "animated opacity: {}", pictures[0].alpha);
        runtime::send(runtime::send(plain, "layer", &[]), "removeAllAnimations", &[]);
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
        let _files = TEXT_FILES.lock().unwrap_or_else(|e| e.into_inner());
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

    /// A window does not clip what it holds, the screen does: a game's
    /// 320x480 window on a 568x320 screen still shows the right side of its
    /// 568x320 views (as on an iPhone 5, where only touches miss there).
    #[test]
    fn windows_do_not_clip_what_they_hold() {
        let small = Node { alpha: 1.0, frame: rect(0.0, 0.0, 320.0, 480.0), children: vec![view(rect(0.0, 0.0, 568.0, 320.0), Some("menu.png"), vec![])], ..Default::default() };
        let pictures = flatten(&small);
        let (wide, high, _) = crate::fw::uikit::screen_points();
        assert_eq!(pictures[0].clip, rect(0.0, 0.0, wide.max(320.0), high.max(480.0)));
    }

    /// A new layer of a Core Animation class with a frame.
    fn new_layer(class: &str, frame: Rect) -> Id {
        let layer = runtime::send(runtime::send(runtime::class_named(class), "alloc", &[]), "init", &[]);
        send_struct(layer, "setFrame:", frame);
        layer
    }

    /// Layers show on screen as Core Animation draws them: the shadow under
    /// everything, then the background (round corners), the contents, the
    /// sublayers (one put at index 0 behind the view's subviews, a higher
    /// zPosition in front, hidden ones not at all, opacity multiplied), and
    /// the border on top.
    #[test]
    fn layers_draw_in_core_animation_order() {
        setup();
        let rgba = crate::fw::graphics::make_color;
        let (red, green, blue, yellow, white) = ([1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0], [1.0, 1.0, 0.0, 1.0], [1.0, 1.0, 1.0, 1.0]);
        let window = new_view("UIWindow", rect(0.0, 0.0, 480.0, 320.0));
        let panel = new_view("UIView", rect(10.0, 20.0, 100.0, 50.0));
        runtime::send(window, "addSubview:", &[panel]);
        let child = new_view("UIView", rect(0.0, 0.0, 10.0, 10.0));
        runtime::send(child, "setBackgroundColor:", &[rgba(blue)]);
        runtime::send(panel, "addSubview:", &[child]);
        let host = runtime::send(panel, "layer", &[]);
        runtime::send(host, "setBackgroundColor:", &[rgba(red)]);
        send_struct(host, "setCornerRadius:", 8.0f64);
        send_struct(host, "setBorderWidth:", 2.0f64);
        runtime::send(host, "setBorderColor:", &[rgba(green)]);
        send_struct(host, "setShadowOpacity:", 0.5f32);
        let under = new_layer("CALayer", rect(0.0, 0.0, 100.0, 50.0));
        runtime::send(under, "setBackgroundColor:", &[rgba(yellow)]);
        runtime::send(host, "insertSublayer:atIndex:", &[under, 0]);
        let front = new_layer("CALayer", rect(0.0, 0.0, 5.0, 5.0));
        runtime::send(front, "setBackgroundColor:", &[rgba(white)]);
        send_struct(front, "setZPosition:", 1.0f64);
        runtime::send(host, "addSublayer:", &[front]);
        let over = new_layer("CALayer", rect(5.0, 5.0, 20.0, 10.0));
        let bitmap = crate::fw::graphics::Bitmap { width: 2, height: 1, bits_per_pixel: 32, bytes_per_row: 8, info: crate::fw::graphics::ALPHA_LAST, space: crate::fw::graphics::Space::Rgb, data: vec![255; 8] };
        runtime::send(over, "setContents:", &[crate::fw::graphics::make_image(bitmap)]);
        send_struct(over, "setOpacity:", 0.5f32);
        runtime::send(host, "addSublayer:", &[over]);
        let hidden = new_layer("CALayer", rect(0.0, 0.0, 5.0, 5.0));
        runtime::send(hidden, "setBackgroundColor:", &[rgba(white)]);
        runtime::send(hidden, "setHidden:", &[1]);
        runtime::send(host, "addSublayer:", &[hidden]);
        let pictures = flatten(&snapshot(window, &Classes::find(), 0));
        let kinds: Vec<String> = pictures.iter().map(|p| if p.path.starts_with(CG_IMAGE_PREFIX) { CG_IMAGE_PREFIX.into() } else { p.path.clone() }).collect();
        let size = (100.0, 50.0);
        assert_eq!(
            kinds,
            [
                GROUP_MARK.to_string(),
                box_path(size, 8.0, red, None),
                color_path(yellow),
                color_path(blue),
                CG_IMAGE_PREFIX.into(),
                color_path(white),
                box_path(size, 8.0, [0.0; 4], Some((green, 2.0))),
                END_MARK.into(),
            ]
        );
        let shadow = pictures[0].group.as_ref().and_then(|g| g.shadow.clone());
        assert_eq!(shadow, Some(Shadow { color: [0.0, 0.0, 0.0, 0.5], radius: 3.0, offset: (0.0, -3.0), path: None }), "Core Animation's defaults");
        let contents = &pictures[4];
        assert_eq!((at(contents, 0.0, 0.0), contents.area, contents.alpha), ((15.0, 25.0), (20.0, 10.0), 0.5));
    }

    /// Every contentsGravity, for a plain iOS layer (contentsAreFlipped:
    /// Core Animation's "top" is the visual bottom) and a geometryFlipped one
    /// (not flipped: "top" is the top). 20x20 contents in a 100x50 layer.
    #[test]
    fn contents_gravity_places_contents() {
        let size = Size { width: 100.0, height: 50.0 };
        // Name, then where it goes flipped (iOS's default) and not flipped.
        let table = [
            ("center", rect(40.0, 15.0, 20.0, 20.0), rect(40.0, 15.0, 20.0, 20.0)),
            ("top", rect(40.0, 30.0, 20.0, 20.0), rect(40.0, 0.0, 20.0, 20.0)),
            ("bottom", rect(40.0, 0.0, 20.0, 20.0), rect(40.0, 30.0, 20.0, 20.0)),
            ("left", rect(0.0, 15.0, 20.0, 20.0), rect(0.0, 15.0, 20.0, 20.0)),
            ("right", rect(80.0, 15.0, 20.0, 20.0), rect(80.0, 15.0, 20.0, 20.0)),
            ("topLeft", rect(0.0, 30.0, 20.0, 20.0), rect(0.0, 0.0, 20.0, 20.0)),
            ("topRight", rect(80.0, 30.0, 20.0, 20.0), rect(80.0, 0.0, 20.0, 20.0)),
            ("bottomLeft", rect(0.0, 0.0, 20.0, 20.0), rect(0.0, 30.0, 20.0, 20.0)),
            ("bottomRight", rect(80.0, 0.0, 20.0, 20.0), rect(80.0, 30.0, 20.0, 20.0)),
            ("resize", rect(0.0, 0.0, 100.0, 50.0), rect(0.0, 0.0, 100.0, 50.0)),
            ("resizeAspect", rect(25.0, 0.0, 50.0, 50.0), rect(25.0, 0.0, 50.0, 50.0)),
            ("resizeAspectFill", rect(0.0, -25.0, 100.0, 100.0), rect(0.0, -25.0, 100.0, 100.0)),
        ];
        for (name, flipped, upright) in table {
            assert_eq!(gravity_rect(name, (20.0, 20.0), size, true), flipped, "{name}, flipped");
            assert_eq!(gravity_rect(name, (20.0, 20.0), size, false), upright, "{name}, geometryFlipped");
        }
    }

    /// A CGImage as a layer's contents shows at contentsScale pixels per
    /// point where its gravity puts it; geometryFlipped turns "bottom" up.
    #[test]
    fn cg_image_contents_follow_scale_and_gravity() {
        setup();
        let window = new_view("UIWindow", rect(0.0, 0.0, 200.0, 200.0));
        let host = runtime::send(window, "layer", &[]);
        let layer = new_layer("CALayer", rect(10.0, 10.0, 100.0, 100.0));
        let bitmap = crate::fw::graphics::Bitmap { width: 40, height: 20, bits_per_pixel: 32, bytes_per_row: 160, info: crate::fw::graphics::ALPHA_LAST, space: crate::fw::graphics::Space::Rgb, data: vec![255; 3200] };
        runtime::send(layer, "setContents:", &[crate::fw::graphics::make_image(bitmap)]);
        send_struct(layer, "setContentsScale:", 2.0f64);
        runtime::send(layer, "setContentsGravity:", &[crate::fw::string::make("bottomRight")]);
        runtime::send(host, "addSublayer:", &[layer]);
        let contents = || {
            let pictures = flatten(&snapshot(window, &Classes::find(), 0));
            let p = pictures.into_iter().find(|p| p.path.starts_with(CG_IMAGE_PREFIX)).expect("the contents");
            (at(&p, 0.0, 0.0), p.area)
        };
        assert_eq!(contents(), ((90.0, 10.0), (20.0, 10.0)), "40x20 pixels at 2x, Core Animation's bottom: the visual top");
        runtime::send(layer, "setGeometryFlipped:", &[1]);
        assert_eq!(runtime::send(layer, "contentsAreFlipped", &[]) & 0xFF, 0);
        assert_eq!(contents(), ((90.0, 100.0), (20.0, 10.0)), "geometryFlipped: the bottom");
    }

    /// Round corners that clip, masks and shadows are drawn into pictures:
    /// what the corners, the bounds or the mask's clear part cut away is
    /// clear, and the shadow lies under the layer, moved by its offset.
    #[test]
    fn clipped_masked_and_shadowed_layers_become_pictures() {
        setup();
        let _files = TEXT_FILES.lock().unwrap_or_else(|e| e.into_inner());
        let rgba = crate::fw::graphics::make_color;
        let (red, blue, black) = ([1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0], [0.0, 0.0, 0.0, 1.0]);
        let window = new_view("UIWindow", rect(0.0, 0.0, 200.0, 200.0));
        let host = runtime::send(window, "layer", &[]);
        let round = new_layer("CALayer", rect(10.0, 10.0, 40.0, 40.0));
        runtime::send(round, "setBackgroundColor:", &[rgba(red)]);
        send_struct(round, "setCornerRadius:", 20.0f64);
        runtime::send(round, "setMasksToBounds:", &[1]);
        let inside = new_layer("CALayer", rect(-10.0, -10.0, 60.0, 60.0));
        runtime::send(inside, "setBackgroundColor:", &[rgba(blue)]);
        runtime::send(round, "addSublayer:", &[inside]);
        let masked = new_layer("CALayer", rect(100.0, 10.0, 40.0, 40.0));
        runtime::send(masked, "setBackgroundColor:", &[rgba(red)]);
        let mask = new_layer("CALayer", rect(0.0, 0.0, 20.0, 40.0));
        runtime::send(mask, "setBackgroundColor:", &[rgba(black)]);
        runtime::send(masked, "setMask:", &[mask]);
        let shadowed = new_layer("CALayer", rect(10.0, 100.0, 40.0, 40.0));
        runtime::send(shadowed, "setBackgroundColor:", &[rgba(red)]);
        send_struct(shadowed, "setShadowOpacity:", 1.0f32);
        send_struct(shadowed, "setShadowRadius:", 0.0f64);
        send_struct(shadowed, "setShadowOffset:", Size { width: 10.0, height: 10.0 });
        for layer in [round, masked, shadowed] {
            runtime::send(host, "addSublayer:", &[layer]);
        }
        let pictures = screen_pictures(&[window], &Classes::find());
        assert!(pictures.iter().all(|p| !p.path.starts_with('\u{1}')), "every picture has its file: {pictures:?}");
        // The colour of the topmost picture not clear at a screen point.
        let seen = |x: f64, y: f64| -> [u8; 4] {
            for p in pictures.iter().rev() {
                let Some(back) = crate::fw::geometry::invert(p.place) else { continue };
                let (u, v) = crate::fw::geometry::apply(back, x, y);
                if !(0.0..p.area.0).contains(&u) || !(0.0..p.area.1).contains(&v) {
                    continue;
                }
                let (bitmap, _) = crate::fw::graphics::decode(&std::fs::read(&p.path).unwrap()).unwrap();
                let (px, py) = ((u / p.area.0 * bitmap.width as f64) as usize, (v / p.area.1 * bitmap.height as f64) as usize);
                let at = py * bitmap.bytes_per_row + px * 4;
                let pixel: [u8; 4] = bitmap.data[at..at + 4].try_into().unwrap();
                if pixel[3] > 0 {
                    return pixel;
                }
            }
            [0; 4]
        };
        assert_eq!(seen(30.0, 30.0), [0, 0, 255, 255], "the sublayer inside the round layer");
        assert_eq!(seen(13.0, 13.0), [0; 4], "cut by the round corner");
        assert_eq!(seen(5.0, 30.0), [0; 4], "cut at the bounds");
        assert_eq!(seen(110.0, 30.0), [255, 0, 0, 255], "the mask lets it show");
        assert_eq!(seen(130.0, 30.0), [0; 4], "the mask is clear there");
        assert_eq!(seen(30.0, 120.0), [255, 0, 0, 255]);
        assert_eq!(seen(55.0, 145.0), [0, 0, 0, 255], "the shadow, 10 points right and down");
        assert_eq!(seen(15.0, 145.0), [0; 4], "no shadow where it was moved away from");
    }
}
