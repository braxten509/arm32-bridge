//! Core Graphics geometry: rectangles, points, sizes and transforms, as the
//! game passes them (groups of doubles in floating-point registers).

use crate::symbols::{leak, Registry};

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub origin: Point,
    pub size: Size,
}

impl Rect {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect { origin: Point { x, y }, size: Size { width, height } }
    }

    fn standard(self) -> Rect {
        let mut r = self;
        if r.size.width < 0.0 {
            r.origin.x += r.size.width;
            r.size.width = -r.size.width;
        }
        if r.size.height < 0.0 {
            r.origin.y += r.size.height;
            r.size.height = -r.size.height;
        }
        r
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub tx: f64,
    pub ty: f64,
}

pub const IDENTITY: Transform = Transform { a: 1.0, b: 0.0, c: 0.0, d: 1.0, tx: 0.0, ty: 0.0 };

/// The point (x, y) moved by t.
pub fn apply(t: Transform, x: f64, y: f64) -> (f64, f64) {
    (t.a * x + t.c * y + t.tx, t.b * x + t.d * y + t.ty)
}

/// The transform that undoes t, or None when t flattens the plane.
pub fn invert(t: Transform) -> Option<Transform> {
    let det = t.a * t.d - t.b * t.c;
    if det == 0.0 || !det.is_finite() {
        return None;
    }
    Some(Transform {
        a: t.d / det,
        b: -t.b / det,
        c: -t.c / det,
        d: t.a / det,
        tx: (t.c * t.ty - t.d * t.tx) / det,
        ty: (t.b * t.tx - t.a * t.ty) / det,
    })
}

/// t1 first, then t2.
pub fn concat(t1: Transform, t2: Transform) -> Transform {
    Transform {
        a: t1.a * t2.a + t1.b * t2.c,
        b: t1.a * t2.b + t1.b * t2.d,
        c: t1.c * t2.a + t1.d * t2.c,
        d: t1.c * t2.b + t1.d * t2.d,
        tx: t1.tx * t2.a + t1.ty * t2.c + t2.tx,
        ty: t1.tx * t2.b + t1.ty * t2.d + t2.ty,
    }
}

extern "C" fn rect_get_width(r: Rect) -> f64 {
    r.standard().size.width
}
extern "C" fn rect_get_height(r: Rect) -> f64 {
    r.standard().size.height
}
extern "C" fn rect_get_min_x(r: Rect) -> f64 {
    r.standard().origin.x
}
extern "C" fn rect_get_min_y(r: Rect) -> f64 {
    r.standard().origin.y
}
extern "C" fn rect_get_max_x(r: Rect) -> f64 {
    let r = r.standard();
    r.origin.x + r.size.width
}
extern "C" fn rect_get_max_y(r: Rect) -> f64 {
    let r = r.standard();
    r.origin.y + r.size.height
}
extern "C" fn rect_get_mid_x(r: Rect) -> f64 {
    let r = r.standard();
    r.origin.x + r.size.width / 2.0
}
extern "C" fn rect_get_mid_y(r: Rect) -> f64 {
    let r = r.standard();
    r.origin.y + r.size.height / 2.0
}
extern "C" fn rect_contains_point(r: Rect, p: Point) -> bool {
    let r = r.standard();
    p.x >= r.origin.x && p.x < r.origin.x + r.size.width && p.y >= r.origin.y && p.y < r.origin.y + r.size.height
}
extern "C" fn rect_inset(r: Rect, dx: f64, dy: f64) -> Rect {
    let r = r.standard();
    Rect::new(r.origin.x + dx, r.origin.y + dy, r.size.width - 2.0 * dx, r.size.height - 2.0 * dy)
}
extern "C" fn rect_offset(r: Rect, dx: f64, dy: f64) -> Rect {
    Rect::new(r.origin.x + dx, r.origin.y + dy, r.size.width, r.size.height)
}
extern "C" fn rect_equal(a: Rect, b: Rect) -> bool {
    a.standard() == b.standard()
}
extern "C" fn rect_is_empty(r: Rect) -> bool {
    r.size.width == 0.0 || r.size.height == 0.0
}
extern "C" fn rect_make(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect::new(x, y, w, h)
}
extern "C" fn transform_make_rotation(angle: f64) -> Transform {
    let (s, c) = angle.sin_cos();
    Transform { a: c, b: s, c: -s, d: c, tx: 0.0, ty: 0.0 }
}
extern "C" fn transform_make_scale(sx: f64, sy: f64) -> Transform {
    Transform { a: sx, b: 0.0, c: 0.0, d: sy, tx: 0.0, ty: 0.0 }
}
extern "C" fn transform_make_translation(tx: f64, ty: f64) -> Transform {
    Transform { a: 1.0, b: 0.0, c: 0.0, d: 1.0, tx, ty }
}
extern "C" fn transform_rotate(t: Transform, angle: f64) -> Transform {
    concat(transform_make_rotation(angle), t)
}
extern "C" fn transform_scale(t: Transform, sx: f64, sy: f64) -> Transform {
    concat(transform_make_scale(sx, sy), t)
}
extern "C" fn transform_translate(t: Transform, tx: f64, ty: f64) -> Transform {
    concat(transform_make_translation(tx, ty), t)
}
extern "C" fn transform_concat(t1: Transform, t2: Transform) -> Transform {
    concat(t1, t2)
}
extern "C" fn transform_is_identity(t: Transform) -> bool {
    t == IDENTITY
}
/// Apple hands a transform that can't be undone back unchanged.
extern "C" fn transform_invert(t: Transform) -> Transform {
    invert(t).unwrap_or(t)
}

