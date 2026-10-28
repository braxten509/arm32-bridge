//! -[CALayer renderInContext:]: a layer and everything inside it drawn into
//! a context, in the layer's own space (the context's user space is the
//! layer's: a scrolled layer's bounds origin is not moved to 0, 0), as Core
//! Animation renders it.
//!
//! Drawn: background colours (round corners too), a layer's contents
//! picture (stretched over its bounds) or else its own -drawInContext:,
//! gradient layers, shape layers, picture views' images (stretchable ones
//! in nine parts; a button's parts are such views), label text, borders on
//! top, replicator copies; each layer placed by its position, anchor point,
//! bounds and transform and its superlayer's sublayerTransform, in its
//! superlayer's list order (zPosition only orders the screen), clipped
//! (round corners too) where it masks to its bounds. A layer that is see-
//! through or casts a shadow is drawn whole in a transparency layer, then
//! put down with its opacity and shadow. Not drawn here: masks (Apple
//! doesn't either).

use super::*;
use crate::fw::layers::{self, Child};

pub fn render_layer(context: Id, layer: Id) {
    if layer == 0 || canvas("-[CALayer renderInContext:]", context).is_none() {
        return;
    }
    // Core Animation lays the tree out first: a view never shown (a card
    // made just for its picture) adds its parts in -layoutSubviews.
    let view = layers::view_of(layer);
    if view != 0 {
        runtime::send(view, "layoutIfNeeded", &[]);
        crate::fw::uikit::display_tree(view);
    }
    context_save_gstate(context);
    draw_layer(context, layer, 0);
    context_restore_gstate(context);
}

fn is_kind(object: Id, name: &str) -> bool {
    let class = runtime::class_named(name);
    let rt = lock();
    object != 0 && class != 0 && rt.is_subclass(rt.class_of(object), class)
}

/// The layers drawn inside `layer`, back to front: a view's subviews'
/// layers with the layers the game added among them, in their list's
/// order (-[CALayer _renderSublayersInContext:] doesn't sort by zPosition).
fn children(layer: Id) -> Vec<Id> {
    let mut list: Vec<Id> = layers::children(layer)
        .into_iter()
        .map(|child| match child {
            Child::View(view) => runtime::send(view, "layer", &[]),
            Child::Layer(layer) => layer,
        })
        .collect();
    list.retain(|&l| l != 0);
    list
}

