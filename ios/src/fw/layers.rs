//! CALayer beyond the basics in fw/uikit.rs (geometry, looks, timing,
//! sublayers, key-value coding), plus CAGradientLayer, CAReplicatorLayer
//! and CAShapeLayer.
//!
//! A view's layer is the view's other face, as in UIKit: its bounds,
//! position (the view's center), frame, transform, opacity (alpha), hidden
//! and background colour are the view's. A layer of its own (made by the
//! game) keeps them itself. Views and layers are drawn on screen by
//! fw/pictures.rs (with their animations, fw/animation.rs) and into
//! contexts by -renderInContext: (fw/graphics/render.rs).

use crate::fw::geometry::{apply, concat, invert, Point, Rect, Size, Transform, Transform3D, IDENTITY, IDENTITY_3D};
use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::uikit::{property_get, property_set, property_set_object, view_state};
use crate::fw::{collections, string};
use crate::objc::runtime::{self, lock, Class, Id, Sel};
use std::collections::HashMap;
use std::sync::Mutex;

/// What a layer keeps that doesn't fit the property store.
#[derive(Clone)]
struct LayerState {
    bounds: Rect,
    position: Point,
    anchor: Point,
    z: f64,
    transform: Transform3D,
    superlayer: Id,
    sublayers: Vec<Id>,
    /// In a view's layer: the subview this layer is drawn just above (0: under
    /// every subview). On Apple a view's layer keeps its subviews' layers and
    /// the game's in one list; here subviews are the view's, so this says
    /// where among them the game put it.
    above: Id,
    /// CAMediaTiming: the layer's own time is (parent time - begin) * speed + offset.
    begin: f64,
    speed: f64,
    offset: f64,
    /// CAReplicatorLayer's step between copies.
    instance_transform: Transform3D,
}

impl Default for LayerState {
    fn default() -> Self {
        LayerState {
            bounds: Rect::default(),
            position: Point::default(),
            anchor: Point { x: 0.5, y: 0.5 },
            z: 0.0,
            transform: IDENTITY_3D,
            superlayer: 0,
            sublayers: Vec::new(),
            above: 0,
            begin: 0.0,
            speed: 1.0,
            offset: 0.0,
            instance_transform: IDENTITY_3D,
        }
    }
}

static LAYERS: Mutex<Option<HashMap<Id, LayerState>>> = Mutex::new(None);

/// The layer's state (made on first use). No message may be sent inside `f`.
fn with<R>(layer: Id, f: impl FnOnce(&mut LayerState) -> R) -> R {
    let mut all = LAYERS.lock().unwrap_or_else(|e| e.into_inner());
    f(all.get_or_insert_with(HashMap::new).entry(layer).or_default())
}

fn read<R>(layer: Id, f: impl FnOnce(&LayerState) -> R) -> R {
    let all = LAYERS.lock().unwrap_or_else(|e| e.into_inner());
    match all.as_ref().and_then(|m| m.get(&layer)) {
        Some(state) => f(state),
        None => f(&LayerState::default()),
    }
}

fn is_kind(object: Id, class_name: &str) -> bool {
    let class = runtime::class_named(class_name);
    let rt = lock();
    object != 0 && class != 0 && rt.is_subclass(rt.class_of(object), class)
}

/// The view whose layer this is (0 for a layer of its own).
pub fn view_of(layer: Id) -> Id {
    let delegate = property_get(layer, "delegate").unwrap_or(0) as Id;
    if delegate != 0 && is_kind(delegate, "UIView") && view_state(delegate).layer == layer {
        delegate
    } else {
        0
    }
}

pub fn anchor(layer: Id) -> Point {
    read(layer, |s| s.anchor)
}

/// The anchor point of a view's layer (the middle when it has none yet).
pub fn view_anchor(view: Id) -> Point {
    match view_state(view).layer {
        0 => Point { x: 0.5, y: 0.5 },
        layer => anchor(layer),
    }
}

pub fn z_position(layer: Id) -> f64 {
    read(layer, |s| s.z)
}

/// The layers the game added to this one, back to front.
pub fn sublayers_of(layer: Id) -> Vec<Id> {
    read(layer, |s| s.sublayers.clone())
}

// ----- geometry -----

pub fn bounds(layer: Id) -> Rect {
    match view_of(layer) {
        0 => read(layer, |s| s.bounds),
        view => {
            let state = view_state(view);
            Rect { origin: state.bounds_origin, size: state.frame.size }
        }
    }
}

pub fn position(layer: Id) -> Point {
    match view_of(layer) {
        0 => read(layer, |s| s.position),
        view => {
            let (frame, anchor) = (view_state(view).frame, anchor(layer));
            Point { x: frame.origin.x + frame.size.width * anchor.x, y: frame.origin.y + frame.size.height * anchor.y }
        }
    }
}

/// The frame of a layer of its own: its bounds turned and scaled by its
/// transform around the anchor point, put at its position, and the
/// smallest rectangle holding that (Core Animation's rule).
fn own_frame(s: &LayerState) -> Rect {
    let offset = transformed_box(s.bounds.size, s.anchor, affine_of(s.transform));
    Rect { origin: Point { x: s.position.x + offset.origin.x, y: s.position.y + offset.origin.y }, size: offset.size }
}

/// Where a `size` box with its anchor at (0, 0) lands under `t`, as the
/// smallest rectangle holding it.
fn transformed_box(size: Size, anchor: Point, t: Transform) -> Rect {
    let (left, top) = (-size.width * anchor.x, -size.height * anchor.y);
    let corners = [(left, top), (left + size.width, top), (left, top + size.height), (left + size.width, top + size.height)];
    let points = corners.map(|(x, y)| (t.a * x + t.c * y + t.tx, t.b * x + t.d * y + t.ty));
    let (x0, x1) = points.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| (lo.min(p.0), hi.max(p.0)));
    let (y0, y1) = points.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| (lo.min(p.1), hi.max(p.1)));
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

/// setFrame: on a layer of its own: the bounds size whose transformed box
/// is the frame's size (kept when the transform squashes it flat), and the
/// position that puts that box at the frame.
fn set_own_frame(s: &mut LayerState, rect: Rect) {
    let t = affine_of(s.transform);
    let (a, b, c, d) = (t.a.abs(), t.b.abs(), t.c.abs(), t.d.abs());
    // width' = |a| w + |c| h, height' = |b| w + |d| h.
    let det = a * d - b * c;
    if det.abs() > 1e-12 {
        let w = (d * rect.size.width - c * rect.size.height) / det;
        let h = (a * rect.size.height - b * rect.size.width) / det;
        if w >= 0.0 && h >= 0.0 {
            s.bounds.size = Size { width: w, height: h };
        }
    }
    let offset = transformed_box(s.bounds.size, s.anchor, t);
    s.position = Point { x: rect.origin.x - offset.origin.x, y: rect.origin.y - offset.origin.y };
}

pub fn frame(layer: Id) -> Rect {
    match view_of(layer) {
        0 => read(layer, own_frame),
        view => view_state(view).frame,
    }
}

/// The layer's transform: a view's is its 2D transform.
pub fn transform(layer: Id) -> Transform3D {
    match view_of(layer) {
        0 => read(layer, |s| s.transform),
        view => affine_to_3d(view_state(view).transform),
    }
}

pub fn affine_to_3d(t: Transform) -> Transform3D {
    let mut m = IDENTITY_3D;
    (m.0[0], m.0[1], m.0[4], m.0[5], m.0[12], m.0[13]) = (t.a, t.b, t.c, t.d, t.tx, t.ty);
    m
}

pub fn affine_of(m: Transform3D) -> Transform {
    Transform { a: m.0[0], b: m.0[1], c: m.0[4], d: m.0[5], tx: m.0[12], ty: m.0[13] }
}

extern "C" fn layer_bounds(this: Id, _: Sel) -> Rect {
    bounds(this)
}

