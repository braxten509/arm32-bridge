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
/// As CoreGraphics' (0x1942a): null and infinite rects come back as they
/// are; a side that would go below zero gives CGRectNull.
extern "C" fn rect_inset(r: Rect, dx: f64, dy: f64) -> Rect {
    if is_null(r) || is_infinite(r) {
        return r;
    }
    let r = r.standard();
    let (width, height) = (r.size.width - 2.0 * dx, r.size.height - 2.0 * dy);
    if width < 0.0 || height < 0.0 {
        return NULL_RECT;
    }
    Rect::new(r.origin.x + dx, r.origin.y + dy, width, height)
}

/// CGRectInfinite: the origin at -CGFLOAT_MAX/2, both sides CGFLOAT_MAX
/// (FLT_MAX in a 32-bit game, DBL_MAX in a 64-bit one).
fn is_infinite(r: Rect) -> bool {
    let infinite = |max: f64| r.origin.x == -max / 2.0 && r.origin.y == -max / 2.0 && r.size.width == max && r.size.height == max;
    infinite(f64::MAX) || infinite(f32::MAX as f64)
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
pub(crate) extern "C" fn transform_3d_make_rotation(angle: f64, x: f64, y: f64, z: f64) -> Transform3D {
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

// ----- a 3D transform taken apart, as Core Animation interpolates one -----

/// A CATransform3D as scale, skew, turn (a unit quaternion), move and
/// perspective, applied in that order (a point is a row on the left): the
/// matrix is S·K·R·T·P. Animations interpolate these parts, so a card turned
/// about y turns about y the whole way (Core Animation's way, not the parts
/// of the matrix one by one).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Decomposed {
    pub translate: [f64; 3],
    pub scale: [f64; 3],
    /// xy, xz, yz.
    pub skew: [f64; 3],
    /// The matrix's last column once the rest is taken out.
    pub perspective: [f64; 4],
    /// x, y, z, w.
    pub quaternion: [f64; 4],
}

type V3 = [f64; 3];

fn dot3(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross3(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn length3(a: V3) -> f64 {
    dot3(a, a).sqrt()
}

fn scaled3(a: V3, k: f64) -> V3 {
    [a[0] * k, a[1] * k, a[2] * k]
}

fn minus3(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

/// A 4x4 row-major matrix's inverse (None when it has none).
fn invert_4(m: &[f64; 16]) -> Option<[f64; 16]> {
    let mut a = *m;
    let mut inverse = IDENTITY_3D.0;
    for column in 0..4 {
        let pivot = (column..4).max_by(|&i, &j| a[i * 4 + column].abs().total_cmp(&a[j * 4 + column].abs()))?;
        if a[pivot * 4 + column].abs() < 1e-12 {
            return None;
        }
        for k in 0..4 {
            a.swap(column * 4 + k, pivot * 4 + k);
            inverse.swap(column * 4 + k, pivot * 4 + k);
        }
        let p = a[column * 4 + column];
        for k in 0..4 {
            a[column * 4 + k] /= p;
            inverse[column * 4 + k] /= p;
        }
        for row in (0..4).filter(|&row| row != column) {
            let f = a[row * 4 + column];
            for k in 0..4 {
                a[row * 4 + k] -= f * a[column * 4 + k];
                inverse[row * 4 + k] -= f * inverse[column * 4 + k];
            }
        }
    }
    Some(inverse)
}

/// The rows of the turn a unit quaternion makes.
fn quaternion_rows([x, y, z, w]: [f64; 4]) -> [V3; 3] {
    // The usual (column) matrix; a row-vector turn is its transpose.
    let c = [
        [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y - z * w), 2.0 * (x * z + y * w)],
        [2.0 * (x * y + z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z - x * w)],
        [2.0 * (x * z - y * w), 2.0 * (y * z + x * w), 1.0 - 2.0 * (x * x + y * y)],
    ];
    [[c[0][0], c[1][0], c[2][0]], [c[0][1], c[1][1], c[2][1]], [c[0][2], c[1][2], c[2][2]]]
}

/// A turn's quaternion from its rows (orthonormal, right-handed).
fn rows_quaternion(r: [V3; 3]) -> [f64; 4] {
    let c = |i: usize, j: usize| r[j][i];
    let sign = |v: f64| if v < 0.0 { -1.0 } else { 1.0 };
    let half_root = |v: f64| 0.5 * v.max(0.0).sqrt();
    let q = [
        half_root(1.0 + c(0, 0) - c(1, 1) - c(2, 2)) * sign(c(2, 1) - c(1, 2)),
        half_root(1.0 - c(0, 0) + c(1, 1) - c(2, 2)) * sign(c(0, 2) - c(2, 0)),
        half_root(1.0 - c(0, 0) - c(1, 1) + c(2, 2)) * sign(c(1, 0) - c(0, 1)),
        half_root(1.0 + c(0, 0) + c(1, 1) + c(2, 2)),
    ];
    let n = q.iter().map(|v| v * v).sum::<f64>().sqrt();
    if n > 0.0 { q.map(|v| v / n) } else { [0.0, 0.0, 0.0, 1.0] }
}

/// Unit rows where some are missing (a scale of 0 leaves no direction).
fn fill_axes(rows: [Option<V3>; 3]) -> [V3; 3] {
    let unit = |v: V3| scaled3(v, 1.0 / length3(v));
    let any_across = |v: V3| {
        let pick = if v[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
        unit(minus3(pick, scaled3(v, dot3(pick, v))))
    };
    match rows {
        [Some(a), Some(b), Some(c)] => [a, b, c],
        [None, Some(b), Some(c)] => [cross3(b, c), b, c],
        [Some(a), None, Some(c)] => [a, cross3(c, a), c],
        [Some(a), Some(b), None] => [a, b, cross3(a, b)],
        [Some(a), None, None] => {
            let b = any_across(a);
            [a, b, cross3(a, b)]
        }
        [None, Some(b), None] => {
            let c = any_across(b);
            [cross3(b, c), b, c]
        }
        [None, None, Some(c)] => {
            let a = any_across(c);
            [a, cross3(c, a), c]
        }
        [None, None, None] => [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
    }
}

/// Takes a transform apart (None when its m44 is 0).
pub fn decompose(t: Transform3D) -> Option<Decomposed> {
    let w = t.0[15];
    if w.abs() < 1e-12 {
        return None;
    }
    let m = t.0.map(|v| v / w);
    let mut affine = m;
    (affine[3], affine[7], affine[11], affine[15]) = (0.0, 0.0, 0.0, 1.0);
    let column = [m[3], m[7], m[11], m[15]];
    let perspective = if column[..3].iter().all(|v| v.abs() < 1e-12) {
        [0.0, 0.0, 0.0, 1.0]
    } else {
        match invert_4(&affine) {
            Some(inverse) => std::array::from_fn(|i| (0..4).map(|j| inverse[i * 4 + j] * column[j]).sum()),
            None => column,
        }
    };
    let row = |i: usize| [m[i * 4], m[i * 4 + 1], m[i * 4 + 2]];
    const TINY: f64 = 1e-12;
    let sx = length3(row(0));
    let r0 = (sx > TINY).then(|| scaled3(row(0), 1.0 / sx));
    let mut xy = r0.map_or(0.0, |r0| dot3(r0, row(1)));
    let r1 = r0.map_or(row(1), |r0| minus3(row(1), scaled3(r0, xy)));
    let sy = length3(r1);
    let r1 = (sy > TINY).then(|| scaled3(r1, 1.0 / sy));
    xy = if sy > TINY { xy / sy } else { 0.0 };
    let mut xz = r0.map_or(0.0, |r0| dot3(r0, row(2)));
    let r2 = r0.map_or(row(2), |r0| minus3(row(2), scaled3(r0, xz)));
    let mut yz = r1.map_or(0.0, |r1| dot3(r1, r2));
    let r2 = r1.map_or(r2, |r1| minus3(r2, scaled3(r1, yz)));
    let sz = length3(r2);
    let r2 = (sz > TINY).then(|| scaled3(r2, 1.0 / sz));
    (xz, yz) = if sz > TINY { (xz / sz, yz / sz) } else { (0.0, 0.0) };
    let mut rows = fill_axes([r0, r1, r2]);
    let mut scale = [sx, sy, sz];
    // Mirrored: the scale takes the sign, so the rows are a turn.
    if dot3(rows[0], cross3(rows[1], rows[2])) < 0.0 {
        scale = scale.map(|s| -s);
        rows = rows.map(|r| scaled3(r, -1.0));
    }
    Some(Decomposed { translate: [m[12], m[13], m[14]], scale, skew: [xy, xz, yz], perspective, quaternion: rows_quaternion(rows) })
}

/// Puts a taken-apart transform back together.
pub fn recompose(d: &Decomposed) -> Transform3D {
    let r = quaternion_rows(d.quaternion);
    let [xy, xz, yz] = d.skew;
    let add = |a: V3, b: V3| [a[0] + b[0], a[1] + b[1], a[2] + b[2]];
    let rows = [
        scaled3(r[0], d.scale[0]),
        scaled3(add(r[1], scaled3(r[0], xy)), d.scale[1]),
        scaled3(add(add(r[2], scaled3(r[0], xz)), scaled3(r[1], yz)), d.scale[2]),
    ];
    let p = d.perspective;
    let mut m = [0.0; 16];
    for (i, row) in rows.iter().enumerate() {
        m[i * 4..i * 4 + 3].copy_from_slice(row);
        m[i * 4 + 3] = dot3(*row, [p[0], p[1], p[2]]);
    }
    m[12..15].copy_from_slice(&d.translate);
    m[15] = dot3(d.translate, [p[0], p[1], p[2]]) + p[3];
    Transform3D(m)
}

impl Decomposed {
    /// `f` of the way from `self` to `other`; the turn takes the short way.
    pub fn lerp(&self, other: &Decomposed, f: f64) -> Decomposed {
        let mix = |a: f64, b: f64| a + (b - a) * f;
        Decomposed {
            translate: std::array::from_fn(|i| mix(self.translate[i], other.translate[i])),
            scale: std::array::from_fn(|i| mix(self.scale[i], other.scale[i])),
            skew: std::array::from_fn(|i| mix(self.skew[i], other.skew[i])),
            perspective: std::array::from_fn(|i| mix(self.perspective[i], other.perspective[i])),
            quaternion: slerp(self.quaternion, other.quaternion, f),
        }
    }

    /// The turn as angles about x, then y, then z (radians).
    pub fn angles(&self) -> [f64; 3] {
        let r = quaternion_rows(self.quaternion);
        [r[1][2].atan2(r[2][2]), (-r[0][2]).clamp(-1.0, 1.0).asin(), r[0][1].atan2(r[0][0])]
    }

    /// Turns about x, then y, then z (radians).
    pub fn set_angles(&mut self, [x, y, z]: [f64; 3]) {
        let turn = concat_3d(concat_3d(transform_3d_make_rotation(x, 1.0, 0.0, 0.0), transform_3d_make_rotation(y, 0.0, 1.0, 0.0)), transform_3d_make_rotation(z, 0.0, 0.0, 1.0));
        let row = |i: usize| [turn.0[i * 4], turn.0[i * 4 + 1], turn.0[i * 4 + 2]];
        self.quaternion = rows_quaternion([row(0), row(1), row(2)]);
    }

    /// `n` of it added up (whole laps of a cumulative animation).
    pub fn times(&self, n: f64) -> Decomposed {
        let [x, y, z, w] = self.quaternion;
        let half = w.clamp(-1.0, 1.0).acos();
        let s = half.sin();
        let quaternion = if s.abs() < 1e-12 { [0.0, 0.0, 0.0, 1.0] } else { let k = (half * n).sin() / s; [x * k, y * k, z * k, (half * n).cos()] };
        Decomposed {
            translate: self.translate.map(|v| v * n),
            scale: self.scale.map(|v| v.signum() * v.abs().powf(n)),
            skew: self.skew.map(|v| v * n),
            perspective: self.perspective,
            quaternion,
        }
    }
}

/// Spherical interpolation of unit quaternions, the short way round.
fn slerp(a: [f64; 4], b: [f64; 4], f: f64) -> [f64; 4] {
    let mut d: f64 = (0..4).map(|i| a[i] * b[i]).sum();
    let b = if d < 0.0 {
        d = -d;
        b.map(|v| -v)
    } else {
        b
    };
    let (ka, kb) = if d > 0.9995 {
        (1.0 - f, f)
    } else {
        let angle = d.min(1.0).acos();
        let s = angle.sin();
        (((1.0 - f) * angle).sin() / s, (f * angle).sin() / s)
    };
    let q: [f64; 4] = std::array::from_fn(|i| a[i] * ka + b[i] * kb);
    let n = q.iter().map(|v| v * v).sum::<f64>().sqrt();
    q.map(|v| v / n)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// CoreGraphics' CGRectInset (0x1942a): CGRectNull when the result would
    /// have a negative side; null and infinite rects come back unchanged.
    #[test]
    fn insetting_too_far_gives_the_null_rect() {
        assert_eq!(rect_inset(Rect::new(0.0, 0.0, 10.0, 10.0), 2.0, 3.0), Rect::new(2.0, 3.0, 6.0, 4.0));
        assert!(is_null(rect_inset(Rect::new(0.0, 0.0, 10.0, 10.0), 6.0, 0.0)), "too wide an inset");
        assert!(is_null(rect_inset(NULL_RECT, 1.0, 1.0)));
        let infinite = Rect::new(-f64::MAX / 2.0, -f64::MAX / 2.0, f64::MAX, f64::MAX);
        assert_eq!(rect_inset(infinite, 5.0, 5.0), infinite);
        // A 32-bit game's CGRectInfinite is built from FLT_MAX.
        let infinite32 = Rect::new(-f32::MAX as f64 / 2.0, -f32::MAX as f64 / 2.0, f32::MAX as f64, f32::MAX as f64);
        assert_eq!(rect_inset(infinite32, 1e37, 1e37), infinite32);
    }
}
