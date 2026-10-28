//! UIBezierPath: a CGPath (fw/graphics/shapes.rs) with drawing settings,
//! filled and stroked into the current UIKit context, and UIColor's
//! -set, -setFill and -setStroke for it.
//!
//! UIKit's angles grow clockwise on screen (y points down), so a path's
//! "clockwise" arc is Core Graphics' counterclockwise one.

use crate::fw::geometry::{Point, Rect, Transform};
use crate::fw::graphics;
use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::uikit::{property, property_get, property_set};
use crate::objc::runtime::{self, Class, Id, Sel};
use std::collections::HashMap;
use std::sync::Mutex;

/// The path's CGPath (made on first use).
fn path_of(this: Id) -> Id {
    match property_get(this, "cgPath") {
        Some(path) if path != 0 => path as Id,
        _ => {
            let path = graphics::new_path();
            property_set(this, "cgPath", path as u64);
            path
        }
    }
}

fn make(class: Class) -> Id {
    runtime::send(runtime::send(class, "alloc", &[]), "init", &[])
}

extern "C" fn bezier_path(class: Class, _: Sel) -> Id {
    make(class)
}

extern "C" fn bezier_rect(class: Class, _: Sel, rect: Rect) -> Id {
    let made = make(class);
    graphics::path_rect(path_of(made), rect);
    made
}

extern "C" fn bezier_oval(class: Class, _: Sel, rect: Rect) -> Id {
    let made = make(class);
    graphics::path_oval(path_of(made), rect);
    made
}

extern "C" fn bezier_rounded(class: Class, _: Sel, rect: Rect, radius: f64) -> Id {
    let made = make(class);
    graphics::path_rounded_rect(path_of(made), rect, radius);
    made
}

extern "C" fn bezier_arc(class: Class, _: Sel, center: Point, radius: f64, start: f64, end: f64, clockwise: u8) -> Id {
    let made = make(class);
    graphics::path_arc(path_of(made), center, radius, start, end, clockwise == 0);
    made
}

extern "C" fn bezier_with_cg_path(class: Class, _: Sel, path: Id) -> Id {
    let made = make(class);
    property_set(made, "cgPath", graphics::path_copy(path) as u64);
    made
}

extern "C" fn bezier_init(this: Id, _: Sel) -> Id {
    path_of(this);
    this
}

extern "C" fn bezier_move(this: Id, _: Sel, point: Point) {
    graphics::path_move(path_of(this), point);
}

extern "C" fn bezier_line(this: Id, _: Sel, point: Point) {
    graphics::path_line(path_of(this), point);
}

extern "C" fn bezier_curve(this: Id, _: Sel, end: Point, c1: Point, c2: Point) {
    graphics::path_curve(path_of(this), c1, c2, end);
}

extern "C" fn bezier_quad(this: Id, _: Sel, end: Point, control: Point) {
    graphics::path_quad(path_of(this), control, end);
}

extern "C" fn bezier_add_arc(this: Id, _: Sel, center: Point, radius: f64, start: f64, end: f64, clockwise: u8) {
    graphics::path_arc(path_of(this), center, radius, start, end, clockwise == 0);
}

extern "C" fn bezier_close(this: Id, _: Sel) {
    graphics::path_close(path_of(this));
}

extern "C" fn bezier_remove_all(this: Id, _: Sel) {
    property_set(this, "cgPath", graphics::new_path() as u64);
}

extern "C" fn bezier_append(this: Id, _: Sel, other: Id) {
    if other != 0 {
        graphics::path_append(path_of(this), path_of(other));
    }
}

extern "C" fn bezier_apply(this: Id, _: Sel, t: Transform) {
    graphics::path_apply(path_of(this), t);
}

extern "C" fn bezier_cg_path(this: Id, _: Sel) -> Id {
    path_of(this)
}

extern "C" fn bezier_set_cg_path(this: Id, _: Sel, path: Id) {
    let path = if path == 0 { graphics::new_path() } else { graphics::path_copy(path) };
    property_set(this, "cgPath", path as u64);
}

extern "C" fn bezier_is_empty(this: Id, _: Sel) -> bool {
    graphics::path_is_empty(path_of(this))
}

extern "C" fn bezier_bounds(this: Id, _: Sel) -> Rect {
    graphics::path_bounds(path_of(this))
}

extern "C" fn bezier_current_point(this: Id, _: Sel) -> Point {
    graphics::path_current(path_of(this)).unwrap_or_default()
}

