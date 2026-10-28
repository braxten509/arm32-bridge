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
//! What is shown: picture views (UIImageView and kinds of it) and labels'
//! text, so also buttons' state pictures and titles (their subviews, laid
//! out in controls.rs as UIKit's). Text is drawn into a PNG at the phone's pixels
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
//! may draw in. A rasterized layer that isn't opaque is drawn into one
//! picture too and faded as a whole. A layer turned in 3D is flattened into
//! its parent with its perspective (`Proj`; the phone draws the projective
//! place); one turned away (its plane's normal pointing back) doesn't draw its
//! own background, contents and border unless it is doubleSided. A
//! replicator's copies run its sublayers' animations instanceDelay apart.

use crate::fw::geometry::{Point, Rect, Transform, Transform3D, IDENTITY, IDENTITY_3D};
use crate::fw::graphics::TextLook;
use crate::fw::uikit::{property_get, view_state};
use crate::objc::runtime::{self, Class, Id};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
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
    /// A rounded rectangle it is also cut to: the round corners of a layer
    /// around it that clips (masksToBounds), cut where it is shown (the
    /// phone, compose.rs) as Core Animation cuts them on the GPU.
    pub corner: Option<Corner>,
    /// Inside a layer turned in 3D with perspective (m34): `place` holds
    /// the first two columns of a projective map whose third column this is
    /// (see `Proj`), and the phone draws it so.
    pub perspective: Option<[f64; 3]>,
}

/// A rounded rectangle in screen points: its corners' radii across and down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Corner {
    pub rect: Rect,
    pub radius: (f64, f64),
}

impl Corner {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        let r = self.rect;
        let (rx, ry) = (self.radius.0.min(r.size.width / 2.0), self.radius.1.min(r.size.height / 2.0));
        if !(r.origin.x..r.origin.x + r.size.width).contains(&x) || !(r.origin.y..r.origin.y + r.size.height).contains(&y) {
            return false;
        }
        // Inside the nearest corner's ellipse when in a corner square.
        let dx = (r.origin.x + rx - x).max(x - (r.origin.x + r.size.width - rx)).max(0.0);
        let dy = (r.origin.y + ry - y).max(y - (r.origin.y + r.size.height - ry)).max(0.0);
        rx <= 0.0 || ry <= 0.0 || (dx / rx).powi(2) + (dy / ry).powi(2) <= 1.0
    }
}

impl Picture {
    /// Whether the screen point (points) is inside what it may draw in.
    pub fn shows_at(&self, x: f64, y: f64) -> bool {
        let c = self.clip;
        (c.origin.x..c.origin.x + c.size.width).contains(&x)
            && (c.origin.y..c.origin.y + c.size.height).contains(&y)
            && self.corner.is_none_or(|corner| corner.contains(x, y))
    }
}

/// A layer whose pictures are drawn together (fw/graphics/compose.rs):
/// cut to its round corners, through its mask, or with a shadow under them.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    /// The layer's own points to screen points.
    pub place: Transform,
    /// Turned in 3D with perspective: `place` and this are its projective
    /// place, as a picture's (`Picture::perspective`); its bounds cut so.
    pub perspective: Option<[f64; 3]>,
    /// Its bounds in its own points.
    pub bounds: Rect,
    /// What shows is cut to the bounds, their corners rounded by `radius` (masksToBounds).
    pub clips: bool,
    pub radius: f64,
    /// Its mask layer's pictures come after a MASK_MARK: where they are clear, it is too.
    pub masked: bool,
    pub shadow: Option<Shadow>,
    /// The whole group's opacity (a rasterized layer's: drawn as one
    /// picture, then faded); its pictures carry only what is inside it.
    pub opacity: f64,
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

/// Starts the pictures of a layer a CATransition runs on (`transitions`):
/// first ":" and the layer, then (once its old pictures follow) a tab and
/// how the transition runs. Its place and area are the layer's own points
/// (from its bounds' corner) to screen points.
const TRANSITION_MARK: &str = "\u{1}transition";
/// Between a transition's old pictures and its new ones.
const TRANSITION_TO_MARK: &str = "\u{1}transition-to";
/// Ends a transition's pictures.
const TRANSITION_END_MARK: &str = "\u{1}transition-end";

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
    /// How see-through it is drawn, on top of the view's own alpha.
    pub alpha: f64,
}

/// A view as far as drawing pictures goes.
#[derive(Default)]
pub struct Node {
    /// The view or layer it was built from (its key in NODE_CACHE); 0 for a
    /// replicator's copy.
    pub id: Id,
    /// Its layer (0 for none), where a CATransition may run (`transitions`).
    pub layer: Id,
    pub frame: Rect,
    pub bounds_origin: Point,
    /// Its layer's transform (3D: flattened into its parent with its perspective).
    pub transform: Option<Transform3D>,
    /// Its layer isn't doubleSided: turned away from the viewer, it isn't drawn.
    pub one_sided: bool,
    /// Its layer is rasterized (shouldRasterize): drawn as one picture, its
    /// opacity applies to the whole.
    pub rasterized: bool,
    /// Its layer's anchor point (unit square), what it turns around; None: the middle.
    pub anchor: Option<Point>,
    pub hidden: bool,
    pub alpha: f64,
    /// Its pictures, back to front.
    pub pictures: Vec<Shown>,
    /// It shows a video: what is under it is covered (on iOS also before and
    /// after the video plays, while the view is there).
    pub video: bool,
    /// It changes every tick without a mark (a picker's spinning wheel, a
    /// page control, a progress bar, a text field's cursor, a table): the
    /// pictures are rebuilt every tick while one is on screen.
    pub changing: bool,
    /// It shows the GL picture, which is drawn under every UIKit picture:
    /// what is behind it is covered.
    pub gl: bool,
    /// Its CAEAGLLayer, when `gl` (whose frames the GL picture may hold).
    pub gl_layer: Id,
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
    pub mask: Option<Rc<Node>>,
    /// Its layer's sublayerTransform: applied to what is inside it about its
    /// anchor point, in 3D before they flatten into it (a perspective here
    /// shows its sublayers' 3D turns).
    pub sublayer_transform: Option<Transform3D>,
    pub children: Vec<Rc<Node>>,
    /// How deep in the tree it was built: what is under it stops at 64
    /// levels, so it is reused only at the same depth.
    pub depth: usize,
    /// This node can be kept in the cache and reused on the next commit
    /// (A2): false for anything that changes without a mark or whose child
    /// does, so those are rebuilt every tick.
    pub reusable: bool,
    /// Moving by Core Animation: where it is in each coming frame after the
    /// first (see `Sampling`); empty when it stays as it is.
    pub motion: Vec<Pose>,
    /// Its pictures in each coming frame after the first while a contents
    /// animation changes them (an image view's animationImages); empty:
    /// `pictures` throughout.
    pub picture_motion: Vec<Vec<Shown>>,
}

/// A view's place in one frame: its frame's origin, transform, alpha and
/// background colour.
#[derive(Clone, Copy, Debug)]
pub struct Pose {
    pub origin: Point,
    pub transform: Transform3D,
    pub alpha: f64,
    pub background: Option<[f64; 4]>,
    /// Its bounds then (a layer's size can ease).
    pub bounds: Rect,
}

impl Node {
    /// Its place in coming frame `k` (0: now).
    fn pose(&self, k: usize) -> Pose {
        match k.checked_sub(1).and_then(|i| self.motion.get(i).or(self.motion.last())) {
            Some(pose) if k > 0 => *pose,
            _ => Pose {
                origin: self.frame.origin,
                transform: self.transform.unwrap_or(IDENTITY_3D),
                alpha: self.alpha,
                background: self.background,
                bounds: Rect { origin: self.bounds_origin, size: self.frame.size },
            },
        }
    }

    /// Its pictures in coming frame `k` (0: now).
    fn pictures_at(&self, k: usize) -> &[Shown] {
        match k.checked_sub(1).and_then(|i| self.picture_motion.get(i).or(self.picture_motion.last())) {
            Some(pictures) => pictures,
            None => &self.pictures,
        }
    }
}

/// A projective map of the plane, as a layer turned in 3D is flattened into
/// its parent (a point is a row on the left): (x, y, 1) · m = (X, Y, W) is
/// the point (X / W, Y / W). Rows (a, b, p0), (c, d, p1), (tx, ty, w); an
/// affine one has p0 = p1 = 0 and w = 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Proj(pub [f64; 9]);

impl Proj {
    fn affine(t: Transform) -> Proj {
        Proj([t.a, t.b, 0.0, t.c, t.d, 0.0, t.tx, t.ty, 1.0])
    }

    /// A CATransform3D flattened: its z row and column dropped.
    fn of_3d(m: Transform3D) -> Proj {
        let m = m.0;
        Proj([m[0], m[1], m[3], m[4], m[5], m[7], m[12], m[13], m[15]])
    }

    /// `self` then `other`.
    fn then(self, other: Proj) -> Proj {
        let (a, b) = (self.0, other.0);
        Proj(std::array::from_fn(|i| {
            let (row, column) = (i / 3, i % 3);
            (0..3).map(|k| a[row * 3 + k] * b[k * 3 + column]).sum()
        }))
    }

    fn is_affine(&self) -> bool {
        let m = self.0;
        m[2].abs() < 1e-12 && m[5].abs() < 1e-12 && (m[8] - 1.0).abs() < 1e-12
    }

    /// Its nearest affine map (exact when it is affine).
    fn flat(&self) -> Transform {
        let m = self.0;
        let w = if m[8].abs() > 1e-12 { m[8] } else { 1.0 };
        Transform { a: m[0] / w, b: m[1] / w, c: m[3] / w, d: m[4] / w, tx: m[6] / w, ty: m[7] / w }
    }

    /// As a picture carries it: the affine map, or the first two columns
    /// and the third (`Picture::perspective`).
    fn for_picture(&self) -> (Transform, Option<[f64; 3]>) {
        let m = self.0;
        if self.is_affine() {
            (self.flat(), None)
        } else {
            (Transform { a: m[0], b: m[1], c: m[3], d: m[4], tx: m[6], ty: m[7] }, Some([m[2], m[5], m[8]]))
        }
    }

    fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        let m = self.0;
        let w = x * m[2] + y * m[5] + m[8];
        let w = if w.abs() > 1e-9 { w } else { 1e-9 };
        ((x * m[0] + y * m[3] + m[6]) / w, (x * m[1] + y * m[4] + m[7]) / w)
    }

    /// The screen area (axis-aligned) a rect in its own points covers.
    fn screen_box(&self, rect: Rect) -> Rect {
        if self.is_affine() {
            return screen_box(self.flat(), rect);
        }
        let corners = [(0.0, 0.0), (rect.size.width, 0.0), (0.0, rect.size.height), (rect.size.width, rect.size.height)]
            .map(|(x, y)| self.apply(rect.origin.x + x, rect.origin.y + y));
        let (left, right) = (corners.iter().map(|c| c.0).fold(f64::INFINITY, f64::min), corners.iter().map(|c| c.0).fold(f64::NEG_INFINITY, f64::max));
        let (top, bottom) = (corners.iter().map(|c| c.1).fold(f64::INFINITY, f64::min), corners.iter().map(|c| c.1).fold(f64::NEG_INFINITY, f64::max));
        Rect::new(left, top, right - left, bottom - top)
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

thread_local! {
    /// A GL view's last frame, while its transition's old pictures are made:
    /// its CAEAGLLayer and the CGImage picture path standing for the frame.
    static GL_CONTENTS: std::cell::RefCell<Option<(Id, String)>> = const { std::cell::RefCell::new(None) };
}

/// `make` with the GL view's last frame as a picture (`value`: its layer and
/// picture path), so a transition's old side shows what the GL view last drew
/// where the view is. Nothing changes when `value` is None.
fn with_gl_contents<T>(value: Option<(Id, String)>, make: impl FnOnce() -> T) -> T {
    let before = GL_CONTENTS.with(|c| std::mem::replace(&mut *c.borrow_mut(), value));
    let made = make();
    GL_CONTENTS.with(|c| *c.borrow_mut() = before);
    made
}

/// The pictures of each coming frame (see `Sampling`) of the windows, a
/// running transition's layer's between its marks with its old pictures.
fn frames(windows: &[Rc<Node>], count: usize) -> Vec<Vec<Picture>> {
    let transitions = TRANSITIONS.with(|t| t.borrow().clone());
    let layers = transitions.iter().map(|t| t.layer).collect();
    with_marked(layers, || (0..count.max(1)).map(|k| with_old_pictures(windows.iter().flat_map(|w| flatten_at(w, k)).collect(), &transitions)).collect())
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

/// The pictures under `node` (a window, at the screen's top-left, turned
/// as an old app's window is: uikit::window_turn), back to front.
pub fn flatten(node: &Node) -> Vec<Picture> {
    let mut out = Vec::new();
    // A window clips nothing (its own clipsToBounds aside), the screen does:
    // on an iPhone 5 a 320x480 window still shows its views below 480. The
    // area reaches over the window too; past the screen the phone cuts it.
    let (wide, high, _) = crate::fw::uikit::screen_points();
    let area = Rect::new(0.0, 0.0, wide.max(node.frame.size.width), high.max(node.frame.size.height));
    walk(node, 0, crate::fw::uikit::window_turn().map(|turn| (Proj::affine(turn), None)), FACING_VIEWER, 1.0, area, None, &mut out);
    out
}

/// `flatten` in coming frame `k` (see `Sampling`).
fn flatten_at(node: &Node, k: usize) -> Vec<Picture> {
    let (wide, high, _) = crate::fw::uikit::screen_points();
    let area = Rect::new(0.0, 0.0, wide.max(node.frame.size.width), high.max(node.frame.size.height));
    let mut out = Vec::new();
    walk(node, k, crate::fw::uikit::window_turn().map(|turn| (Proj::affine(turn), None)), FACING_VIEWER, 1.0, area, None, &mut out);
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

/// The turn (3x3, a point a row on the left) of the window's plane: none.
const FACING_VIEWER: [f64; 9] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];

/// The parts of `area` outside `hole` (a part of it): the bands above and
/// below it, then left and right of it, the empty ones left out.
fn outside(area: Rect, hole: Rect) -> Vec<Rect> {
    let (left, top) = (area.origin.x, area.origin.y);
    let (right, bottom) = (left + area.size.width, top + area.size.height);
    let (hole_left, hole_top) = (hole.origin.x, hole.origin.y);
    let (hole_right, hole_bottom) = (hole_left + hole.size.width, hole_top + hole.size.height);
    [
        Rect::new(left, top, area.size.width, hole_top - top),
        Rect::new(left, hole_bottom, area.size.width, bottom - hole_bottom),
        Rect::new(left, hole_top, hole_left - left, hole.size.height),
        Rect::new(hole_right, hole_top, right - hole_right, hole.size.height),
    ]
    .into_iter()
    .filter(|band| band.size.width > 0.0 && band.size.height > 0.0)
    .collect()
}

/// `parent`: the parent's own points to screen points (None for the window),
/// with the parent's sublayerTransform as a step in its own points (about
/// its anchor point); `facing`: the parent's 3D turns and scales put
/// together (only which way its plane faces is read from it); `clip`: the
/// screen area the node may draw in.
#[allow(clippy::too_many_arguments)]
fn walk(node: &Node, k: usize, parent: Option<(Proj, Option<Transform3D>)>, facing: [f64; 9], parent_alpha: f64, clip: Rect, corner: Option<Corner>, out: &mut Vec<Picture>) {
    use crate::fw::geometry::concat_3d;
    use crate::fw::layers::affine_to_3d;
    let pose = node.pose(k);
    let alpha = parent_alpha * pose.alpha;
    let (size, bounds_origin) = (pose.bounds.size, pose.bounds.origin);
    let anchor = node.anchor.unwrap_or(Point { x: 0.5, y: 0.5 });
    // Its transform in its parent's points, the parent's sublayerTransform
    // after it (still in 3D, so a perspective there applies to its turns).
    let turn = match parent {
        Some((_, Some(step))) => concat_3d(pose.transform, step),
        _ => pose.transform,
    };
    // The view's own points (its bounds) to screen points.
    let own = match parent {
        None => Proj::affine(moved(-bounds_origin.x, -bounds_origin.y)),
        Some((parent, step)) => {
            // It turns and scales around its anchor point (the middle unless
            // moved), flattened into its parent with its perspective.
            let centre = (pose.origin.x + size.width * anchor.x, pose.origin.y + size.height * anchor.y);
            let to_middle = moved(-bounds_origin.x - size.width * anchor.x, -bounds_origin.y - size.height * anchor.y);
            let local = concat_3d(concat_3d(affine_to_3d(to_middle), pose.transform), affine_to_3d(moved(centre.0, centre.1)));
            Proj::of_3d(step.map_or(local, |step| concat_3d(local, step))).then(parent)
        }
    };
    // A layer a transition runs on: its pictures (none when hidden: it may
    // be fading out) between marks.
    let marked = node.layer != 0 && MARKED.with(|m| m.borrow().contains(&node.layer));
    if marked {
        let place = Proj::affine(moved(bounds_origin.x, bounds_origin.y)).then(own).flat();
        // Its clip is what clips it from around it, and its own bounds when
        // it masks to them: that stays put while the transition moves it.
        let still = if node.clips { meet(clip, own.screen_box(Rect { origin: bounds_origin, size })) } else { clip };
        out.push(Picture { path: format!("{TRANSITION_MARK}:{:x}", node.layer), place, area: (size.width, size.height), alpha: 1.0, mode: 0, size: (size.width, size.height), text: None, clip: still, group: None, corner: None, perspective: None });
    }
    if node.hidden || alpha <= 0.01 {
        if marked {
            out.push(Picture { path: TRANSITION_END_MARK.into(), ..mark_picture(clip) });
        }
        return;
    }
    // Its own sublayerTransform about its anchor point, for what is inside it.
    let inner = node.sublayer_transform.map(|s| {
        let at = (bounds_origin.x + size.width * anchor.x, bounds_origin.y + size.height * anchor.y);
        concat_3d(concat_3d(affine_to_3d(moved(-at.0, -at.1)), s), affine_to_3d(moved(at.0, at.1)))
    });
    // Which way its plane faces: its normal's z after its own and its
    // parents' 3D turns (a mirror in its plane still faces the viewer).
    let m = turn.0;
    let facing: [f64; 9] = std::array::from_fn(|i| {
        let (row, column) = (i / 3, i % 3);
        (0..3).map(|k| m[row * 4 + k] * facing[k * 3 + column]).sum()
    });
    // Turned away, a one-sided layer (doubleSided NO) doesn't draw its own
    // background, contents and border; its sublayers decide for themselves.
    let turned_away = node.one_sided && facing[8] < 0.0;
    let flat = own.flat();
    let visible = Rect { origin: bounds_origin, size };
    if node.video || node.gl {
        // What is under its box is covered (the video or GL picture is drawn
        // under every UIKit picture); what is beside it stays, a picture
        // partly under it cut to the parts outside.
        let covered = meet(screen_box(flat, visible), clip);
        let pictures = std::mem::take(out);
        for p in pictures {
            let shown = meet(screen_box(p.place, Rect::new(0.0, 0.0, p.area.0, p.area.1)), p.clip);
            let met = meet(shown, covered);
            if met.size.width <= 0.0 || met.size.height <= 0.0 || p.path.starts_with(TRANSITION_MARK) {
                out.push(p);
            } else if ![GROUP_MARK, MASK_MARK, END_MARK].contains(&p.path.as_str()) {
                out.extend(outside(shown, met).into_iter().map(|piece| Picture { clip: piece, ..p.clone() }));
            }
        }
    }
    // A GL view's last frame, when one is kept for its transition's old side:
    // drawn where the view is, under its UIKit pictures, as Core Animation
    // draws the CAEAGLLayer's contents.
    if node.gl {
        if let Some(path) = GL_CONTENTS.with(|c| c.borrow().as_ref().filter(|(layer, _)| *layer == node.gl_layer).map(|(_, path)| path.clone())) {
            let (place, perspective) = Proj::affine(moved(bounds_origin.x, bounds_origin.y)).then(own).for_picture();
            out.push(Picture { path, place, area: (size.width, size.height), alpha, mode: 0, size: (size.width, size.height), text: None, clip, group: None, corner, perspective });
        }
    }
    // A rasterized layer is drawn as one picture and faded as a whole: what
    // is inside is drawn at full strength into it.
    let faded = node.rasterized && pose.alpha < 1.0;
    let inner_alpha = if faded { 1.0 } else { alpha };
    // Round corners alone are cut where the pictures are shown (`Corner`);
    // drawn together (after resolve, by fw/graphics/compose.rs) when cut to
    // turned bounds or round ones inside round ones, masked, shadowed or rasterized and faded.
    let turned = flat.b.abs() > 1e-9 || flat.c.abs() > 1e-9 || !own.is_affine();
    let rounded = node.clips && node.radius > 0.0 && !turned && node.mask.is_none() && node.shadow.is_none() && corner.is_none() && !faded;
    let group = (node.clips && (node.radius > 0.0 && !rounded || turned) || node.mask.is_some() || node.shadow.is_some() || faded).then(|| Group {
        place: own.for_picture().0,
        perspective: own.for_picture().1,
        bounds: visible,
        clips: node.clips,
        radius: node.radius,
        masked: node.mask.is_some(),
        shadow: node.shadow.clone().map(|shadow| Shadow { color: [shadow.color[0], shadow.color[1], shadow.color[2], shadow.color[3] * inner_alpha], ..shadow }),
        opacity: if faded { alpha } else { 1.0 },
    });
    let mark = |path: &str, group: Option<Group>| Picture {
        path: path.into(),
        place: flat,
        area: (size.width, size.height),
        alpha,
        mode: 0,
        size: (size.width, size.height),
        text: None,
        clip,
        group: group.map(Box::new),
        corner,
        perspective: None,
    };
    let grouped = group.is_some();
    if let Some(group) = group {
        out.push(mark(GROUP_MARK, Some(group)));
    }
    let clip = if node.clips { meet(clip, own.screen_box(visible)) } else { clip };
    let corner = if rounded { Some(Corner { rect: screen_box(flat, visible), radius: (node.radius * flat.a.abs(), node.radius * flat.d.abs()) }) } else { corner };
    let over_bounds = |path: String| Shown { path, mode: 0, size: (1.0, 1.0), rect: Rect { origin: Point::default(), size }, caps: None, tile: false, text: None, alpha: 1.0 };
    // The background colour first, under what the view shows (the GL picture covers a GL view's).
    let background = pose.background.filter(|color| color[3] > 0.0 && !node.gl).map(|color| {
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
                let place = Proj::affine(moved(bounds_origin.x + rect.origin.x, bounds_origin.y + rect.origin.y)).then(own);
                // A tile reaching past its stretch is cut off at its end.
                let clip = match cut {
                    Some(area) => meet(clip, own.screen_box(Rect { origin: Point { x: bounds_origin.x + area.origin.x, y: bounds_origin.y + area.origin.y }, size: area.size })),
                    None => clip,
                };
                let seen = meet(place.screen_box(Rect { origin: Point::default(), size: rect.size }), clip);
                if seen.size.width <= 0.0 || seen.size.height <= 0.0 {
                    continue; // wholly clipped away
                }
                let text = shown.text.clone();
                let alpha = inner_alpha * shown.alpha;
                let (place, perspective) = place.for_picture();
                out.push(Picture { path, place, area: (rect.size.width, rect.size.height), alpha, mode: shown.mode, size: picture_size, text, clip, group: None, corner, perspective });
            }
        }
    };
    // Core Animation's order: background, contents, sublayers, the border on top.
    if !turned_away {
        for shown in background.iter().chain(node.pictures_at(k)) {
            draw(shown, out);
        }
    }
    for child in &node.children {
        walk(child, k, Some((own, inner)), facing, inner_alpha, clip, corner, out);
    }
    if let Some(border) = border.as_ref().filter(|_| !turned_away) {
        draw(border, out);
    }
    if grouped {
        if let Some(mask) = &node.mask {
            out.push(mark(MASK_MARK, None));
            // The mask counts by its own opacity alone, wherever the layer's clip is.
            walk(mask, k, Some((own, None)), facing, 1.0, Rect::new(-1e9, -1e9, 2e9, 2e9), None, out);
        }
        out.push(mark(END_MARK, None));
    }
    if marked {
        out.push(Picture { path: TRANSITION_END_MARK.into(), ..mark_picture(clip) });
    }
}

thread_local! {
    /// The layers `walk` marks the pictures of (see TRANSITION_MARK).
    static MARKED: std::cell::RefCell<Vec<Id>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// `make` with `layers`' pictures marked.
fn with_marked<T>(layers: Vec<Id>, make: impl FnOnce() -> T) -> T {
    let before = MARKED.with(|m| m.replace(layers));
    let made = make();
    MARKED.with(|m| *m.borrow_mut() = before);
    made
}

/// A mark that draws nothing.
fn mark_picture(clip: Rect) -> Picture {
    Picture { path: String::new(), place: IDENTITY, area: (0.0, 0.0), alpha: 1.0, mode: 0, size: (0.0, 0.0), text: None, clip, group: None, corner: None, perspective: None }
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


/// Starts making the files a picture will be drawn from (its parts when it
/// is stretchable), so it can show the moment it is needed.
fn start_files(image: Id) {
    match crate::fw::uikit::image_info(image).map(|info| info.caps) {
        Some(Some(_)) => drop(crate::fw::graphics::picture_part_file(image, 0)),
        Some(None) => drop(crate::fw::graphics::picture_file(image)),
        None => {}
    }
}

thread_local! {
    /// B1+B2: labels' looks, with the content stamp and size each was made
    /// for (Apple keeps a layer's backing store until its content changes):
    /// this commit's, and the last commit's (a label off the screen for a
    /// whole commit is let go of).
    static LOOKS: RefCell<[HashMap<Id, (u64, (f64, f64), Option<TextLook>)>; 2]> = RefCell::new([HashMap::new(), HashMap::new()]);
    /// Looks worked out (not kept), for the test.
    static LOOKS_MADE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// `label_look` of a label `size` big, kept from an earlier build while
/// nothing it draws changed (a fade, a move or hiding keep it); worked out
/// afresh when not caching (a full build for the verify check).
fn label_look_kept(label: Id, size: (f64, f64)) -> Option<TextLook> {
    let caching = CACHING.with(|c| c.get());
    let stamp = crate::fw::uikit::content_stamp(label);
    if caching {
        let kept = LOOKS.with(|looks| {
            let mut looks = looks.borrow_mut();
            let kept = looks[0].get(&label).or_else(|| looks[1].get(&label)).filter(|(s, z, _)| *s == stamp && *z == size).cloned();
            kept.inspect(|entry| drop(looks[0].insert(label, entry.clone())))
        });
        if let Some((_, _, look)) = kept {
            return look;
        }
    }
    LOOKS_MADE.with(|m| m.set(m.get() + 1));
    let look = crate::fw::controls::label_look(label);
    if caching {
        LOOKS.with(|looks| looks.borrow_mut()[0].insert(label, (stamp, size, look.clone())));
    }
    look
}

/// A commit's looks become the last commit's.
fn turn_looks() {
    LOOKS.with(|looks| {
        let mut looks = looks.borrow_mut();
        looks[1] = std::mem::take(&mut looks[0]);
    });
}

/// Text drawn over `rect` (None for no text).
fn text_shown(look: Option<TextLook>, rect: Rect) -> Option<Shown> {
    let look = look?;
    Some(Shown { path: String::new(), mode: 0, size: look.size, rect, caps: None, tile: false, text: Some(look), alpha: 1.0 })
}

thread_local! {
    /// The reusable nodes of the last committed screen, keyed by view id (a
    /// `snapshot`) or layer id (a `layer_node`): on the next commit an
    /// untouched subtree is taken from here instead of built again (A2,
    /// Apple's CA::Layer::commit_if_needed visiting only marked parts).
    static NODE_CACHE: RefCell<HashMap<Id, Rc<Node>>> = RefCell::new(HashMap::new());
    /// Whether the nodes being built may come from NODE_CACHE (only a tick's
    /// own builds, not a replicator's delayed copies).
    static CACHING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Nodes built (not taken from the cache) since the count was reset.
    static BUILT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// The screen size, window turn and pixels per point the cache was built for.
    static CACHE_SCREEN: std::cell::Cell<Option<((f64, f64, f64), Option<Transform>, f64)>> = const { std::cell::Cell::new(None) };
}

/// `id`'s node from the last commit, while caching, nothing marked it and
/// it is wanted at the depth it was built at (moved deeper, less of it shows).
fn cached(id: Id, depth: usize) -> Option<Rc<Node>> {
    if !CACHING.with(|c| c.get()) {
        return None;
    }
    NODE_CACHE.with(|c| c.borrow().get(&id).filter(|node| node.depth == depth).cloned())
}

/// `make` with caching on or off.
fn with_caching<T>(on: bool, make: impl FnOnce() -> T) -> T {
    let before = CACHING.with(|c| c.replace(on));
    let made = make();
    CACHING.with(|c| c.set(before));
    made
}

/// The windows' node trees, untouched parts taken from the last commit
/// (all built afresh with IOS_PICTURES_FULL=1).
fn build_windows(windows: &[Id], classes: &Classes) -> Vec<Rc<Node>> {
    with_caching(!full_pictures(), || windows.iter().map(|&w| snapshot(w, classes, 0)).collect())
}

/// The committed screen's reusable nodes become the cache (what is no
/// longer on the screen leaves it).
fn keep_reusable(nodes: &[Rc<Node>]) {
    fn keep(node: &Rc<Node>, into: &mut HashMap<Id, Rc<Node>>) {
        // A replicator's copy (no id) is built its delay late: none of it is kept.
        if node.id == 0 {
            return;
        }
        if node.reusable {
            into.insert(node.id, node.clone());
        }
        node.children.iter().chain(node.mask.iter()).for_each(|child| keep(child, into));
    }
    let mut kept = HashMap::new();
    nodes.iter().for_each(|node| keep(node, &mut kept));
    NODE_CACHE.with(|c| *c.borrow_mut() = kept);
    turn_looks();
}

/// Whether a built node can be kept and reused on the next commit: nothing
/// about it changes without a mark (`still`: no running animation, no
/// table cell or such, checked where it is built), and the same holds for
/// its mask and every child.
fn reusable(node: &Node, still: bool) -> bool {
    still
        && !node.changing
        && !node.video
        && node.motion.is_empty()
        && node.picture_motion.is_empty()
        && node.mask.as_ref().is_none_or(|m| m.reusable)
        && node.children.iter().all(|c| c.reusable)
}

/// At most this many marked objects are followed one by one; more means
/// the whole tree is built again (a burst of changes).
const DIRTY_LIMIT: usize = 4096;

/// Before a commit's build: drops the cached nodes the marks since the last
/// commit touched (see `invalidate`); everything when the screen size, turn
/// or scale changed, or after too many marks.
fn commit_dirty() {
    let screen = (crate::fw::uikit::screen_points(), crate::fw::uikit::window_turn(), pixels_per_point());
    let moved = CACHE_SCREEN.with(|s| s.replace(Some(screen))) != Some(screen);
    let dirty = crate::fw::changes::take_dirty();
    let cleared = NODE_CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        let reason = match dirty {
            _ if moved => Some("the screen size, turn or scale changed".to_string()),
            None => Some("too many marks to list".to_string()),
            Some(mut dirty) => {
                dirty.sort_unstable();
                dirty.dedup();
                if dirty.len() > DIRTY_LIMIT {
                    Some(format!("{} objects marked", dirty.len()))
                } else {
                    if !cache.is_empty() {
                        let kinds = Kinds::find();
                        for object in dirty {
                            invalidate(object, &kinds, &mut cache);
                        }
                    }
                    None
                }
            }
        };
        if reason.is_some() {
            cache.clear();
        }
        reason
    });
    if let Some(reason) = cleared.filter(|_| verify_pictures()) {
        thread_local! {
            static SAID: RefCell<std::collections::HashSet<String>> = RefCell::new(std::collections::HashSet::new());
        }
        if SAID.with(|said| { let mut said = said.borrow_mut(); said.len() < 50 && said.insert(reason.clone()) }) {
            crate::log!("pictures: cache dropped: {reason}");
        }
    }
}

/// The classes a marked object is told apart by.
struct Kinds {
    view: Class,
    layer: Class,
    control: Class,
}

impl Kinds {
    fn find() -> Kinds {
        Kinds { view: runtime::class_named("UIView"), layer: runtime::class_named("CALayer"), control: runtime::class_named("UIControl") }
    }
}

/// Drops the cached nodes a change to `object` makes stale: its own and
/// each one above it up to its window (Apple's mark flags the parents:
/// superviews, superlayers, a mask's layer), and for a control everything
/// inside it too (its parts are drawn by its state). Off the screen the
/// climb ends where the view hangs loose: what is there is out of the cache
/// after the next commit anyway, and comes back built afresh.
fn invalidate(object: Id, kinds: &Kinds, cache: &mut HashMap<Id, Rc<Node>>) {
    use crate::fw::layers;
    let is_view = |o: Id| runtime::is_kind_of(o, kinds.view);
    if object == 0 || !is_view(object) && !runtime::is_kind_of(object, kinds.layer) {
        // Anything else drawn is read through a view or layer that is marked
        // when it changes (pictures and colours don't change; an edited
        // CGPath marks the layers drawing it).
        return;
    }
    if runtime::is_kind_of(object, kinds.control) {
        fn drop_inside(view: Id, cache: &mut HashMap<Id, Rc<Node>>, depth: usize) {
            for &sub in view_state(view).subviews.iter().filter(|_| depth < 64) {
                cache.remove(&sub);
                drop_inside(sub, cache, depth + 1);
            }
        }
        drop_inside(object, cache, 0);
    }
    // A layer's next one up: its superlayer, or the layer it masks.
    let above_layer = |layer: Id| match layers::parent_of(layer) {
        0 => layers::mask_owner(layer),
        parent => parent,
    };
    let mut current = object;
    for _ in 0..256 {
        cache.remove(&current);
        current = if is_view(current) {
            let state = view_state(current);
            match (state.superview, state.layer) {
                (0, 0) => 0,
                // (A view's layer may hang in a layer of its own.)
                (0, layer) => above_layer(layer),
                (superview, _) => superview,
            }
        } else {
            match layers::view_of(current) {
                0 => above_layer(current),
                view => view,
            }
        };
        if current == 0 {
            break;
        }
    }
}

/// The view tree under `view` as `flatten` reads it (main thread), taken
/// from the last commit when nothing in it changed.
fn snapshot(view: Id, classes: &Classes, depth: usize) -> Rc<Node> {
    cached(view, depth).unwrap_or_else(|| {
        BUILT.with(|b| b.set(b.get() + 1));
        Rc::new(snapshot_built(view, classes, depth))
    })
}

/// The view tree under `view`, built from scratch (no cache).
fn snapshot_built(view: Id, classes: &Classes, depth: usize) -> Node {
    // Rows a table makes here missed this turn's layout pass: laid out now,
    // so buttons in them show their parts in this picture.
    if Classes::is(view, classes.table) && crate::fw::tables::ensure_rows(view) {
        runtime::send(view, "layoutIfNeeded", &[]);
    }
    let state = view_state(view);
    let bounds = Rect { origin: Point::default(), size: state.frame.size };
    let mut pictures = Vec::new();
    let now = shown_now();
    // An animating image view's picture: where it is in `pictures`, and how it is placed.
    let mut animated_image = None;
    if Classes::is(view, classes.image_view) {
        let mode = property_get(view, "contentMode").unwrap_or(0) as i64;
        // While it animates (startAnimating), the animation picture of the moment.
        let image = match crate::fw::uikit::image_view_animating_at(view, now) {
            Some(image) => {
                animated_image = Some((pictures.len(), mode));
                image
            }
            None => crate::fw::uikit::image_view_shown(view),
        };
        // A button's part pressed or disabled is drawn darker (its draw mode).
        pictures.extend(image_shown(crate::fw::uikit::image_view_drawn(view, image), mode, bounds));
    }
    // A label that draws itself has its text in its layer's contents.
    if Classes::is(view, classes.label) && !crate::fw::uikit::draws_itself(view) {
        pictures.extend(text_shown(label_look_kept(view, (bounds.size.width, bounds.size.height)), bounds));
    }
    // A button's parts are its subviews; a press may last one frame, so the
    // pictures it would show have their files started now.
    if Classes::is(view, classes.button) {
        crate::fw::controls::button_press_pictures(view).into_iter().for_each(start_files);
    }
    if Classes::is(view, classes.progress) {
        // The track over the bounds, the part done from the left: each its picture or colour.
        let ((track_image, track_color), (done_image, done_color)) = crate::fw::progress::parts(view);
        let done = Rect { size: crate::fw::geometry::Size { width: bounds.size.width * crate::fw::progress::shown_progress(view), ..bounds.size }, ..bounds };
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
    let picker_pieces = crate::fw::pickers::pieces(view);
    let changing = picker_pieces.is_some()
        || Classes::is(view, classes.table)
        || Classes::is(view, classes.text_field)
        || Classes::is(view, classes.progress);
    if let Some(pieces) = picker_pieces {
        pictures.extend(pieces.into_iter().map(piece_shown));
    }
    let alpha = property_get(view, "alpha").map(f64::from_bits).unwrap_or(1.0);
    let anchor = if state.layer == 0 { Point { x: 0.5, y: 0.5 } } else { crate::fw::layers::anchor(state.layer) };
    // The model position (from the model size): an animated size grows
    // about it, as Core Animation draws bounds around the position.
    let model_size = state.frame.size;
    let position = Point { x: state.frame.origin.x + model_size.width * anchor.x, y: state.frame.origin.y + model_size.height * anchor.y };
    let transform = if state.layer == 0 { crate::fw::layers::affine_to_3d(state.transform) } else { crate::fw::layers::transform(state.layer) };
    let mut model = crate::fw::animation::Shown::new(alpha, position, transform, background);
    // A UIView block's size change animates the view's bounds (fw/uikit.rs);
    // its colour change goes through `background`.
    if state.layer != 0 {
        model.bounds = Some(Rect { origin: state.bounds_origin, size: model_size });
    }
    let (shown, motion) = presented(state.layer, model);
    let size = shown.bounds.map_or(model_size, |b| b.size);
    // A contents animation's pictures in the coming frames (worked out ahead
    // while the game thread may be busy), its own and an image view's.
    let mut picture_motion = Vec::new();
    if state.layer != 0 {
        let layer_bounds = Rect { origin: state.bounds_origin, size };
        let animates = crate::fw::animation::animates_contents(state.layer);
        if animates {
            for t in coming_times() {
                let mut own = layer_content(state.layer, layer_bounds, classes, t);
                let mut mine = pictures.clone();
                if let Some((at, mode)) = animated_image {
                    let image = crate::fw::uikit::image_view_animating_at(view, t).unwrap_or_else(|| crate::fw::uikit::image_view_shown(view));
                    if let (Some(shown), Some(slot)) = (image_shown(image, mode, bounds), mine.get_mut(at)) {
                        *slot = shown;
                    }
                }
                own.append(&mut mine);
                picture_motion.push(own);
            }
        }
        // What its layer shows (contents, a gradient or shape layer's) comes first.
        let mut own = layer_content(state.layer, layer_bounds, classes, now);
        own.append(&mut pictures);
        pictures = own;
    }
    let children = if depth >= 64 {
        Vec::new()
    } else if state.layer != 0 {
        layer_children(state.layer, view, classes, depth)
    } else {
        in_z_order(state.subviews.iter().map(|&child| (z_of(child), snapshot(child, classes, depth + 1))).collect())
    };
    let mut node = Node {
        id: view,
        layer: state.layer,
        frame: Rect { origin: Point { x: shown.position.x - size.width * anchor.x, y: shown.position.y - size.height * anchor.y }, size },
        bounds_origin: state.bounds_origin,
        transform: Some(shown.transform),
        one_sided: state.layer != 0 && !crate::fw::layers::double_sided(state.layer),
        rasterized: state.layer != 0 && crate::fw::layers::rasterizes(state.layer),
        anchor: Some(anchor),
        hidden: property_get(view, "hidden").unwrap_or(0) != 0,
        alpha: shown.opacity,
        pictures,
        video: crate::fw::media::shows_video(view),
        changing,
        gl: state.layer != 0 && Classes::is(state.layer, classes.gl_layer),
        gl_layer: if state.layer != 0 && Classes::is(state.layer, classes.gl_layer) { state.layer } else { 0 },
        background: shown.background,
        clips: property_get(view, "clips").unwrap_or(0) != 0,
        children,
        depth,
        motion: poses(motion, Rect { origin: state.bounds_origin, size }, anchor),
        picture_motion,
        ..Default::default()
    };
    let mask_owned = state.layer == 0 || dress(&mut node, state.layer, classes, depth);
    // Animations, an image view's animation pictures and a table cell's
    // selection change it without a mark.
    let still = mask_owned
        && animated_image.is_none()
        && !Classes::is(view, classes.cell)
        && (state.layer == 0 || !(crate::fw::animation::animating(state.layer) || crate::fw::animation::animates_contents(state.layer)));
    node.reusable = reusable(&node, still);
    node
}

/// The media times of the coming frames after the first (see `Sampling`),
/// less a replicator copy's delay; none when they aren't worked out.
fn coming_times() -> Vec<f64> {
    let late = DELAY.with(|d| d.get());
    SAMPLING.with(|s| s.get()).map_or_else(Vec::new, |s| (1..s.count).map(|k| s.start + k as f64 * s.step - late).collect())
}

thread_local! {
    /// How far behind the clock the layers being listed run: a replicator's
    /// copy i runs its sublayers' animations i * instanceDelay late.
    static DELAY: std::cell::Cell<f64> = const { std::cell::Cell::new(0.0) };
}

/// The media time animations are shown at: the first sampled frame's (or
/// now), less a replicator copy's delay.
fn shown_now() -> f64 {
    let late = DELAY.with(|d| d.get());
    SAMPLING.with(|s| s.get()).map_or_else(crate::fw::runloop::media_time, |s| s.start) - late
}

/// Where a layer is, how it is turned, how see-through and its background
/// colour, with its running Core Animation animations shown over its own
/// values (`model`): now, and (while sampling, see `Sampling`) in each
/// coming frame after the first.
fn presented(layer: Id, model: crate::fw::animation::Shown) -> (crate::fw::animation::Shown, Vec<crate::fw::animation::Shown>) {
    if layer == 0 || !crate::fw::animation::animating(layer) {
        return (model, Vec::new());
    }
    let late = DELAY.with(|d| d.get());
    let at = |t: f64| crate::fw::animation::presented_at(layer, model, t - late);
    match SAMPLING.with(|s| s.get()) {
        Some(s) => (at(s.start), (1..s.count).map(|k| at(s.start + k as f64 * s.step)).collect()),
        None => (at(crate::fw::runloop::media_time()), Vec::new()),
    }
}

/// Layer positions (at the anchor point) as frame origins, with the
/// bounds shown then (`bounds` while they don't animate).
fn poses(motion: Vec<crate::fw::animation::Shown>, bounds: Rect, anchor: Point) -> Vec<Pose> {
    motion
        .into_iter()
        .map(|shown| {
            let bounds = shown.bounds.unwrap_or(bounds);
            let size = bounds.size;
            Pose {
                origin: Point { x: shown.position.x - size.width * anchor.x, y: shown.position.y - size.height * anchor.y },
                transform: shown.transform,
                alpha: shown.opacity,
                background: shown.background,
                bounds,
            }
        })
        .collect()
}

/// Layers with a higher zPosition show in front of their siblings (a stable sort keeps the rest in order).
fn in_z_order(mut children: Vec<(f64, Rc<Node>)>) -> Vec<Rc<Node>> {
    children.sort_by(|a, b| a.0.total_cmp(&b.0));
    children.into_iter().map(|(_, node)| node).collect()
}

/// What is drawn inside a layer: its view's subviews (`view`: 0 for a layer
/// of its own) and the layers the game added, in order; a replicator
/// layer's, once per copy.
fn layer_children(layer: Id, view: Id, classes: &Classes, depth: usize) -> Vec<Rc<Node>> {
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
    let (copies, step, delay) = layers::replication(layer);
    // Each copy one instanceTransform further than the last, around the
    // replicator's middle, and instanceDelay later in time. Copies keep moving
    // after the base animation stops (removedOnCompletion = NO), so a
    // replicator counts as always changing: the tick must not skip it.
    let bounds = layers::bounds(layer);
    let mut turn = IDENTITY_3D;
    (0..copies)
        .map(|i| {
            let before = DELAY.with(|d| d.replace(d.get() + i as f64 * delay));
            // (Built afresh: a copy's animations run its delay late.)
            let copy = Rc::new(Node { frame: bounds, bounds_origin: bounds.origin, transform: Some(turn), alpha: 1.0, changing: true, children: with_caching(false, nodes), ..Default::default() });
            DELAY.with(|d| d.set(before));
            turn = crate::fw::geometry::concat_3d(turn, step);
            copy
        })
        .collect()
}

/// A layer of its own (made by the game) as `flatten` reads it, taken from
/// the last commit when nothing in it changed.
fn layer_node(layer: Id, classes: &Classes, depth: usize) -> Rc<Node> {
    cached(layer, depth).unwrap_or_else(|| {
        BUILT.with(|b| b.set(b.get() + 1));
        Rc::new(layer_node_built(layer, classes, depth))
    })
}

/// A layer of its own, built from scratch (no cache).
fn layer_node_built(layer: Id, classes: &Classes, depth: usize) -> Node {
    use crate::fw::layers;
    let anchor = layers::anchor(layer);
    let (shown, motion) = presented(layer, crate::fw::animation::model_of(layer));
    // Its bounds as shown now (they ease after an implicit size change).
    let bounds = shown.bounds.unwrap_or_else(|| layers::bounds(layer));
    let size = bounds.size;
    let mut node = Node {
        id: layer,
        layer,
        frame: Rect { origin: Point { x: shown.position.x - size.width * anchor.x, y: shown.position.y - size.height * anchor.y }, size },
        bounds_origin: bounds.origin,
        transform: Some(shown.transform),
        one_sided: !layers::double_sided(layer),
        rasterized: layers::rasterizes(layer),
        anchor: Some(anchor),
        hidden: layers::hidden(layer),
        alpha: shown.opacity,
        pictures: layer_content(layer, bounds, classes, shown_now()),
        background: shown.background,
        video: crate::fw::media::layer_shows_video(layer),
        clips: layers::masks(layer),
        gl: Classes::is(layer, classes.gl_layer),
        gl_layer: if Classes::is(layer, classes.gl_layer) { layer } else { 0 },
        children: layer_children(layer, 0, classes, depth),
        depth,
        motion: poses(motion, bounds, anchor),
        // A contents animation's pictures in the coming frames.
        picture_motion: if crate::fw::animation::animates_contents(layer) { coming_times().into_iter().map(|t| layer_content(layer, bounds, classes, t)).collect() } else { Vec::new() },
        ..Default::default()
    };
    let mask_owned = dress(&mut node, layer, classes, depth);
    let still = mask_owned && !crate::fw::animation::animating(layer) && !crate::fw::animation::animates_contents(layer);
    node.reusable = reusable(&node, still);
    node
}

/// A layer's corners, border, shadow and mask. False when its mask isn't
/// known to belong to it (a change to the mask couldn't be followed up to
/// this layer, so its node is built again every commit).
fn dress(node: &mut Node, layer: Id, classes: &Classes, depth: usize) -> bool {
    let looks = crate::fw::layers::looks(layer);
    node.radius = looks.corner_radius;
    node.border = looks.border;
    node.shadow = looks.shadow.map(|(color, radius, offset, path)| Shadow { color, radius, offset, path: (path != 0).then(|| crate::fw::graphics::path_points(path)) });
    node.sublayer_transform = Some(crate::fw::layers::sublayer_transform(layer)).filter(|&s| s != IDENTITY_3D);
    if looks.mask != 0 && depth < 64 {
        node.mask = Some(layer_node(looks.mask, classes, depth + 1));
    }
    looks.mask == 0 || crate::fw::layers::mask_owner(looks.mask) == layer
}

/// What a layer itself shows over its background: its contents (placed by
/// its contentsGravity), a gradient layer's colours, a shape layer's path.
/// `bounds`: the layer's bounds; `now`: the media time a running contents
/// animation is shown at.
fn layer_content(layer: Id, bounds: Rect, classes: &Classes, now: f64) -> Vec<Shown> {
    use crate::fw::layers;
    let size = bounds.size;
    let plain = |path: String, rect: Rect| Shown { path, mode: 0, size: (rect.size.width, rect.size.height), rect, caps: None, tile: false, text: None, alpha: 1.0 };
    let mut out = Vec::new();
    // A running contents animation's picture of the moment (an image view
    // shows its own animation pictures, `snapshot`).
    let animated = crate::fw::animation::presented_contents_at(layer, now).filter(|_| {
        let view = layers::view_of(layer);
        view == 0 || !Classes::is(view, classes.image_view)
    });
    let contents = animated.unwrap_or_else(|| layers::contents(layer));
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
        let (path, fill, stroke, even_odd) = layers::shape_look(layer);
        let line = layers::shape_line(layer);
        let width = line.width;
        let figures = if path == 0 { Vec::new() } else { crate::fw::graphics::path_points(path) };
        let points: Vec<&Point> = figures.iter().flatten().collect();
        let fill = crate::fw::graphics::color_rgba(fill).unwrap_or([0.0; 4]);
        let stroke = crate::fw::graphics::color_rgba(stroke).filter(|_| width > 0.0).unwrap_or([0.0; 4]);
        if !points.is_empty() {
            // The path may reach past the bounds (often a layer with no size
            // draws it all); a stroke reaches half its width past it, a miter
            // corner up to the miter limit times that, a square end √2 times.
            let corner = if line.join == 0 { line.miter_limit.max(std::f64::consts::SQRT_2) } else { std::f64::consts::SQRT_2 };
            let reach = if stroke[3] > 0.0 { width / 2.0 * corner } else { 0.0 } + 1.0;
            let (x0, x1) = points.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| (lo.min(p.x), hi.max(p.x)));
            let (y0, y1) = points.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| (lo.min(p.y), hi.max(p.y)));
            let area = Rect::new(x0 - reach, y0 - reach, x1 - x0 + 2.0 * reach, y1 - y0 + 2.0 * reach);
            let (phase, dashes) = line.dash.unwrap_or_default();
            let mut key = format!(
                "{SHAPE_PREFIX}{},{},{},{},{},{},{},{},{},{},{},{},{},{width},{},{},{},{phase}",
                area.origin.x, area.origin.y, area.size.width, area.size.height, even_odd as u8, fill[0], fill[1], fill[2], fill[3], stroke[0], stroke[1], stroke[2], stroke[3], line.cap, line.join, line.miter_limit
            );
            for length in dashes {
                key += &format!(",{length}");
            }
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
/// points), even-odd, fill and stroke colours, line width, cap, join, miter
/// limit, dash phase and dash lengths, then `;` and each figure's points.
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
            (n.len() >= 18).then_some(())?;
            let figures: Option<Vec<Vec<Point>>> = lists.map(|figure| figure.map(|f| f.chunks_exact(2).map(|p| Point { x: p[0], y: p[1] }).collect())).collect();
            let (fill, stroke) = (rgba(&n[5..9]), rgba(&n[9..13]));
            let style = graphics::StrokeStyle { width: n[13], cap: n[14] as i32, join: n[15] as i32, miter_limit: n[16], dash: (n.len() > 18).then(|| (n[17], n[18..].to_vec())) };
            let stroke = (stroke[3] > 0.0 && n[13] > 0.0).then_some((stroke, style));
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
    /// Draws a list, given with its number.
    pub show: fn(&str, u64),
    /// Whether the screen says when it put a list up (`phone_put_up`).
    pub says_when_up: bool,
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
        FILE_SCREEN.get_or_init(|| std::env::var("IOS_FRAMES_DIR").ok().map(|_| PictureScreen { show: save_list, says_when_up: false })).as_ref()
    })
}

