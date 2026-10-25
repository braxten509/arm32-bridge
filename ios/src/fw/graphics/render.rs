//! -[CALayer renderInContext:]: a layer and everything inside it drawn into
//! a context, in the layer's own space (its bounds' origin at the context's
//! origin), as Core Animation renders it.
//!
//! Drawn: background colours, a layer's contents picture (stretched over
//! its bounds), gradient layers, shape layers, picture views' images, label
//! text, replicator copies; each layer placed by its position, anchor point,
//! bounds and transform, with its opacity, sorted by zPosition, clipped
//! where it masks to its bounds. Not drawn: shadows, borders, round corners
//! and masks (as on screen here).

use super::*;
use crate::fw::layers;

pub fn render_layer(context: Id, layer: Id) {
    if layer == 0 || canvas("-[CALayer renderInContext:]", context).is_none() {
        return;
    }
    let bounds = layers::bounds(layer);
    context_save_gstate(context);
    context_translate_ctm(context, -bounds.origin.x, -bounds.origin.y);
    draw_layer(context, layer, 1.0, 0);
    context_restore_gstate(context);
}

fn is_kind(object: Id, name: &str) -> bool {
    let class = runtime::class_named(name);
    let rt = lock();
    object != 0 && class != 0 && rt.is_subclass(rt.class_of(object), class)
}

/// The layers drawn inside `layer`, back to front: a view's subviews'
/// layers, then the layers the game added, in zPosition order.
fn children(layer: Id) -> Vec<Id> {
    let mut list = Vec::new();
    let view = layers::view_of(layer);
    if view != 0 {
        for child in uikit::view_state(view).subviews.clone() {
            list.push(runtime::send(child, "layer", &[]));
        }
    }
    list.extend(layers::sublayers_of(layer));
    list.retain(|&l| l != 0);
    list.sort_by(|&a, &b| layers::z_position(a).total_cmp(&layers::z_position(b)));
    list
}

/// From the layer's own space to its parent's: bounds origin and anchor,
/// then its transform, then its position.
fn placement(layer: Id) -> Transform {
    let (bounds, anchor, position) = (layers::bounds(layer), layers::anchor(layer), layers::position(layer));
    let to_anchor = Transform { tx: -bounds.origin.x - bounds.size.width * anchor.x, ty: -bounds.origin.y - bounds.size.height * anchor.y, ..IDENTITY };
    let turn = layers::affine_of(layers::transform(layer));
    geometry::concat(geometry::concat(to_anchor, turn), Transform { tx: position.x, ty: position.y, ..IDENTITY })
}

/// Draws `layer` in its own space (the context set up for it).
fn draw_layer(context: Id, layer: Id, parent_alpha: f64, depth: usize) {
    if depth > 64 || layers::hidden(layer) {
        return;
    }
    let alpha = parent_alpha * layers::opacity(layer);
    if alpha <= 0.0 {
        return;
    }
    context_set_alpha(context, alpha);
    let bounds = layers::bounds(layer);
    draw_own(context, layer, bounds);
    let (copies, step) = layers::replication(layer);
    let step = layers::affine_of(step);
    if layers::masks(layer) {
        clip_to_rect_in(context, bounds);
    }
    // A replicator shows `copies` of each sublayer, each one step further.
    for child in children(layer) {
        let mut offset = IDENTITY;
        for _ in 0..copies {
            context_save_gstate(context);
            context_concat_ctm(context, geometry::concat(placement(child), offset));
            draw_layer(context, child, alpha, depth + 1);
            context_restore_gstate(context);
            offset = geometry::concat(offset, step);
        }
    }
}

fn clip_to_rect_in(context: Id, rect: Rect) {
    if let Some(mut c) = canvas("-[CALayer renderInContext:]", context) {
        let mut outline = shapes::Outline::default();
        let to_device = c.to_device();
        outline.rect(rect, &to_device);
        c.clip_to(&outline, false);
    }
}