extern "C" fn bezier_contains(this: Id, _: Sel, point: Point) -> bool {
    graphics::path_contains(path_of(this), point, bezier_even_odd(this, 0))
}

property!(bezier_line_width, bezier_set_line_width, "lineWidth", f64, 1.0);
property!(bezier_even_odd, bezier_set_even_odd, "usesEvenOdd", bool, false);
property!(bezier_cap, bezier_set_cap, "lineCap", isize, 0);
property!(bezier_join, bezier_set_join, "lineJoin", isize, 0);
property!(bezier_miter, bezier_set_miter, "miterLimit", f64, 10.0);
property!(bezier_flatness, bezier_set_flatness, "flatness", f64, 0.6);

/// Fills the path in the current UIKit context with its fill colour.
extern "C" fn bezier_fill(this: Id, _: Sel) {
    let context = graphics::current_ui_context();
    if context != 0 {
        graphics::fill_path_in(context, path_of(this), bezier_even_odd(this, 0));
    }
}

/// Strokes the path in the current UIKit context with its stroke colour and
/// its own width, cap, join, miter limit and dash.
extern "C" fn bezier_stroke(this: Id, _: Sel) {
    let context = graphics::current_ui_context();
    if context != 0 {
        let style = graphics::StrokeStyle {
            width: bezier_line_width(this, 0),
            cap: bezier_cap(this, 0) as i32,
            join: bezier_join(this, 0) as i32,
            miter_limit: bezier_miter(this, 0),
            dash: DASHES.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|d| d.get(&this).cloned()),
        };
        graphics::stroke_path_in(context, path_of(this), &style);
    }
}

/// The blend mode and alpha are set for the drawing (CGContextSetBlendMode,
/// CGContextSetAlpha), not added to the context's.
extern "C" fn bezier_fill_blend(this: Id, s: Sel, mode: i32, alpha: f64) {
    with_alpha(mode, alpha, || bezier_fill(this, s));
}

extern "C" fn bezier_stroke_blend(this: Id, s: Sel, mode: i32, alpha: f64) {
    with_alpha(mode, alpha, || bezier_stroke(this, s));
}

fn with_alpha(mode: i32, alpha: f64, draw: impl FnOnce()) {
    let context = graphics::current_ui_context();
    if context == 0 {
        return;
    }
    graphics::save_state(context);
    graphics::set_alpha_and_blend(context, alpha, mode);
    draw();
    graphics::restore_state(context);
}

extern "C" fn bezier_add_clip(this: Id, _: Sel) {
    let context = graphics::current_ui_context();
    if context != 0 {
        graphics::clip_path_in(context, path_of(this), bezier_even_odd(this, 0));
    }
}

/// Paths' dashes (phase, lengths), by path.
static DASHES: Mutex<Option<HashMap<Id, (f64, Vec<f64>)>>> = Mutex::new(None);

/// -setLineDash:count:phase: (no pattern: lines whole again).
extern "C" fn bezier_set_dash(this: Id, _: Sel, pattern: *const f64, count: isize, phase: f64) {
    let mut dashes = DASHES.lock().unwrap_or_else(|e| e.into_inner());
    let dashes = dashes.get_or_insert_with(HashMap::new);
    if pattern.is_null() || count <= 0 {
        dashes.remove(&this);
    } else {
        // At most as many lengths as a 32-bit game's -getLineDash:count:phase:
        // has room for (guest32/objc/convert.rs COUNTED_OUT_MOST).
        dashes.insert(this, (phase, unsafe { std::slice::from_raw_parts(pattern, (count as usize).min(MOST_DASHES)) }.to_vec()));
    }
}

const MOST_DASHES: usize = 4096;

/// -getLineDash:count:phase: (each out pointer may be NULL).
extern "C" fn bezier_get_dash(this: Id, _: Sel, pattern: *mut f64, count: *mut isize, phase: *mut f64) {
    let dash = DASHES.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|d| d.get(&this).cloned());
    let (start, lengths) = dash.unwrap_or_default();
    unsafe {
        if !pattern.is_null() {
            std::ptr::copy_nonoverlapping(lengths.as_ptr(), pattern, lengths.len());
        }
        if !count.is_null() {
            *count = lengths.len() as isize;
        }
        if !phase.is_null() {
            *phase = start;
        }
    }
}