fn save_list(text: &str, _number: u64) {
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
/// the rounded rectangle it is cut to when it is (`Corner`: x, y, width,
/// height, radii), its place's third column when it has perspective
/// (`Picture::perspective`), file. A transition's pictures come between a
/// line "!", "transition", how it runs (type, subtype, milliseconds, when it
/// began in nanoseconds, its timing function's x1, y1, x2, y2, start and end
/// progress), its layer's place and size, the screen area that clips it and
/// stays put while it moves (x, y, width, height); its old pictures; "!", "to"; its
/// new pictures; and "!", "end".
pub fn encode(screen: (f64, f64), pictures: &[Picture]) -> String {
    let mut text = format!("{}\t{}\n", screen.0, screen.1);
    encode_lines(&mut text, pictures);
    text
}

fn encode_lines(text: &mut String, pictures: &[Picture]) {
    for p in pictures {
        let (t, c) = (p.place, p.clip);
        if p.path.starts_with(TRANSITION_MARK) {
            if p.path == TRANSITION_TO_MARK {
                text.push_str("!\tto\n");
            } else if p.path == TRANSITION_END_MARK {
                text.push_str("!\tend\n");
            } else if p.path.starts_with(&format!("{TRANSITION_MARK}\t")) {
                text.push_str("!\t");
                text.push_str(&p.path[1..]);
                for v in [t.a, t.b, t.c, t.d, t.tx, t.ty, p.area.0, p.area.1, c.origin.x, c.origin.y, c.size.width, c.size.height] {
                    text.push('\t');
                    push_number(text, v);
                }
                text.push('\n');
            }
            continue;
        }
        for v in [t.a, t.b, t.c, t.d, t.tx, t.ty, p.area.0, p.area.1, p.alpha] {
            push_number(text, v);
            text.push('\t');
        }
        push_whole(text, p.mode);
        text.push('\t');
        for v in [p.size.0, p.size.1, c.origin.x, c.origin.y, c.size.width, c.size.height] {
            push_number(text, v);
            text.push('\t');
        }
        if let Some(Corner { rect: r, radius }) = p.corner {
            for v in [r.origin.x, r.origin.y, r.size.width, r.size.height, radius.0, radius.1] {
                push_number(text, v);
                text.push('\t');
            }
        }
        if let Some(perspective) = p.perspective {
            for v in perspective {
                push_number(text, v);
                text.push('\t');
            }
        }
        text.push_str(&p.path);
        text.push('\n');
    }
}

/// A list number: whole numbers plainly, others to six decimals (a
/// millionth of a point) without trailing zeros. Rust's own float writing
/// (shortest round trip) took most of a list's time.
fn push_number(text: &mut String, v: f64) {
    let millionths = (v * 1e6).round();
    if !(millionths.abs() < 9e15) {
        use std::fmt::Write;
        let _ = write!(text, "{v}");
        return;
    }
    let n = millionths as i64;
    if n < 0 {
        text.push('-');
    }
    let n = n.unsigned_abs();
    push_digits(text, n / 1_000_000, 1);
    let mut fraction = n % 1_000_000;
    if fraction != 0 {
        let mut digits = 6;
        while fraction % 10 == 0 {
            fraction /= 10;
            digits -= 1;
        }
        text.push('.');
        push_digits(text, fraction, digits);
    }
}

fn push_whole(text: &mut String, v: i64) {
    if v < 0 {
        text.push('-');
    }
    push_digits(text, v.unsigned_abs(), 1);
}

/// `n` in decimal, padded with leading zeros to at least `width` digits.
fn push_digits(text: &mut String, mut n: u64, width: usize) {
    let mut buf = [b'0'; 20];
    let mut at = buf.len();
    while n > 0 || buf.len() - at < width {
        at -= 1;
        buf[at] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    text.push_str(std::str::from_utf8(&buf[at..]).unwrap_or("0"));
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
                Some(i) => transition_side(content.split_off(i).split_off(1), false),
                None => Vec::new(),
            };
            let opening = format!("{TRANSITION_MARK}\t");
            match content.iter().find(|q| q.path.starts_with(&opening)).map(|q| q.path.clone()) {
                None => out.extend(crate::fw::graphics::group_pictures(&start, &content, &mask, scale)),
                Some(line) => {
                    // A transition inside: the group drawn as it was and as it is, the
                    // two dissolving in the transition's time (one layer can't slide or
                    // turn inside the group's one picture).
                    let (old, new) = (transition_side(content.clone(), true), transition_side(content, false));
                    out.push(Picture { path: dissolving(&line), place: start.place, area: start.area, ..mark_picture(start.clip) });
                    out.extend(crate::fw::graphics::group_pictures(&start, &old, &mask, scale));
                    out.push(Picture { path: TRANSITION_TO_MARK.into(), ..mark_picture(start.clip) });
                    out.extend(crate::fw::graphics::group_pictures(&start, &new, &mask, scale));
                    out.push(Picture { path: TRANSITION_END_MARK.into(), ..mark_picture(start.clip) });
                }
            }
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

/// The most coming frames one list carries. A long sleep (the game asleep on
/// the main thread) is covered at a coarser step rather than with thousands
/// of full screen lists.
const MAX_AHEAD_FRAMES: usize = 240;

/// How far a list that was already sent must reach to be reused as it is:
/// a little past the next tick, or the whole sleep the main thread is about
/// to take (a reused list that stops before the sleep would leave the screen
/// still while the thread sleeps).
fn list_until(sampling: Sampling, sleeping: Option<f64>) -> f64 {
    let end = sampling.start + (sampling.count - 1) as f64 * sampling.step;
    if sleeping.is_some() {
        end
    } else {
        end.min(sampling.start + 0.15)
    }
}

/// The coming frames' step and count for an animation ending at `end`:
/// one screen refresh apart, at least AHEAD ahead (through `sleep_until`
/// when the main thread is about to sleep that long), at most
/// MAX_AHEAD_FRAMES frames.
fn sampling_plan(now: f64, end: f64, refresh: f64, sleep_until: Option<f64>) -> (f64, usize) {
    let start = (now / refresh).ceil() * refresh;
    let mut step = refresh;
    // Frames reach a moment past a sleep on the main thread, so the phone
    // keeps animating until the thread is awake and sends the next list.
    let limit = end.min(sleep_until.map_or(now + AHEAD, |until| until.max(now + AHEAD)));
    let mut count = 1 + (((limit - start) / step).ceil().max(0.0) as usize);
    if count > MAX_AHEAD_FRAMES {
        step = (limit - start) / (MAX_AHEAD_FRAMES - 1) as f64;
        count = MAX_AHEAD_FRAMES;
    }
    (step, count)
}

/// Until when a coming-frames list must reach because the main thread is
/// about to sleep (media time); None otherwise.
static SLEEP_UNTIL: Mutex<Option<f64>> = Mutex::new(None);

/// The main thread is about to sleep `seconds`: send the pictures now, with
/// coming frames reaching through the sleep, so moving pictures keep moving
/// while it sleeps. On Apple the render server is another process and runs
/// the animations itself; here the frames are worked out ahead instead.
pub fn sleep_starting(seconds: f64) {
    if seconds <= 0.2 || !crate::fw::runloop::is_main_thread() || !active() || crate::fw::animation::latest_end().is_none() {
        return;
    }
    let until = crate::fw::runloop::media_time() + seconds + crate::fw::runloop::refresh_seconds();
    *SLEEP_UNTIL.lock().unwrap_or_else(|e| e.into_inner()) = Some(until);
    // A tick that just ran would turn this list away: it must go before the sleep.
    DRAW_SOON.store(true, std::sync::atomic::Ordering::Relaxed);
    tick();
    *SLEEP_UNTIL.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// The coming frames to work out now (see `Sampling`): one screen refresh
/// apart on a fixed grid (a moment is sampled the same way every tick),
/// until the running animations end (at most AHEAD, or through a sleep on
/// the main thread); None when none runs.
fn sampling_now(now: f64) -> Option<Sampling> {
    // While a finger drags or a scroll coasts, what shows changes every
    // refresh anyway: coming frames would be stale at once (and making 0.3 s
    // of them each list held scrolling to half the screen's rate).
    if crate::fw::controls::scrolling() {
        return None;
    }
    let end = crate::fw::animation::latest_end()?;
    let sleep_until = *SLEEP_UNTIL.lock().unwrap_or_else(|e| e.into_inner());
    let (step, count) = sampling_plan(now, end, crate::fw::runloop::refresh_seconds(), sleep_until);
    let start = (now / step).ceil() * step;
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

/// A CGImage kept alive while a transition shows it: each copy holds one
/// reference, released when the last copy goes.
struct HeldImage(Id);

impl HeldImage {
    /// Takes the caller's one reference (from `last_frame_image`).
    fn new(image: Id) -> HeldImage {
        HeldImage(image)
    }
}

impl Clone for HeldImage {
    fn clone(&self) -> HeldImage {
        crate::fw::counted::retain(self.0);
        HeldImage(self.0)
    }
}

impl Drop for HeldImage {
    fn drop(&mut self) {
        crate::fw::counted::release(self.0);
    }
}

/// A CATransition the screen shows (Core Animation's render server keeps
/// the layer's last committed picture and goes from it to the new one): its
/// layer, our copy of the animation, how it runs, and the pictures the
/// layer showed on the screen before it was added.
#[derive(Clone)]
struct ScreenTransition {
    layer: Id,
    animation: Id,
    look: crate::fw::animation::TransitionLook,
    old: Vec<Picture>,
    /// A GL view's last frame kept for the old pictures, released with them.
    frame: Option<HeldImage>,
}

impl ScreenTransition {
    /// Its mark's path: how it runs (see TRANSITION_MARK and `encode`).
    fn mark(&self) -> String {
        let look = &self.look;
        let [x1, y1, x2, y2] = look.curve.0;
        let mut text = format!("{TRANSITION_MARK}\t{}\t{}\t{}\t{}", look.kind, look.subtype, (look.duration * 1000.0).round() as i64, nanoseconds(look.begin));
        for v in [x1, y1, x2, y2, look.progress.0, look.progress.1] {
            text.push('\t');
            push_number(&mut text, v);
        }
        text
    }
}

thread_local! {
    /// The windows as the last list showed them (the main thread's).
    static LAST_SCREEN: std::cell::RefCell<Vec<Rc<Node>>> = const { std::cell::RefCell::new(Vec::new()) };
    /// The transitions the lists show now.
    static TRANSITIONS: std::cell::RefCell<Vec<ScreenTransition>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The windows a list was made of: a transition added later goes from them.
fn screen_seen(nodes: Vec<Rc<Node>>) {
    LAST_SCREEN.with(|last| *last.borrow_mut() = nodes);
}

/// Whether a GL view whose layer is `gl_layer` is under the node whose layer
/// is `layer` (the transition runs on `layer`) among `windows`.
fn gl_layer_inside(windows: &[Rc<Node>], layer: Id, gl_layer: Id) -> bool {
    fn has_gl(node: &Node, gl_layer: Id) -> bool {
        (node.gl && node.gl_layer == gl_layer) || node.children.iter().any(|child| has_gl(child, gl_layer))
    }
    fn under(node: &Node, layer: Id, gl_layer: Id) -> bool {
        if node.layer == layer {
            return has_gl(node, gl_layer);
        }
        node.children.iter().any(|child| under(child, layer, gl_layer))
    }
    windows.iter().any(|window| under(window, layer, gl_layer))
}

/// Follows the running CATransitions: one newly added takes the pictures
/// its layer showed on the last screen (none when it wasn't there) as its
/// old ones; one that ended or was removed is dropped.
fn pick_transitions() {
    let running = crate::fw::animation::transitions();
    let mut shown = TRANSITIONS.with(|t| std::mem::take(&mut *t.borrow_mut()));
    shown.retain(|t| running.iter().any(|(_, animation, _)| *animation == t.animation));
    for (layer, animation, look) in running {
        match shown.iter_mut().find(|t| t.animation == animation) {
            // (Its begin moves to its turn's end when that turn is done.)
            Some(t) => t.look = look,
            None => {
                let (old, frame) = LAST_SCREEN.with(|last| {
                    let last = last.borrow();
                    // A GL view inside the layer keeps its last frame in the
                    // old pictures (Core Animation renders its contents).
                    let gl_layer = crate::fw::eagl::shown_frame_layer().filter(|&gl_layer| gl_layer_inside(&last, layer, gl_layer));
                    let image = gl_layer.and_then(crate::fw::eagl::last_frame_image);
                    let old = match (gl_layer, image) {
                        (Some(gl_layer), Some(image)) => with_gl_contents(Some((gl_layer, format!("{CG_IMAGE_PREFIX}{image:x}"))), || layer_pictures(&last, layer)).unwrap_or_default(),
                        _ => layer_pictures(&last, layer).unwrap_or_default(),
                    };
                    (old, image.map(HeldImage::new))
                });
                shown.push(ScreenTransition { layer, animation, look, old, frame });
            }
        }
    }
    TRANSITIONS.with(|t| *t.borrow_mut() = shown);
}

/// The pictures `layer` (and what is inside it) shows among `windows`, None
/// when it isn't there.
fn layer_pictures(windows: &[Rc<Node>], layer: Id) -> Option<Vec<Picture>> {
    let all: Vec<Picture> = with_marked(vec![layer], || windows.iter().flat_map(|w| flatten_at(w, 0)).collect());
    let start = all.iter().position(|p| p.path == format!("{TRANSITION_MARK}:{layer:x}"))?;
    let end = start + all[start..].iter().position(|p| p.path == TRANSITION_END_MARK)?;
    Some(all[start + 1..end].to_vec())
}

/// `pictures` with each transition's old pictures put in after its mark, then "to".
fn with_old_pictures(pictures: Vec<Picture>, transitions: &[ScreenTransition]) -> Vec<Picture> {
    if transitions.is_empty() {
        return pictures;
    }
    let mut out = Vec::with_capacity(pictures.len());
    for p in pictures {
        let layer = p.path.strip_prefix(TRANSITION_MARK).and_then(|rest| rest.strip_prefix(':')).and_then(|hex| usize::from_str_radix(hex, 16).ok());
        match layer.and_then(|layer| transitions.iter().find(|t| t.layer == layer)) {
            Some(t) => {
                let clip = p.clip;
                out.push(Picture { path: t.mark(), ..p });
                out.extend(t.old.iter().cloned());
                out.push(Picture { path: TRANSITION_TO_MARK.into(), ..mark_picture(clip) });
            }
            None => out.push(p),
        }
    }
    out
}

/// The pictures as they were (`old`) or as they are: each transition among
/// them gives its old or its new pictures, without its marks.
fn transition_side(pictures: Vec<Picture>, old: bool) -> Vec<Picture> {
    // For each transition open here: whether its old pictures come now.
    let mut sides: Vec<bool> = Vec::new();
    pictures
        .into_iter()
        .filter(|p| {
            if p.path == TRANSITION_TO_MARK {
                if let Some(side) = sides.last_mut() {
                    *side = false;
                }
                return false;
            }
            if p.path == TRANSITION_END_MARK {
                sides.pop();
                return false;
            }
            if p.path.starts_with(TRANSITION_MARK) {
                sides.push(true);
                return false;
            }
            sides.iter().all(|&side| side == old)
        })
        .collect()
}

/// A transition line made a dissolve (type "fade", no subtype), its timing kept.
fn dissolving(line: &str) -> String {
    let mut fields: Vec<&str> = line.split('\t').collect();
    if fields.len() > 2 {
        (fields[1], fields[2]) = ("fade", "");
    }
    fields.join("\t")
}

/// The GL layers in `nodes` (CAEAGLLayers whose frames the GL picture shows).
fn gl_layers(nodes: &[Rc<Node>]) -> Vec<Id> {
    fn walk(node: &Node, out: &mut Vec<Id>) {
        if node.gl && node.gl_layer != 0 && !out.contains(&node.gl_layer) {
            out.push(node.gl_layer);
        }
        node.children.iter().for_each(|child| walk(child, out));
    }
    let mut out = Vec::new();
    nodes.iter().for_each(|node| walk(node, &mut out));
    out
}

/// Which GL layers' frames show: an iPhone shows a CAEAGLLayer's frames from
/// the commit that adds it, and an old one's until the commit that takes it
/// away. A list is that commit once it is up on the screen; the phone first
/// decodes its new pictures (a fade's snapshot takes some frames), so there
/// its GL layers count from the phone's word (`phone_put_up`).
#[derive(Default)]
struct GlCommit {
    /// The GL layers of the list up on the screen (None before the first).
    on: Option<Vec<Id>>,
    /// Lists sent and not yet up: their number, when they went, their GL layers.
    waiting: std::collections::VecDeque<(u64, std::time::Instant, Vec<Id>)>,
}

impl GlCommit {
    /// The longest a list waits for the phone's word before it counts as up
    /// anyway (a word lost, a list the phone dropped).
    const LONGEST: std::time::Duration = std::time::Duration::from_millis(250);

    /// A tick's GL layers, with the number of the list it `sent` (None when
    /// nothing went); `waits` when the screen says when a list is up.
    /// True when the layers whose frames show changed.
    fn list(&mut self, layers: Vec<Id>, sent: Option<u64>, waits: bool, now: std::time::Instant) -> bool {
        match sent {
            Some(number) if waits => self.waiting.push_back((number, now, layers)),
            _ if self.waiting.is_empty() => return self.put(layers),
            _ => {}
        }
        match self.waiting.iter().rposition(|(_, sent, _)| now.duration_since(*sent) >= Self::LONGEST) {
            Some(late) => self.up_to(late),
            None => false,
        }
    }

    /// The phone put list `number` up (it skips lists a newer one replaced).
    fn shown(&mut self, number: u64) -> bool {
        match self.waiting.iter().rposition(|(waiting, _, _)| *waiting <= number) {
            Some(index) => self.up_to(index),
            None => false,
        }
    }

    fn up_to(&mut self, index: usize) -> bool {
        let layers = self.waiting.drain(..=index).last().map(|(_, _, layers)| layers).unwrap_or_default();
        self.put(layers)
    }

    fn put(&mut self, layers: Vec<Id>) -> bool {
        self.on.replace(layers.clone()).is_none_or(|before| before != layers)
    }
}

static GL_COMMIT: Mutex<GlCommit> = Mutex::new(GlCommit { on: None, waiting: std::collections::VecDeque::new() });

/// Whether the list up on the screen holds GL layer `layer` (None before any list).
pub fn gl_committed(layer: Id) -> Option<bool> {
    GL_COMMIT.lock().unwrap_or_else(|e| e.into_inner()).on.as_ref().map(|layers| layers.contains(&layer))
}

/// The phone put list `number` up on its screen (any thread).
pub fn phone_put_up(number: u64) {
    if GL_COMMIT.lock().unwrap_or_else(|e| e.into_inner()).shown(number) {
        draw_soon();
    }
}

/// How much of the window's GL picture shows (0 to 1): as Core Animation
/// draws a CAEAGLLayer's frame as that layer's contents, only while the
/// layer that drew it (`frame`) is shown, at its opacity. A layer that left
/// the window takes its frame along, and a new one shows nothing of another
/// layer's frame before drawing its own. A frame not known by a view's
/// layer (None) shows as it is, and so does all while a `video` shows: the
/// phone's video player sits under the pictures too, and a movie often
/// plays with the GL view hidden or out of the window (under a full-screen
/// screen).
fn gl_shown(nodes: &[Rc<Node>], frame: Option<Id>, video: bool) -> f64 {
    gl_shown_at(nodes, frame, video, 0)
}

/// `gl_shown` in coming frame `k`.
fn gl_shown_at(nodes: &[Rc<Node>], frame: Option<Id>, video: bool, k: usize) -> f64 {
    fn shown(node: &Node, parent_alpha: f64, frame: Id, k: usize) -> f64 {
        let alpha = parent_alpha * node.pose(k).alpha;
        if node.hidden || alpha <= 0.01 {
            return 0.0;
        }
        let own = if node.gl && node.gl_layer == frame { alpha } else { 0.0 };
        node.children.iter().map(|child| shown(child, alpha, frame, k)).fold(own, f64::max)
    }
    let Some(frame) = frame.filter(|_| !video) else { return 1.0 };
    nodes.iter().map(|node| shown(node, 1.0, frame, k)).fold(0.0, f64::max)
}

/// `gl_shown` in each of the `count` coming frames (see `Sampling`): a GL
/// view whose opacity animates shows its picture at that frame's opacity.
fn gl_amounts(nodes: &[Rc<Node>], frame: Option<Id>, video: bool, count: usize) -> Vec<f64> {
    (0..count.max(1)).map(|k| gl_shown_at(nodes, frame, video, k)).collect()
}

/// A list of coming frames with each later frame's GL line after its "@"
/// line, when the amount changes over the frames (the phone takes a frame's
/// own amount, else the first line's).
fn frame_gl_lines(text: &str, amounts: &[f64]) -> String {
    if amounts.iter().all(|&a| gl_mark(a) == gl_mark(amounts[0])) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + amounts.len() * 12);
    let mut frame = 0;
    for line in text.split_inclusive('\n') {
        out += line;
        if line.starts_with("@\t") {
            frame += 1;
            if let Some(&amount) = amounts.get(frame) {
                out += &gl_mark(amount);
                out.push('\n');
            }
        }
    }
    out
}

/// `text` with its GL line after the first line: "!", "gl" and how much of
/// the GL picture shows, or "off" (the phone covers it with the window's
/// black).
fn gl_line(text: &str, shown: f64) -> String {
    let (first, rest) = text.split_once('\n').unwrap_or((text, ""));
    format!("{first}\n{}\n{rest}", gl_mark(shown))
}

fn gl_mark(shown: f64) -> String {
    let amount = if shown <= 0.01 { "off".to_string() } else { format!("{}", (shown * 1000.0).round() / 1000.0) };
    format!("!\tgl\t{amount}")
}

/// When the pictures last changed (frame 0 of a list), for `gap`.
static LAST_CHANGE: Mutex<Option<std::time::Instant>> = Mutex::new(None);

/// How long the main loop waits between ticks. As Core Animation commits
/// at the end of a turn that changed something, a change not yet committed
/// goes at the next refresh (at most one list a refresh), and so do moving
/// pictures (a finger down, or the pictures changed in the last 100 ms: a
/// card dragged, a view the game moves itself). What changes by itself (a
/// caret, a picker) or animates keeps a 50 ms tick; a still screen waits a
/// quarter second, as a mark from another thread wakes the loop
/// (changes.rs).
pub fn gap() -> f64 {
    let moving = LAST_CHANGE.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|at| at.elapsed().as_secs_f64() < 0.1);
    let pending = DRAW_SOON.load(std::sync::atomic::Ordering::Relaxed) || commit_needed();
    let busy = crate::fw::animation::latest_end().is_some() || LAST_SCREEN.with(|last| always_changing(&last.borrow()));
    gap_for(pending, moving || crate::fw::touch::fingers_down(), busy, crate::fw::runloop::refresh_seconds())
}

/// `gap`: with a change waiting, moving pictures, or something on the screen
/// changing by itself or animating (`busy`).
fn gap_for(pending: bool, moving: bool, busy: bool, refresh: f64) -> f64 {
    if pending || moving {
        refresh
    } else if busy {
        0.05
    } else {
        0.25
    }
}

/// The longest a list is held back for picture files still being written:
/// a picture or two of a change (a character's new clothes, some ms on the
/// phone) come in time, while a burst of new pictures (a whole new screen,
/// a game making one every frame) shows what is ready after six frames at
/// 60 Hz, about as late as a tap's answer can come and still feel at once.
const HOLD_MOST: std::time::Duration = std::time::Duration::from_millis(100);

/// Whether this tick's list is held back: a picture file it needs is still
/// being written (`waiting`), so the old list stays and the whole change
/// shows at once a little later, as on iPhone. `since`: when the holding
/// began; after HOLD_MOST every list goes until a tick has nothing waiting.
fn hold_list(waiting: bool, since: &mut Option<std::time::Instant>, now: std::time::Instant) -> bool {
    if !waiting {
        *since = None;
        return false;
    }
    let began = *since.get_or_insert(now);
    now.duration_since(began) < HOLD_MOST
}

/// The first frame's pictures as files (see `resolve`), and whether a
/// picture file they need is still being written on its own thread.
fn first_frame(nodes: &[Rc<Node>]) -> (Vec<Picture>, bool) {
    crate::fw::graphics::noting_unready(|| compose_groups(resolve(frames(nodes, 1).remove(0))))
}

pub fn draw_soon() {
    crate::fw::changes::touch();
    DRAW_SOON.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// The parts of the screen watched between lists, any of which changing
/// without a per-object `changed()` mark means the list must be rebuilt:
/// the change counter, the screen, the window turn and the picture pixels,
/// the GL frame shown and a shown video.
#[derive(Clone, Copy, PartialEq, Debug)]
struct SkipKey {
    generation: u64,
    screen: (f64, f64, f64),
    turn: Option<Transform>,
    pixels: f64,
    frame_layer: Option<Id>,
    video: bool,
}

/// The current skip key.
fn skip_key() -> SkipKey {
    SkipKey {
        generation: crate::fw::changes::commit_generation(),
        screen: crate::fw::uikit::screen_points(),
        turn: crate::fw::uikit::window_turn(),
        pixels: pixels_per_point(),
        frame_layer: crate::fw::eagl::shown_frame_layer(),
        video: crate::fw::media::video_showing(),
    }
}

thread_local! {
    /// The skip key of the last list this thread committed.
    static LAST_KEY: std::cell::Cell<Option<SkipKey>> = const { std::cell::Cell::new(None) };
}

/// Whether the pictures must be rebuilt: the watched screen parts changed
/// since the last committed list.
fn commit_needed() -> bool {
    LAST_KEY.with(|last| last.get() != Some(skip_key()))
}

/// Record the current skip key as committed (a tick built and sent it).
fn mark_committed() {
    LAST_KEY.with(|last| last.set(Some(skip_key())));
}

/// IOS_PICTURES_FULL=1 turns the skip off: every tick rebuilds and sends.
fn full_pictures() -> bool {
    static FULL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FULL.get_or_init(|| std::env::var("IOS_PICTURES_FULL").is_ok_and(|v| v == "1"))
}

thread_local! {
    /// Tests turn verification on without an environment variable.
    static VERIFY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// IOS_PICTURES_VERIFY=1 (or a test's thread-local) rebuilds on ticks the
/// skip would have skipped, to check nothing was missed.
fn verify_pictures() -> bool {
    static VERIFY_ENV: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    VERIFY.with(|v| v.get()) || *VERIFY_ENV.get_or_init(|| std::env::var("IOS_PICTURES_VERIFY").is_ok_and(|v| v == "1"))
}

/// Turns verification on for this thread (tests).
pub fn set_verify_pictures(on: bool) {
    VERIFY.with(|v| v.set(on));
}

/// Whether the last committed screen holds a view that changes every tick
/// without a mark (a picker, page control, progress bar, text field, table
/// or video view): the pictures can't be skipped while one is on screen.
fn always_changing(nodes: &[Rc<Node>]) -> bool {
    changing_node(nodes).is_some()
}

/// The view or layer of the first node that changes every tick (see
/// `always_changing`; a replicator copy's: the replicator's).
fn changing_node(nodes: &[Rc<Node>]) -> Option<Id> {
    fn find(node: &Node, above: Id) -> Option<Id> {
        let id = if node.id != 0 { node.id } else { above };
        if node.video || node.changing {
            return Some(id);
        }
        node.children.iter().find_map(|c| find(c, id))
    }
    nodes.iter().find_map(|n| find(n, 0))
}

/// An object's class for a log line, a view's layer with its view's.
fn named(object: Id) -> String {
    let view = crate::fw::layers::view_of(object);
    let name = crate::fw::string::describe_class_of(object);
    if view != 0 { format!("{name} of {} {view:#x}", crate::fw::string::describe_class_of(view)) } else { format!("{name} {object:#x}") }
}

/// IOS_PICTURES_VERIFY=1: says once (per reason) why a tick could not be
/// skipped: what was marked, what changes every tick, or which animation runs.
fn log_not_skipped() {
    let reason = if commit_needed() {
        match LAST_KEY.with(|last| last.get()) {
            None => "nothing was sent yet".to_string(),
            Some(last) => {
                let now = skip_key();
                if last.generation != now.generation {
                    let marked: Vec<String> = crate::fw::changes::peek_dirty(3).into_iter().map(named).collect();
                    if marked.is_empty() { "a list-only change (picture file, scroll step, GL frame)".to_string() } else { format!("marked {}", marked.join(", ")) }
                } else if last.screen != now.screen || last.turn != now.turn || last.pixels != now.pixels {
                    "the screen size, turn or scale changed".to_string()
                } else {
                    "the GL frame layer or video changed".to_string()
                }
            }
        }
    } else if let Some(id) = LAST_SCREEN.with(|last| changing_node(&last.borrow())) {
        format!("it changes every tick: {}", named(id))
    } else {
        let layers: Vec<String> = crate::fw::animation::running_layers().into_iter().take(3).map(named).collect();
        format!("an animation runs on {}", layers.join(", "))
    };
    thread_local! {
        static SAID: RefCell<std::collections::HashSet<String>> = RefCell::new(std::collections::HashSet::new());
    }
    if SAID.with(|said| { let mut said = said.borrow_mut(); said.len() < 200 && said.insert(reason.clone()) }) {
        crate::log!("pictures: not skipped because {reason}");
    }
}

/// IOS_PICTURES_VERIFY=1: the nodes taken partly from the cache must show
/// what a full build shows. At a difference the object whose cached node
/// went stale is logged (once per object) and the full build is used, so
/// the cache then holds only fresh nodes.
fn checked_against_full(nodes: Vec<Rc<Node>>, windows: &[Id], classes: &Classes, sampling: Option<Sampling>) -> Vec<Rc<Node>> {
    let built = BUILT.with(|b| b.get());
    let full_build = || with_caching(false, || windows.iter().map(|&w| snapshot(w, classes, 0)).collect::<Vec<_>>());
    let full = match sampling {
        Some(sampling) => with_sampling(sampling, full_build),
        None => full_build(),
    };
    BUILT.with(|b| b.set(built));
    log_reuse(&nodes, built);
    let count = sampling.map_or(1, |s| s.count.max(1));
    let lines = |node: &Node| (0..count).map(|k| lines_of(&flatten_at(node, k))).collect::<Vec<_>>().join("@\n");
    let mut same = true;
    for (cached, fresh) in nodes.iter().zip(&full) {
        if lines(cached) == lines(fresh) {
            continue;
        }
        same = false;
        let (id, line) = stale_node(cached, fresh, &lines);
        thread_local! {
            static SAID: RefCell<std::collections::HashSet<Id>> = RefCell::new(std::collections::HashSet::new());
        }
        if SAID.with(|said| said.borrow_mut().insert(id)) {
            crate::log!("pictures: cached build differs: {id:#x} {} {line}", crate::fw::string::describe_class_of(id));
        }
    }
    if same { nodes } else { full }
}

/// The deepest node whose cached and fresh builds differ themselves (not
/// only through a child), and the first line where they differ.
fn stale_node(cached: &Rc<Node>, fresh: &Rc<Node>, lines: &dyn Fn(&Node) -> String) -> (Id, String) {
    if cached.children.len() == fresh.children.len() && cached.mask.is_some() == fresh.mask.is_some() {
        let pairs = cached.children.iter().zip(&fresh.children).chain(cached.mask.iter().zip(fresh.mask.iter()));
        for (a, b) in pairs {
            if lines(a) != lines(b) {
                return stale_node(a, b, lines);
            }
        }
    }
    let line = first_differing_line(&lines(cached), &lines(fresh)).unwrap_or_else(|| "(same lines alone, differs in place)".to_string());
    (fresh.id, line)
}

/// IOS_PICTURES_VERIFY=1: how many of the committed nodes were taken from
/// the cache, each time the counts change (at most 500 lines).
fn log_reuse(nodes: &[Rc<Node>], built: usize) {
    fn count(node: &Node) -> usize {
        usize::from(node.id != 0) + node.children.iter().chain(node.mask.iter()).map(|c| count(c)).sum::<usize>()
    }
    let total: usize = nodes.iter().map(|n| count(n)).sum();
    thread_local! {
        static LAST: std::cell::Cell<(usize, usize, usize)> = const { std::cell::Cell::new((0, 0, 0)) };
    }
    let (logged, last_total, last_built) = LAST.with(|l| l.get());
    if logged < 500 && (total, built) != (last_total, last_built) {
        LAST.with(|l| l.set((logged + 1, total, built)));
        crate::log!("pictures: reused {} of {total} nodes, built {built}", total.saturating_sub(built));
    }
}

/// The windows' pictures in each coming frame (made into files; frames
/// after the first only while their groups are made already), with times.
fn screen_frames(nodes: &[Rc<Node>], sampling: Sampling) -> Vec<(f64, Vec<Picture>)> {
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

/// The first line where two lists differ, for the verify log.
fn first_differing_line(old: &str, new: &str) -> Option<String> {
    let mut old_lines = old.lines();
    let mut new_lines = new.lines();
    loop {
        match (old_lines.next(), new_lines.next()) {
            (Some(a), Some(b)) if a == b => continue,
            (Some(a), Some(b)) => return Some(format!("was: {a}, now: {b}")),
            (Some(a), None) => return Some(format!("was: {a}, now: (end)")),
            (None, Some(b)) => return Some(format!("was: (end), now: {b}")),
            (None, None) => return None,
        }
    }
}

thread_local! {
    /// Whether a tick is running on this thread (see `tick`).
    static TICKING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A tick in progress on this thread. A second tick (from a sleep inside
/// game drawing) runs nothing; the outer one sends the pictures.
struct TickGuard;

impl TickGuard {
    fn enter() -> Option<TickGuard> {
        (!TICKING.with(|ticking| ticking.replace(true))).then_some(TickGuard)
    }
}

impl Drop for TickGuard {
    fn drop(&mut self) {
        TICKING.with(|ticking| ticking.set(false));
    }
}

/// From the main loop: sends the pictures when they changed (at most 20
/// times a second, or at once after an animation was added). While
/// animations run, the list carries the coming frames too (encode_frames);
/// they are worked out again only when what is shown stops matching them or
/// they run short.
pub fn tick() {
    // Game drawing (-drawRect:, below) can sleep; the sleep hook calls
    // tick() again to send frames for the sleep. That one runs nothing: the
    // outer tick holds the picture locks and is about to send its own list.
    let Some(_tick) = TickGuard::enter() else { return };
    let Some(screen) = screen() else { return };
    static LAST: Mutex<Option<(std::time::Instant, String)>> = Mutex::new(None);
    static SENT: Mutex<Option<Sent>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let soon = DRAW_SOON.load(std::sync::atomic::Ordering::Relaxed);
    // A little under the gap, so a loop woken on time isn't turned away.
    if !soon && last.as_ref().is_some_and(|(at, _)| at.elapsed().as_secs_f64() < gap() * 0.9) {
        crate::fw::animation::commit();
        return;
    }
    DRAW_SOON.store(false, std::sync::atomic::Ordering::Relaxed);
    // The gap counts from this list's start, so lists keep the screen's
    // pace while making one takes most of a refresh.
    let started = std::time::Instant::now();
    // Views that draw themselves (-drawRect:) are drawn before they are shown.
    crate::fw::uikit::display_pass();
    // A turn with nothing new: run the commit and skip the rebuild (kept off
    // by IOS_PICTURES_FULL=1; IOS_PICTURES_VERIFY=1 rebuilds to check).
    let would_skip = !full_pictures()
        && !commit_needed()
        && LAST_SCREEN.with(|last| !always_changing(&last.borrow()))
        && crate::fw::animation::latest_end().is_none();
    if !would_skip && !full_pictures() && verify_pictures() {
        log_not_skipped();
    }
    if would_skip && !verify_pictures() {
        crate::fw::animation::commit();
        crate::fw::layers::commit_new();
        after_check(false);
        return;
    }
    // A full build stands for the key it was made from. Saving it before the
    // build means a mark during it (a picture file finishing on its own
    // thread) rebuilds on the next tick rather than leaving a stale picture.
    mark_committed();
    commit_dirty();
    let classes = Classes::find();
    let windows = crate::fw::uikit::windows();
    // As Apple's commit draws new layers' contents before their animations
    // begin, the new pictures are made first: making them (SpellCraft's spell
    // panel cards, ~200 ms on the phone) used up a 0.2 s slide before its
    // first frame was sent.
    if crate::fw::animation::uncommitted() {
        let nodes = build_windows(&windows, &classes);
        compose_groups(resolve(frames(&nodes, 1).remove(0)));
        crate::fw::animation::commit();
        // The animations beginning now marked their layers.
        commit_dirty();
    }
    let (wide, high, _) = crate::fw::uikit::screen_points();
    let now = crate::fw::runloop::media_time();
    pick_transitions();
    let mut sent = SENT.lock().unwrap_or_else(|e| e.into_inner());
    crate::fw::graphics::new_screen_check();
    let sampling = sampling_now(now);
    BUILT.with(|b| b.set(0));
    // IOS_PICTURES_VERIFY=1 builds twice: both at one moment, so an
    // animation ending between them doesn't count as a difference.
    let verify = verify_pictures() && !full_pictures();
    let build_sampling = sampling.or_else(|| verify.then_some(Sampling { start: now, step: crate::fw::runloop::refresh_seconds(), count: 1 }));
    let nodes = match build_sampling {
        Some(sampling) => with_sampling(sampling, || build_windows(&windows, &classes)),
        None => build_windows(&windows, &classes),
    };
    let nodes = if verify { checked_against_full(nodes, &windows, &classes, build_sampling) } else { nodes };
    keep_reusable(&nodes);
    let gl_amount = gl_shown(&nodes, crate::fw::eagl::shown_frame_layer(), crate::fw::media::video_showing());
    let (first, waiting) = first_frame(&nodes);
    static HOLDING: Mutex<Option<std::time::Instant>> = Mutex::new(None);
    if hold_list(waiting, &mut HOLDING.lock().unwrap_or_else(|e| e.into_inner()), std::time::Instant::now()) {
        // The old list stays on the screen (and `last`, SENT and LAST_SCREEN
        // still say what it shows). The next tick must not skip: its key is
        // forgotten (a file that turns out unmakeable marks nothing), and it
        // comes at the screen's pace.
        LAST_KEY.with(|key| key.set(None));
        DRAW_SOON.store(true, std::sync::atomic::Ordering::Relaxed);
        *LAST_CHANGE.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());
        drop(sent);
        drop(last);
        crate::fw::layers::commit_new();
        return;
    }
    let (pictures, text, shown_lines) = match sampling {
        None => {
            *sent = None;
            let pictures = first;
            let lines = lines_of(&pictures);
            let text = gl_line(&format!("{}\t{}\n{lines}", wide, high), gl_amount);
            (pictures, text, lines)
        }
        Some(sampling) => {
            let lines = lines_of(&first);
            // What was sent still holds while it matches now and reaches the next
            // tick or the end (or through a sleep the thread is taking).
            let until = list_until(sampling, *SLEEP_UNTIL.lock().unwrap_or_else(|e| e.into_inner()));
            // (And the GL picture shows as it did: the GL line comes after
            // the first line.)
            let mark = gl_mark(gl_amount);
            let same_gl = |text: &str| text.lines().take(3).any(|line| line == mark);
            let held = last.as_ref().filter(|(_, text)| same_gl(text)).map(|(_, text)| text.clone());
            match held.filter(|_| sent.as_ref().is_some_and(|s| s.holds(sampling.start, &lines, until))) {
                Some(text) => (first, text, lines),
                None => {
                    let timed = screen_frames(&nodes, sampling);
                    *sent = Some(Sent { frames: timed.iter().map(|(t, p)| (*t, lines_of(p))).collect() });
                    let amounts = gl_amounts(&nodes, crate::fw::eagl::shown_frame_layer(), crate::fw::media::video_showing(), timed.len());
                    let text = frame_gl_lines(&gl_line(&encode_frames((wide, high), &timed), gl_amount), &amounts);
                    (first, text, lines)
                }
            }
        }
    };
    drop(sent);
    let nodes_gl = gl_layers(&nodes);
    screen_seen(nodes);
    let changed = last.as_ref().is_none_or(|(_, sent)| *sent != text);
    // A missed mark, only while IOS_PICTURES_VERIFY=1: the skip would have
    // kept the old list, but a fresh one differs from it.
    if would_skip && verify_pictures() && changed {
        if let Some((_, old)) = last.as_ref() {
            if let Some(line) = first_differing_line(old, &text) {
                crate::log!("pictures: skipped commit missed a change: {line}");
            }
        }
    }
    // What counts as a change for "settled" (tests tap on a settled screen):
    // the shown pictures, not the coming frames' times; and with an animation
    // that repeats forever, only which pictures show, not where.
    static SHOWN: Mutex<String> = Mutex::new(String::new());
    let shown = if crate::fw::animation::latest_end().is_some_and(f64::is_infinite) {
        pictures.iter().map(|p| p.path.as_str()).collect::<Vec<_>>().join("\n")
    } else {
        shown_lines
    };
    let new_screen = std::mem::replace(&mut *SHOWN.lock().unwrap_or_else(|e| e.into_inner()), shown.clone()) != shown;
    if changed {
        *LAST_CHANGE.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());
    }
    let mut sent = None;
    if changed && (last.is_some() || !pictures.is_empty()) {
        // The first changes in full (where each picture goes), then only how many.
        static LOGGED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        if LOGGED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 20 {
            crate::log!("pictures: {} shown:\n{}", pictures.len(), encode((wide, high), &pictures).trim_end());
        } else {
            crate::log!("pictures: {} shown", pictures.len());
        }
        static NUMBER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let number = NUMBER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        (screen.show)(&text, number);
        sent = Some(number);
    }
    *last = Some((started, text));
    drop(last);
    // Its GL views' frames show once it is up (eagl::present), the new
    // screen's under its own pictures and fade, not before them.
    if GL_COMMIT.lock().unwrap_or_else(|e| e.into_inner()).list(nodes_gl, sent, screen.says_when_up, std::time::Instant::now()) {
        draw_soon();
    }
    // The layers just drawn are committed (later changes run their actions).
    crate::fw::layers::commit_new();
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

    fn view(frame: Rect, picture: Option<&str>, children: Vec<Rc<Node>>) -> Node {
        let rect = rect(0.0, 0.0, frame.size.width, frame.size.height);
        let pictures = picture.map(|p| Shown { path: p.to_string(), mode: 0, size: (10.0, 10.0), rect, caps: None, tile: false, text: None, alpha: 1.0 }).into_iter().collect();
        Node { frame, alpha: 1.0, pictures, children, ..Default::default() }
    }

    fn window(children: Vec<Rc<Node>>) -> Node {
        Node { alpha: 1.0, frame: rect(0.0, 0.0, 960.0, 540.0), children, ..Default::default() }
    }

    /// Where a point of the picture's area lands on the screen.
    fn at(p: &Picture, x: f64, y: f64) -> (f64, f64) {
        let t = p.place;
        ((t.a * x + t.c * y + t.tx).round(), (t.b * x + t.d * y + t.ty).round())
    }

    /// The phone's coming frames reach through a sleep on the main thread
    /// (Apple's render server keeps animating while the app's thread sleeps),
    /// at most MAX_AHEAD_FRAMES of them.
    #[test]
    fn coming_frames_reach_through_a_sleep_on_the_main_thread() {
        let refresh = 1.0 / 120.0;
        let plain = sampling_plan(0.0, 3.0, refresh, None);
        assert!(plain.1 as f64 * plain.0 >= 0.29 && plain.1 as f64 * plain.0 < 0.4, "a busy thread is covered for AHEAD: {plain:?}");
        let sleeping = sampling_plan(0.0, 3.0, refresh, Some(1.5));
        assert!(sleeping.1 as f64 * sleeping.0 >= 1.5, "a sleeping main thread is covered through its sleep: {sleeping:?}");
        let long = sampling_plan(0.0, 30.0, refresh, Some(10.0));
        assert!(long.1 <= MAX_AHEAD_FRAMES, "a long sleep keeps the lists bounded: {long:?}");
        assert!(long.1 as f64 * long.0 >= 9.9, "still through the sleep: {long:?}");
    }

    /// A list already sent is reused only if it reaches the next tick; for a
    /// sleep it must reach through the whole sleep, not the 0.15 s a normal
    /// tick needs.
    #[test]
    fn a_sent_list_that_stops_short_of_a_sleep_is_made_again() {
        // A sleep to 20 s makes sampling_now plan frames out to 20 s.
        let sampling = Sampling { start: 10.0, step: 0.1, count: 101 };
        let short = Sent { frames: vec![(10.0, "same".to_string()), (10.30, "same".to_string())] };
        assert!(short.holds(sampling.start, "same", list_until(sampling, None)), "covers the next 0.15 s");
        assert!(!short.holds(sampling.start, "same", list_until(sampling, Some(20.0))), "a list to 10.30 s doesn't cover a sleep to 20 s");
        let long = Sent { frames: vec![(10.0, "same".to_string()), (20.01, "same".to_string())] };
        assert!(long.holds(sampling.start, "same", list_until(sampling, Some(20.0))), "one that reaches it is reused");
    }

    /// A tick that starts while one runs on the same thread (a -drawRect:
    /// that sleeps calls the sleep hook, which ticks) runs nothing: the outer
    /// tick holds the picture locks.
    #[test]
    fn a_tick_inside_a_tick_runs_nothing() {
        let first = TickGuard::enter().expect("a first tick runs");
        assert!(TickGuard::enter().is_none(), "a nested tick is refused");
        drop(first);
        assert!(TickGuard::enter().is_some(), "the next tick runs again");
    }

    /// A window and label with no change since their commit need nothing;
    /// committing new layers alone still needs nothing; setText, setAlpha,
    /// cornerRadius and addSubview each mark a change (Apple's layer mark).
    #[test]
    fn no_change_commits_nothing_and_view_changes_do() {
        setup();
        let window = new_view("UIWindow", rect(0.0, 0.0, 320.0, 480.0));
        let label = new_view("UILabel", rect(10.0, 20.0, 100.0, 30.0));
        runtime::send(label, "setText:", &[crate::fw::string::make("Score")]);
        runtime::send(window, "addSubview:", &[label]);
        mark_committed();
        assert!(!commit_needed(), "a no-change commit needs nothing");
        crate::fw::layers::commit_new();
        assert!(!commit_needed(), "committing new layers alone needs nothing");
        runtime::send(label, "setText:", &[crate::fw::string::make("More")]);
        assert!(commit_needed(), "setText marks a change");
        mark_committed();
        send_struct(label, "setAlpha:", 0.5f64);
        assert!(commit_needed(), "setAlpha marks a change");
        mark_committed();
        let layer = runtime::send(label, "layer", &[]);
        send_struct(layer, "setCornerRadius:", 4.0f64);
        assert!(commit_needed(), "cornerRadius marks a change");
        mark_committed();
        let child = new_view("UIView", rect(0.0, 0.0, 10.0, 10.0));
        runtime::send(window, "addSubview:", &[child]);
        assert!(commit_needed(), "addSubview marks a change");
    }

    /// A picture file made on its own thread marks a change when it is ready,
    /// so the next tick shows the picture that was missing before.
    #[test]
    fn a_picture_file_finishing_in_the_background_marks_a_change() {
        setup();
        let image = white_picture(4, 4);
        let before = crate::fw::changes::generation();
        crate::fw::graphics::picture_file(image);
        let mut marked = false;
        for _ in 0..20 {
            if crate::fw::changes::generation() != before {
                marked = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(marked, "the background picture file marks a change");
    }

    /// Polls (20 ms steps, at most 0.5 s) until `done`.
    fn soon(done: impl Fn() -> bool) -> bool {
        (0..25).any(|_| done() || { std::thread::sleep(std::time::Duration::from_millis(20)); false }) || done()
    }

    /// An image view given a new picture made in memory: while its file is
    /// still being written the list is held back (the old one stays on the
    /// screen), and once the file is there the next list shows the picture
    /// (Apple shows new contents in the frame they are committed).
    #[test]
    fn a_new_picture_comes_with_the_rest_of_its_change() {
        setup();
        let window = new_view("UIWindow", rect(0.0, 0.0, 320.0, 480.0));
        let shown = new_view("UIImageView", rect(0.0, 0.0, 4.0, 4.0));
        runtime::send(window, "addSubview:", &[shown]);
        let picture = white_picture(4, 4);
        crate::fw::graphics::test_hold_writing(picture, true);
        runtime::send(shown, "setImage:", &[picture]);
        let nodes = vec![commit_build(window, None)];
        let (_, waiting) = first_frame(&nodes);
        let start = std::time::Instant::now();
        let mut since = None;
        assert!(waiting && hold_list(waiting, &mut since, start), "the list waits for the picture's file");
        crate::fw::graphics::test_hold_writing(picture, false);
        assert!(soon(|| crate::fw::graphics::picture_file(picture).is_some()), "the file is written once let go");
        let (pictures, waiting) = first_frame(&nodes);
        assert!(!waiting && !hold_list(waiting, &mut since, start + std::time::Duration::from_millis(10)), "then the list goes");
        let file = crate::fw::graphics::picture_file(picture).unwrap();
        assert!(pictures.iter().any(|p| p.path == file), "with the picture: {pictures:?}");
    }

    /// A GL layer added to the window is in the list that puts it on the
    /// screen: its frames may show from when that list goes (an iPhone shows
    /// a CAEAGLLayer's frames from the commit that adds it, eagl::present).
    #[test]
    fn a_list_tells_which_gl_layers_it_puts_on_the_screen() {
        setup();
        let window = new_view("UIWindow", rect(0.0, 0.0, 320.0, 480.0));
        let gl = runtime::send(runtime::class_named("CAEAGLLayer"), "layer", &[]);
        send_struct(gl, "setFrame:", rect(0.0, 0.0, 320.0, 480.0));
        runtime::send(runtime::send(window, "layer", &[]), "addSublayer:", &[gl]);
        let nodes = vec![commit_build(window, None)];
        assert_eq!(gl_layers(&nodes), vec![gl], "the GL layer is in the list");
        let empty = new_view("UIWindow", rect(0.0, 0.0, 320.0, 480.0));
        assert!(gl_layers(&[commit_build(empty, None)]).is_empty(), "no GL layer, none listed");
    }

    /// B1+B2: a label's look is worked out again only when what it shows
    /// changed (its text, font, colour; its size), not when it fades, hides
    /// or moves, as Apple keeps a layer's backing store until -setNeedsDisplay.
    #[test]
    fn a_labels_look_is_made_again_only_when_its_content_changes() {
        setup();
        let label = new_view("UILabel", rect(0.0, 0.0, 100.0, 20.0));
        runtime::send(label, "setText:", &[crate::fw::string::make("Gold")]);
        let size = (100.0, 20.0);
        let made = || LOOKS_MADE.with(|m| m.get());
        // As a tick's own build does (a full build for the verify check makes every look).
        let kept = |label: Id, size: (f64, f64)| with_caching(true, || label_look_kept(label, size));
        let start = made();
        let first = kept(label, size);
        assert_eq!(first.as_ref().map(|l| l.text.as_str()), Some("Gold"));
        kept(label, size);
        assert_eq!(made(), start + 1, "the same label again: kept");
        crate::fw::uikit::property_set(label, "alpha", 0.5f64.to_bits());
        crate::fw::uikit::property_set(label, "hidden", 1);
        kept(label, size);
        assert_eq!(made(), start + 1, "fading or hiding keeps the look");
        runtime::send(label, "setText:", &[crate::fw::string::make("Silver")]);
        assert_eq!(kept(label, size).map(|l| l.text), Some("Silver".to_string()));
        assert_eq!(made(), start + 2, "new text: made again");
        kept(label, (120.0, 20.0));
        assert_eq!(made(), start + 3, "a new size: made again");
    }

    /// A3: the next tick comes a refresh after a change not yet committed
    /// (or while things move), every 50 ms while something on the screen
    /// changes by itself or animates, and else only every quarter second.
    #[test]
    fn the_next_tick_comes_with_a_change_not_a_clock() {
        let refresh = 1.0 / 120.0;
        assert_eq!(gap_for(true, false, false, refresh), refresh, "a change waits at most a refresh");
        assert_eq!(gap_for(false, true, false, refresh), refresh, "moving keeps the screen's pace");
        assert_eq!(gap_for(true, false, true, refresh), refresh);
        assert_eq!(gap_for(false, false, true, refresh), 0.05, "a caret or a picker keeps its 50 ms");
        assert_eq!(gap_for(false, false, false, refresh), 0.25, "a still screen waits a quarter second");
    }

    /// On the phone a list is up only once its new pictures are decoded:
    /// its GL layers count from the phone's word that it put the list up
    /// (or a newer one), else from a quarter second after it went.
    #[test]
    fn gl_layers_count_from_when_the_phone_puts_their_list_up() {
        let start = std::time::Instant::now();
        let at = |ms: u64| start + std::time::Duration::from_millis(ms);
        let mut gl = GlCommit::default();
        let (hall, garden) = (vec![0x10], vec![0x20]);
        assert!(gl.list(hall.clone(), Some(1), false, at(0)), "without the phone's word, at once");
        assert_eq!(gl.on, Some(hall.clone()));
        assert!(!gl.list(garden.clone(), Some(2), true, at(10)), "the garden waits for its list to be up");
        assert!(!gl.list(garden.clone(), None, true, at(20)), "a tick with nothing new still waits");
        assert_eq!(gl.on, Some(hall.clone()), "the hall's frames show meanwhile");
        assert!(gl.shown(2), "the phone put the garden's list up");
        assert_eq!(gl.on, Some(garden.clone()));
        gl.list(hall.clone(), Some(3), true, at(30));
        gl.list(garden.clone(), Some(4), true, at(40));
        assert!(!gl.shown(1) && gl.on == Some(garden.clone()), "an older list's word changes nothing");
        gl.shown(4);
        assert!(gl.on == Some(garden.clone()) && gl.waiting.is_empty(), "a newer list up puts the older ones by (the hall of list 3 never shows)");
        gl.list(hall.clone(), Some(5), true, at(50));
        assert!(!gl.list(hall.clone(), None, true, at(290)) && gl.on == Some(garden.clone()));
        assert!(gl.list(hall.clone(), None, true, at(310)), "no word after a quarter second: it counts as up");
        assert_eq!(gl.on, Some(hall));
    }

    /// A picture that can't be made into a file holds nothing back: it is
    /// left out, as before.
    #[test]
    fn a_picture_that_cant_be_made_holds_nothing() {
        setup();
        let window = new_view("UIWindow", rect(0.0, 0.0, 320.0, 480.0));
        let shown = new_view("UIImageView", rect(0.0, 0.0, 4.0, 4.0));
        runtime::send(window, "addSubview:", &[shown]);
        // A UIImage whose pixels can't be read (no file, no bitmap).
        let unreadable = || crate::fw::uikit::make_image(crate::fw::uikit::ImageInfo { pixels: (4, 4), scale: 1.0, ..Default::default() });
        runtime::send(shown, "setImage:", &[unreadable()]);
        let nodes = vec![commit_build(window, None)];
        assert!(soon(|| !first_frame(&nodes).1), "once its writer gave up, nothing waits");
        let mut since = None;
        assert!(!hold_list(false, &mut since, std::time::Instant::now()), "nothing is held");
        // And while its writer still tries, it waits like any other.
        let other = unreadable();
        crate::fw::graphics::test_hold_writing(other, true);
        runtime::send(shown, "setImage:", &[other]);
        let nodes = vec![commit_build(window, None)];
        assert!(first_frame(&nodes).1, "a writer still at work is waited for");
        crate::fw::graphics::test_hold_writing(other, false);
        assert!(soon(|| !first_frame(&nodes).1), "until it gives up");
    }

    /// A picture whose file never comes (a game making a new picture every
    /// frame) holds the screen back at most HOLD_MOST: after that each list
    /// goes, and holding starts afresh only once nothing waited.
    #[test]
    fn a_picture_file_that_never_comes_holds_the_screen_at_most_a_moment() {
        setup();
        let window = new_view("UIWindow", rect(0.0, 0.0, 320.0, 480.0));
        let shown = new_view("UIImageView", rect(0.0, 0.0, 4.0, 4.0));
        runtime::send(window, "addSubview:", &[shown]);
        let picture = white_picture(4, 4);
        crate::fw::graphics::test_hold_writing(picture, true);
        runtime::send(shown, "setImage:", &[picture]);
        let nodes = vec![commit_build(window, None)];
        let start = std::time::Instant::now();
        let at = |ms: u64| start + std::time::Duration::from_millis(ms);
        let mut since = None;
        let held: Vec<bool> = [0, 50, 99, 101, 120, 200].into_iter().map(|ms| hold_list(first_frame(&nodes).1, &mut since, at(ms))).collect();
        assert_eq!(held, [true, true, true, false, false, false], "held for the first {HOLD_MOST:?}, then every list goes");
        assert!(!hold_list(false, &mut since, at(210)), "nothing waits");
        assert!(hold_list(true, &mut since, at(220)), "a new wait holds again");
        crate::fw::graphics::test_hold_writing(picture, false);
    }

    /// Editing a CGPath the game installed on a shape layer (or used by a
    /// shadow) changes what is drawn: the mutation marks a change.
    #[test]
    fn editing_an_installed_path_marks_a_change() {
        setup();
        let shape = runtime::send(runtime::class_named("CAShapeLayer"), "layer", &[]);
        let path = crate::fw::graphics::new_path();
        crate::fw::graphics::path_move(path, Point { x: 0.0, y: 0.0 });
        runtime::send(shape, "setPath:", &[path]);
        mark_committed();
        assert!(!commit_needed(), "installing the path committed cleanly");
        crate::fw::graphics::path_line(path, Point { x: 10.0, y: 10.0 });
        assert!(commit_needed(), "editing an installed path marks a change");
        mark_committed();
        // Paths built while drawing (no layer holds them) change nothing shown.
        let loose = crate::fw::graphics::new_path();
        crate::fw::graphics::path_move(loose, Point { x: 0.0, y: 0.0 });
        crate::fw::graphics::path_line(loose, Point { x: 5.0, y: 5.0 });
        assert!(!commit_needed(), "editing a path no layer draws marks nothing");
    }

    /// One commit as `tick` makes it: the marks since the last one are
    /// followed, the window built with the cache, and its reusable nodes kept.
    fn commit_build(window: Id, sampling: Option<Sampling>) -> Rc<Node> {
        commit_dirty();
        let build = || build_windows(&[window], &Classes::find());
        let nodes = match sampling {
            Some(sampling) => with_sampling(sampling, build),
            None => build(),
        };
        keep_reusable(&nodes);
        nodes[0].clone()
    }

    /// The window built from scratch (no cache).
    fn full_build(window: Id, sampling: Option<Sampling>) -> Rc<Node> {
        let build = || snapshot(window, &Classes::find(), 0);
        match sampling {
            Some(sampling) => with_sampling(sampling, build),
            None => build(),
        }
    }

    /// The pictures of each coming frame (just now without sampling).
    fn all_frames(node: &Node, sampling: Option<Sampling>) -> Vec<Vec<Picture>> {
        (0..sampling.map_or(1, |s| s.count)).map(|k| flatten_at(node, k)).collect()
    }

    /// Changing one sibling rebuilds only its own part of the tree: the
    /// untouched sibling's node is reused (the same Rc), and the list equals
    /// a full build's.
    #[test]
    fn changing_one_sibling_reuses_the_other_and_matches_a_full_build() {
        setup();
        let window = new_view("UIWindow", rect(0.0, 0.0, 320.0, 480.0));
        let a = new_view("UILabel", rect(10.0, 10.0, 100.0, 30.0));
        let b = new_view("UILabel", rect(10.0, 50.0, 100.0, 30.0));
        runtime::send(a, "setText:", &[crate::fw::string::make("A")]);
        runtime::send(b, "setText:", &[crate::fw::string::make("B")]);
        runtime::send(window, "addSubview:", &[a]);
        runtime::send(window, "addSubview:", &[b]);
        let first = commit_build(window, None);
        let again = commit_build(window, None);
        assert!(Rc::ptr_eq(&first, &again), "with no mark the whole window is reused");
        runtime::send(a, "setText:", &[crate::fw::string::make("A2")]);
        let second = commit_build(window, None);
        assert!(Rc::ptr_eq(&first.children[1], &second.children[1]), "the untouched sibling's node is reused");
        assert!(!Rc::ptr_eq(&first.children[0], &second.children[0]), "the changed sibling is built again");
        assert_eq!(flatten(&second), flatten(&full_build(window, None)), "the list equals a full build");
    }

    /// A shape layer keeps a copy of the dash pattern it was given (Core
    /// Animation's lineDashPattern is `copy`): editing the game's own array
    /// later changes nothing drawn, so a cached build still equals a full one.
    #[test]
    fn editing_a_dash_array_after_setting_it_keeps_the_cache_true() {
        setup();
        use crate::fw::graphics as g;
        let window = new_view("UIWindow", rect(0.0, 0.0, 320.0, 480.0));
        let shape = runtime::send(runtime::class_named("CAShapeLayer"), "layer", &[]);
        send_struct(shape, "setFrame:", rect(0.0, 0.0, 30.0, 10.0));
        let path = g::new_path();
        g::path_move(path, Point { x: 4.0, y: 5.0 });
        g::path_line(path, Point { x: 20.0, y: 5.0 });
        runtime::send(shape, "setPath:", &[path]);
        runtime::send(shape, "setStrokeColor:", &[g::make_color([1.0, 0.0, 0.0, 1.0])]);
        let number = crate::fw::collections::number_float;
        let dashes = runtime::send(crate::fw::collections::array_from(&[number(4.0), number(8.0)]), "mutableCopy", &[]);
        runtime::send(shape, "setLineDashPattern:", &[dashes]);
        runtime::send(runtime::send(window, "layer", &[]), "addSublayer:", &[shape]);
        commit_build(window, None);
        runtime::send(dashes, "replaceObjectAtIndex:withObject:", &[0, number(1.0)]);
        let shown = commit_build(window, None);
        assert_eq!(flatten(&shown), flatten(&full_build(window, None)), "the list equals a full build");
    }

    /// A view moved deeper keeps only what is drawn at its new depth: the
    /// tree stops at 64 levels, so a child built shallow (with its own child)
    /// is not reused there (also for a layer of its own).
    #[test]
    fn a_subtree_moved_near_the_depth_limit_matches_a_full_build() {
        setup();
        let red = || runtime::send(runtime::class_named("UIColor"), "redColor", &[]);
        let window = new_view("UIWindow", rect(0.0, 0.0, 320.0, 480.0));
        let mut deepest = window;
        for _ in 0..62 {
            let next = new_view("UIView", rect(0.0, 0.0, 320.0, 480.0));
            runtime::send(deepest, "addSubview:", &[next]);
            deepest = next;
        }
        let (moved, child, grandchild) = (new_view("UIView", rect(0.0, 0.0, 50.0, 50.0)), new_view("UIView", rect(0.0, 0.0, 40.0, 40.0)), new_view("UIView", rect(0.0, 0.0, 30.0, 30.0)));
        runtime::send(child, "setBackgroundColor:", &[red()]);
        runtime::send(grandchild, "setBackgroundColor:", &[red()]);
        runtime::send(child, "addSubview:", &[grandchild]);
        runtime::send(moved, "addSubview:", &[child]);
        // A layer of its own with a sublayer, beside them.
        let (layer, sublayer) = (runtime::send(runtime::class_named("CALayer"), "layer", &[]), runtime::send(runtime::class_named("CALayer"), "layer", &[]));
        send_struct(sublayer, "setFrame:", rect(0.0, 0.0, 20.0, 20.0));
        let color = |l: Id| runtime::send(l, "setBackgroundColor:", &[runtime::send(red(), "CGColor", &[])]);
        color(sublayer);
        let deep = runtime::send(runtime::class_named("CALayer"), "layer", &[]);
        send_struct(deep, "setFrame:", rect(0.0, 0.0, 10.0, 10.0));
        color(deep);
        runtime::send(sublayer, "addSublayer:", &[deep]);
        runtime::send(layer, "addSublayer:", &[sublayer]);
        runtime::send(runtime::send(moved, "layer", &[]), "addSublayer:", &[layer]);
        runtime::send(window, "addSubview:", &[moved]);
        commit_build(window, None);
        // From depth 1 to 63: its child now at 64 is drawn without its own child.
        runtime::send(deepest, "addSubview:", &[moved]);
        let shown = commit_build(window, None);
        assert_eq!(flatten(&shown), flatten(&full_build(window, None)), "the list equals a full build");
    }

    /// A view changed while it is off the window shows the change once it is
    /// added back.
    #[test]
    fn an_off_screen_change_shows_when_the_view_is_re_added() {
        setup();
        let window = new_view("UIWindow", rect(0.0, 0.0, 320.0, 480.0));
        let holder = new_view("UIView", rect(0.0, 0.0, 320.0, 480.0));
        let label = new_view("UILabel", rect(10.0, 10.0, 100.0, 30.0));
        runtime::send(label, "setText:", &[crate::fw::string::make("old")]);
        runtime::send(holder, "addSubview:", &[label]);
        runtime::send(window, "addSubview:", &[holder]);
        commit_build(window, None);
        runtime::send(holder, "removeFromSuperview", &[]);
        runtime::send(label, "setText:", &[crate::fw::string::make("new")]);
        commit_build(window, None);
        runtime::send(window, "addSubview:", &[holder]);
        let shown = commit_build(window, None);
        let text: Vec<String> = flatten(&shown).iter().filter_map(|p| p.text.as_ref()).map(|t| t.text.clone()).collect();
        assert_eq!(text, ["new"], "the off-screen change shows once added back");
        assert_eq!(flatten(&shown), flatten(&full_build(window, None)), "the list equals a full build");
    }

    /// An animated view under a still parent is built again every commit
    /// (also when its last build had no coming frames to work out), its
    /// still sibling is reused, and every coming frame equals a full build's.
    #[test]
    fn an_animated_child_under_a_static_parent_is_rebuilt_while_the_list_matches_a_full_build() {
        setup();
        let window = new_view("UIWindow", rect(0.0, 0.0, 320.0, 480.0));
        let parent = new_view("UIView", rect(0.0, 0.0, 320.0, 480.0));
        let moving = new_view("UIView", rect(100.0, 0.0, 10.0, 10.0));
        let still = new_view("UILabel", rect(10.0, 300.0, 100.0, 30.0));
        runtime::send(still, "setText:", &[crate::fw::string::make("still")]);
        runtime::send(moving, "setBackgroundColor:", &[runtime::send(runtime::class_named("UIColor"), "redColor", &[])]);
        runtime::send(parent, "addSubview:", &[moving]);
        runtime::send(parent, "addSubview:", &[still]);
        runtime::send(window, "addSubview:", &[parent]);
        let first = commit_build(window, None);
        let animation = runtime::send(runtime::class_named("CABasicAnimation"), "animationWithKeyPath:", &[crate::fw::string::make("position")]);
        runtime::send(animation, "setFromValue:", &[crate::fw::layers::boxed("valueWithCGPoint:", Point { x: 5.0, y: 5.0 })]);
        runtime::send(animation, "setToValue:", &[crate::fw::layers::boxed("valueWithCGPoint:", Point { x: 105.0, y: 5.0 })]);
        send_struct(animation, "setDuration:", 1.0f64);
        let layer = runtime::send(moving, "layer", &[]);
        let start = crate::fw::runloop::media_time();
        send_struct(animation, "setBeginTime:", start);
        runtime::send(layer, "addAnimation:forKey:", &[animation, 0]);
        // A build without coming frames (as before an animation's commit),
        // then one with them.
        let unsampled = commit_build(window, None);
        let sampling = Some(Sampling { start, step: 0.25, count: 5 });
        let second = commit_build(window, sampling);
        assert_eq!(all_frames(&second, sampling), all_frames(&full_build(window, sampling), sampling), "every coming frame equals a full build");
        assert!(!Rc::ptr_eq(&unsampled.children[0], &second.children[0]), "the animated part is built again");
        assert!(Rc::ptr_eq(&first.children[0].children[1], &second.children[0].children[1]), "the still sibling is reused");
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
        let mut panel = view(rect(10.0, 20.0, 300.0, 200.0), Some("panel.png"), vec![Rc::new(view(rect(5.0, 6.0, 50.0, 40.0), Some("button.png"), vec![])), Rc::new(hidden), Rc::new(faint)]);
        panel.bounds_origin = Point { x: 1.0, y: 2.0 };
        let pictures = flatten(&window(vec![Rc::new(view(rect(100.0, 100.0, 960.0, 540.0), None, vec![Rc::new(panel)]))]));
        let names: Vec<&str> = pictures.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(names, ["panel.png", "button.png", "faint.png"]);
        assert_eq!((at(&pictures[0], 0.0, 0.0), pictures[0].area), ((110.0, 120.0), (300.0, 200.0)));
        // Inside the panel, its bounds origin (1, 2) shifts what it holds.
        assert_eq!(at(&pictures[1], 0.0, 0.0), (114.0, 124.0));
        assert_eq!(at(&pictures[1], 50.0, 40.0), (164.0, 164.0));
        assert_eq!(pictures[2].alpha, 0.5);
    }

    /// The old side of a transition shows a GL view's last frame (Core Animation
    /// renders the CAEAGLLayer's contents into it), so what lies behind the GL
    /// view can't show through once the game deleted its framebuffer.
    #[test]
    fn a_transitions_old_side_keeps_the_gl_views_last_frame() {
        let full = rect(0.0, 0.0, 960.0, 540.0);
        let mut gl = view(full, None, vec![]);
        (gl.gl, gl.gl_layer, gl.layer) = (true, 0x100, 0x100);
        let hud = view(rect(10.0, 10.0, 50.0, 50.0), Some("hud.png"), vec![]);
        let mut container = view(full, None, vec![Rc::new(view(full, None, vec![Rc::new(gl), Rc::new(hud)]))]);
        container.layer = 0x20;
        let mut root = window(vec![Rc::new(container)]);
        root.background = Some([1.0, 1.0, 1.0, 1.0]);
        let windows = [Rc::new(root)];
        let old = with_gl_contents(Some((0x100, "frame.png".into())), || layer_pictures(&windows, 0x20)).unwrap();
        assert_eq!(old.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(), ["frame.png", "hud.png"]);
        assert_eq!((at(&old[0], 0.0, 0.0), at(&old[0], 960.0, 540.0), old[0].alpha), ((0.0, 0.0), (960.0, 540.0), 1.0));
        let other = with_gl_contents(Some((0x200, "frame.png".into())), || layer_pictures(&windows, 0x20)).unwrap();
        assert_eq!(other.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(), ["hud.png"]);
    }

    #[test]
    fn a_playing_video_covers_what_is_under_it() {
        let mut video = view(rect(0.0, 0.0, 960.0, 540.0), None, vec![]);
        video.video = true;
        let window = window(vec![Rc::new(view(rect(0.0, 0.0, 960.0, 540.0), Some("splash.png"), vec![])), Rc::new(video), Rc::new(view(rect(700.0, 350.0, 200.0, 80.0), Some("start.png"), vec![]))]);
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
        let pictures = flatten(&window(vec![Rc::new(button)]));
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
        let widths: Vec<(String, f64)> = flatten(&window(vec![Rc::new(narrow)])).into_iter().map(|p| (p.path, p.area.0)).collect();
        assert_eq!(widths.iter().filter(|(path, _)| path.ends_with(&format!("{PART_MARK}1"))).count(), 0, "{widths:?}");
        assert!(widths.iter().all(|&(_, w)| w == 3.0), "{widths:?}");
        // Shown at its own size (centred): whole.
        let mut centred = view(rect(0.0, 0.0, 100.0, 40.0), None, vec![]);
        centred.pictures.push(Shown { path: "b.png".into(), mode: 4, size: (20.0, 10.0), rect: rect(0.0, 0.0, 100.0, 40.0), caps, tile: false, text: None, alpha: 1.0 });
        let paths: Vec<String> = flatten(&window(vec![Rc::new(centred)])).into_iter().map(|p| p.path).collect();
        assert_eq!(paths, ["b.png"]);
    }

    /// A tiling picture repeats its edges and middle at their own size
    /// between the caps, the last copy cut off at the cap.
    #[test]
    fn tiling_pictures_repeat_their_middle() {
        let mut button = view(rect(10.0, 10.0, 100.0, 40.0), None, vec![]);
        let caps = Some([3.0, 4.0, 3.0, 4.0]);
        button.pictures.push(Shown { path: "b.png".into(), mode: 0, size: (20.0, 10.0), rect: rect(0.0, 0.0, 100.0, 40.0), caps, tile: true, text: None, alpha: 1.0 });
        let pictures = flatten(&window(vec![Rc::new(button)]));
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
    /// A CATransition takes only its layer (and what is inside it) from what
    /// it showed on the last screen to what it shows now: the list holds,
    /// where that layer is drawn, a transition line (type, subtype, time,
    /// when it began, its timing function's points, start and end progress,
    /// the layer's place and size), its old pictures, "to", its new pictures
    /// and "end". What is beside it is drawn once, as it is. Whatever key it
    /// was added under, it runs under "transition" (as Apple's addAnimation:forKey:).
    #[test]
    fn a_transition_takes_only_its_layer_from_its_old_pictures_to_its_new() {
        setup();
        let color = |name: &str| runtime::send(runtime::class_named("UIColor"), name, &[]);
        let window = new_view("UIWindow", rect(0.0, 0.0, 320.0, 480.0));
        let (beside, card, inside) = (new_view("UIView", rect(0.0, 0.0, 100.0, 100.0)), new_view("UIView", rect(50.0, 0.0, 100.0, 100.0)), new_view("UIView", rect(10.0, 10.0, 20.0, 20.0)));
        // The card sits in a holder that clips: that clip stays where it is while the card moves.
        let holder = new_view("UIView", rect(50.0, 0.0, 200.0, 200.0));
        runtime::send(holder, "setClipsToBounds:", &[1]);
        runtime::send(beside, "setBackgroundColor:", &[color("redColor")]);
        runtime::send(card, "setBackgroundColor:", &[color("greenColor")]);
        runtime::send(inside, "setBackgroundColor:", &[color("blueColor")]);
        runtime::send(window, "addSubview:", &[beside]);
        runtime::send(window, "addSubview:", &[holder]);
        runtime::send(holder, "addSubview:", &[card]);
        runtime::send(card, "addSubview:", &[inside]);
        let nodes = || vec![snapshot(window, &Classes::find(), 0)];
        screen_seen(nodes());
        let transition = runtime::send(runtime::class_named("CATransition"), "animation", &[]);
        runtime::send(transition, "setType:", &[crate::fw::string::make("push")]);
        runtime::send(transition, "setSubtype:", &[crate::fw::string::make("fromRight")]);
        send_struct(transition, "setDuration:", 0.5f64);
        let curve = runtime::send(runtime::class_named("CAMediaTimingFunction"), "functionWithName:", &[crate::fw::string::make("easeInEaseOut")]);
        runtime::send(transition, "setTimingFunction:", &[curve]);
        let layer = runtime::send(card, "layer", &[]);
        runtime::send(layer, "addAnimation:forKey:", &[transition, crate::fw::string::make("x")]);
        let keys = crate::fw::collections::array_items(runtime::send(layer, "animationKeys", &[]));
        assert_eq!(keys.into_iter().map(crate::fw::string::get).collect::<Vec<_>>(), ["transition"]);
        runtime::send(inside, "setBackgroundColor:", &[color("yellowColor")]);
        pick_transitions();
        let text = encode((320.0, 480.0), &frames(&nodes(), 1).remove(0));
        runtime::send(layer, "removeAllAnimations", &[]);
        pick_transitions();
        let lines: Vec<&str> = text.lines().skip(1).collect();
        let shown: Vec<String> = lines.iter().map(|line| if line.starts_with('!') { line.split('\t').take(4).collect::<Vec<_>>().join(" ") } else { line.rsplit('\t').next().unwrap().to_string() }).collect();
        let [red, green, blue, yellow] = [[1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0], [1.0, 1.0, 0.0, 1.0]].map(color_path);
        assert_eq!(shown, [red, "! transition push fromRight".into(), green.clone(), blue, "! to".into(), green, yellow, "! end".into()], "{text}");
        let fields: Vec<&str> = lines[1].split('\t').collect();
        assert_eq!(fields[4], "500", "{text}");
        assert_eq!(
            fields[6..],
            ["0.42", "0", "0.58", "1", "0", "1", "1", "0", "0", "1", "100", "0", "100", "100", "50", "0", "200", "200"],
            "curve, progress, the card's place and size, the clip around it that doesn't move"
        );
    }

    /// A transition inside a group drawn as one picture (here a rasterized,
    /// faded parent) still runs: the group is drawn once as it was and once
    /// as it is, and the two dissolve along the transition's timing (a slide
    /// or flip of one layer can't be drawn inside the group's one picture).
    #[test]
    fn a_transition_inside_a_group_dissolves_the_groups_old_and_new_pictures() {
        setup();
        let _files = TEXT_FILES.lock().unwrap_or_else(|e| e.into_inner());
        let rgba = crate::fw::graphics::make_color;
        let window = new_view("UIWindow", rect(0.0, 0.0, 200.0, 200.0));
        let group = new_layer("CALayer", rect(10.0, 10.0, 100.0, 100.0));
        runtime::send(group, "setShouldRasterize:", &[1]);
        send_struct(group, "setOpacity:", 0.5f32);
        let card = new_layer("CALayer", rect(10.0, 10.0, 50.0, 50.0));
        runtime::send(card, "setBackgroundColor:", &[rgba([1.0, 0.0, 0.0, 1.0])]);
        runtime::send(group, "addSublayer:", &[card]);
        runtime::send(runtime::send(window, "layer", &[]), "addSublayer:", &[group]);
        let nodes = || vec![snapshot(window, &Classes::find(), 0)];
        screen_seen(nodes());
        let transition = runtime::send(runtime::class_named("CATransition"), "animation", &[]);
        runtime::send(transition, "setType:", &[crate::fw::string::make("push")]);
        send_struct(transition, "setDuration:", 0.5f64);
        runtime::send(card, "addAnimation:forKey:", &[transition, 0]);
        runtime::send(card, "setBackgroundColor:", &[rgba([0.0, 0.0, 1.0, 1.0])]);
        pick_transitions();
        let pictures = compose_groups(resolve(frames(&nodes(), 1).remove(0)));
        runtime::send(card, "removeAllAnimations", &[]);
        pick_transitions();
        let text = encode((200.0, 200.0), &pictures);
        let lines: Vec<&str> = text.lines().skip(1).collect();
        let shown: Vec<String> = lines.iter().map(|line| if line.starts_with('!') { line.split('\t').take(4).collect::<Vec<_>>().join(" ") } else { "picture".into() }).collect();
        assert_eq!(shown, ["! transition fade ", "picture", "! to", "picture", "! end"], "{text}");
        assert_ne!(lines[1].rsplit('\t').next(), lines[3].rsplit('\t').next(), "the group as it was and as it is");
        assert_eq!(lines[0].split('\t').nth(4), Some("500"), "the transition's own time");
    }

    /// As UIKit's +[UIViewAnimationState popAnimationState] (0x48e22, its
    /// table at 0x48e76): a view transition is a CATransition on the view's
    /// layer, type and subtype by the transition (1 flip from left, 2 from
    /// right, 3 curl up "pageCurl", 4 curl down "pageUnCurl", 5 "fade", 6 and
    /// 7 flips from top and bottom; flips are "oglFlip"), with the block's
    /// duration and curve (ease-in-out unless one is picked), filling both
    /// ways. transitionWithView: and an iOS 3 block's setAnimationTransition: alike.
    #[test]
    fn view_transitions_are_uikits_catransitions_on_the_view() {
        setup();
        let view = new_view("UIView", rect(0.0, 0.0, 100.0, 100.0));
        let layer = runtime::send(view, "layer", &[]);
        let ui_view = runtime::class_named("UIView");
        let shown = || {
            let transition = runtime::send(layer, "animationForKey:", &[crate::fw::string::make("transition")]);
            assert_ne!(transition, 0, "a transition on the view's layer");
            let text = |name: &str| match runtime::send(transition, name, &[]) {
                0 => String::new(),
                value => crate::fw::string::get(value),
            };
            let duration = crate::fw::uikit::property_get(transition, "duration").map(f64::from_bits);
            let curve = crate::fw::animation::curve_of(runtime::send(transition, "timingFunction", &[])).map(|c| c.0);
            runtime::send(layer, "removeAllAnimations", &[]);
            (text("type"), text("subtype"), duration, curve, text("fillMode"))
        };
        let s = runtime::sel("transitionWithView:duration:options:animations:completion:");
        let transition_with: extern "C" fn(Id, Sel, Id, f64, usize, usize, usize) = unsafe { std::mem::transmute(runtime::imp_for(ui_view, s).unwrap()) };
        transition_with(ui_view, s, view, 0.3, 6 << 20, 0, 0);
        assert_eq!(shown(), ("oglFlip".into(), "fromTop".into(), Some(0.3), Some([0.42, 0.0, 0.58, 1.0]), "both".into()));
        transition_with(ui_view, s, view, 0.5, 5 << 20 | 1 << 8 | 3 << 16, 0, 0);
        assert_eq!(shown(), ("fade".into(), String::new(), Some(0.5), Some([0.0, 0.0, 1.0, 1.0]), "both".into()), "a cross-dissolve, linear");
        runtime::send(ui_view, "beginAnimations:context:", &[0, 0]);
        send_struct(ui_view, "setAnimationDuration:", 0.4f64);
        runtime::send(ui_view, "setAnimationTransition:forView:cache:", &[3, view, 1]);
        runtime::send(ui_view, "commitAnimations", &[]);
        assert_eq!(shown(), ("pageCurl".into(), String::new(), Some(0.4), Some([0.42, 0.0, 0.58, 1.0]), "both".into()), "an iOS 3 block's curl up");
    }

    /// A view resized in place (setBounds: keeps its centre) grows around
    /// its anchor on the way, as Core Animation draws a layer's bounds about
    /// its position.
    #[test]
    fn a_view_growing_in_place_grows_around_its_middle() {
        setup();
        let view = new_view("UIView", rect(0.0, 0.0, 100.0, 100.0));
        crate::fw::uikit::send_rect(view, "setBounds:", rect(0.0, 0.0, 200.0, 100.0));
        let animation = runtime::send(runtime::class_named("CABasicAnimation"), "animationWithKeyPath:", &[crate::fw::string::make("bounds")]);
        runtime::send(animation, "setFromValue:", &[crate::fw::layers::boxed("valueWithCGRect:", rect(0.0, 0.0, 100.0, 100.0))]);
        send_struct(animation, "setDuration:", 1.0f64);
        let start = crate::fw::runloop::media_time();
        send_struct(animation, "setBeginTime:", start);
        runtime::send(runtime::send(view, "layer", &[]), "addAnimation:forKey:", &[animation, 0]);
        let sampling = Sampling { start: start + 0.5, step: 0.25, count: 1 };
        let node = with_sampling(sampling, || snapshot(view, &Classes::find(), 0));
        assert!((node.frame.size.width - 150.0).abs() < 1e-6 && (node.frame.origin.x + 25.0).abs() < 1e-6, "half grown about x = 50: {:?}", node.frame);
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
        let frames = frames(&[Rc::new(window(vec![node]))], sampling.count);
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

    /// A label whose class draws its own text (SpellCraft's price labels
    /// draw a coin, then [super drawTextInRect:]) is drawn by the display
    /// pass into its layer, coin and text, and has no separate text
    /// picture (the text would show twice). Plain labels keep the picture.
    #[test]
    fn labels_that_draw_their_own_text_draw_into_their_layer() {
        setup();
        extern "C" fn draw_text(this: Id, s: Sel, area: Rect) {
            crate::fw::graphics::test_fill(rect(area.origin.x, area.origin.y, 2.0, 2.0), [1.0, 0.0, 0.0, 1.0]);
            let plain = new_view("UILabel", rect(0.0, 0.0, 1.0, 1.0));
            let imp = runtime::imp_for(plain, s).expect("UILabel's own drawTextInRect:");
            let own: extern "C" fn(Id, Sel, Rect) = unsafe { std::mem::transmute(imp) };
            own(this, s, area);
        }
        let class = crate::fw::object::define_class("G32CoinLabel", "UILabel", 16);
        crate::fw::object::methods(class, &[("drawTextInRect:", draw_text as usize)]);
        let white = runtime::send(runtime::class_named("UIColor"), "whiteColor", &[]);
        let make = |class: &str| {
            let label = new_view(class, rect(0.0, 0.0, 40.0, 10.0));
            runtime::send(label, "setText:", &[crate::fw::string::make("30")]);
            runtime::send(label, "setTextColor:", &[white]);
            label
        };
        let (coin, plain) = (make("G32CoinLabel"), make("UILabel"));
        crate::fw::uikit::display_tree(coin);
        crate::fw::uikit::display_tree(plain);
        let contents = crate::fw::layers::contents(runtime::send(coin, "layer", &[]));
        assert_ne!(contents, 0, "the coin label's drawing is its layer's contents");
        let pixels = crate::fw::graphics::straight_pixels(contents).unwrap();
        assert_eq!(pixels[..4], [255, 0, 0, 255], "the coin");
        let width = crate::fw::graphics::cg_size(contents).unwrap().0;
        let text = pixels.chunks(4).enumerate().filter(|(i, p)| i % width > width / 4 && p[3] > 0).count();
        assert!(text > 0, "the text is drawn after the coin");
        assert!(snapshot(coin, &Classes::find(), 0).pictures.iter().all(|p| p.text.is_none()), "no second text picture");
        assert_eq!(crate::fw::layers::contents(runtime::send(plain, "layer", &[])), 0, "a plain label isn't drawn");
        assert!(snapshot(plain, &Classes::find(), 0).pictures.iter().any(|p| p.text.is_some()));
    }

    /// The list's numbers: whole numbers plainly, others to six decimals
    /// (a millionth of a point, far under a pixel) with trailing zeros cut,
    /// so the phone reads the same places. A list goes out each refresh while
    /// a finger scrolls, so writing them must be quick.
    #[test]
    fn list_numbers_are_short() {
        let picture = Picture {
            path: "/p.png".into(),
            place: Transform { a: 1.0, b: -0.0, c: 1.0 / 3.0, d: 0.5, tx: -12.0, ty: 1234567.25 },
            area: (100.0, 2.0 / 3.0),
            alpha: 0.1,
            mode: 2,
            size: (1e-9, -0.0000004),
            text: None,
            clip: rect(0.0, 0.0, 568.0, 320.0),
            group: None,
            corner: None,
            perspective: None,
        };
        let mut text = String::new();
        encode_lines(&mut text, &[picture]);
        assert_eq!(text, "1\t0\t0.333333\t0.5\t-12\t1234567.25\t100\t0.666667\t0.1\t2\t0\t0\t0\t0\t568\t320\t/p.png\n");
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

    /// What a button's parts show once it is laid out: the pictures of its
    /// shown subviews, back to front, placed in the button's own points.
    fn button_pictures(button: Id) -> Vec<Shown> {
        runtime::send(button, "layoutIfNeeded", &[]);
        let node = snapshot(button, &Classes::find(), 0);
        let mut found = node.pictures.clone();
        for part in node.children.iter().filter(|part| !part.hidden) {
            let at = part.frame.origin;
            found.extend(part.pictures.iter().map(|p| Shown { rect: Rect { origin: Point { x: p.rect.origin.x + at.x, y: p.rect.origin.y + at.y }, ..p.rect }, ..p.clone() }));
        }
        found
    }

    /// A UIButton shows its state's background picture over its bounds
    /// (stretchable ones keep their caps), then its picture and its title
    /// side by side in the middle of its content (its bounds less the
    /// content insets), each moved by its own insets; the title in the
    /// state's colour and shadow, the title label's font, on one line.
    /// They are its subviews: the title label hidden by the game isn't shown.
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
        let shown = || button_pictures(button);
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
        // The game hides the title label: no title shown.
        runtime::send(runtime::send(button, "titleLabel", &[]), "setHidden:", &[1]);
        assert_eq!(shown().len(), 2);
        // No title and no pictures: nothing.
        let plain = new_view("UIButton", rect(0.0, 0.0, 100.0, 40.0));
        assert!(button_pictures(plain).is_empty());
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
        let found = button_pictures(button);
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
        let image_rect = || button_pictures(button)[0].rect;
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
        let shown = || snapshot(bar, &Classes::find(), 0).pictures.iter().map(|p| (p.path.clone(), p.rect)).collect::<Vec<_>>();
        let track = color_path(crate::fw::progress::TRACK);
        assert_eq!(shown(), [(track.clone(), rect(0.0, 0.0, 100.0, 9.0)), (color_path([1.0, 0.0, 0.0, 1.0]), rect(0.0, 0.0, 25.0, 9.0))]);
        send_struct(bar, "setProgress:", 3.0f32);
        assert_eq!(shown()[1].1, rect(0.0, 0.0, 100.0, 9.0));
        send_struct(bar, "setProgress:", 0.0f32);
        assert_eq!(shown(), [(track, rect(0.0, 0.0, 100.0, 9.0))]);
    }

    /// A shape layer strokes with its own lineCap, lineJoin, miterLimit,
    /// lineDashPattern and lineDashPhase (as -[CAShapeLayer
    /// _renderForegroundInContext:] sets them), on the screen and in a
    /// picture of it (renderInContext:).
    #[test]
    fn shape_layers_stroke_with_their_caps_joins_and_dashes() {
        setup();
        use crate::fw::graphics as g;
        let shape = runtime::send(runtime::class_named("CAShapeLayer"), "layer", &[]);
        send_struct(shape, "setFrame:", rect(0.0, 0.0, 30.0, 10.0));
        let path = g::new_path();
        g::path_move(path, Point { x: 4.0, y: 5.0 });
        g::path_line(path, Point { x: 20.0, y: 5.0 });
        runtime::send(shape, "setPath:", &[path]);
        runtime::send(shape, "setFillColor:", &[0]);
        runtime::send(shape, "setStrokeColor:", &[g::make_color([1.0, 0.0, 0.0, 1.0])]);
        send_struct(shape, "setLineWidth:", 4.0f64);
        runtime::send(shape, "setLineCap:", &[crate::fw::string::make("round")]);
        // On 4, off 8 from x 4; each dash gets round ends 2 long.
        let dashes: Vec<Id> = [4.0, 8.0].iter().map(|&l| crate::fw::collections::number_float(l)).collect();
        runtime::send(shape, "setLineDashPattern:", &[crate::fw::collections::array_from(&dashes)]);
        // The alpha at layer points, on the screen's picture and in a snapshot.
        let screen = || {
            let shown = layer_content(shape, rect(0.0, 0.0, 30.0, 10.0), &Classes::find(), 0.0).pop().expect("a shape picture");
            let file = drawn_file(&shown.path, 1.0).flatten().expect("drawn");
            let pixels = image::open(file).expect("a picture").to_rgba8();
            move |x: f64, y: f64| pixels.get_pixel((x - shown.rect.origin.x) as u32, (y - shown.rect.origin.y) as u32)[3]
        };
        let snapshot = || {
            let context = g::test_begin_context(30.0, 10.0);
            runtime::send(shape, "renderInContext:", &[context]);
            let pixels = g::test_end_context();
            move |x: f64, y: f64| pixels[(y as usize * 30 + x as usize) * 4 + 3]
        };
        for (what, at) in [("screen", Box::new(screen()) as Box<dyn Fn(f64, f64) -> u8>), ("snapshot", Box::new(snapshot()))] {
            assert!(at(3.5, 5.5) > 128, "{what}: a round end past the start");
            assert_eq!((at(6.5, 5.5), at(12.5, 5.5)), (255, 0), "{what}: on, then off");
        }
        send_struct(shape, "setLineDashPhase:", 4.0f64);
        for (what, at) in [("screen", Box::new(screen()) as Box<dyn Fn(f64, f64) -> u8>), ("snapshot", Box::new(snapshot()))] {
            assert_eq!((at(6.5, 5.5), at(12.5, 5.5)), (0, 255), "{what}: the dash moved by its phase");
        }
    }

    /// An image view's animationImages show one after another while it
    /// animates (Apple: a discrete key frame animation of their pictures on
    /// its layer's contents, animationDuration long, repeating for ever), then
    /// its image again once stopped.
    #[test]
    fn image_views_show_their_animation_pictures_in_turn() {
        setup();
        let shown = new_view("UIImageView", rect(0.0, 0.0, 4.0, 4.0));
        let (still, first, second) = (white_picture(4, 4), white_picture(4, 4), white_picture(4, 4));
        runtime::send(shown, "setImage:", &[still]);
        runtime::send(shown, "setAnimationImages:", &[crate::fw::collections::array_from(&[first, second])]);
        send_struct(shown, "setAnimationDuration:", 2.0f64);
        assert_eq!(runtime::send(shown, "isAnimating", &[]) & 0xFF, 0);
        runtime::send(shown, "startAnimating", &[]);
        crate::fw::animation::commit();
        assert_eq!(runtime::send(shown, "isAnimating", &[]) & 0xFF, 1);
        let start = crate::fw::runloop::media_time();
        let at = |t: f64| {
            let node = with_sampling(Sampling { start: start + t, step: 0.1, count: 1 }, || snapshot(shown, &Classes::find(), 0));
            node.pictures.iter().map(|p| p.path.clone()).collect::<Vec<_>>()
        };
        let path = |image: Id| format!("{IMAGE_PREFIX}{image:x}");
        assert_eq!(at(0.5), [path(first)], "the first picture for its half");
        assert_eq!(at(1.5), [path(second)], "then the second");
        assert_eq!(at(2.5), [path(first)], "and round again");
        // The coming frames worked out ahead (while the game thread is busy)
        // step through the pictures too.
        let window_node = with_sampling(Sampling { start: start + 0.25, step: 0.5, count: 4 }, || window(vec![snapshot(shown, &Classes::find(), 0)]));
        let coming: Vec<String> = frames(&[Rc::new(window_node)], 4).into_iter().map(|pictures| pictures.last().unwrap().path.clone()).collect();
        assert_eq!(coming, [path(first), path(first), path(second), path(second)], "frames at 0.25, 0.75, 1.25 and 1.75 seconds");
        runtime::send(shown, "stopAnimating", &[]);
        assert_eq!(runtime::send(shown, "isAnimating", &[]) & 0xFF, 0);
        assert_eq!(at(0.5), [path(still)], "its image once stopped");
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
    /// are drawn darker too, still solid (adjustsImageWhenDisabled; Apple's
    /// docs: "drawn darker"). The title stays as it is.
    #[test]
    fn pressed_and_disabled_buttons_darken() {
        setup();
        let (background, icon) = (white_picture(4, 4), white_picture(2, 2));
        let button = new_view("UIButton", rect(0.0, 0.0, 100.0, 40.0));
        runtime::send(button, "setBackgroundImage:forState:", &[background, 0]);
        runtime::send(button, "setImage:forState:", &[icon, 0]);
        runtime::send(button, "setTitle:forState:", &[crate::fw::string::make("Go"), 0]);
        let shown = || button_pictures(button);
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
        // Disabled: the same darker pictures, solid; the title as it is.
        runtime::send(button, "setEnabled:", &[0]);
        let disabled = shown();
        assert_eq!((image_of(&disabled[0]), image_of(&disabled[1])), (dark_background, dark_icon), "darker copies");
        assert!(disabled.iter().all(|p| p.alpha == 1.0), "solid: {disabled:?}");
        runtime::send(button, "setAdjustsImageWhenDisabled:", &[0]);
        let plain_again = shown();
        assert_eq!((image_of(&plain_again[0]), image_of(&plain_again[1])), (background, icon));
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
        let drawn = || {
            runtime::send(button, "layoutIfNeeded", &[]);
            resolve(flatten(&window(vec![snapshot(button, &Classes::find(), 0)]))).len()
        };
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

    /// A layer's sublayerTransform moves and turns what is inside it about
    /// its anchor point, in 3D before flattening: with a perspective there
    /// (as card flips set it), a sublayer turned about y is drawn in
    /// perspective.
    #[test]
    fn sublayer_transforms_move_and_turn_what_is_inside() {
        setup();
        let holder = new_view("UIView", rect(0.0, 0.0, 100.0, 100.0));
        let inner = new_view("UIView", rect(0.0, 0.0, 10.0, 10.0));
        runtime::send(inner, "setBackgroundColor:", &[runtime::send(runtime::class_named("UIColor"), "redColor", &[])]);
        runtime::send(holder, "addSubview:", &[inner]);
        let layer = runtime::send(holder, "layer", &[]);
        send_struct(layer, "setSublayerTransform:", crate::fw::layers::affine_to_3d(Transform { tx: 5.0, ..IDENTITY }));
        let pictures = flatten(&window(vec![snapshot(holder, &Classes::find(), 0)]));
        assert_eq!(at(&pictures[0], 0.0, 0.0), (5.0, 0.0), "moved by the sublayerTransform");
        // A perspective (m34) on the holder, the sublayer turned about y.
        let mut perspective = IDENTITY_3D;
        perspective.0[11] = -1.0 / 500.0;
        send_struct(layer, "setSublayerTransform:", perspective);
        let (c, s) = (0.5f64.cos(), 0.5f64.sin());
        let mut turn = IDENTITY_3D;
        (turn.0[0], turn.0[2], turn.0[8], turn.0[10]) = (c, -s, s, c);
        send_struct(runtime::send(inner, "layer", &[]), "setTransform:", turn);
        let pictures = flatten(&window(vec![snapshot(holder, &Classes::find(), 0)]));
        assert!(pictures[0].perspective.is_some(), "drawn in perspective: {:?}", pictures[0]);
    }

    #[test]
    fn turned_and_scaled_views_keep_their_centre() {
        // An upright 320x480 launch picture turned a quarter so it fills a 480x320 screen.
        let mut splash = view(rect(80.0, -80.0, 320.0, 480.0), Some("Default.png"), vec![]);
        splash.transform = Some(crate::fw::layers::affine_to_3d(Transform { a: 0.0, b: 1.0, c: -1.0, d: 0.0, tx: 0.0, ty: 0.0 }));
        let mut grown = view(rect(100.0, 100.0, 20.0, 10.0), Some("grown.png"), vec![]);
        grown.transform = Some(crate::fw::layers::affine_to_3d(Transform { a: 2.0, b: 0.0, c: 0.0, d: 2.0, tx: 5.0, ty: 0.0 }));
        let pictures = flatten(&window(vec![Rc::new(splash), Rc::new(grown)]));
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
        let mut panel = view(rect(10.0, 20.0, 100.0, 50.0), Some("icon.png"), vec![Rc::new(view(rect(0.0, 0.0, 10.0, 10.0), Some("child.png"), vec![]))]);
        panel.background = Some([1.0, 0.0, 0.0, 0.5]);
        let mut clear = view(rect(0.0, 0.0, 50.0, 50.0), None, vec![]);
        clear.background = Some([0.0, 0.0, 0.0, 0.0]);
        let pictures = flatten(&window(vec![Rc::new(panel), Rc::new(clear)]));
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
        let mut gl = view(full, None, vec![Rc::new(view(rect(5.0, 5.0, 20.0, 20.0), Some("hud.png"), vec![]))]);
        gl.gl = true;
        gl.background = Some([0.0, 0.0, 0.0, 1.0]);
        let front = view(rect(0.0, 0.0, 30.0, 30.0), Some("front.png"), vec![]);
        let paths: Vec<String> = flatten(&window(vec![Rc::new(behind), Rc::new(gl), Rc::new(front)])).into_iter().map(|p| p.path).collect();
        assert_eq!(paths, ["hud.png", "front.png"]);
    }

    /// A GL view covers only its own box: a picture beside it stays.
    #[test]
    fn a_gl_view_covers_only_what_is_under_it() {
        let beside = view(rect(500.0, 0.0, 100.0, 100.0), Some("beside.png"), vec![]);
        let under = view(rect(10.0, 10.0, 50.0, 50.0), Some("under.png"), vec![]);
        let mut gl = view(rect(0.0, 0.0, 400.0, 300.0), None, vec![]);
        gl.gl = true;
        let paths: Vec<String> = flatten(&window(vec![Rc::new(beside), Rc::new(under), Rc::new(gl)])).into_iter().map(|p| p.path).collect();
        assert_eq!(paths, ["beside.png"]);
        // A picture partly under it keeps the rest: cut to the bands around it.
        let background = view(rect(0.0, 0.0, 600.0, 300.0), Some("background.png"), vec![]);
        let mut video = view(rect(200.0, 100.0, 100.0, 100.0), None, vec![]);
        video.video = true;
        let clips: Vec<(String, Rect)> = flatten(&window(vec![Rc::new(background), Rc::new(video)])).into_iter().map(|p| (p.path, p.clip)).collect();
        let band = |x, y, w, h| ("background.png".to_string(), rect(x, y, w, h));
        assert_eq!(clips, [band(0.0, 0.0, 600.0, 100.0), band(0.0, 200.0, 600.0, 100.0), band(0.0, 100.0, 200.0, 100.0), band(300.0, 100.0, 300.0, 100.0)]);
    }

    /// The list says how much of the window's GL picture shows: only while
    /// the layer that drew it is shown (at its opacity); nothing once it
    /// left the window or another GL layer that has drawn nothing yet shows.
    #[test]
    fn the_gl_picture_shows_only_while_its_layer_is_shown() {
        let gl_view = |layer: Id, alpha: f64| {
            let mut v = view(rect(0.0, 0.0, 960.0, 540.0), None, vec![]);
            (v.gl, v.gl_layer, v.alpha) = (true, layer, alpha);
            v
        };
        let line = |nodes: &[Rc<Node>], frame: Option<Id>| gl_line("960\t540\nx.png", gl_shown(nodes, frame, false));
        assert_eq!(line(&[Rc::new(window(vec![Rc::new(gl_view(0x100, 1.0))]))], Some(0x100)), "960\t540\n!\tgl\t1\nx.png");
        // A video showing (the phone's player sits under the pictures, as the
        // GL picture does): nothing is covered, even with the GL view gone
        // (a movie after a full-screen screen took the GL view out).
        assert_eq!(gl_line("960\t540", gl_shown(&[Rc::new(window(vec![]))], Some(0x100), true)), "960\t540\n!\tgl\t1\n");
        assert_eq!(line(&[Rc::new(window(vec![Rc::new(gl_view(0x100, 0.5))]))], Some(0x100)), "960\t540\n!\tgl\t0.5\nx.png");
        // The view left the window: the old frame is gone.
        assert_eq!(line(&[Rc::new(window(vec![]))], Some(0x100)), "960\t540\n!\tgl\toff\nx.png");
        // A new GL view before its first frame: not the old one's frame.
        assert_eq!(line(&[Rc::new(window(vec![Rc::new(gl_view(0x200, 1.0))]))], Some(0x100)), "960\t540\n!\tgl\toff\nx.png");
        // A frame not known by a view's layer shows as it is.
        assert_eq!(line(&[Rc::new(window(vec![]))], None), "960\t540\n!\tgl\t1\nx.png");
    }

    /// A GL view whose alpha animates fades: each coming frame of the list
    /// carries how much of the GL picture shows then (after its "@" line),
    /// not only the first frame's amount.
    #[test]
    fn a_fading_gl_view_fades_its_picture_frame_by_frame() {
        let pose = |alpha: f64| Pose { origin: Point::default(), transform: IDENTITY_3D, alpha, background: None, bounds: rect(0.0, 0.0, 960.0, 540.0) };
        let mut gl = view(rect(0.0, 0.0, 960.0, 540.0), None, vec![]);
        (gl.gl, gl.gl_layer, gl.alpha) = (true, 0x100, 1.0);
        gl.motion = vec![pose(0.5), pose(0.0)];
        let nodes = [Rc::new(window(vec![Rc::new(gl)]))];
        let amounts = gl_amounts(&nodes, Some(0x100), false, 3);
        assert_eq!(amounts, [1.0, 0.5, 0.0]);
        let timed: Vec<(f64, Vec<Picture>)> = vec![(1.0, vec![]), (1.5, vec![]), (2.0, vec![])];
        let text = frame_gl_lines(&gl_line(&encode_frames((960.0, 540.0), &timed), amounts[0]), &amounts);
        assert_eq!(text, "960\t540\t1000000000\n!\tgl\t1\n@\t1500000000\n!\tgl\t0.5\n@\t2000000000\n!\tgl\toff\n");
        // A GL picture that stays as it is: no lines for the coming frames.
        let still = [1.0, 1.0, 1.0];
        assert_eq!(frame_gl_lines(&gl_line(&encode_frames((960.0, 540.0), &timed), 1.0), &still), "960\t540\t1000000000\n!\tgl\t1\n@\t1500000000\n@\t2000000000\n");
    }

    /// A view that clips to its bounds keeps what it holds inside them: each
    /// picture carries the screen area it may draw in (nested clips meet),
    /// and one wholly outside is left out. Others may use the whole screen.
    #[test]
    fn clipping_views_keep_what_they_hold_inside() {
        let inner = view(rect(20.0, 20.0, 100.0, 100.0), Some("inner.png"), vec![]);
        let mut small = view(rect(30.0, -10.0, 40.0, 40.0), None, vec![Rc::new(view(rect(0.0, 0.0, 100.0, 100.0), Some("deep.png"), vec![]))]);
        small.clips = true;
        let away = view(rect(200.0, 200.0, 10.0, 10.0), Some("away.png"), vec![]);
        let mut clipping = view(rect(100.0, 100.0, 50.0, 50.0), Some("box.png"), vec![Rc::new(inner), Rc::new(small), Rc::new(away)]);
        clipping.clips = true;
        let free = view(rect(0.0, 0.0, 10.0, 10.0), Some("free.png"), vec![]);
        let pictures = flatten(&window(vec![Rc::new(clipping), Rc::new(free)]));
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
        let small = Node { alpha: 1.0, frame: rect(0.0, 0.0, 320.0, 480.0), children: vec![Rc::new(view(rect(0.0, 0.0, 568.0, 320.0), Some("menu.png"), vec![]))], ..Default::default() };
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
        // A shadow over a round layer: the round layer's corners still cut.
        let shadowed_round = new_layer("CALayer", rect(100.0, 100.0, 40.0, 40.0));
        send_struct(shadowed_round, "setShadowOpacity:", 1.0f32);
        send_struct(shadowed_round, "setShadowRadius:", 0.0f64);
        send_struct(shadowed_round, "setShadowOffset:", Size { width: 50.0, height: 50.0 });
        let round_inside = new_layer("CALayer", rect(0.0, 0.0, 40.0, 40.0));
        runtime::send(round_inside, "setBackgroundColor:", &[rgba(blue)]);
        send_struct(round_inside, "setCornerRadius:", 20.0f64);
        runtime::send(round_inside, "setMasksToBounds:", &[1]);
        runtime::send(shadowed_round, "addSublayer:", &[round_inside]);
        // A shadowed layer inside a round one: its shadow comes from all of it (the round corner cuts afterwards).
        let round_parent = new_layer("CALayer", rect(150.0, 100.0, 40.0, 40.0));
        send_struct(round_parent, "setCornerRadius:", 20.0f64);
        runtime::send(round_parent, "setMasksToBounds:", &[1]);
        let corner_child = new_layer("CALayer", rect(0.0, 0.0, 4.0, 4.0));
        runtime::send(corner_child, "setBackgroundColor:", &[rgba(red)]);
        send_struct(corner_child, "setShadowOpacity:", 1.0f32);
        send_struct(corner_child, "setShadowRadius:", 0.0f64);
        send_struct(corner_child, "setShadowOffset:", Size { width: 18.0, height: 18.0 });
        runtime::send(round_parent, "addSublayer:", &[corner_child]);
        for layer in [round, masked, shadowed, shadowed_round, round_parent] {
            runtime::send(host, "addSublayer:", &[layer]);
        }
        let pictures = screen_pictures(&[window], &Classes::find());
        assert!(pictures.iter().all(|p| !p.path.starts_with('\u{1}')), "every picture has its file: {pictures:?}");
        // Round corners alone are cut where the pictures are shown (as Core
        // Animation does on the GPU), not drawn into one new picture: the
        // sublayer inside the round layer is still its own picture.
        assert!(pictures.iter().any(|p| p.area == (60.0, 60.0)), "the round layer's sublayer is shown as it is: {pictures:?}");
        // The colour of the topmost picture not clear at a screen point.
        let seen = |x: f64, y: f64| -> [u8; 4] {
            for p in pictures.iter().rev() {
                if !p.shows_at(x, y) {
                    continue;
                }
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
        assert_eq!(seen(120.0, 120.0), [0, 0, 255, 255], "the round layer under a shadow");
        assert_eq!(seen(103.0, 103.0), [0; 4], "its corner cut, in the shadowed layer's picture too");
        assert_eq!(seen(170.0, 120.0), [0, 0, 0, 255], "the shadow of a layer in a round layer's corner");
    }

    /// A layer turned in 3D with perspective (m34) is drawn with it: its line
    /// carries the projective part; turned away with doubleSided off, it
    /// isn't drawn.
    #[test]
    fn layers_turned_in_3d_keep_their_perspective_and_back_faces_hide() {
        setup();
        let parent = new_view("UIView", rect(0.0, 0.0, 300.0, 300.0));
        let red = runtime::send(runtime::class_named("UIColor"), "redColor", &[]);
        let (near, away) = (new_view("UIView", rect(100.0, 100.0, 100.0, 100.0)), new_view("UIView", rect(0.0, 0.0, 50.0, 50.0)));
        for card in [near, away] {
            runtime::send(card, "setBackgroundColor:", &[red]);
            runtime::send(parent, "addSubview:", &[card]);
        }
        let turned = |degrees: f64| {
            let (s, c) = degrees.to_radians().sin_cos();
            let mut m = crate::fw::geometry::IDENTITY_3D;
            (m.0[0], m.0[2], m.0[8], m.0[10]) = (c, -s, s, c);
            m.0[3] = s * 0.002; // then m34 = -1/500, as the game's CATransform3DRotate after it
            m.0[11] = -0.002 * c;
            m
        };
        send_struct(runtime::send(near, "layer", &[]), "setTransform:", turned(45.0));
        let away_layer = runtime::send(away, "layer", &[]);
        send_struct(away_layer, "setTransform:", turned(180.0));
        runtime::send(away_layer, "setDoubleSided:", &[0]);
        let pictures = flatten(&window(vec![snapshot(parent, &Classes::find(), 0)]));
        assert_eq!(pictures.len(), 1, "the card turned away is hidden: {pictures:?}");
        let text = encode((300.0, 300.0), &pictures);
        let fields: Vec<&str> = text.lines().nth(1).unwrap().split('\t').collect();
        assert_eq!(fields.len(), 20, "{text}");
        assert!(fields[16].parse::<f64>().unwrap() != 0.0, "{text}");
    }

    /// doubleSided NO hides only the layer's own contents, and only when its
    /// plane faces away (its normal's z): a mirrored layer still shows, and
    /// the sublayers of one turned away still draw (theirs default to YES).
    #[test]
    fn one_sided_layers_hide_only_their_own_contents_when_turned_away() {
        setup();
        let parent = new_view("UIView", rect(0.0, 0.0, 300.0, 300.0));
        let color = |name: &str| runtime::send(runtime::class_named("UIColor"), name, &[]);
        let (mirrored, away, inside) = (new_view("UIView", rect(0.0, 0.0, 50.0, 50.0)), new_view("UIView", rect(100.0, 100.0, 50.0, 50.0)), new_view("UIView", rect(10.0, 10.0, 20.0, 20.0)));
        runtime::send(mirrored, "setBackgroundColor:", &[color("redColor")]);
        runtime::send(away, "setBackgroundColor:", &[color("blueColor")]);
        runtime::send(inside, "setBackgroundColor:", &[color("greenColor")]);
        runtime::send(parent, "addSubview:", &[mirrored]);
        runtime::send(parent, "addSubview:", &[away]);
        runtime::send(away, "addSubview:", &[inside]);
        let mut flip = crate::fw::geometry::IDENTITY_3D;
        flip.0[0] = -1.0; // CATransform3DMakeScale(-1, 1, 1)
        let mut turned = crate::fw::geometry::IDENTITY_3D;
        (turned.0[0], turned.0[10]) = (-1.0, -1.0); // half a turn about y
        for (view, transform) in [(mirrored, flip), (away, turned)] {
            let layer = runtime::send(view, "layer", &[]);
            send_struct(layer, "setTransform:", transform);
            runtime::send(layer, "setDoubleSided:", &[0]);
        }
        let pictures = flatten(&window(vec![snapshot(parent, &Classes::find(), 0)]));
        let paths: Vec<&str> = pictures.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(paths, [color_path([1.0, 0.0, 0.0, 1.0]), color_path([0.0, 1.0, 0.0, 1.0])], "the mirrored card and the turned card's subview");
    }

    /// A replicator's copies run its sublayers' animations instanceDelay apart.
    #[test]
    fn replicator_copies_run_instance_delay_apart() {
        setup();
        let parent = new_view("UIView", rect(0.0, 0.0, 300.0, 300.0));
        let replicator = new_layer("CAReplicatorLayer", rect(0.0, 0.0, 300.0, 300.0));
        runtime::send(replicator, "setInstanceCount:", &[2]);
        send_struct(replicator, "setInstanceDelay:", 0.1f64);
        let dot = new_layer("CALayer", rect(0.0, 0.0, 10.0, 10.0));
        runtime::send(dot, "setBackgroundColor:", &[runtime::send(runtime::class_named("UIColor"), "redColor", &[])]);
        runtime::send(replicator, "addSublayer:", &[dot]);
        runtime::send(runtime::send(parent, "layer", &[]), "addSublayer:", &[replicator]);
        let slide = runtime::send(runtime::class_named("CABasicAnimation"), "animationWithKeyPath:", &[crate::fw::string::make("position.x")]);
        runtime::send(slide, "setFromValue:", &[crate::fw::collections::number_float(5.0)]);
        runtime::send(slide, "setToValue:", &[crate::fw::collections::number_float(105.0)]);
        send_struct(slide, "setDuration:", 0.2f64);
        let start = crate::fw::runloop::media_time();
        send_struct(slide, "setBeginTime:", start);
        runtime::send(dot, "addAnimation:forKey:", &[slide, 0]);
        let sampling = Sampling { start: start + 0.15, step: 1.0, count: 1 };
        let node = with_sampling(sampling, || snapshot(parent, &Classes::find(), 0));
        let lefts: Vec<f64> = flatten(&window(vec![node])).iter().map(|p| at(p, 0.0, 0.0).0).collect();
        assert_eq!(lefts, [75.0, 25.0], "the second copy 0.1 s behind");
        runtime::send(dot, "removeAllAnimations", &[]);
    }

    /// A layer turned in 3D with perspective inside a group drawn as one
    /// picture (here a rasterized, faded one) keeps its perspective there:
    /// turned 60 degrees about y with m34 = -1/200, a 100-point square
    /// centred at (100, 100) spans x 68 to 121, its near (left) edge y 36 to
    /// 164 and its far one 59 to 141 (not drawn by the affine part alone,
    /// which skews it down to the right, from x 53 to 147).
    #[test]
    fn layers_turned_in_3d_keep_their_perspective_inside_groups() {
        setup();
        let _files = TEXT_FILES.lock().unwrap_or_else(|e| e.into_inner());
        let window = new_view("UIWindow", rect(0.0, 0.0, 200.0, 200.0));
        let group = new_layer("CALayer", rect(0.0, 0.0, 200.0, 200.0));
        runtime::send(group, "setShouldRasterize:", &[1]);
        send_struct(group, "setOpacity:", 0.5f32);
        let card = new_layer("CALayer", rect(50.0, 50.0, 100.0, 100.0));
        runtime::send(card, "setBackgroundColor:", &[crate::fw::graphics::make_color([1.0, 0.0, 0.0, 1.0])]);
        let (s, c) = 60f64.to_radians().sin_cos();
        let mut turn = crate::fw::geometry::IDENTITY_3D;
        (turn.0[0], turn.0[2], turn.0[8], turn.0[10]) = (c, -s, s, c);
        // Then m34 = -1/200 (CATransform3DRotate's turn, then the perspective).
        (turn.0[3], turn.0[11]) = (-s * -1.0 / 200.0, c * -1.0 / 200.0);
        send_struct(card, "setTransform:", turn);
        runtime::send(group, "addSublayer:", &[card]);
        runtime::send(runtime::send(window, "layer", &[]), "addSublayer:", &[group]);
        let pictures = screen_pictures(&[window], &Classes::find());
        assert_eq!(pictures.len(), 1, "{pictures:?}");
        let p = &pictures[0];
        let (bitmap, _) = crate::fw::graphics::decode(&std::fs::read(&p.path).unwrap()).unwrap();
        let alpha_at = |x: f64, y: f64| {
            let (u, v) = (x - p.place.tx, y - p.place.ty);
            if !(0.0..p.area.0).contains(&u) || !(0.0..p.area.1).contains(&v) {
                return 0;
            }
            let (px, py) = ((u / p.area.0 * bitmap.width as f64) as usize, (v / p.area.1 * bitmap.height as f64) as usize);
            bitmap.data[py * bitmap.bytes_per_row + px * 4 + 3]
        };
        assert!(alpha_at(100.0, 100.0) > 200, "the middle");
        assert!(alpha_at(70.0, 160.0) > 200, "the near edge reaches far down");
        assert_eq!(alpha_at(60.0, 100.0), 0, "nothing left of the near edge");
        assert_eq!(alpha_at(110.0, 150.0), 0, "the far side is shorter");
    }

    /// A rasterized layer is drawn as one picture and its opacity applies to
    /// it as a whole (overlapping sublayers don't show through each other).
    #[test]
    fn rasterized_layers_fade_as_one_picture() {
        setup();
        let _files = TEXT_FILES.lock().unwrap_or_else(|e| e.into_inner());
        let rgba = crate::fw::graphics::make_color;
        let window = new_view("UIWindow", rect(0.0, 0.0, 200.0, 200.0));
        let group = new_layer("CALayer", rect(10.0, 10.0, 60.0, 60.0));
        runtime::send(group, "setShouldRasterize:", &[1]);
        send_struct(group, "setOpacity:", 0.5f32);
        for (color, frame) in [([1.0, 0.0, 0.0, 1.0], rect(0.0, 0.0, 40.0, 40.0)), ([0.0, 0.0, 1.0, 1.0], rect(20.0, 20.0, 40.0, 40.0))] {
            let part = new_layer("CALayer", frame);
            runtime::send(part, "setBackgroundColor:", &[rgba(color)]);
            runtime::send(group, "addSublayer:", &[part]);
        }
        runtime::send(runtime::send(window, "layer", &[]), "addSublayer:", &[group]);
        let pictures = screen_pictures(&[window], &Classes::find());
        assert_eq!(pictures.len(), 1, "{pictures:?}");
        assert!((pictures[0].alpha - 0.5).abs() < 1e-9);
    }
}