extern "C" fn layer_set_bounds(this: Id, _: Sel, rect: Rect) {
    match view_of(this) {
        0 => with(this, |s| s.bounds = rect),
        view => crate::fw::uikit::send_rect(view, "setBounds:", rect),
    }
}

extern "C" fn layer_position(this: Id, _: Sel) -> Point {
    position(this)
}

extern "C" fn layer_set_position(this: Id, _: Sel, point: Point) {
    match view_of(this) {
        0 => with(this, |s| s.position = point),
        view => {
            let s = runtime::sel("setCenter:");
            if let Some(imp) = runtime::imp_for(view, s) {
                let f: extern "C" fn(Id, Sel, Point) = unsafe { std::mem::transmute(imp) };
                f(view, s, point);
            }
        }
    }
}

extern "C" fn layer_frame(this: Id, _: Sel) -> Rect {
    frame(this)
}

extern "C" fn layer_set_frame(this: Id, _: Sel, rect: Rect) {
    match view_of(this) {
        0 => with(this, |s| set_own_frame(s, rect)),
        view => crate::fw::uikit::send_rect(view, "setFrame:", rect),
    }
}

extern "C" fn layer_anchor(this: Id, _: Sel) -> Point {
    anchor(this)
}

/// A new anchor point keeps the position: the layer moves around it (a
/// view's frame moves with it, as in UIKit).
extern "C" fn layer_set_anchor(this: Id, _: Sel, point: Point) {
    let view = view_of(this);
    let kept = if view != 0 { Some(position(this)) } else { None };
    with(this, |s| s.anchor = point);
    if let Some(at) = kept {
        let state = view_state(view);
        let size = state.frame.size;
        state.frame.origin = Point { x: at.x - size.width * point.x, y: at.y - size.height * point.y };
    }
}

extern "C" fn layer_z(this: Id, _: Sel) -> f64 {
    z_position(this)
}

extern "C" fn layer_set_z(this: Id, _: Sel, z: f64) {
    with(this, |s| s.z = z);
}

extern "C" fn layer_transform(this: Id, _: Sel) -> Transform3D {
    transform(this)
}

extern "C" fn layer_set_transform(this: Id, _: Sel, t: Transform3D) {
    match view_of(this) {
        0 => with(this, |s| s.transform = t),
        view => view_state(view).transform = affine_of(t),
    }
}

extern "C" fn layer_affine_transform(this: Id, _: Sel) -> Transform {
    affine_of(transform(this))
}

extern "C" fn layer_set_affine_transform(this: Id, s: Sel, t: Transform) {
    layer_set_transform(this, s, affine_to_3d(t));
}

/// The part of the layer that can show: its bounds (nothing here scrolls
/// layers the way CAScrollLayer does).
extern "C" fn layer_visible_rect(this: Id, _: Sel) -> Rect {
    bounds(this)
}

/// One layer's space to its superlayer's: bounds origin, transform around
/// the anchor point, position (a view's layer: its view's geometry).
pub fn to_parent(layer: Id) -> Transform {
    if let view @ 1.. = view_of(layer) {
        return crate::fw::touch::view_step(view);
    }
    read(layer, |s| {
        let (ax, ay) = (s.bounds.size.width * s.anchor.x, s.bounds.size.height * s.anchor.y);
        let to_anchor = Transform { tx: -(s.bounds.origin.x + ax), ty: -(s.bounds.origin.y + ay), ..IDENTITY };
        let to_parent = Transform { tx: s.position.x, ty: s.position.y, ..IDENTITY };
        concat(concat(to_anchor, affine_of(s.transform)), to_parent)
    })
}

/// A layer's space to its root layer's (the window's for views' layers).
pub fn to_root(layer: Id) -> Transform {
    let mut t = IDENTITY;
    let mut current = layer;
    for _ in 0..256 {
        let parent = parent_of(current);
        if current == 0 || parent == 0 {
            break;
        }
        t = concat(t, to_parent(current));
        current = parent;
    }
    t
}

/// From `from`'s space to `to`'s, through their root (nil: the root's own space).
fn conversion(from: Id, to: Id) -> Transform {
    let root = |layer: Id| if layer == 0 { IDENTITY } else { to_root(layer) };
    concat(root(from), invert(root(to)).unwrap_or(IDENTITY))
}

extern "C" fn layer_convert_point_from(this: Id, _: Sel, point: Point, other: Id) -> Point {
    let (x, y) = apply(conversion(other, this), point.x, point.y);
    Point { x, y }
}

extern "C" fn layer_convert_point_to(this: Id, _: Sel, point: Point, other: Id) -> Point {
    let (x, y) = apply(conversion(this, other), point.x, point.y);
    Point { x, y }
}

extern "C" fn layer_convert_rect_from(this: Id, _: Sel, rect: Rect, other: Id) -> Rect {
    crate::fw::uikit::convert_rect(conversion(other, this), rect)
}

extern "C" fn layer_convert_rect_to(this: Id, _: Sel, rect: Rect, other: Id) -> Rect {
    crate::fw::uikit::convert_rect(conversion(this, other), rect)
}

// ----- hierarchy -----

/// The layer above: for a view's layer, its superview's layer.
fn parent_of(layer: Id) -> Id {
    match view_of(layer) {
        0 => read(layer, |s| s.superlayer),
        view => match view_state(view).superview {
            0 => 0,
            parent => view_state(parent).layer,
        },
    }
}

fn detach(layer: Id) {
    let parent = read(layer, |s| s.superlayer);
    if parent != 0 {
        with(parent, |s| s.sublayers.retain(|&l| l != layer));
        with(layer, |s| s.superlayer = 0);
    }
}

/// Something drawn inside a layer: a subview (its layer) or a layer the game added.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Child {
    View(Id),
    Layer(Id),
}

/// What is drawn inside `layer`, back to front before zPosition: a view's
/// layer has its subviews, with the game's layers among them where they
/// were put (as Apple's one list of sublayers has them).
pub fn children(layer: Id) -> Vec<Child> {
    match view_of(layer) {
        0 => sublayers_of(layer).into_iter().map(Child::Layer).collect(),
        view => view_children(view, layer),
    }
}

/// `children` of a view's layer.
pub fn view_children(view: Id, layer: Id) -> Vec<Child> {
    let own = sublayers_of(layer);
    let above: Vec<Id> = own.iter().map(|&l| read(l, |s| s.above)).collect();
    let mut placed = vec![false; own.len()];
    let mut out = Vec::new();
    let mut put_above = |subview: Id, out: &mut Vec<Child>| {
        for (i, &l) in own.iter().enumerate() {
            if above[i] == subview && !placed[i] {
                placed[i] = true;
                out.push(Child::Layer(l));
            }
        }
    };
    put_above(0, &mut out);
    for subview in view_state(view).subviews.clone() {
        out.push(Child::View(subview));
        put_above(subview, &mut out);
    }
    // Above a subview that has gone: where it was is unknown, so on top.
    out.extend(own.iter().zip(&placed).filter(|(_, &p)| !p).map(|(&l, _)| Child::Layer(l)));
    out
}

/// Where a layer put at `index` of `parent`'s whole list (`children`) goes:
/// the subview it is just above and its place in the parent's own sublayers.
fn place_at(parent: Id, index: usize) -> (Id, usize) {
    let order = children(parent);
    let own = sublayers_of(parent);
    match order.get(index.min(order.len()).wrapping_sub(1)) {
        None => (0, 0),
        Some(&Child::View(subview)) => (subview, own.iter().position(|&l| read(l, |s| s.above) == subview).unwrap_or(own.len())),
        Some(&Child::Layer(before)) => (read(before, |s| s.above), own.iter().position(|&l| l == before).map_or(own.len(), |i| i + 1)),
    }
}

/// Where `sibling` is in `parent`'s whole list: one of its layers or a subview's.
fn index_of(parent: Id, sibling: Id) -> Option<usize> {
    let view = view_of(sibling);
    children(parent).iter().position(|&child| child == Child::Layer(sibling) || (view != 0 && child == Child::View(view)))
}