extern "C" fn bezier_copy(this: Id, _: Sel, _zone: usize) -> Id {
    // The class first: alloc_instance takes the runtime lock too.
    let class = crate::objc::runtime::lock().class_of(this);
    let copy = runtime::alloc_instance(class);
    property_set(copy, "cgPath", graphics::path_copy(path_of(this)) as u64);
    for key in ["lineWidth", "usesEvenOdd", "lineCap", "lineJoin", "miterLimit", "flatness"] {
        if let Some(value) = property_get(this, key) {
            property_set(copy, key, value);
        }
    }
    let mut dashes = DASHES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(dash) = dashes.as_ref().and_then(|d| d.get(&this).cloned()) {
        dashes.get_or_insert_with(HashMap::new).insert(copy, dash);
    }
    copy
}

// ----- UIColor in drawing -----

extern "C" fn color_set_fill(this: Id, _: Sel) {
    let context = graphics::current_ui_context();
    if let (true, Some(rgba)) = (context != 0, graphics::color_rgba(this)) {
        graphics::set_fill_in(context, rgba);
    }
}

extern "C" fn color_set_stroke(this: Id, _: Sel) {
    let context = graphics::current_ui_context();
    if let (true, Some(rgba)) = (context != 0, graphics::color_rgba(this)) {
        graphics::set_stroke_in(context, rgba);
    }
}

/// -set: both fill and stroke.
extern "C" fn color_set(this: Id, s: Sel) {
    color_set_fill(this, s);
    color_set_stroke(this, s);
}