const NULL_RECT: Rect = Rect { origin: Point { x: f64::INFINITY, y: f64::INFINITY }, size: Size { width: 0.0, height: 0.0 } };

fn is_null(r: Rect) -> bool {
    r.origin.x.is_infinite() || r.origin.y.is_infinite()
}
extern "C" fn rect_is_null(r: Rect) -> bool {
    is_null(r)
}
/// The smallest rectangle holding both (a null rectangle adds nothing; empty ones still count).
extern "C" fn rect_union(r1: Rect, r2: Rect) -> Rect {
    if is_null(r1) {
        return r2.standard();
    }
    if is_null(r2) {
        return r1.standard();
    }
    let (a, b) = (r1.standard(), r2.standard());
    let (x0, y0) = (a.origin.x.min(b.origin.x), a.origin.y.min(b.origin.y));
    let x1 = (a.origin.x + a.size.width).max(b.origin.x + b.size.width);
    let y1 = (a.origin.y + a.size.height).max(b.origin.y + b.size.height);
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

/// The smallest rectangle holding the transformed rectangle (a null one stays null).
extern "C" fn rect_apply_affine_transform(r: Rect, t: Transform) -> Rect {
    if is_null(r) {
        return r;
    }
    let r = r.standard();
    let corners = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)]
        .map(|(fx, fy)| apply(t, r.origin.x + fx * r.size.width, r.origin.y + fy * r.size.height));
    let (x0, x1) = corners.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), c| (lo.min(c.0), hi.max(c.0)));
    let (y0, y1) = corners.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), c| (lo.min(c.1), hi.max(c.1)));
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}
extern "C" fn point_apply_affine_transform(p: Point, t: Transform) -> Point {
    let (x, y) = apply(t, p.x, p.y);
    Point { x, y }
}
extern "C" fn size_apply_affine_transform(s: Size, t: Transform) -> Size {
    Size { width: t.a * s.width + t.c * s.height, height: t.b * s.width + t.d * s.height }
}

/// Core Animation's 4x4 transform (m11, m12, ... m44).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform3D(pub [f64; 16]);

pub const IDENTITY_3D: Transform3D = Transform3D([1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0]);

extern "C" fn transform_3d_make_scale(sx: f64, sy: f64, sz: f64) -> Transform3D {
    let mut m = IDENTITY_3D;
    (m.0[0], m.0[5], m.0[10]) = (sx, sy, sz);
    m
}
extern "C" fn transform_3d_make_translation(tx: f64, ty: f64, tz: f64) -> Transform3D {
    let mut m = IDENTITY_3D;
    (m.0[12], m.0[13], m.0[14]) = (tx, ty, tz);
    m
}

/// a then b: the row-vector product a * b (a point is a row on the left).
pub fn concat_3d(a: Transform3D, b: Transform3D) -> Transform3D {
    let mut m = [0.0; 16];
    for row in 0..4 {
        for column in 0..4 {
            m[row * 4 + column] = (0..4).map(|k| a.0[row * 4 + k] * b.0[k * 4 + column]).sum();
        }
    }
    Transform3D(m)
}