/// Puts `layer` in `parent` at `index` of its whole list (`children`; None: on top).
fn attach(parent: Id, layer: Id, index: Option<usize>) {
    if parent == 0 || layer == 0 || parent == layer {
        return;
    }
    // The layer holds what it shows, as on Apple.
    runtime::send(layer, "retain", &[]);
    let old = read(layer, |s| s.superlayer);
    detach(layer);
    let (above, at) = match index {
        Some(i) => place_at(parent, i),
        None => (view_state_of_layer(parent).and_then(|subviews| subviews.last().copied()).unwrap_or(0), usize::MAX),
    };
    with(parent, |s| s.sublayers.insert(at.min(s.sublayers.len()), layer));
    with(layer, |s| (s.superlayer, s.above) = (parent, above));
    if old != 0 {
        runtime::send(layer, "release", &[]);
    }
}

/// A view's layer's subviews (None for a layer of its own).
fn view_state_of_layer(layer: Id) -> Option<Vec<Id>> {
    match view_of(layer) {
        0 => None,
        view => Some(view_state(view).subviews.clone()),
    }
}

extern "C" fn layer_superlayer(this: Id, _: Sel) -> Id {
    parent_of(this)
}

extern "C" fn layer_sublayers(this: Id, _: Sel) -> Id {
    let list = sublayers_of(this);
    if list.is_empty() {
        0
    } else {
        collections::array_from(&list)
    }
}

extern "C" fn layer_set_sublayers(this: Id, _: Sel, list: Id) {
    for old in sublayers_of(this) {
        detach(old);
        runtime::send(old, "release", &[]);
    }
    for layer in if list == 0 { Vec::new() } else { collections::array_items(list) } {
        attach(this, layer, None);
    }
}

extern "C" fn layer_add_sublayer(this: Id, _: Sel, layer: Id) {
    attach(this, layer, None);
}

extern "C" fn layer_insert_at(this: Id, _: Sel, layer: Id, index: u32) {
    attach(this, layer, Some(index as usize));
}

extern "C" fn layer_insert_above(this: Id, _: Sel, layer: Id, sibling: Id) {
    let at = index_of(this, sibling).map(|i| i + 1);
    attach(this, layer, at);
}

extern "C" fn layer_insert_below(this: Id, _: Sel, layer: Id, sibling: Id) {
    let at = index_of(this, sibling);
    attach(this, layer, at);
}

extern "C" fn layer_replace_sublayer(this: Id, _: Sel, old: Id, new: Id) {
    if !sublayers_of(this).contains(&old) {
        return;
    }
    let at = index_of(this, old);
    detach(old);
    attach(this, new, at);
    runtime::send(old, "release", &[]);
}

extern "C" fn layer_remove_from_superlayer(this: Id, _: Sel) {
    if read(this, |s| s.superlayer) != 0 {
        detach(this);
        runtime::send(this, "release", &[]);
    }
}

// ----- looks: a view's are the view's own -----

extern "C" fn layer_opacity(this: Id, _: Sel) -> f32 {
    match view_of(this) {
        0 => property_get(this, "opacity").map(f64::from_bits).unwrap_or(1.0) as f32,
        view => property_get(view, "alpha").map(f64::from_bits).unwrap_or(1.0) as f32,
    }
}

extern "C" fn layer_set_opacity(this: Id, _: Sel, value: f32) {
    let value = (value as f64).clamp(0.0, 1.0);
    match view_of(this) {
        0 => property_set(this, "opacity", value.to_bits()),
        view => property_set(view, "alpha", value.to_bits()),
    }
}

extern "C" fn layer_hidden(this: Id, _: Sel) -> bool {
    let owner = match view_of(this) {
        0 => this,
        view => view,
    };
    property_get(owner, "hidden").unwrap_or(0) != 0
}

extern "C" fn layer_set_hidden(this: Id, _: Sel, value: u8) {
    let owner = match view_of(this) {
        0 => this,
        view => view,
    };
    property_set(owner, "hidden", (value != 0) as u64);
}

/// CGColors are UIColor objects here (fw/graphics.rs reads both).
extern "C" fn layer_background(this: Id, _: Sel) -> Id {
    match view_of(this) {
        0 => property_get(this, "backgroundColor").unwrap_or(0) as Id,
        view => property_get(view, "background").unwrap_or(0) as Id,
    }
}

extern "C" fn layer_set_background(this: Id, _: Sel, color: Id) {
    match view_of(this) {
        0 => property_set_object(this, "backgroundColor", color),
        view => property_set_object(view, "background", color),
    }
}

extern "C" fn layer_masks(this: Id, _: Sel) -> bool {
    let owner = match view_of(this) {
        0 => this,
        view => view,
    };
    property_get(owner, "clips").unwrap_or(0) != 0
}

/// masksToBounds is a view's clipsToBounds.
extern "C" fn layer_set_masks(this: Id, _: Sel, value: u8) {
    let owner = match view_of(this) {
        0 => this,
        view => view,
    };
    property_set(owner, "clips", (value != 0) as u64);
}

crate::fw::uikit::property!(layer_corner_radius, layer_set_corner_radius, "cornerRadius", f64, 0.0);
crate::fw::uikit::property!(layer_border_width, layer_set_border_width, "borderWidth", f64, 0.0);
crate::fw::uikit::property!(layer_border_color, layer_set_border_color, "borderColor", id, 0);
crate::fw::uikit::property!(layer_shadow_radius, layer_set_shadow_radius, "shadowRadius", f64, 3.0);
crate::fw::uikit::property!(layer_shadow_color, layer_set_shadow_color, "shadowColor", id, 0);
crate::fw::uikit::property!(layer_shadow_path, layer_set_shadow_path, "shadowPath", id, 0);
crate::fw::uikit::property!(layer_rasterizes, layer_set_rasterizes, "shouldRasterize", bool, false);
crate::fw::uikit::property!(layer_rasterization_scale, layer_set_rasterization_scale, "rasterizationScale", f64, 1.0);
crate::fw::uikit::property!(layer_mask, layer_set_mask, "mask", id, 0);
crate::fw::uikit::property!(layer_contents, layer_set_contents, "contents", id, 0);
crate::fw::uikit::property!(layer_contents_gravity, layer_set_contents_gravity, "contentsGravity", id, 0);
crate::fw::uikit::property!(layer_contents_scale, layer_set_contents_scale, "contentsScale", f64, 1.0);
crate::fw::uikit::property!(layer_geometry_flipped, layer_set_geometry_flipped, "geometryFlipped", bool, false);

/// On iOS a layer's contents are drawn flipped (Core Animation's y goes up,
/// UIKit's down) unless its geometry is flipped; contentsGravity's "top"
/// and "bottom" are then the other way round.
extern "C" fn layer_contents_are_flipped(this: Id, _: Sel) -> bool {
    !layer_geometry_flipped(this, 0)
}

pub fn contents_are_flipped(layer: Id) -> bool {
    layer_contents_are_flipped(layer, 0)
}
crate::fw::uikit::property!(layer_name, layer_set_name, "name", id, 0);
crate::fw::uikit::property!(layer_double_sided, layer_set_double_sided, "doubleSided", bool, true);
crate::fw::uikit::property!(layer_bounds_redraw, layer_set_bounds_redraw, "needsDisplayOnBoundsChange", bool, false);

extern "C" fn layer_shadow_opacity(this: Id, _: Sel) -> f32 {
    property_get(this, "shadowOpacity").map(f64::from_bits).unwrap_or(0.0) as f32
}

extern "C" fn layer_set_shadow_opacity(this: Id, _: Sel, value: f32) {
    property_set(this, "shadowOpacity", (value as f64).to_bits());
}

extern "C" fn layer_shadow_offset(this: Id, _: Sel) -> Size {
    Size { width: get_f64(this, "shadowOffsetX", 0.0), height: get_f64(this, "shadowOffsetY", -3.0) }
}