/// The layer's sublayerTransform as a step in its own space: applied around
/// its anchor point, as Core Animation applies it to each sublayer.
fn sublayer_step(layer: Id, bounds: Rect) -> Transform {
    let anchor = layers::anchor(layer);
    let at = Transform { tx: bounds.origin.x + bounds.size.width * anchor.x, ty: bounds.origin.y + bounds.size.height * anchor.y, ..IDENTITY };
    let turn = layers::affine_of(layers::sublayer_transform(layer));
    geometry::concat(geometry::concat(geometry::invert(at).unwrap_or(IDENTITY), turn), at)
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
fn draw_layer(context: Id, layer: Id, depth: usize) {
    if depth > 64 || layers::hidden(layer) {
        return;
    }
    let opacity = layers::opacity(layer);
    if opacity <= 0.0 {
        return;
    }
    let bounds = layers::bounds(layer);
    let looks = layers::looks(layer);
    context_save_gstate(context);
    // See-through or shadowed: drawn whole on its own, then put down with
    // its opacity and shadow (one faded picture, its parts not showing
    // through each other). The shadow's offset is in the layer's space.
    let grouped = opacity < 1.0 || looks.shadow.is_some();
    if grouped {
        let ctm = context_get_ctm(context);
        let scale = (ctm.a * ctm.d - ctm.b * ctm.c).abs().sqrt();
        let (offset, blur, color) = match looks.shadow {
            // Core Animation asks Quartz for a blur of twice the radius (renderInContext: +0x36e).
            Some((color, radius, (x, y), _)) => ((ctm.a * x + ctm.c * y, ctm.b * x + ctm.d * y), 2.0 * radius * scale, Some(color)),
            None => ((0.0, 0.0), 0.0, None),
        };
        shapes::set_shadow_and_alpha_in(context, offset, blur, color, (opacity < 1.0).then_some(opacity));
        shapes::begin_layer_in(context);
    }
    // A layer that masks to its bounds cuts its own contents too, not just its sublayers.
    if layers::masks(layer) {
        clip_to_round_rect_in(context, bounds, looks.corner_radius);
    }
    draw_own(context, layer, bounds, looks.corner_radius);
    let (copies, step, _) = layers::replication(layer);
    // Each copy one step further, the step turning around the replicator's middle.
    let middle = Transform { tx: bounds.origin.x + bounds.size.width / 2.0, ty: bounds.origin.y + bounds.size.height / 2.0, ..IDENTITY };
    let step = geometry::concat(geometry::concat(geometry::invert(middle).unwrap_or(IDENTITY), layers::affine_of(step)), middle);
    let inner = sublayer_step(layer, bounds);
    for child in children(layer) {
        let mut offset = IDENTITY;
        for _ in 0..copies {
            context_save_gstate(context);
            context_concat_ctm(context, geometry::concat(geometry::concat(placement(child), offset), inner));
            draw_layer(context, child, depth + 1);
            context_restore_gstate(context);
            offset = geometry::concat(offset, step);
        }
    }
    // The border on top of everything inside, inside the edge.
    if let Some((color, width)) = looks.border {
        if let Some(mut c) = canvas("-[CALayer renderInContext:]", context) {
            let to_device = c.to_device();
            let mut outline = shapes::Outline::default();
            outline.rounded_rect(bounds, looks.corner_radius, &to_device);
            let inner = Rect::new(bounds.origin.x + width, bounds.origin.y + width, bounds.size.width - 2.0 * width, bounds.size.height - 2.0 * width);
            if inner.size.width > 0.0 && inner.size.height > 0.0 {
                outline.rounded_rect(inner, (looks.corner_radius - width).max(0.0), &to_device);
            }
            let saved = c.state.fill;
            c.state.fill = color;
            c.fill_outline(&outline, true);
            c.state.fill = saved;
        }
    }
    if grouped {
        shapes::end_layer_in(context);
    }
    context_restore_gstate(context);
}

fn clip_to_round_rect_in(context: Id, rect: Rect, radius: f64) {
    if let Some(mut c) = canvas("-[CALayer renderInContext:]", context) {
        let mut outline = shapes::Outline::default();
        let to_device = c.to_device();
        outline.rounded_rect(rect, radius, &to_device);
        c.clip_to(&outline, false);
    }
}

/// What the layer itself shows, under its sublayers (the background's
/// corners rounded by `radius`).
fn draw_own(context: Id, layer: Id, bounds: Rect, radius: f64) {
    if let Some(color) = color_rgba(layers::background(layer)) {
        if let Some(mut c) = canvas("-[CALayer renderInContext:]", context) {
            let saved = c.state.fill;
            c.state.fill = color;
            if radius > 0.0 {
                let mut outline = shapes::Outline::default();
                outline.rounded_rect(bounds, radius, &c.to_device());
                c.fill_outline(&outline, false);
            } else {
                c.fill_rect(bounds);
            }
            c.state.fill = saved;
        }
    }
    let contents = layers::contents(layer);
    if contents != 0 {
        draw_upright(context, image_cg_or_self(contents), 0, bounds);
    } else {
        // No backing picture: the layer draws itself straight into the
        // context (-drawInContext:, by default its delegate's -drawLayer:inContext:).
        context_save_gstate(context);
        runtime::send(layer, "drawInContext:", &[context]);
        context_restore_gstate(context);
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
        let (path, fill, stroke, even_odd) = layers::shape_look(layer);
        let style = layers::shape_line(layer);
        if path != 0 {
            if let Some(mut c) = canvas("-[CALayer renderInContext:]", context) {
                shapes::draw_path_on(&mut c, path, color_rgba(fill), color_rgba(stroke).map(|s| (s, &style)), even_odd);
            }
        }
    }
    let view = layers::view_of(layer);
    if view == 0 {
        return;
    }
    // A button's background, picture and title are its subviews (one of them
    // pressed or disabled drawn darker, by its draw mode).
    if is_kind(view, "UIImageView") {
        let picture = uikit::image_view_drawn(view, uikit::image_view_shown(view));
        if picture != 0 {
            let mode = uikit::property_get(view, "contentMode").unwrap_or(0) as i64;
            let size = uikit::image_points(picture);
            draw_picture_in(context, picture, content_rect(bounds, (size.width, size.height), mode));
        }
    }
    // A label that draws itself drew its text into its contents (above).
    if is_kind(view, "UILabel") && !uikit::draws_itself(view) {
        if let Some(look) = crate::fw::controls::label_look(view) {
            draw_text_look("-[CALayer renderInContext:]", context, &look, Rect { origin: bounds.origin, size: Size { width: look.size.0, height: look.size.1 } });
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

/// Where a picture of `size` lands in `bounds` for a UIViewContentMode (as
/// the phone's ContentMode.place): scale to fill (0) and redraw (3) stretch
/// it over the bounds, aspect fit (1) and fill (2) scale it keeping its
/// shape, the rest keep its own size at an edge or corner: 4 center, 5 top,
/// 6 bottom, 7 left, 8 right, 9 top left, 10 top right, 11 bottom left,
/// 12 bottom right.
pub(super) fn content_rect(bounds: Rect, size: (f64, f64), mode: i64) -> Rect {
    let (w, h) = (bounds.size.width, bounds.size.height);
    if mode == 0 || mode == 3 || size.0 <= 0.0 || size.1 <= 0.0 {
        return bounds;
    }
    let k = match mode {
        1 => (w / size.0).min(h / size.1),
        2 => (w / size.0).max(h / size.1),
        _ => 1.0,
    };
    let (sw, sh) = (size.0 * k, size.1 * k);
    let x = match mode {
        7 | 9 | 11 => 0.0,
        8 | 10 | 12 => w - sw,
        _ => (w - sw) / 2.0,
    };
    let y = match mode {
        5 | 9 | 10 => 0.0,
        6 | 11 | 12 => h - sh,
        _ => (h - sh) / 2.0,
    };
    Rect::new(bounds.origin.x + x, bounds.origin.y + y, sw, sh)
}

/// A UIImage drawn over `rect` as -drawInRect: draws it (stretchable ones
/// in nine parts).
fn draw_picture_in(context: Id, picture: Id, rect: Rect) {
    let Some(shown) = shown_image(picture) else { return };
    if let Some(mut c) = canvas("-[CALayer renderInContext:]", context) {
        draw_shown(&mut c, &shown, rect);
    }
}

/// A picture drawn upright in UIKit's space (y down), turned by `orientation`.
fn draw_upright(context: Id, picture: Id, orientation: i64, rect: Rect) {
    let Some(bitmap) = drawable(picture) else { return };
    if let Some(mut c) = canvas("-[CALayer renderInContext:]", context) {
        c.draw_bitmap(rect, &bitmap, &|u, v| orient(orientation, u, 1.0 - v));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fw::layers::send_struct;

    fn setup() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
    }

    fn view(class: &str, frame: Rect, rgba: Option<[f64; 4]>) -> Id {
        let view = runtime::send(runtime::class_named(class), "alloc", &[]);
        uikit::send_rect(view, "initWithFrame:", frame);
        if let Some(rgba) = rgba {
            runtime::send(view, "setBackgroundColor:", &[make_color(rgba)]);
        }
        view
    }

    fn plain_layer(class: &str, frame: Rect, rgba: Option<[f64; 4]>) -> Id {
        let layer = runtime::send(runtime::class_named(class), "layer", &[]);
        send_struct(layer, "setFrame:", frame);
        if let Some(rgba) = rgba {
            runtime::send(layer, "setBackgroundColor:", &[make_color(rgba)]);
        }
        layer
    }

    /// `layer` rendered into a new `width` x `height` UIKit context (white
    /// first when `white`): straight RGBA rows from the top.
    fn picture(layer: Id, width: f64, height: f64, white: bool) -> Vec<u8> {
        let context = test_begin_context(width, height);
        if white {
            test_fill(Rect::new(0.0, 0.0, width, height), [1.0, 1.0, 1.0, 1.0]);
        }
        runtime::send(layer, "renderInContext:", &[context]);
        test_end_context()
    }

    fn at(pixels: &[u8], width: usize, x: usize, y: usize) -> [u8; 4] {
        pixels[(y * width + x) * 4..(y * width + x) * 4 + 4].try_into().unwrap()
    }

    fn near(a: [u8; 4], b: [u8; 4]) -> bool {
        a.iter().zip(b).all(|(&a, b)| a.abs_diff(b) <= 2)
    }

    /// A half see-through view is drawn as one flat picture faded as a
    /// whole (Core Animation's transparency layer), not part by part: its
    /// red part under its blue one doesn't show through.
    #[test]
    fn a_faded_view_fades_as_one_picture() {
        setup();
        let holder = view("UIView", Rect::new(0.0, 0.0, 4.0, 4.0), None);
        send_struct(holder, "setAlpha:", 0.5f64);
        for rgba in [[1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]] {
            runtime::send(holder, "addSubview:", &[view("UIView", Rect::new(0.0, 0.0, 4.0, 4.0), Some(rgba))]);
        }
        let pixels = picture(runtime::send(holder, "layer", &[]), 4.0, 4.0, true);
        assert!(near(at(&pixels, 4, 1, 1), [128, 128, 255, 255]), "{:?}", at(&pixels, 4, 1, 1));
    }

    /// Sublayers are drawn in their list's order (zPosition only orders
    /// the screen), and the layer in its own space: a scrolled root's
    /// sublayer at y 10 lands at y 10.
    #[test]
    fn sublayers_draw_in_list_order_in_the_layers_own_space() {
        setup();
        let holder = plain_layer("CALayer", Rect::new(0.0, 0.0, 4.0, 4.0), None);
        let (red, blue) = (plain_layer("CALayer", Rect::new(0.0, 0.0, 4.0, 4.0), Some([1.0, 0.0, 0.0, 1.0])), plain_layer("CALayer", Rect::new(0.0, 0.0, 4.0, 4.0), Some([0.0, 0.0, 1.0, 1.0])));
        send_struct(red, "setZPosition:", 1.0f64);
        runtime::send(holder, "addSublayer:", &[red]);
        runtime::send(holder, "addSublayer:", &[blue]);
        assert_eq!(at(&picture(holder, 4.0, 4.0, false), 4, 1, 1), [0, 0, 255, 255], "the later sublayer is on top");
        let scrolled = view("UIView", Rect::new(0.0, 0.0, 10.0, 10.0), None);
        uikit::send_rect(scrolled, "setBounds:", Rect::new(0.0, 10.0, 10.0, 10.0));
        runtime::send(runtime::send(scrolled, "layer", &[]), "addSublayer:", &[plain_layer("CALayer", Rect::new(0.0, 10.0, 10.0, 10.0), Some([1.0, 0.0, 0.0, 1.0]))]);
        let pixels = picture(runtime::send(scrolled, "layer", &[]), 10.0, 20.0, false);
        let red_rows: Vec<usize> = (0..20).filter(|&y| at(&pixels, 10, 5, y) == [255, 0, 0, 255]).collect();
        assert_eq!(red_rows, (10..20).collect::<Vec<_>>());
    }

    /// A layer's shadow is drawn under it (offset in the layer's space,
    /// down on screen), and a superlayer's sublayerTransform moves what is
    /// inside it.
    #[test]
    fn shadows_and_sublayer_transforms_are_drawn() {
        setup();
        let holder = plain_layer("CALayer", Rect::new(0.0, 0.0, 12.0, 12.0), None);
        let caster = plain_layer("CALayer", Rect::new(2.0, 2.0, 4.0, 4.0), Some([0.0, 0.0, 1.0, 1.0]));
        runtime::send(caster, "setShadowColor:", &[make_color([1.0, 0.0, 0.0, 1.0])]);
        send_struct(caster, "setShadowOpacity:", 1.0f32);
        send_struct(caster, "setShadowRadius:", 0.0f64);
        send_struct(caster, "setShadowOffset:", Size { width: 5.0, height: 5.0 });
        runtime::send(holder, "addSublayer:", &[caster]);
        let pixels = picture(holder, 12.0, 12.0, false);
        assert_eq!(at(&pixels, 12, 3, 3), [0, 0, 255, 255], "the layer");
        assert_eq!(at(&pixels, 12, 8, 8), [255, 0, 0, 255], "its shadow, right and down");
        assert_eq!(at(&pixels, 12, 8, 3)[3], 0, "nothing else");
        let moved = plain_layer("CALayer", Rect::new(0.0, 0.0, 10.0, 10.0), None);
        runtime::send(moved, "addSublayer:", &[plain_layer("CALayer", Rect::new(0.0, 0.0, 5.0, 10.0), Some([1.0, 0.0, 0.0, 1.0]))]);
        send_struct(moved, "setSublayerTransform:", crate::fw::layers::affine_to_3d(Transform { tx: 5.0, ..IDENTITY }));
        let pixels = picture(moved, 10.0, 10.0, false);
        assert_eq!((at(&pixels, 10, 7, 5), at(&pixels, 10, 2, 5)[3]), ([255, 0, 0, 255], 0));
    }

    /// A layer's shadow is blurred as Core Animation asks Quartz to blur it:
    /// CGContextSetShadowWithColor with shadowRadius + shadowRadius (a
    /// Gaussian of deviation shadowRadius). Five points out from a radius-4
    /// shadow's edge, about a tenth of it still shows.
    #[test]
    fn shadows_blur_by_twice_their_radius() {
        setup();
        let holder = plain_layer("CALayer", Rect::new(0.0, 0.0, 44.0, 44.0), None);
        let caster = plain_layer("CALayer", Rect::new(12.0, 12.0, 20.0, 20.0), Some([0.0, 0.0, 1.0, 1.0]));
        runtime::send(caster, "setShadowColor:", &[make_color([1.0, 0.0, 0.0, 1.0])]);
        send_struct(caster, "setShadowOpacity:", 1.0f32);
        send_struct(caster, "setShadowRadius:", 4.0f64);
        send_struct(caster, "setShadowOffset:", Size { width: 0.0, height: 0.0 });
        runtime::send(holder, "addSublayer:", &[caster]);
        let pixels = picture(holder, 44.0, 44.0, false);
        let alpha = at(&pixels, 44, 7, 22)[3];
        assert!((15..=45).contains(&alpha), "alpha 5 points out: {alpha}");
    }

    /// A layer with no contents draws itself into the context through
    /// -drawInContext: (a CALayer subclass's own drawing).
    #[test]
    fn a_layer_without_contents_draws_in_the_context() {
        setup();
        extern "C" fn draw(_this: Id, _: Sel, context: Id) {
            context_set_rgb_fill_color(context, 0.0, 1.0, 0.0, 1.0);
            context_fill_rect(context, Rect::new(0.0, 0.0, 2.0, 2.0));
        }
        let class = crate::fw::object::define_class("G32SelfDrawnLayer", "CALayer", 8);
        crate::fw::object::methods(class, &[("drawInContext:", draw as usize)]);
        let holder = plain_layer("CALayer", Rect::new(0.0, 0.0, 4.0, 4.0), None);
        runtime::send(holder, "addSublayer:", &[plain_layer("G32SelfDrawnLayer", Rect::new(2.0, 2.0, 2.0, 2.0), None)]);
        let pixels = picture(holder, 4.0, 4.0, false);
        assert_eq!((at(&pixels, 4, 3, 3), at(&pixels, 4, 1, 1)[3]), ([0, 255, 0, 255], 0));
    }

    /// A button's background picture (its parts are subviews on Apple) and
    /// an image view's highlighted picture show in a picture of them.
    #[test]
    fn buttons_and_highlighted_image_views_show_their_pictures() {
        setup();
        let button = view("UIButton", Rect::new(0.0, 0.0, 20.0, 10.0), None);
        let red = test_solid_image(20.0, 10.0, [1.0, 0.0, 0.0, 1.0]);
        runtime::send(button, "setBackgroundImage:forState:", &[red, 0]);
        assert_eq!(at(&picture(runtime::send(button, "layer", &[]), 20.0, 10.0, false), 20, 10, 5), [255, 0, 0, 255]);
        let shown = view("UIImageView", Rect::new(0.0, 0.0, 4.0, 4.0), None);
        runtime::send(shown, "setImage:", &[test_solid_image(4.0, 4.0, [1.0, 0.0, 0.0, 1.0])]);
        runtime::send(shown, "setHighlightedImage:", &[test_solid_image(4.0, 4.0, [0.0, 0.0, 1.0, 1.0])]);
        runtime::send(shown, "setHighlighted:", &[1]);
        assert_eq!(runtime::send(shown, "isHighlighted", &[]) & 0xFF, 1);
        assert_eq!(at(&picture(runtime::send(shown, "layer", &[]), 4.0, 4.0, false), 4, 1, 1), [0, 0, 255, 255]);
    }
}