/// A turn by `angle` radians about the axis (x, y, z); no axis turns nothing.
#[rustfmt::skip]
extern "C" fn transform_3d_make_rotation(angle: f64, x: f64, y: f64, z: f64) -> Transform3D {
    let length = (x * x + y * y + z * z).sqrt();
    if length == 0.0 || !length.is_finite() {
        return IDENTITY_3D;
    }
    let (x, y, z) = (x / length, y / length, z / length);
    let (s, c) = angle.sin_cos();
    let t = 1.0 - c;
    Transform3D([
        t * x * x + c, t * x * y + s * z, t * x * z - s * y, 0.0,
        t * x * y - s * z, t * y * y + c, t * y * z + s * x, 0.0,
        t * x * z + s * y, t * y * z - s * x, t * z * z + c, 0.0,
        0.0, 0.0, 0.0, 1.0,
    ])
}
// Each step comes before t (in the layer's own space), as Core Animation's.
extern "C" fn transform_3d_rotate(t: Transform3D, angle: f64, x: f64, y: f64, z: f64) -> Transform3D {
    concat_3d(transform_3d_make_rotation(angle, x, y, z), t)
}
extern "C" fn transform_3d_scale(t: Transform3D, sx: f64, sy: f64, sz: f64) -> Transform3D {
    concat_3d(transform_3d_make_scale(sx, sy, sz), t)
}
extern "C" fn transform_3d_translate(t: Transform3D, tx: f64, ty: f64, tz: f64) -> Transform3D {
    concat_3d(transform_3d_make_translation(tx, ty, tz), t)
}
extern "C" fn transform_3d_concat(a: Transform3D, b: Transform3D) -> Transform3D {
    concat_3d(a, b)
}
extern "C" fn transform_3d_is_identity(t: Transform3D) -> bool {
    t == IDENTITY_3D
}
extern "C" fn transform_3d_make_affine_transform(t: Transform) -> Transform3D {
    let mut m = IDENTITY_3D;
    (m.0[0], m.0[1], m.0[4], m.0[5], m.0[12], m.0[13]) = (t.a, t.b, t.c, t.d, t.tx, t.ty);
    m
}

/// Whether the rectangles share any area (touching edges don't; a null one never does).
extern "C" fn rect_intersects_rect(r1: Rect, r2: Rect) -> bool {
    if is_null(r1) || is_null(r2) {
        return false;
    }
    let (a, b) = (r1.standard(), r2.standard());
    a.origin.x < b.origin.x + b.size.width
        && b.origin.x < a.origin.x + a.size.width
        && a.origin.y < b.origin.y + b.size.height
        && b.origin.y < a.origin.y + a.size.height
}

/// CGPointMakeWithDictionaryRepresentation: numbers under "X" and "Y"
/// (CGPointCreateDictionaryRepresentation's form); false when one is missing.
extern "C" fn point_make_with_dictionary(dictionary: crate::objc::runtime::Id, point: *mut Point) -> bool {
    use crate::fw::collections::{dictionary_get, is_dictionary, is_number, number_as_f64};
    if !is_dictionary(dictionary) || point.is_null() {
        return false;
    }
    let value = |key: &str| Some(dictionary_get(dictionary, crate::fw::string::make(key))).filter(|&v| is_number(v)).map(number_as_f64);
    let (Some(x), Some(y)) = (value("X"), value("Y")) else { return false };
    unsafe { *point = Point { x, y } };
    true
}

// ----- UIKit's text forms: "{x, y}", "{w, h}", "{{x, y}, {w, h}}" -----