extern "C" fn layer_set_shadow_offset(this: Id, _: Sel, size: Size) {
    property_set(this, "shadowOffsetX", size.width.to_bits());
    property_set(this, "shadowOffsetY", size.height.to_bits());
}

fn get_f64(object: Id, key: &'static str, default: f64) -> f64 {
    property_get(object, key).map(f64::from_bits).unwrap_or(default)
}

fn get_point(object: Id, keys: (&'static str, &'static str), default: Point) -> Point {
    Point { x: get_f64(object, keys.0, default.x), y: get_f64(object, keys.1, default.y) }
}

fn set_point(object: Id, keys: (&'static str, &'static str), point: Point) {
    property_set(object, keys.0, point.x.to_bits());
    property_set(object, keys.1, point.y.to_bits());
}

pub fn opacity(layer: Id) -> f64 {
    layer_opacity(layer, 0) as f64
}

pub fn hidden(layer: Id) -> bool {
    layer_hidden(layer, 0)
}

pub fn masks(layer: Id) -> bool {
    layer_masks(layer, 0)
}

pub fn background(layer: Id) -> Id {
    layer_background(layer, 0)
}

pub fn contents(layer: Id) -> Id {
    layer_contents(layer, 0)
}

/// contentsGravity's name ("resize" until the game sets one).
pub fn contents_gravity(layer: Id) -> String {
    match layer_contents_gravity(layer, 0) {
        0 => "resize".into(),
        name => string::get(name),
    }
}

pub fn contents_scale(layer: Id) -> f64 {
    layer_contents_scale(layer, 0)
}

/// How a layer's edge and shadow look (Core Animation's defaults until set).
#[derive(Clone, Debug, PartialEq)]
pub struct Looks {
    pub corner_radius: f64,
    /// Colour (straight) and width, when there is one to see.
    pub border: Option<([f64; 4], f64)>,
    /// Colour with shadowOpacity in its alpha, blur radius, offset and shadowPath (0: none).
    pub shadow: Option<([f64; 4], f64, (f64, f64), Id)>,
    pub mask: Id,
}

pub fn looks(layer: Id) -> Looks {
    let rgba = |color: Id| crate::fw::graphics::color_rgba(color);
    // Borders and shadows are opaque black until given a colour (nil: none).
    let colour = |key: &'static str| match property_get(layer, key) {
        None => Some([0.0, 0.0, 0.0, 1.0]),
        Some(color) => rgba(color as Id),
    };
    let width = layer_border_width(layer, 0);
    let border = colour("borderColor").filter(|c| width > 0.0 && c[3] > 0.0).map(|c| (c, width));
    let opacity = layer_shadow_opacity(layer, 0) as f64;
    let shadow = colour("shadowColor").map(|[r, g, b, a]| [r, g, b, a * opacity.clamp(0.0, 1.0)]).filter(|c| c[3] > 0.0).map(|c| {
        let offset = layer_shadow_offset(layer, 0);
        (c, layer_shadow_radius(layer, 0).max(0.0), (offset.width, offset.height), layer_shadow_path(layer, 0))
    });
    Looks { corner_radius: layer_corner_radius(layer, 0).max(0.0), border, shadow, mask: layer_mask(layer, 0) }
}

/// A shape layer's path, fill and stroke colours (nil for none), line width
/// and whether it fills even-odd.
pub fn shape_look(layer: Id) -> (Id, Id, Id, f64, bool) {
    let rule = shape_fill_rule(layer, 0);
    let even_odd = rule != 0 && string::get(rule) == "even-odd";
    (shape_path(layer, 0), shape_fill(layer), shape_stroke_color(layer, 0), shape_line_width(layer, 0), even_odd)
}

// ----- time (CAMediaTiming) -----

/// The layers from the top down to `layer`, each with (begin, speed, offset).
fn time_chain(layer: Id) -> Vec<(f64, f64, f64)> {
    let mut chain = Vec::new();
    let mut current = layer;
    for _ in 0..64 {
        if current == 0 {
            break;
        }
        chain.push(read(current, |s| (s.begin, s.speed, s.offset)));
        current = parent_of(current);
    }
    chain.reverse();
    chain
}

/// The layer's own time at media time `global` (CACurrentMediaTime).
pub fn local_time(layer: Id, global: f64) -> f64 {
    time_chain(layer).into_iter().fold(global, |t, (begin, speed, offset)| (t - begin) * speed + offset)
}

/// The media time when the layer's own time is `local`; None when a
/// stopped layer (speed 0) holds it still.
pub fn global_time(layer: Id, local: f64) -> Option<f64> {
    let mut t = local;
    for (begin, speed, offset) in time_chain(layer).into_iter().rev() {
        if speed == 0.0 {
            return None;
        }
        t = (t - offset) / speed + begin;
    }
    Some(t)
}

extern "C" fn layer_convert_time(this: Id, _: Sel, time: f64, from: Id) -> f64 {
    let global = if from == 0 { Some(time) } else { global_time(from, time) };
    local_time(this, global.unwrap_or(time))
}

extern "C" fn layer_convert_time_to(this: Id, _: Sel, time: f64, to: Id) -> f64 {
    let global = global_time(this, time).unwrap_or(time);
    if to == 0 {
        global
    } else {
        local_time(to, global)
    }
}

extern "C" fn layer_begin(this: Id, _: Sel) -> f64 {
    read(this, |s| s.begin)
}

extern "C" fn layer_set_begin(this: Id, _: Sel, value: f64) {
    with(this, |s| s.begin = value);
    crate::fw::animation::reschedule();
}

extern "C" fn layer_speed(this: Id, _: Sel) -> f32 {
    read(this, |s| s.speed) as f32
}

pub(crate) extern "C" fn layer_set_speed(this: Id, _: Sel, value: f32) {
    with(this, |s| s.speed = value as f64);
    crate::fw::animation::reschedule();
}

extern "C" fn layer_offset(this: Id, _: Sel) -> f64 {
    read(this, |s| s.offset)
}

extern "C" fn layer_set_offset(this: Id, _: Sel, value: f64) {
    with(this, |s| s.offset = value);
    crate::fw::animation::reschedule();
}

// ----- drawing into a context -----

/// -renderInContext: draws the layer and what is inside it into a context
/// (fw/graphics/render.rs).
extern "C" fn layer_render_in_context(this: Id, _: Sel, context: Id) {
    crate::fw::graphics::render_layer(context, this);
}

extern "C" fn layer_draw_in_context(_this: Id, _: Sel, _context: Id) {}

// ----- key-value coding -----

/// How a key's value is held, for valueForKey: / setValue:forKey:.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Kind {
    Object,
    Double,
    Float,
    Bool,
    Integer,
    Point,
    Size,
    Rect,
}

/// A key's getter: the key, or isKey for some booleans.
fn getter(object: Id, key: &str) -> Option<Sel> {
    let mut chars = key.chars();
    let capital = chars.next().map(|c| c.to_ascii_uppercase().to_string() + chars.as_str()).unwrap_or_default();
    [key.to_string(), format!("is{capital}")].into_iter().map(|name| runtime::sel(&name)).find(|&s| runtime::imp_for(object, s).is_some())
}

fn setter(object: Id, key: &str) -> Option<Sel> {
    let mut chars = key.chars();
    let capital = chars.next().map(|c| c.to_ascii_uppercase().to_string() + chars.as_str()).unwrap_or_default();
    Some(runtime::sel(&format!("set{capital}:"))).filter(|&s| runtime::imp_for(object, s).is_some())
}

/// Sends a message with one struct argument (nothing when not answered).
pub(crate) fn send_struct<T: Copy>(object: Id, name: &str, value: T) {
    let s = runtime::sel(name);
    if let Some(imp) = runtime::imp_for(object, s) {
        let f: extern "C" fn(Id, Sel, T) = unsafe { std::mem::transmute(imp) };
        f(object, s, value);
    }
}