/// What the layer itself shows, under its sublayers.
fn draw_own(context: Id, layer: Id, bounds: Rect) {
    if let Some(color) = color_rgba(layers::background(layer)) {
        if let Some(mut c) = canvas("-[CALayer renderInContext:]", context) {
            let saved = c.state.fill;
            c.state.fill = color;
            c.fill_rect(bounds);
            c.state.fill = saved;
        }
    }
    let contents = layers::contents(layer);
    if contents != 0 {
        draw_upright(context, image_cg_or_self(contents), 0, bounds);
    }
    if is_kind(layer, "CAGradientLayer") {
        let stops = layers::gradient_stops(layer);
        if !stops.is_empty() {
            let (start, end) = layers::gradient_points(layer);
            let at = |p: Point| Point { x: bounds.origin.x + p.x * bounds.size.width, y: bounds.origin.y + p.y * bounds.size.height };
            let mut sorted: Vec<(f64, [f64; 4])> = stops.into_iter().map(|(c, l)| (l, c)).collect();
            sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
            if let Some(mut c) = canvas("-[CALayer renderInContext:]", context) {
                let saved = c.state.clip.clone();
                let mut outline = shapes::Outline::default();
                let to_device = c.to_device();
                outline.rect(bounds, &to_device);
                c.clip_to(&outline, false);
                c.draw_linear_gradient(&Gradient { stops: sorted }, at(start), at(end), 3);
                c.state.clip = saved;
            }
        }
    }
    if is_kind(layer, "CAShapeLayer") {
        let (path, fill, stroke, width, even_odd) = layers::shape_look(layer);
        if path != 0 {
            if let Some(mut c) = canvas("-[CALayer renderInContext:]", context) {
                shapes::draw_path_on(&mut c, path, color_rgba(fill), color_rgba(stroke).map(|s| (s, width)), even_odd);
            }
        }
    }
    let view = layers::view_of(layer);
    if view == 0 {
        return;
    }
    if is_kind(view, "UIImageView") {
        let picture = uikit::property_get(view, "image").unwrap_or(0) as Id;
        if picture != 0 {
            let mode = uikit::property_get(view, "contentMode").unwrap_or(0) as i64;
            let size = uikit::image_points(picture);
            let rect = content_rect(bounds, (size.width, size.height), mode);
            draw_upright(context, image_cg(picture), ui_image_orientation(picture, 0), rect);
        }
    }
    if is_kind(view, "UILabel") {
        if let Some(look) = crate::fw::controls::label_look(view) {
            // At the context's own pixels per point, so text is sharp.
            let scale = canvas("-[CALayer renderInContext:]", context).map_or(1.0, |c| (c.state.ctm.a * c.state.ctm.d - c.state.ctm.b * c.state.ctm.c).abs().sqrt()).clamp(1.0, 4.0);
            if let Some(bitmap) = text::look_bitmap(&look, scale) {
                if let Some(mut c) = canvas("-[CALayer renderInContext:]", context) {
                    c.draw_bitmap(Rect { origin: bounds.origin, size: Size { width: look.size.0, height: look.size.1 } }, &bitmap, &|u, v| (u, 1.0 - v));
                }
            }
        }
    }
}

/// A layer's contents may be a CGImage, or (as some games set it) a UIImage.
fn image_cg_or_self(contents: Id) -> Id {
    if is_kind(contents, "UIImage") {
        image_cg(contents)
    } else {
        contents
    }
}

/// Where a picture of `size` lands in `bounds` for a UIViewContentMode.
fn content_rect(bounds: Rect, size: (f64, f64), mode: i64) -> Rect {
    let (w, h) = (bounds.size.width, bounds.size.height);
    if size.0 <= 0.0 || size.1 <= 0.0 {
        return bounds;
    }
    let (sw, sh) = match mode {
        0 => return bounds,
        1 | 2 => {
            let (fit, fill) = ((w / size.0).min(h / size.1), (w / size.0).max(h / size.1));
            let k = if mode == 1 { fit } else { fill };
            (size.0 * k, size.1 * k)
        }
        _ => size,
    };
    Rect::new(bounds.origin.x + (w - sw) / 2.0, bounds.origin.y + (h - sh) / 2.0, sw, sh)
}

/// A picture drawn upright in UIKit's space (y down), turned by `orientation`.
fn draw_upright(context: Id, picture: Id, orientation: i64, rect: Rect) {
    let Some(bitmap) = image(picture) else { return };
    if let Some(mut c) = canvas("-[CALayer renderInContext:]", context) {
        c.draw_bitmap(rect, &bitmap, &|u, v| orient(orientation, u, 1.0 - v));
    }
}