/// The numbers in `text` in order (missing ones are 0, as UIKit gives).
pub(crate) fn numbers<const N: usize>(text: &str) -> [f64; N] {
    let mut out = [0.0; N];
    let parsed = text
        .split(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E')))
        .filter(|part| !part.is_empty())
        .map(|part| part.parse::<f64>().unwrap_or(0.0));
    for (slot, value) in out.iter_mut().zip(parsed) {
        *slot = value;
    }
    out
}

/// A number as UIKit writes it (%g: no trailing zeros).
fn number_text(value: f64) -> String {
    if value == value.trunc() && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        let text = format!("{value:.6}");
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

extern "C" fn point_from_string(text: crate::objc::runtime::Id) -> Point {
    let [x, y] = numbers(&crate::fw::string::get(text));
    Point { x, y }
}
extern "C" fn size_from_string(text: crate::objc::runtime::Id) -> Size {
    let [width, height] = numbers(&crate::fw::string::get(text));
    Size { width, height }
}
extern "C" fn rect_from_string(text: crate::objc::runtime::Id) -> Rect {
    let [x, y, width, height] = numbers(&crate::fw::string::get(text));
    Rect::new(x, y, width, height)
}
extern "C" fn string_from_point(p: Point) -> crate::objc::runtime::Id {
    crate::fw::string::make(&format!("{{{}, {}}}", number_text(p.x), number_text(p.y)))
}
extern "C" fn string_from_size(s: Size) -> crate::objc::runtime::Id {
    crate::fw::string::make(&format!("{{{}, {}}}", number_text(s.width), number_text(s.height)))
}
extern "C" fn string_from_rect(r: Rect) -> crate::objc::runtime::Id {
    crate::fw::string::make(&format!(
        "{{{{{}, {}}}, {{{}, {}}}}}",
        number_text(r.origin.x),
        number_text(r.origin.y),
        number_text(r.size.width),
        number_text(r.size.height)
    ))
}

pub fn register(r: &mut Registry) {
    r.function("CGRectApplyAffineTransform", rect_apply_affine_transform as usize);
    r.function("CGPointApplyAffineTransform", point_apply_affine_transform as usize);
    r.function("CGSizeApplyAffineTransform", size_apply_affine_transform as usize);
    r.function("CATransform3DMakeScale", transform_3d_make_scale as usize);
    r.function("CATransform3DMakeTranslation", transform_3d_make_translation as usize);
    r.function("CATransform3DMakeRotation", transform_3d_make_rotation as usize);
    r.function("CATransform3DMakeAffineTransform", transform_3d_make_affine_transform as usize);
    r.function("CATransform3DRotate", transform_3d_rotate as usize);
    r.function("CATransform3DScale", transform_3d_scale as usize);
    r.function("CATransform3DTranslate", transform_3d_translate as usize);
    r.function("CATransform3DConcat", transform_3d_concat as usize);
    r.function("CATransform3DIsIdentity", transform_3d_is_identity as usize);
    r.function("CGRectIntersectsRect", rect_intersects_rect as usize);
    r.function("CGPointMakeWithDictionaryRepresentation", point_make_with_dictionary as usize);
    r.data("CATransform3DIdentity", leak(IDENTITY_3D));
    r.function("CGPointFromString", point_from_string as usize);
    r.function("CGSizeFromString", size_from_string as usize);
    r.function("CGRectFromString", rect_from_string as usize);
    r.function("NSStringFromCGPoint", string_from_point as usize);
    r.function("NSStringFromCGSize", string_from_size as usize);
    r.function("NSStringFromCGRect", string_from_rect as usize);
    r.function("CGRectGetWidth", rect_get_width as usize);
    r.function("CGRectGetHeight", rect_get_height as usize);
    r.function("CGRectGetMinX", rect_get_min_x as usize);
    r.function("CGRectGetMinY", rect_get_min_y as usize);
    r.function("CGRectGetMaxX", rect_get_max_x as usize);
    r.function("CGRectGetMaxY", rect_get_max_y as usize);
    r.function("CGRectGetMidX", rect_get_mid_x as usize);
    r.function("CGRectGetMidY", rect_get_mid_y as usize);
    r.function("CGRectContainsPoint", rect_contains_point as usize);
    r.function("CGRectInset", rect_inset as usize);
    r.function("CGRectOffset", rect_offset as usize);
    r.function("CGRectEqualToRect", rect_equal as usize);
    r.function("CGRectIsEmpty", rect_is_empty as usize);
    r.function("CGRectMake", rect_make as usize);
    r.function("CGAffineTransformMakeRotation", transform_make_rotation as usize);
    r.function("CGAffineTransformMakeScale", transform_make_scale as usize);
    r.function("CGAffineTransformMakeTranslation", transform_make_translation as usize);
    r.function("CGAffineTransformRotate", transform_rotate as usize);
    r.function("CGAffineTransformScale", transform_scale as usize);
    r.function("CGAffineTransformTranslate", transform_translate as usize);
    r.function("CGAffineTransformConcat", transform_concat as usize);
    r.function("CGAffineTransformIsIdentity", transform_is_identity as usize);
    r.function("CGAffineTransformInvert", transform_invert as usize);
    r.function("CGRectUnion", rect_union as usize);
    r.function("CGRectIsNull", rect_is_null as usize);
    r.data("CGRectNull", leak(NULL_RECT));
    r.data("CGAffineTransformIdentity", leak(IDENTITY));
    r.data("CGRectZero", leak(Rect::default()));
    r.data("CGPointZero", leak(Point::default()));
    r.data("CGSizeZero", leak(Size::default()));
    // UIEdgeInsets: top, left, bottom, right.
    r.data("UIEdgeInsetsZero", leak([0.0f64; 4]));
}