/// Boxes a struct in an NSValue, when NSValue knows how.
pub(crate) fn boxed<T: Copy>(name: &str, value: T) -> Id {
    let class = runtime::class_named("NSValue");
    let s = runtime::sel(name);
    match runtime::imp_for(class, s) {
        Some(imp) => {
            let f: extern "C" fn(Class, Sel, T) -> Id = unsafe { std::mem::transmute(imp) };
            f(class, s, value)
        }
        None => 0,
    }
}

/// A struct out of an NSValue (None when it doesn't answer).
pub(crate) fn unboxed<T: Copy>(value: Id, name: &str) -> Option<T> {
    if value == 0 {
        return None;
    }
    let s = runtime::sel(name);
    let imp = runtime::imp_for(value, s)?;
    let f: extern "C" fn(Id, Sel) -> T = unsafe { std::mem::transmute(imp) };
    Some(f(value, s))
}

/// A number object's value (0 for anything else).
pub(crate) fn number_value(value: Id) -> f64 {
    if collections::is_number(value) {
        collections::number_as_f64(value)
    } else {
        0.0
    }
}

/// valueForKey: on a Core Animation object: known keys through their
/// getters (numbers and structs boxed), any other key from the values the
/// game stored, as Apple's layers and animations keep any key.
pub(crate) fn kvc_get(object: Id, key: Id, keys: &[(&str, Kind)]) -> Id {
    let name = string::get(key);
    let Some(&(_, kind)) = keys.iter().find(|(k, _)| *k == name) else {
        let store = property_get(object, "kvcValues").unwrap_or(0) as Id;
        return if store == 0 { 0 } else { collections::dictionary_get(store, key) };
    };
    let Some(s) = getter(object, &name) else { return 0 };
    let imp = runtime::imp_for(object, s).unwrap();
    unsafe {
        match kind {
            Kind::Object => std::mem::transmute::<usize, extern "C" fn(Id, Sel) -> Id>(imp)(object, s),
            Kind::Double => collections::number_float(std::mem::transmute::<usize, extern "C" fn(Id, Sel) -> f64>(imp)(object, s)),
            Kind::Float => collections::number_float(std::mem::transmute::<usize, extern "C" fn(Id, Sel) -> f32>(imp)(object, s) as f64),
            Kind::Bool => collections::number_bool(std::mem::transmute::<usize, extern "C" fn(Id, Sel) -> bool>(imp)(object, s)),
            Kind::Integer => collections::number_int(std::mem::transmute::<usize, extern "C" fn(Id, Sel) -> isize>(imp)(object, s) as i64),
            Kind::Point => boxed("valueWithCGPoint:", std::mem::transmute::<usize, extern "C" fn(Id, Sel) -> Point>(imp)(object, s)),
            Kind::Size => boxed("valueWithCGSize:", std::mem::transmute::<usize, extern "C" fn(Id, Sel) -> Size>(imp)(object, s)),
            Kind::Rect => boxed("valueWithCGRect:", std::mem::transmute::<usize, extern "C" fn(Id, Sel) -> Rect>(imp)(object, s)),
        }
    }
}

/// setValue:forKey: on a Core Animation object (see `kvc_get`).
pub(crate) fn kvc_set(object: Id, value: Id, key: Id, keys: &[(&str, Kind)]) {
    let name = string::get(key);
    let known = keys.iter().find(|(k, _)| *k == name).map(|&(_, kind)| kind);
    if let (Some(kind), Some(s)) = (known, setter(object, &name)) {
        let imp = runtime::imp_for(object, s).unwrap();
        unsafe {
            match kind {
                Kind::Object => std::mem::transmute::<usize, extern "C" fn(Id, Sel, Id)>(imp)(object, s, value),
                Kind::Double => std::mem::transmute::<usize, extern "C" fn(Id, Sel, f64)>(imp)(object, s, number_value(value)),
                Kind::Float => std::mem::transmute::<usize, extern "C" fn(Id, Sel, f32)>(imp)(object, s, number_value(value) as f32),
                Kind::Bool => std::mem::transmute::<usize, extern "C" fn(Id, Sel, u8)>(imp)(object, s, (number_value(value) != 0.0) as u8),
                Kind::Integer => std::mem::transmute::<usize, extern "C" fn(Id, Sel, isize)>(imp)(object, s, number_value(value) as isize),
                Kind::Point => {
                    if let Some(p) = unboxed::<Point>(value, "CGPointValue") {
                        std::mem::transmute::<usize, extern "C" fn(Id, Sel, Point)>(imp)(object, s, p)
                    }
                }
                Kind::Size => {
                    if let Some(p) = unboxed::<Size>(value, "CGSizeValue") {
                        std::mem::transmute::<usize, extern "C" fn(Id, Sel, Size)>(imp)(object, s, p)
                    }
                }
                Kind::Rect => {
                    if let Some(p) = unboxed::<Rect>(value, "CGRectValue") {
                        std::mem::transmute::<usize, extern "C" fn(Id, Sel, Rect)>(imp)(object, s, p)
                    }
                }
            }
        }
        return;
    }
    let mut store = property_get(object, "kvcValues").unwrap_or(0) as Id;
    if store == 0 {
        store = runtime::send(collections::dictionary_from(&[]), "mutableCopy", &[]);
        property_set(object, "kvcValues", store as u64);
    }
    if value == 0 {
        runtime::send(store, "removeObjectForKey:", &[key]);
    } else {
        runtime::send(store, "setObject:forKey:", &[value, key]);
    }
}

const LAYER_KEYS: &[(&str, Kind)] = &[
    ("bounds", Kind::Rect),
    ("frame", Kind::Rect),
    ("position", Kind::Point),
    ("anchorPoint", Kind::Point),
    ("zPosition", Kind::Double),
    ("opacity", Kind::Float),
    ("hidden", Kind::Bool),
    ("masksToBounds", Kind::Bool),
    ("cornerRadius", Kind::Double),
    ("borderWidth", Kind::Double),
    ("borderColor", Kind::Object),
    ("backgroundColor", Kind::Object),
    ("shadowOpacity", Kind::Float),
    ("shadowRadius", Kind::Double),
    ("shadowOffset", Kind::Size),
    ("shadowColor", Kind::Object),
    ("shouldRasterize", Kind::Bool),
    ("rasterizationScale", Kind::Double),
    ("contents", Kind::Object),
    ("mask", Kind::Object),
    ("name", Kind::Object),
    ("delegate", Kind::Object),
    ("speed", Kind::Float),
    ("timeOffset", Kind::Double),
    ("beginTime", Kind::Double),
    ("startPoint", Kind::Point),
    ("endPoint", Kind::Point),
    ("colors", Kind::Object),
    ("locations", Kind::Object),
    ("instanceCount", Kind::Integer),
    ("instanceDelay", Kind::Double),
    ("fillColor", Kind::Object),
    ("strokeColor", Kind::Object),
    ("lineWidth", Kind::Double),
    ("strokeStart", Kind::Double),
    ("strokeEnd", Kind::Double),
];

extern "C" fn layer_value_for_key(this: Id, _: Sel, key: Id) -> Id {
    kvc_get(this, key, LAYER_KEYS)
}

extern "C" fn layer_set_value_for_key(this: Id, _: Sel, value: Id, key: Id) {
    kvc_set(this, value, key, LAYER_KEYS)
}

extern "C" fn layer_class_layer(class: Class, _: Sel) -> Id {
    runtime::send(runtime::send(class, "alloc", &[]), "init", &[])
}

// ----- CAGradientLayer -----

crate::fw::uikit::property!(gradient_colors, gradient_set_colors, "colors", id, 0);
crate::fw::uikit::property!(gradient_locations, gradient_set_locations, "locations", id, 0);
crate::fw::uikit::property!(gradient_type, gradient_set_type, "gradientType", id, 0);

extern "C" fn gradient_start(this: Id, _: Sel) -> Point {
    get_point(this, ("startX", "startY"), Point { x: 0.5, y: 0.0 })
}

extern "C" fn gradient_set_start(this: Id, _: Sel, point: Point) {
    set_point(this, ("startX", "startY"), point);
}