pub fn register() {
    let class = define_class("UIBezierPath", "NSObject", 8);
    class_methods(
        class,
        &[
            ("bezierPath", bezier_path as usize),
            ("bezierPathWithRect:", bezier_rect as usize),
            ("bezierPathWithOvalInRect:", bezier_oval as usize),
            ("bezierPathWithRoundedRect:cornerRadius:", bezier_rounded as usize),
            ("bezierPathWithArcCenter:radius:startAngle:endAngle:clockwise:", bezier_arc as usize),
            ("bezierPathWithCGPath:", bezier_with_cg_path as usize),
        ],
    );
    methods(
        class,
        &[
            ("init", bezier_init as usize),
            ("moveToPoint:", bezier_move as usize),
            ("addLineToPoint:", bezier_line as usize),
            ("addCurveToPoint:controlPoint1:controlPoint2:", bezier_curve as usize),
            ("addQuadCurveToPoint:controlPoint:", bezier_quad as usize),
            ("addArcWithCenter:radius:startAngle:endAngle:clockwise:", bezier_add_arc as usize),
            ("closePath", bezier_close as usize),
            ("removeAllPoints", bezier_remove_all as usize),
            ("appendPath:", bezier_append as usize),
            ("applyTransform:", bezier_apply as usize),
            ("CGPath", bezier_cg_path as usize),
            ("setCGPath:", bezier_set_cg_path as usize),
            ("isEmpty", bezier_is_empty as usize),
            ("bounds", bezier_bounds as usize),
            ("currentPoint", bezier_current_point as usize),
            ("containsPoint:", bezier_contains as usize),
            ("lineWidth", bezier_line_width as usize),
            ("setLineWidth:", bezier_set_line_width as usize),
            ("usesEvenOddFillRule", bezier_even_odd as usize),
            ("setUsesEvenOddFillRule:", bezier_set_even_odd as usize),
            ("lineCapStyle", bezier_cap as usize),
            ("setLineCapStyle:", bezier_set_cap as usize),
            ("lineJoinStyle", bezier_join as usize),
            ("setLineJoinStyle:", bezier_set_join as usize),
            ("miterLimit", bezier_miter as usize),
            ("setMiterLimit:", bezier_set_miter as usize),
            ("flatness", bezier_flatness as usize),
            ("setFlatness:", bezier_set_flatness as usize),
            ("fill", bezier_fill as usize),
            ("stroke", bezier_stroke as usize),
            ("fillWithBlendMode:alpha:", bezier_fill_blend as usize),
            ("strokeWithBlendMode:alpha:", bezier_stroke_blend as usize),
            ("addClip", bezier_add_clip as usize),
            ("setLineDash:count:phase:", bezier_set_dash as usize),
            ("getLineDash:count:phase:", bezier_get_dash as usize),
            ("copyWithZone:", bezier_copy as usize),
        ],
    );
    methods(
        runtime::class_named("UIColor"),
        &[("set", color_set as usize), ("setFill", color_set_fill as usize), ("setStroke", color_set_stroke as usize)],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A red oval filled into a 20x10 context covers its middle and leaves
    /// its corners; a stroked line marks its pixels; a curve's points stay
    /// inside its control hull and end at its end point.
    #[test]
    fn bezier_paths_fill_and_stroke_the_current_context() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let class = runtime::class_named("UIBezierPath");
        let red = crate::fw::graphics::make_color([1.0, 0.0, 0.0, 1.0]);
        let blue = crate::fw::graphics::make_color([0.0, 0.0, 1.0, 1.0]);
        graphics::test_begin_context(20.0, 10.0);
        color_set_fill(red, 0);
        bezier_fill(bezier_oval(class, 0, Rect::new(0.0, 0.0, 20.0, 10.0)), 0);
        color_set_stroke(blue, 0);
        let line = bezier_path(class, 0);
        bezier_move(line, 0, Point { x: 0.0, y: 9.5 });
        bezier_line(line, 0, Point { x: 20.0, y: 9.5 });
        bezier_set_line_width(line, 0, 1.0);
        bezier_stroke(line, 0);
        let pixels = graphics::test_end_context();
        let at = |x: usize, y: usize| pixels[(y * 20 + x) * 4..(y * 20 + x) * 4 + 4].to_vec();
        assert_eq!(at(10, 5), [255, 0, 0, 255], "the oval's middle");
        assert_eq!(at(0, 0)[3], 0, "outside the oval");
        assert_eq!(at(10, 9), [0, 0, 255, 255], "the stroked bottom line (UIKit's y goes down)");
        let curve = bezier_path(class, 0);
        bezier_move(curve, 0, Point { x: 0.0, y: 0.0 });
        bezier_curve(curve, 0, Point { x: 30.0, y: 0.0 }, Point { x: 10.0, y: 20.0 }, Point { x: 20.0, y: 20.0 });
        let bounds = bezier_bounds(curve, 0);
        assert_eq!(bezier_current_point(curve, 0), Point { x: 30.0, y: 0.0 });
        assert!(bounds.size.height > 10.0 && bounds.size.height <= 20.0, "{bounds:?}");
        let rounded = bezier_rounded(class, 0, Rect::new(0.0, 0.0, 10.0, 10.0), 4.0);
        assert!(bezier_contains(rounded, 0, Point { x: 5.0, y: 5.0 }));
        assert!(!bezier_contains(rounded, 0, Point { x: 0.3, y: 0.3 }), "the corner is cut round");
    }

    /// fillWithBlendMode:alpha: sets the alpha and blend mode (not on top
    /// of the context's); -stroke draws with the path's cap and dashes.
    #[test]
    fn bezier_settings_apply_to_their_drawing() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let class = runtime::class_named("UIBezierPath");
        let red = crate::fw::graphics::make_color([1.0, 0.0, 0.0, 1.0]);
        graphics::test_begin_context(10.0, 4.0);
        let context = graphics::current_ui_context();
        color_set(red, 0);
        graphics::save_state(context);
        graphics::multiply_alpha(context, 0.5);
        bezier_fill_blend(bezier_rect(class, 0, Rect::new(0.0, 0.0, 2.0, 2.0)), 0, 0, 1.0);
        bezier_fill_blend(bezier_rect(class, 0, Rect::new(0.0, 0.0, 1.0, 1.0)), 0, 16, 1.0);
        graphics::restore_state(context);
        let capped = bezier_path(class, 0);
        bezier_move(capped, 0, Point { x: 3.0, y: 1.0 });
        bezier_line(capped, 0, Point { x: 5.0, y: 1.0 });
        bezier_set_line_width(capped, 0, 2.0);
        bezier_set_cap(capped, 0, 1);
        bezier_stroke(capped, 0);
        let dashed = bezier_path(class, 0);
        bezier_move(dashed, 0, Point { x: 0.0, y: 3.0 });
        bezier_line(dashed, 0, Point { x: 10.0, y: 3.0 });
        bezier_set_line_width(dashed, 0, 2.0);
        let pattern = [2.0, 2.0];
        bezier_set_dash(dashed, 0, pattern.as_ptr(), 2, 0.0);
        bezier_stroke(dashed, 0);
        let pixels = graphics::test_end_context();
        let at = |x: usize, y: usize| pixels[(y * 10 + x) * 4..(y * 10 + x) * 4 + 4].to_vec();
        assert_eq!(at(1, 1), [255, 0, 0, 255], "its own alpha, 1");
        assert_eq!(at(0, 0)[3], 0, "the clear blend mode clears");
        assert!(at(5, 1)[3] > 100, "a round cap past the end: {:?}", at(5, 1));
        assert_eq!((at(0, 3)[3], at(2, 3)[3]), (255, 0), "a dash, then a gap");
    }
}