extern "C" fn gradient_end(this: Id, _: Sel) -> Point {
    get_point(this, ("endX", "endY"), Point { x: 0.5, y: 1.0 })
}

extern "C" fn gradient_set_end(this: Id, _: Sel, point: Point) {
    set_point(this, ("endX", "endY"), point);
}

/// The gradient's colours (straight RGBA) and where each sits (0 to 1;
/// evenly spread without locations), as -renderInContext: draws them.
pub fn gradient_stops(layer: Id) -> Vec<([f64; 4], f64)> {
    let colors = gradient_colors(layer, 0);
    let colors: Vec<[f64; 4]> = if colors == 0 { Vec::new() } else { collections::array_items(colors) }
        .into_iter()
        .filter_map(crate::fw::graphics::color_rgba)
        .collect();
    let locations = gradient_locations(layer, 0);
    let locations: Vec<f64> = if locations == 0 { Vec::new() } else { collections::array_items(locations).into_iter().map(number_value).collect() };
    let n = colors.len();
    colors
        .into_iter()
        .enumerate()
        .map(|(i, c)| (c, if locations.len() == n { locations[i] } else if n > 1 { i as f64 / (n - 1) as f64 } else { 0.0 }))
        .collect()
}

/// Start and end points in the layer's unit square.
pub fn gradient_points(layer: Id) -> (Point, Point) {
    (gradient_start(layer, 0), gradient_end(layer, 0))
}

// ----- CAReplicatorLayer -----

crate::fw::uikit::property!(replicator_count, replicator_set_count, "instanceCount", isize, 1);
crate::fw::uikit::property!(replicator_delay, replicator_set_delay, "instanceDelay", f64, 0.0);
crate::fw::uikit::property!(replicator_color, replicator_set_color, "instanceColor", id, 0);
crate::fw::uikit::property!(replicator_preserves_depth, replicator_set_preserves_depth, "preservesDepth", bool, false);

extern "C" fn replicator_transform(this: Id, _: Sel) -> Transform3D {
    read(this, |s| s.instance_transform)
}

extern "C" fn replicator_set_transform(this: Id, _: Sel, t: Transform3D) {
    with(this, |s| s.instance_transform = t);
}

/// How many copies of its sublayers a replicator shows (1 for other layers)
/// and the transform from one copy to the next.
pub fn replication(layer: Id) -> (usize, Transform3D) {
    if !is_kind(layer, "CAReplicatorLayer") {
        return (1, IDENTITY_3D);
    }
    (replicator_count(layer, 0).clamp(0, 1024) as usize, read(layer, |s| s.instance_transform))
}

macro_rules! float_property {
    ($get:ident, $set:ident, $key:literal, $default:expr) => {
        extern "C" fn $get(this: Id, _: Sel) -> f32 {
            property_get(this, $key).map(f64::from_bits).unwrap_or($default) as f32
        }
        extern "C" fn $set(this: Id, _: Sel, value: f32) {
            property_set(this, $key, (value as f64).to_bits());
        }
    };
}
float_property!(replicator_red, replicator_set_red, "instanceRedOffset", 0.0);
float_property!(replicator_green, replicator_set_green, "instanceGreenOffset", 0.0);
float_property!(replicator_blue, replicator_set_blue, "instanceBlueOffset", 0.0);
float_property!(replicator_alpha, replicator_set_alpha, "instanceAlphaOffset", 0.0);

// ----- CAShapeLayer -----

crate::fw::uikit::property!(shape_path, shape_set_path, "path", id, 0);
crate::fw::uikit::property!(shape_stroke_color, shape_set_stroke_color, "strokeColor", id, 0);
crate::fw::uikit::property!(shape_line_width, shape_set_line_width, "lineWidth", f64, 1.0);
crate::fw::uikit::property!(shape_stroke_start, shape_set_stroke_start, "strokeStart", f64, 0.0);
crate::fw::uikit::property!(shape_stroke_end, shape_set_stroke_end, "strokeEnd", f64, 1.0);
crate::fw::uikit::property!(shape_line_cap, shape_set_line_cap, "lineCap", id, 0);
crate::fw::uikit::property!(shape_line_join, shape_set_line_join, "lineJoin", id, 0);
crate::fw::uikit::property!(shape_dash, shape_set_dash, "lineDashPattern", id, 0);
crate::fw::uikit::property!(shape_fill_rule, shape_set_fill_rule, "fillRule", id, 0);
crate::fw::uikit::property!(shape_miter, shape_set_miter, "miterLimit", f64, 10.0);

/// The fill colour: opaque black until the game sets one (nil fills nothing).
pub fn shape_fill(layer: Id) -> Id {
    match property_get(layer, "fillColor") {
        Some(color) => color as Id,
        None => crate::fw::graphics::make_color([0.0, 0.0, 0.0, 1.0]),
    }
}

extern "C" fn shape_fill_color(this: Id, _: Sel) -> Id {
    shape_fill(this)
}

extern "C" fn shape_set_fill_color(this: Id, _: Sel, color: Id) {
    property_set_object(this, "fillColor", color);
}

pub fn register() {
    let layer = runtime::class_named("CALayer");
    class_methods(layer, &[("layer", layer_class_layer as usize)]);
    methods(
        layer,
        &[
            ("bounds", layer_bounds as usize),
            ("setBounds:", layer_set_bounds as usize),
            ("position", layer_position as usize),
            ("setPosition:", layer_set_position as usize),
            ("frame", layer_frame as usize),
            ("setFrame:", layer_set_frame as usize),
            ("anchorPoint", layer_anchor as usize),
            ("setAnchorPoint:", layer_set_anchor as usize),
            ("zPosition", layer_z as usize),
            ("setZPosition:", layer_set_z as usize),
            ("transform", layer_transform as usize),
            ("setTransform:", layer_set_transform as usize),
            ("affineTransform", layer_affine_transform as usize),
            ("setAffineTransform:", layer_set_affine_transform as usize),
            ("visibleRect", layer_visible_rect as usize),
            ("convertPoint:fromLayer:", layer_convert_point_from as usize),
            ("convertPoint:toLayer:", layer_convert_point_to as usize),
            ("convertRect:fromLayer:", layer_convert_rect_from as usize),
            ("convertRect:toLayer:", layer_convert_rect_to as usize),
            ("superlayer", layer_superlayer as usize),
            ("sublayers", layer_sublayers as usize),
            ("setSublayers:", layer_set_sublayers as usize),
            ("addSublayer:", layer_add_sublayer as usize),
            ("insertSublayer:atIndex:", layer_insert_at as usize),
            ("insertSublayer:above:", layer_insert_above as usize),
            ("insertSublayer:below:", layer_insert_below as usize),
            ("replaceSublayer:with:", layer_replace_sublayer as usize),
            ("removeFromSuperlayer", layer_remove_from_superlayer as usize),
            ("opacity", layer_opacity as usize),
            ("setOpacity:", layer_set_opacity as usize),
            ("isHidden", layer_hidden as usize),
            ("setHidden:", layer_set_hidden as usize),
            ("backgroundColor", layer_background as usize),
            ("setBackgroundColor:", layer_set_background as usize),
            ("masksToBounds", layer_masks as usize),
            ("setMasksToBounds:", layer_set_masks as usize),
            ("cornerRadius", layer_corner_radius as usize),
            ("setCornerRadius:", layer_set_corner_radius as usize),
            ("borderWidth", layer_border_width as usize),
            ("setBorderWidth:", layer_set_border_width as usize),
            ("borderColor", layer_border_color as usize),
            ("setBorderColor:", layer_set_border_color as usize),
            ("shadowOpacity", layer_shadow_opacity as usize),
            ("setShadowOpacity:", layer_set_shadow_opacity as usize),
            ("shadowRadius", layer_shadow_radius as usize),
            ("setShadowRadius:", layer_set_shadow_radius as usize),
            ("shadowOffset", layer_shadow_offset as usize),
            ("setShadowOffset:", layer_set_shadow_offset as usize),
            ("shadowColor", layer_shadow_color as usize),
            ("setShadowColor:", layer_set_shadow_color as usize),
            ("shadowPath", layer_shadow_path as usize),
            ("setShadowPath:", layer_set_shadow_path as usize),
            ("shouldRasterize", layer_rasterizes as usize),
            ("setShouldRasterize:", layer_set_rasterizes as usize),
            ("rasterizationScale", layer_rasterization_scale as usize),
            ("setRasterizationScale:", layer_set_rasterization_scale as usize),
            ("mask", layer_mask as usize),
            ("setMask:", layer_set_mask as usize),
            ("contents", layer_contents as usize),
            ("setContents:", layer_set_contents as usize),
            ("contentsGravity", layer_contents_gravity as usize),
            ("setContentsGravity:", layer_set_contents_gravity as usize),
            ("contentsScale", layer_contents_scale as usize),
            ("setContentsScale:", layer_set_contents_scale as usize),
            ("isGeometryFlipped", layer_geometry_flipped as usize),
            ("setGeometryFlipped:", layer_set_geometry_flipped as usize),
            ("contentsAreFlipped", layer_contents_are_flipped as usize),
            ("name", layer_name as usize),
            ("setName:", layer_set_name as usize),
            ("isDoubleSided", layer_double_sided as usize),
            ("setDoubleSided:", layer_set_double_sided as usize),
            ("needsDisplayOnBoundsChange", layer_bounds_redraw as usize),
            ("setNeedsDisplayOnBoundsChange:", layer_set_bounds_redraw as usize),
            ("convertTime:fromLayer:", layer_convert_time as usize),
            ("convertTime:toLayer:", layer_convert_time_to as usize),
            ("beginTime", layer_begin as usize),
            ("setBeginTime:", layer_set_begin as usize),
            ("speed", layer_speed as usize),
            ("setSpeed:", layer_set_speed as usize),
            ("timeOffset", layer_offset as usize),
            ("setTimeOffset:", layer_set_offset as usize),
            ("renderInContext:", layer_render_in_context as usize),
            ("drawInContext:", layer_draw_in_context as usize),
            ("valueForKey:", layer_value_for_key as usize),
            ("setValue:forKey:", layer_set_value_for_key as usize),
        ],
    );

    let gradient = define_class("CAGradientLayer", "CALayer", 8);
    methods(
        gradient,
        &[
            ("colors", gradient_colors as usize),
            ("setColors:", gradient_set_colors as usize),
            ("locations", gradient_locations as usize),
            ("setLocations:", gradient_set_locations as usize),
            ("startPoint", gradient_start as usize),
            ("setStartPoint:", gradient_set_start as usize),
            ("endPoint", gradient_end as usize),
            ("setEndPoint:", gradient_set_end as usize),
            ("type", gradient_type as usize),
            ("setType:", gradient_set_type as usize),
        ],
    );

    let replicator = define_class("CAReplicatorLayer", "CALayer", 8);
    methods(
        replicator,
        &[
            ("instanceCount", replicator_count as usize),
            ("setInstanceCount:", replicator_set_count as usize),
            ("instanceDelay", replicator_delay as usize),
            ("setInstanceDelay:", replicator_set_delay as usize),
            ("instanceTransform", replicator_transform as usize),
            ("setInstanceTransform:", replicator_set_transform as usize),
            ("instanceColor", replicator_color as usize),
            ("setInstanceColor:", replicator_set_color as usize),
            ("preservesDepth", replicator_preserves_depth as usize),
            ("setPreservesDepth:", replicator_set_preserves_depth as usize),
            ("instanceRedOffset", replicator_red as usize),
            ("setInstanceRedOffset:", replicator_set_red as usize),
            ("instanceGreenOffset", replicator_green as usize),
            ("setInstanceGreenOffset:", replicator_set_green as usize),
            ("instanceBlueOffset", replicator_blue as usize),
            ("setInstanceBlueOffset:", replicator_set_blue as usize),
            ("instanceAlphaOffset", replicator_alpha as usize),
            ("setInstanceAlphaOffset:", replicator_set_alpha as usize),
        ],
    );

    let shape = define_class("CAShapeLayer", "CALayer", 8);
    methods(
        shape,
        &[
            ("path", shape_path as usize),
            ("setPath:", shape_set_path as usize),
            ("fillColor", shape_fill_color as usize),
            ("setFillColor:", shape_set_fill_color as usize),
            ("strokeColor", shape_stroke_color as usize),
            ("setStrokeColor:", shape_set_stroke_color as usize),
            ("lineWidth", shape_line_width as usize),
            ("setLineWidth:", shape_set_line_width as usize),
            ("strokeStart", shape_stroke_start as usize),
            ("setStrokeStart:", shape_set_stroke_start as usize),
            ("strokeEnd", shape_stroke_end as usize),
            ("setStrokeEnd:", shape_set_stroke_end as usize),
            ("lineCap", shape_line_cap as usize),
            ("setLineCap:", shape_set_line_cap as usize),
            ("lineJoin", shape_line_join as usize),
            ("setLineJoin:", shape_set_line_join as usize),
            ("lineDashPattern", shape_dash as usize),
            ("setLineDashPattern:", shape_set_dash as usize),
            ("fillRule", shape_fill_rule as usize),
            ("setFillRule:", shape_set_fill_rule as usize),
            ("miterLimit", shape_miter as usize),
            ("setMiterLimit:", shape_set_miter as usize),
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
    }

    fn new_view(frame: Rect) -> Id {
        let view = runtime::send(runtime::class_named("UIView"), "alloc", &[]);
        crate::fw::uikit::send_rect(view, "initWithFrame:", frame);
        view
    }

    fn call_point(object: Id, name: &str) -> Point {
        unboxed::<Point>(object, name).unwrap()
    }

    /// Layers convert points and rects through positions, anchors, bounds
    /// origins and transforms, views' layers by their views' geometry; nil
    /// is the root layer's space (a window's).
    #[test]
    fn layers_convert_points_and_rects() {
        setup();
        let window = runtime::send(runtime::class_named("UIWindow"), "alloc", &[]);
        crate::fw::uikit::send_rect(window, "initWithFrame:", Rect::new(0.0, 0.0, 320.0, 480.0));
        let view = new_view(Rect::new(10.0, 20.0, 100.0, 100.0));
        runtime::send(window, "addSubview:", &[view]);
        let host = runtime::send(view, "layer", &[]);
        let own = layer_class_layer(runtime::class_named("CALayer"), 0);
        layer_set_bounds(own, 0, Rect::new(0.0, 0.0, 10.0, 10.0));
        layer_set_position(own, 0, Point { x: 50.0, y: 50.0 });
        layer_add_sublayer(host, 0, own);
        let scaled = layer_class_layer(runtime::class_named("CALayer"), 0);
        layer_set_bounds(scaled, 0, Rect::new(0.0, 0.0, 10.0, 10.0));
        layer_set_position(scaled, 0, Point { x: 50.0, y: 50.0 });
        layer_set_affine_transform(scaled, 0, Transform { a: 2.0, b: 0.0, c: 0.0, d: 2.0, tx: 0.0, ty: 0.0 });
        layer_add_sublayer(host, 0, scaled);
        let p = |x: f64, y: f64| Point { x, y };
        assert_eq!(layer_convert_point_to(own, 0, p(0.0, 0.0), host), p(45.0, 45.0));
        assert_eq!(layer_convert_point_to(own, 0, p(0.0, 0.0), 0), p(55.0, 65.0), "nil: the window's layer");
        assert_eq!(layer_convert_point_from(own, 0, p(55.0, 65.0), 0), p(0.0, 0.0));
        assert_eq!(layer_convert_point_to(scaled, 0, p(0.0, 0.0), host), p(40.0, 40.0), "scaled around its anchor");
        assert_eq!(layer_convert_point_from(scaled, 0, p(0.0, 0.0), own), p(2.5, 2.5));
        assert_eq!(layer_convert_rect_to(scaled, 0, Rect::new(0.0, 0.0, 10.0, 10.0), host), Rect::new(40.0, 40.0, 20.0, 20.0));
        assert_eq!(layer_convert_rect_from(host, 0, Rect::new(0.0, 0.0, 10.0, 10.0), own), Rect::new(45.0, 45.0, 10.0, 10.0));
    }

    /// Layers put above or below a subview's layer, or in place of one of
    /// the game's, sit among the subviews as in Apple's one sublayer list.
    #[test]
    fn sublayers_go_among_subviews_where_put() {
        setup();
        let view = new_view(Rect::new(0.0, 0.0, 100.0, 100.0));
        let (first, second) = (new_view(Rect::new(0.0, 0.0, 10.0, 10.0)), new_view(Rect::new(0.0, 0.0, 10.0, 10.0)));
        runtime::send(view, "addSubview:", &[first]);
        runtime::send(view, "addSubview:", &[second]);
        let host = runtime::send(view, "layer", &[]);
        let layer_of = |v: Id| runtime::send(v, "layer", &[]);
        let class = runtime::class_named("CALayer");
        let (below, above, swapped) = (layer_class_layer(class, 0), layer_class_layer(class, 0), layer_class_layer(class, 0));
        layer_insert_below(host, 0, below, layer_of(second));
        layer_insert_above(host, 0, above, layer_of(first));
        assert_eq!(children(host), [Child::View(first), Child::Layer(above), Child::Layer(below), Child::View(second)]);
        layer_replace_sublayer(host, 0, below, swapped);
        assert_eq!(children(host), [Child::View(first), Child::Layer(above), Child::Layer(swapped), Child::View(second)]);
    }

    /// A new anchor point keeps the layer's position and moves its frame;
    /// a view's center is its layer's position, so it stays too, and later
    /// centers and sizes keep to the anchor (UIKit's rules).
    #[test]
    fn anchor_point_keeps_the_position_and_moves_the_frame() {
        setup();
        let view = new_view(Rect::new(100.0, 100.0, 40.0, 20.0));
        let layer = runtime::send(view, "layer", &[]);
        assert_eq!(view_of(layer), view);
        assert_eq!(call_point(layer, "position"), Point { x: 120.0, y: 110.0 });
        layer_set_anchor(layer, 0, Point { x: 0.0, y: 0.0 });
        assert_eq!(call_point(layer, "position"), Point { x: 120.0, y: 110.0 });
        assert_eq!(view_state(view).frame, Rect::new(120.0, 110.0, 40.0, 20.0));
        assert_eq!(call_point(view, "center"), Point { x: 120.0, y: 110.0 });
        layer_set_position(layer, 0, Point { x: 10.0, y: 10.0 });
        assert_eq!(view_state(view).frame.origin, Point { x: 10.0, y: 10.0 });
        crate::fw::uikit::send_rect(view, "setBounds:", Rect::new(0.0, 0.0, 80.0, 80.0));
        assert_eq!(view_state(view).frame, Rect::new(10.0, 10.0, 80.0, 80.0), "bounds keep the anchor in place");
        // A layer of its own: frame from position, size and anchor.
        let own = layer_class_layer(runtime::class_named("CALayer"), 0);
        assert_eq!(view_of(own), 0);
        layer_set_frame(own, 0, Rect::new(0.0, 0.0, 10.0, 10.0));
        assert_eq!(call_point(own, "position"), Point { x: 5.0, y: 5.0 });
        layer_set_anchor(own, 0, Point { x: 1.0, y: 1.0 });
        assert_eq!(frame(own), Rect::new(-5.0, -5.0, 10.0, 10.0));
        // A transformed layer's frame holds its turned bounds; setting one
        // sizes the bounds to fit it.
        let turned = layer_class_layer(runtime::class_named("CALayer"), 0);
        layer_set_frame(turned, 0, Rect::new(0.0, 0.0, 10.0, 20.0));
        let quarter = std::f64::consts::FRAC_PI_2;
        layer_set_affine_transform(turned, 0, Transform { a: quarter.cos(), b: quarter.sin(), c: -quarter.sin(), d: quarter.cos(), tx: 0.0, ty: 0.0 });
        let near = |a: Rect, b: Rect| (a.origin.x - b.origin.x).abs() + (a.origin.y - b.origin.y).abs() + (a.size.width - b.size.width).abs() + (a.size.height - b.size.height).abs() < 1e-9;
        assert!(near(frame(turned), Rect::new(-5.0, 5.0, 20.0, 10.0)), "{:?}", frame(turned));
        layer_set_frame(turned, 0, Rect::new(0.0, 0.0, 40.0, 20.0));
        assert!(near(bounds(turned), Rect::new(0.0, 0.0, 20.0, 40.0)), "{:?}", bounds(turned));
        assert!(near(frame(turned), Rect::new(0.0, 0.0, 40.0, 20.0)), "{:?}", frame(turned));
        // A view's layer opacity is its alpha.
        layer_set_opacity(layer, 0, 0.25);
        assert_eq!(property_get(view, "alpha").map(f64::from_bits), Some(0.25));
    }

    /// Layer time: a stopped layer (speed 0) keeps its timeOffset, which
    /// is how games pause animations; times convert both ways between layers.
    #[test]
    fn layer_time_follows_speed_offset_and_superlayers() {
        setup();
        let class = runtime::class_named("CALayer");
        let (outer, inner) = (layer_class_layer(class, 0), layer_class_layer(class, 0));
        layer_add_sublayer(outer, 0, inner);
        assert_eq!(layer_superlayer(inner, 0), outer);
        assert_eq!(layer_convert_time(inner, 0, 100.0, 0), 100.0);
        layer_set_speed(outer, 0, 2.0);
        layer_set_begin(outer, 0, 10.0);
        assert_eq!(local_time(inner, 20.0), 20.0, "(20 - 10) * 2");
        assert_eq!(global_time(inner, 20.0), Some(20.0));
        assert_eq!(layer_convert_time(outer, 0, 30.0, 0), 40.0);
        assert_eq!(layer_convert_time_to(inner, 0, 40.0, 0), 30.0);
        // Paused at 7: the layer's time stays 7 whatever the clock says.
        layer_set_speed(inner, 0, 0.0);
        layer_set_offset(inner, 0, 7.0);
        assert_eq!((local_time(inner, 50.0), local_time(inner, 90.0)), (7.0, 7.0));
        assert_eq!(global_time(inner, 8.0), None);
        layer_remove_from_superlayer(inner, 0);
        assert_eq!((layer_superlayer(inner, 0), layer_sublayers(outer, 0)), (0, 0));
    }

    /// Key-value coding: known keys go through their accessors (opacity of
    /// a view's layer is the view's alpha), any other key is kept for the game.
    #[test]
    fn layers_keep_any_key() {
        setup();
        let layer = layer_class_layer(runtime::class_named("CAGradientLayer"), 0);
        let tag = string::make("tag");
        layer_set_value_for_key(layer, 0, string::make("hello"), tag);
        assert_eq!(string::get(layer_value_for_key(layer, 0, tag)), "hello");
        layer_set_value_for_key(layer, 0, collections::number_float(0.5), string::make("opacity"));
        assert_eq!(layer_opacity(layer, 0), 0.5);
        assert_eq!(number_value(layer_value_for_key(layer, 0, string::make("opacity"))), 0.5);
        let red = crate::fw::graphics::make_color([1.0, 0.0, 0.0, 1.0]);
        let blue = crate::fw::graphics::make_color([0.0, 0.0, 1.0, 1.0]);
        gradient_set_colors(layer, 0, collections::array_from(&[red, blue]));
        assert_eq!(gradient_stops(layer), vec![([1.0, 0.0, 0.0, 1.0], 0.0), ([0.0, 0.0, 1.0, 1.0], 1.0)]);
        assert_eq!(gradient_points(layer), (Point { x: 0.5, y: 0.0 }, Point { x: 0.5, y: 1.0 }));
    }
}
