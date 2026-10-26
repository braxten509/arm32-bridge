//! Core Animation's animations: CAMediaTimingFunction, CAAnimation and its
//! kinds (CABasicAnimation, CAKeyframeAnimation, CAAnimationGroup,
//! CATransition), and the animations running on layers.
//!
//! An animation added to a layer is copied (as Apple does) and runs on the
//! layer's time (fw/layers.rs): it starts at its beginTime (now when 0),
//! its delegate hears animationDidStart: and then animationDidStop:finished:
//! (YES when it ran to its end, NO when it was removed or replaced first),
//! and with removedOnCompletion (the default) it leaves the layer at its
//! end. Held there (fillMode forwards or both, removedOnCompletion NO), its
//! last value stays. What a view's layer animates (opacity, position,
//! transform and their parts) is read by fw/pictures.rs each time it lists
//! the screen (`presented`), so views move and fade on screen as on iOS;
//! other key paths keep only the timing and the delegate calls. Transforms
//! are taken apart into move, turn and scale (no shear) to animate.

use crate::fw::geometry::{Point, Transform, Transform3D};
use crate::fw::layers::{self, unboxed, Kind};
use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::uikit::{property_get, property_set, property_set_object};
use crate::fw::{collections, runloop, string};
use crate::objc::runtime::{self, lock, Class, Id, Sel};
use crate::symbols::{leak, Registry};
use std::sync::Mutex;

/// What a duration of 0 means: the transaction's default.
const DEFAULT_DURATION: f64 = 0.25;

// ----- CAMediaTimingFunction: { isa, [x1, y1, x2, y2] } -----

/// A cubic curve from (0, 0) to (1, 1) with two control points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Curve(pub [f64; 4]);

impl Curve {
    pub const LINEAR: Curve = Curve([0.0, 0.0, 1.0, 1.0]);

    /// Apple's named curves.
    pub fn named(name: &str) -> Option<Curve> {
        Some(Curve(match name {
            "linear" => [0.0, 0.0, 1.0, 1.0],
            "easeIn" => [0.42, 0.0, 1.0, 1.0],
            "easeOut" => [0.0, 0.0, 0.58, 1.0],
            "easeInEaseOut" => [0.42, 0.0, 0.58, 1.0],
            "default" => [0.25, 0.1, 0.25, 1.0],
            _ => return None,
        }))
    }

    /// The curve's height where it is `x` across (both 0 to 1).
    pub fn at(&self, x: f64) -> f64 {
        let [x1, y1, x2, y2] = self.0;
        let bezier = |t: f64, a: f64, b: f64| 3.0 * (1.0 - t) * (1.0 - t) * t * a + 3.0 * (1.0 - t) * t * t * b + t * t * t;
        let x = x.clamp(0.0, 1.0);
        // x(t) only grows, so halving finds t.
        let (mut low, mut high) = (0.0, 1.0);
        for _ in 0..40 {
            let middle = (low + high) / 2.0;
            if bezier(middle, x1, x2) < x {
                low = middle;
            } else {
                high = middle;
            }
        }
        bezier((low + high) / 2.0, y1, y2)
    }
}

fn make_curve(class: Class, curve: Curve) -> Id {
    let object = runtime::alloc_instance(class);
    unsafe { *((object + 8) as *mut [f64; 4]) = curve.0 };
    object
}

pub fn curve_of(function: Id) -> Option<Curve> {
    let class = runtime::class_named("CAMediaTimingFunction");
    let is = {
        let rt = lock();
        function != 0 && rt.is_subclass(rt.class_of(function), class)
    };
    is.then(|| Curve(unsafe { *((function + 8) as *const [f64; 4]) }))
}

extern "C" fn function_with_name(class: Class, _: Sel, name: Id) -> Id {
    let name = string::get(name);
    match Curve::named(&name) {
        Some(curve) => make_curve(class, curve),
        None => {
            crate::log!("+[CAMediaTimingFunction functionWithName:] unknown name {name}; linear");
            make_curve(class, Curve::LINEAR)
        }
    }
}

extern "C" fn function_with_points(class: Class, _: Sel, x1: f32, y1: f32, x2: f32, y2: f32) -> Id {
    make_curve(class, Curve([x1 as f64, y1 as f64, x2 as f64, y2 as f64]))
}

extern "C" fn function_init_with_points(this: Id, _: Sel, x1: f32, y1: f32, x2: f32, y2: f32) -> Id {
    unsafe { *((this + 8) as *mut [f64; 4]) = [x1 as f64, y1 as f64, x2 as f64, y2 as f64] };
    this
}

/// Control point 0 is (0, 0), 1 and 2 the curve's own, 3 is (1, 1).
extern "C" fn function_get_point(this: Id, _: Sel, index: usize, values: *mut f32) {
    let [x1, y1, x2, y2] = curve_of(this).unwrap_or(Curve::LINEAR).0;
    let point = match index {
        1 => (x1, y1),
        2 => (x2, y2),
        3 => (1.0, 1.0),
        _ => (0.0, 0.0),
    };
    if !values.is_null() {
        unsafe { *values = point.0 as f32 };
        unsafe { *values.add(1) = point.1 as f32 };
    }
}

// ----- CAAnimation and its kinds: everything in the property store -----

/// Keys holding objects, copied (retained) with the animation.
const OBJECT_KEYS: &[&str] = &[
    "fillMode", "timingFunction", "keyPath", "fromValue", "toValue", "byValue", "values", "keyTimes", "timingFunctions",
    "calculationMode", "path", "animations", "type", "subtype", "rotationMode",
];
/// Keys holding numbers (f64 bits).
const NUMBER_KEYS: &[&str] = &[
    "duration", "beginTime", "speed", "timeOffset", "repeatCount", "repeatDuration", "autoreverses", "removedOnCompletion",
    "additive", "cumulative", "startProgress", "endProgress",
];

fn number(object: Id, key: &'static str, default: f64) -> f64 {
    property_get(object, key).map(f64::from_bits).unwrap_or(default)
}

fn set_number(object: Id, key: &'static str, value: f64) {
    property_set(object, key, value.to_bits());
}

fn object(animation: Id, key: &'static str) -> Id {
    property_get(animation, key).unwrap_or(0) as Id
}

macro_rules! double_property {
    ($get:ident, $set:ident, $key:literal, $default:expr) => {
        extern "C" fn $get(this: Id, _: Sel) -> f64 {
            number(this, $key, $default)
        }
        extern "C" fn $set(this: Id, _: Sel, value: f64) {
            set_number(this, $key, value);
        }
    };
}
macro_rules! float_property {
    ($get:ident, $set:ident, $key:literal, $default:expr) => {
        extern "C" fn $get(this: Id, _: Sel) -> f32 {
            number(this, $key, $default) as f32
        }
        extern "C" fn $set(this: Id, _: Sel, value: f32) {
            set_number(this, $key, value as f64);
        }
    };
}
macro_rules! bool_property {
    ($get:ident, $set:ident, $key:literal, $default:expr) => {
        extern "C" fn $get(this: Id, _: Sel) -> bool {
            number(this, $key, $default as u8 as f64) != 0.0
        }
        extern "C" fn $set(this: Id, _: Sel, value: u8) {
            set_number(this, $key, (value != 0) as u8 as f64);
        }
    };
}
macro_rules! object_property {
    ($get:ident, $set:ident, $key:literal) => {
        extern "C" fn $get(this: Id, _: Sel) -> Id {
            object(this, $key)
        }
        extern "C" fn $set(this: Id, _: Sel, value: Id) {
            hold(this, $key, value);
        }
    };
}

/// Stores an object the animation holds on to (retained, as Apple's
/// properties are; strings and arrays are copied).
fn hold(animation: Id, key: &'static str, value: Id) {
    let copied = matches!(key, "fillMode" | "keyPath" | "calculationMode" | "type" | "subtype" | "rotationMode" | "values" | "keyTimes" | "timingFunctions" | "animations");
    let value = if value != 0 && copied { runtime::send(value, "copy", &[]) } else { value };
    property_set_object(animation, key, value);
}

double_property!(anim_duration, anim_set_duration, "duration", 0.0);
double_property!(anim_begin, anim_set_begin, "beginTime", 0.0);
float_property!(anim_speed, anim_set_speed, "speed", 1.0);
double_property!(anim_offset, anim_set_offset, "timeOffset", 0.0);
float_property!(anim_repeat_count, anim_set_repeat_count, "repeatCount", 0.0);
double_property!(anim_repeat_duration, anim_set_repeat_duration, "repeatDuration", 0.0);
bool_property!(anim_autoreverses, anim_set_autoreverses, "autoreverses", false);
bool_property!(anim_removed, anim_set_removed, "removedOnCompletion", true);
bool_property!(anim_additive, anim_set_additive, "additive", false);
bool_property!(anim_cumulative, anim_set_cumulative, "cumulative", false);
float_property!(anim_start_progress, anim_set_start_progress, "startProgress", 0.0);
float_property!(anim_end_progress, anim_set_end_progress, "endProgress", 1.0);
object_property!(anim_timing, anim_set_timing, "timingFunction");
object_property!(anim_key_path, anim_set_key_path, "keyPath");
object_property!(anim_from, anim_set_from, "fromValue");
object_property!(anim_to, anim_set_to, "toValue");
object_property!(anim_by, anim_set_by, "byValue");
object_property!(anim_values, anim_set_values, "values");
object_property!(anim_key_times, anim_set_key_times, "keyTimes");
object_property!(anim_timings, anim_set_timings, "timingFunctions");
object_property!(anim_path, anim_set_path, "path");
object_property!(anim_animations, anim_set_animations, "animations");
object_property!(anim_subtype, anim_set_subtype, "subtype");
object_property!(anim_rotation_mode, anim_set_rotation_mode, "rotationMode");

extern "C" fn anim_fill_mode(this: Id, _: Sel) -> Id {
    match object(this, "fillMode") {
        0 => string::make("removed"),
        mode => mode,
    }
}

extern "C" fn anim_set_fill_mode(this: Id, _: Sel, value: Id) {
    hold(this, "fillMode", value);
}

extern "C" fn anim_calculation_mode(this: Id, _: Sel) -> Id {
    match object(this, "calculationMode") {
        0 => string::make("linear"),
        mode => mode,
    }
}

extern "C" fn anim_set_calculation_mode(this: Id, _: Sel, value: Id) {
    hold(this, "calculationMode", value);
}

extern "C" fn transition_type(this: Id, _: Sel) -> Id {
    match object(this, "type") {
        0 => string::make("fade"),
        kind => kind,
    }
}

extern "C" fn transition_set_type(this: Id, _: Sel, value: Id) {
    hold(this, "type", value);
}

/// The delegate is held strongly (a CAAnimation's is, unlike most).
extern "C" fn anim_delegate(this: Id, _: Sel) -> Id {
    object(this, "delegate")
}

extern "C" fn anim_set_delegate(this: Id, _: Sel, delegate: Id) {
    let old = object(this, "delegate");
    if old == delegate {
        return;
    }
    if delegate != 0 {
        runtime::send(delegate, "retain", &[]);
    }
    property_set(this, "delegate", delegate as u64);
    if old != 0 {
        runtime::send(old, "release", &[]);
    }
}

extern "C" fn anim_class_animation(class: Class, _: Sel) -> Id {
    runtime::send(runtime::send(class, "alloc", &[]), "init", &[])
}

extern "C" fn anim_with_key_path(class: Class, _: Sel, path: Id) -> Id {
    let animation = anim_class_animation(class, 0);
    hold(animation, "keyPath", path);
    animation
}

/// A copy with every setting (the delegate held by both, values the game
/// stored by key copied).
fn copy_animation(this: Id) -> Id {
    // The class first: alloc_instance takes the runtime lock too.
    let class = lock().class_of(this);
    let copy = runtime::alloc_instance(class);
    for &key in OBJECT_KEYS {
        if let Some(value) = property_get(this, key) {
            property_set_object(copy, key, value as Id);
        }
    }
    for &key in NUMBER_KEYS {
        if let Some(value) = property_get(this, key) {
            property_set(copy, key, value);
        }
    }
    anim_set_delegate(copy, 0, object(this, "delegate"));
    let store = object(this, "kvcValues");
    if store != 0 {
        property_set(copy, "kvcValues", runtime::send(store, "mutableCopy", &[]) as u64);
    }
    copy
}

extern "C" fn anim_copy(this: Id, _: Sel, _zone: usize) -> Id {
    copy_animation(this)
}

extern "C" fn anim_should_archive(_: Class, _: Sel, _key: Id) -> bool {
    true
}

const ANIMATION_KEYS: &[(&str, Kind)] = &[
    ("duration", Kind::Double),
    ("beginTime", Kind::Double),
    ("speed", Kind::Float),
    ("timeOffset", Kind::Double),
    ("repeatCount", Kind::Float),
    ("repeatDuration", Kind::Double),
    ("autoreverses", Kind::Bool),
    ("removedOnCompletion", Kind::Bool),
    ("additive", Kind::Bool),
    ("cumulative", Kind::Bool),
    ("fillMode", Kind::Object),
    ("timingFunction", Kind::Object),
    ("delegate", Kind::Object),
    ("keyPath", Kind::Object),
    ("fromValue", Kind::Object),
    ("toValue", Kind::Object),
    ("byValue", Kind::Object),
    ("values", Kind::Object),
    ("keyTimes", Kind::Object),
    ("timingFunctions", Kind::Object),
    ("calculationMode", Kind::Object),
    ("animations", Kind::Object),
    ("type", Kind::Object),
    ("subtype", Kind::Object),
];

extern "C" fn anim_value_for_key(this: Id, _: Sel, key: Id) -> Id {
    layers::kvc_get(this, key, ANIMATION_KEYS)
}

extern "C" fn anim_set_value_for_key(this: Id, _: Sel, value: Id, key: Id) {
    layers::kvc_set(this, value, key, ANIMATION_KEYS)
}

// ----- timing -----

/// An animation's CAMediaTiming settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timing {
    pub begin: f64,
    pub duration: f64,
    pub speed: f64,
    pub offset: f64,
    pub repeat_count: f64,
    pub repeat_duration: f64,
    pub autoreverses: bool,
    pub forwards: bool,
    pub backwards: bool,
}

impl Default for Timing {
    fn default() -> Self {
        Timing { begin: 0.0, duration: 1.0, speed: 1.0, offset: 0.0, repeat_count: 0.0, repeat_duration: 0.0, autoreverses: false, forwards: false, backwards: false }
    }
}

/// Where an animation is at some moment of its parent's time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct State {
    pub started: bool,
    pub ended: bool,
    /// How far through its simple duration (0 to 1) it shows, if it shows.
    pub progress: Option<f64>,
}

impl Timing {
    /// The time it is active, in its own time.
    pub fn active(&self) -> f64 {
        let cycle = if self.autoreverses { 2.0 * self.duration } else { self.duration };
        if self.repeat_duration > 0.0 {
            self.repeat_duration
        } else if self.repeat_count > 0.0 {
            cycle * self.repeat_count
        } else {
            cycle
        }
    }

    /// Progress at `local` of its own time (inside the active time).
    fn progress_at(&self, local: f64) -> f64 {
        let d = self.duration;
        if d <= 0.0 {
            return 1.0;
        }
        let cycle = if self.autoreverses { 2.0 * d } else { d };
        let within = local.rem_euclid(cycle);
        if self.autoreverses && within > d {
            (cycle - within) / d
        } else {
            within / d
        }
    }

    /// Progress at the very end of the active time.
    fn end_progress(&self) -> f64 {
        let active = self.active();
        let cycle = if self.autoreverses { 2.0 * self.duration } else { self.duration };
        if cycle <= 0.0 || !active.is_finite() {
            return 1.0;
        }
        let laps = active / cycle;
        if (laps - laps.round()).abs() < 1e-9 {
            if self.autoreverses { 0.0 } else { 1.0 }
        } else {
            self.progress_at(active)
        }
    }

    /// Where it is at the parent's time `t`.
    pub fn state(&self, t: f64) -> State {
        if t < self.begin {
            return State { started: false, ended: false, progress: self.backwards.then(|| self.progress_at(self.offset.max(0.0))) };
        }
        let local = (t - self.begin) * self.speed + self.offset;
        if local >= self.active() {
            return State { started: true, ended: true, progress: self.forwards.then(|| self.end_progress()) };
        }
        State { started: true, ended: false, progress: Some(self.progress_at(local.max(0.0))) }
    }

    /// How many whole cycles (repeats) are behind it at the parent's time
    /// `t`: 0 in the first; held at its end, the laps before the last.
    pub fn lap(&self, t: f64) -> f64 {
        let cycle = if self.autoreverses { 2.0 * self.duration } else { self.duration };
        if t < self.begin || cycle <= 0.0 {
            return 0.0;
        }
        let (local, active) = ((t - self.begin) * self.speed + self.offset, self.active());
        if local >= active {
            ((active / cycle).ceil() - 1.0).max(0.0)
        } else {
            (local.max(0.0) / cycle).floor()
        }
    }

    /// When it ends, in its parent's time (never at speed 0 or repeating forever).
    pub fn end(&self) -> f64 {
        if self.speed <= 0.0 {
            return f64::INFINITY;
        }
        self.begin + (self.active() - self.offset).max(0.0) / self.speed
    }
}

/// An animation's timing (`fallback`: what a duration of 0 stands for).
fn timing_of(animation: Id, fallback: f64) -> Timing {
    let fill = string::get(anim_fill_mode(animation, 0));
    let duration = number(animation, "duration", 0.0);
    Timing {
        begin: number(animation, "beginTime", 0.0),
        duration: if duration > 0.0 { duration } else { fallback },
        speed: number(animation, "speed", 1.0),
        offset: number(animation, "timeOffset", 0.0),
        repeat_count: number(animation, "repeatCount", 0.0),
        repeat_duration: number(animation, "repeatDuration", 0.0),
        autoreverses: number(animation, "autoreverses", 0.0) != 0.0,
        forwards: matches!(fill.as_str(), "forwards" | "both"),
        backwards: matches!(fill.as_str(), "backwards" | "both"),
    }
}

// ----- values -----

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
    Number(f64),
    Point(Point),
    Transform(Transform3D),
}

impl Value {
    fn lerp(a: Value, b: Value, f: f64) -> Option<Value> {
        let mix = |x: f64, y: f64| x + (y - x) * f;
        Some(match (a, b) {
            (Value::Number(x), Value::Number(y)) => Value::Number(mix(x, y)),
            (Value::Point(p), Value::Point(q)) => Value::Point(Point { x: mix(p.x, q.x), y: mix(p.y, q.y) }),
            (Value::Transform(m), Value::Transform(n)) => {
                // Taken apart and put back together, so turns turn.
                let (pm, pn) = (Parts::of(layers::affine_of(m)), Parts::of(layers::affine_of(n)));
                let parts = Parts { tx: mix(pm.tx, pn.tx), ty: mix(pm.ty, pn.ty), sx: mix(pm.sx, pn.sx), sy: mix(pm.sy, pn.sy), angle: mix(pm.angle, pn.angle) };
                Value::Transform(layers::affine_to_3d(parts.transform()))
            }
            _ => return None,
        })
    }

    fn add(a: Value, b: Value) -> Option<Value> {
        Some(match (a, b) {
            (Value::Number(x), Value::Number(y)) => Value::Number(x + y),
            (Value::Point(p), Value::Point(q)) => Value::Point(Point { x: p.x + q.x, y: p.y + q.y }),
            (Value::Transform(m), Value::Transform(n)) => Value::Transform(crate::fw::geometry::concat_3d(n, m)),
            _ => return None,
        })
    }

    fn subtract(a: Value, b: Value) -> Option<Value> {
        Some(match (a, b) {
            (Value::Number(x), Value::Number(y)) => Value::Number(x - y),
            (Value::Point(p), Value::Point(q)) => Value::Point(Point { x: p.x - q.x, y: p.y - q.y }),
            _ => return None,
        })
    }

    /// `n` of a value added up (`n` whole, for cumulative laps).
    fn times(v: Value, n: f64) -> Value {
        match v {
            Value::Number(x) => Value::Number(x * n),
            Value::Point(p) => Value::Point(Point { x: p.x * n, y: p.y * n }),
            Value::Transform(m) => {
                let p = Parts::of(layers::affine_of(m));
                let parts = Parts { tx: p.tx * n, ty: p.ty * n, sx: p.sx.powf(n), sy: p.sy.powf(n), angle: p.angle * n };
                Value::Transform(layers::affine_to_3d(parts.transform()))
            }
        }
    }

    /// How far apart two values are (for paced key frames).
    fn distance(a: Value, b: Value) -> f64 {
        match (a, b) {
            (Value::Number(x), Value::Number(y)) => (y - x).abs(),
            (Value::Point(p), Value::Point(q)) => (q.x - p.x).hypot(q.y - p.y),
            _ => 1.0,
        }
    }
}

/// A value object as an animation reads it: a number, or an NSValue
/// holding a point, size (as a point) or transform.
pub fn read_value(object: Id) -> Option<Value> {
    if object == 0 {
        return None;
    }
    if collections::is_number(object) {
        return Some(Value::Number(collections::number_as_f64(object)));
    }
    let kind = runtime::sel("objCType");
    let imp = runtime::imp_for(object, kind)?;
    let f: extern "C" fn(Id, Sel) -> *const std::ffi::c_char = unsafe { std::mem::transmute(imp) };
    let pointer = f(object, kind);
    if pointer.is_null() {
        return None;
    }
    let text = unsafe { std::ffi::CStr::from_ptr(pointer) }.to_string_lossy().into_owned();
    if text.starts_with("{CGPoint") {
        unboxed::<Point>(object, "CGPointValue").map(Value::Point)
    } else if text.starts_with("{CGSize") {
        unboxed::<crate::fw::geometry::Size>(object, "CGSizeValue").map(|s| Value::Point(Point { x: s.width, y: s.height }))
    } else if text.starts_with("{CATransform3D") {
        unboxed::<Transform3D>(object, "CATransform3DValue").map(Value::Transform)
    } else if text.starts_with("{CGAffineTransform") {
        unboxed::<Transform>(object, "CGAffineTransformValue").map(|t| Value::Transform(layers::affine_to_3d(t)))
    } else if matches!(text.as_str(), "f" | "d" | "i" | "I" | "l" | "L" | "q" | "Q" | "c" | "C" | "s" | "S") {
        Some(Value::Number(layers::number_value(object)))
    } else {
        None
    }
}

/// A 2D transform as move, turn and scale.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Parts {
    tx: f64,
    ty: f64,
    sx: f64,
    sy: f64,
    angle: f64,
}

impl Parts {
    fn of(t: Transform) -> Parts {
        let sx = t.a.hypot(t.b);
        let angle = t.b.atan2(t.a);
        let sy = if sx != 0.0 { (t.a * t.d - t.b * t.c) / sx } else { t.c.hypot(t.d) };
        Parts { tx: t.tx, ty: t.ty, sx, sy, angle }
    }

    /// Scaled, then turned, then moved.
    fn transform(&self) -> Transform {
        let (s, c) = self.angle.sin_cos();
        Transform { a: self.sx * c, b: self.sx * s, c: -self.sy * s, d: self.sy * c, tx: self.tx, ty: self.ty }
    }
}

/// What a layer shows of the parts animations can change.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shown {
    pub opacity: f64,
    pub position: Point,
    pub transform: Transform,
}

impl Shown {
    fn get(&self, path: &str) -> Option<Value> {
        let parts = Parts::of(self.transform);
        Some(match path {
            "opacity" => Value::Number(self.opacity),
            "position" => Value::Point(self.position),
            "position.x" => Value::Number(self.position.x),
            "position.y" => Value::Number(self.position.y),
            "transform" => Value::Transform(layers::affine_to_3d(self.transform)),
            "transform.scale" | "transform.scale.x" => Value::Number(parts.sx),
            "transform.scale.y" => Value::Number(parts.sy),
            "transform.rotation" | "transform.rotation.z" => Value::Number(parts.angle),
            "transform.translation" => Value::Point(Point { x: parts.tx, y: parts.ty }),
            "transform.translation.x" => Value::Number(parts.tx),
            "transform.translation.y" => Value::Number(parts.ty),
            _ => return None,
        })
    }

    fn set(&mut self, path: &str, value: Value) {
        let mut parts = Parts::of(self.transform);
        match (path, value) {
            ("opacity", Value::Number(v)) => self.opacity = v.clamp(0.0, 1.0),
            ("position", Value::Point(p)) => self.position = p,
            ("position.x", Value::Number(v)) => self.position.x = v,
            ("position.y", Value::Number(v)) => self.position.y = v,
            ("transform", Value::Transform(m)) => self.transform = layers::affine_of(m),
            ("transform.scale", Value::Number(v)) => (parts.sx, parts.sy) = (v, v),
            ("transform.scale.x", Value::Number(v)) => parts.sx = v,
            ("transform.scale.y", Value::Number(v)) => parts.sy = v,
            ("transform.rotation" | "transform.rotation.z", Value::Number(v)) => parts.angle = v,
            ("transform.translation", Value::Point(p)) => (parts.tx, parts.ty) = (p.x, p.y),
            ("transform.translation.x", Value::Number(v)) => parts.tx = v,
            ("transform.translation.y", Value::Number(v)) => parts.ty = v,
            _ => return,
        }
        if path.starts_with("transform.") {
            self.transform = parts.transform();
        }
    }
}

fn eased(animation: Id, progress: f64) -> f64 {
    curve_of(object(animation, "timingFunction")).map_or(progress, |c| c.at(progress))
}

/// The animation's value at `progress` of its simple duration with `lap`
/// cycles behind it, for its key path, over what is shown without it
/// (`under`). A key frame animation's `path` (for a point) wins over its
/// values; cumulative laps each start from the end of the ones before.
fn value_at(animation: Id, progress: f64, lap: f64, under: &Shown) -> Option<(String, Value)> {
    let path = string::get(object(animation, "keyPath"));
    let model = under.get(&path)?;
    let eased = eased(animation, progress);
    let (values, curve_path) = (object(animation, "values"), object(animation, "path"));
    let (value, last) = if curve_path != 0 && matches!(model, Value::Point(_)) {
        (Value::Point(path_sample(curve_path, eased)?.0), Value::Point(path_sample(curve_path, 1.0)?.0))
    } else if values != 0 {
        keyframe_value(animation, values, eased)?
    } else {
        let (from, to, by) = (read_value(object(animation, "fromValue")), read_value(object(animation, "toValue")), read_value(object(animation, "byValue")));
        let (start, end) = match (from, to, by) {
            (Some(f), Some(t), _) => (f, t),
            (Some(f), None, Some(b)) => (f, Value::add(f, b)?),
            (None, Some(t), Some(b)) => (Value::subtract(t, b)?, t),
            (Some(f), None, None) => (f, model),
            (None, Some(t), None) => (model, t),
            (None, None, Some(b)) => (model, Value::add(model, b)?),
            (None, None, None) => (model, model),
        };
        (Value::lerp(start, end, eased)?, end)
    };
    let cumulative = lap > 0.0 && number(animation, "cumulative", 0.0) != 0.0;
    let value = if cumulative { Value::add(Value::times(last, lap), value)? } else { value };
    let value = if number(animation, "additive", 0.0) != 0.0 { Value::add(model, value)? } else { value };
    Some((path, value))
}

/// The point `f` (0 to 1) of the way along a CGPath at even speed, and the
/// path's heading there (radians). Lines between figures take no time.
fn path_sample(path: Id, f: f64) -> Option<(Point, f64)> {
    let figures = crate::fw::graphics::path_points(path);
    let first = *figures.iter().flatten().next()?;
    let steps: Vec<(Point, Point)> = figures.iter().flat_map(|points| points.windows(2).map(|w| (w[0], w[1]))).collect();
    let length = |(a, b): &(Point, Point)| (b.x - a.x).hypot(b.y - a.y);
    let total: f64 = steps.iter().map(length).sum();
    let mut left = f.clamp(0.0, 1.0) * total;
    let mut found = (first, 0.0);
    for step in steps.iter().filter(|s| length(s) > 0.0) {
        let (a, b) = *step;
        let (len, heading) = (length(step), (b.y - a.y).atan2(b.x - a.x));
        let t = (left / len).min(1.0);
        found = (Point { x: a.x + (b.x - a.x) * t, y: a.y + (b.y - a.y) * t }, heading);
        if left <= len {
            break;
        }
        left -= len;
    }
    Some(found)
}

/// A path animation's turn along its path (rotationMode auto or
/// autoReverse), at `progress` of its simple duration.
fn path_turn(animation: Id, progress: f64) -> Option<f64> {
    let path = object(animation, "path");
    let mode = object(animation, "rotationMode");
    if path == 0 || mode == 0 {
        return None;
    }
    let heading = path_sample(path, eased(animation, progress))?.1;
    match string::get(mode).as_str() {
        "auto" => Some(heading),
        "autoReverse" => Some(heading + std::f64::consts::PI),
        _ => None,
    }
}

/// The key frames' value at `progress`, and their last value.
fn keyframe_value(animation: Id, values: Id, progress: f64) -> Option<(Value, Value)> {
    let values: Vec<Value> = collections::array_items(values).into_iter().map(read_value).collect::<Option<_>>()?;
    let n = values.len();
    match n {
        0 => return None,
        1 => return Some((values[0], values[0])),
        _ => {}
    }
    let last = values[n - 1];
    let mode = string::get(anim_calculation_mode(animation, 0));
    let given = object(animation, "keyTimes");
    let given: Vec<f64> = if given == 0 { Vec::new() } else { collections::array_items(given).into_iter().map(layers::number_value).collect() };
    if mode == "discrete" {
        let times: Vec<f64> = if given.len() >= n { given } else { (0..n).map(|i| i as f64 / n as f64).collect() };
        let at = times.iter().take(n).rposition(|&t| t <= progress).unwrap_or(0);
        return Some((values[at], last));
    }
    // Paced modes work out their own times and ignore keyTimes and timingFunctions.
    let paced = mode == "paced" || mode == "cubicPaced";
    let times: Vec<f64> = if paced {
        let steps: Vec<f64> = values.windows(2).map(|w| Value::distance(w[0], w[1])).collect();
        let total: f64 = steps.iter().sum();
        let mut sum = 0.0;
        std::iter::once(0.0).chain(steps.iter().map(|s| {
            sum += s;
            if total > 0.0 { sum / total } else { 0.0 }
        })).collect()
    } else if given.len() == n {
        given
    } else {
        (0..n).map(|i| i as f64 / (n - 1) as f64).collect()
    };
    let segment = (0..n - 1).rfind(|&i| times[i] <= progress).unwrap_or(0);
    let span = times[segment + 1] - times[segment];
    let mut f = if span > 0.0 { ((progress - times[segment]) / span).clamp(0.0, 1.0) } else { 1.0 };
    let curves = object(animation, "timingFunctions");
    if curves != 0 && !paced {
        if let Some(curve) = collections::array_items(curves).get(segment).and_then(|&c| curve_of(c)) {
            f = curve.at(f);
        }
    }
    Some((Value::lerp(values[segment], values[segment + 1], f)?, last))
}

fn is_group(animation: Id) -> bool {
    let class = runtime::class_named("CAAnimationGroup");
    let rt = lock();
    rt.is_subclass(rt.class_of(animation), class)
}

/// Applies an animation shown at its parent's time `t` to `shown`.
fn apply(animation: Id, t: f64, fallback: f64, shown: &mut Shown) {
    let timing = timing_of(animation, fallback);
    let Some(progress) = timing.state(t).progress else { return };
    if is_group(animation) {
        // Its animations run on its own time (clipped to its duration).
        let eased = eased(animation, progress);
        let children = object(animation, "animations");
        for child in if children == 0 { Vec::new() } else { collections::array_items(children) } {
            apply(child, eased * timing.duration, timing.duration, shown);
        }
    } else if let Some((path, value)) = value_at(animation, progress, timing.lap(t), shown) {
        shown.set(&path, value);
        if let Some(turn) = path_turn(animation, progress) {
            shown.set("transform.rotation", Value::Number(turn));
        }
    }
}

// ----- animations running on layers -----

struct Running {
    layer: Id,
    key: Option<String>,
    /// Our copy of what the game added.
    animation: Id,
    started: bool,
    stopped: bool,
}

static RUNNING: Mutex<Vec<Running>> = Mutex::new(Vec::new());

fn running() -> std::sync::MutexGuard<'static, Vec<Running>> {
    RUNNING.lock().unwrap_or_else(|e| e.into_inner())
}

/// What the layer shows now, over `model` (its own values), with its
/// animations: later ones over earlier ones, as Core Animation stacks them.
pub fn presented(layer: Id, model: Shown) -> Shown {
    presented_at(layer, model, runloop::media_time())
}

pub fn presented_at(layer: Id, model: Shown, now: f64) -> Shown {
    let list: Vec<Id> = running().iter().filter(|r| r.layer == layer).map(|r| r.animation).collect();
    if list.is_empty() {
        return model;
    }
    let t = layers::local_time(layer, now);
    let mut shown = model;
    for animation in list {
        apply(animation, t, DEFAULT_DURATION, &mut shown);
    }
    shown
}

/// Whether a layer has animations on it now.
pub fn animating(layer: Id) -> bool {
    running().iter().any(|r| r.layer == layer)
}

fn tell_started(animation: Id) {
    let delegate = object(animation, "delegate");
    if delegate != 0 && runtime::responds(delegate, "animationDidStart:") {
        runtime::send(delegate, "animationDidStart:", &[animation]);
    }
}

fn tell_stopped(animation: Id, finished: bool) {
    let delegate = object(animation, "delegate");
    if delegate != 0 && runtime::responds(delegate, "animationDidStop:finished:") {
        runtime::send(delegate, "animationDidStop:finished:", &[animation, finished as usize]);
    }
}

/// Sends the delegate calls that are due and drops the animations that
/// ended; then waits for the next start or end. Runs on the main thread.
pub fn tick() {
    tick_at(None, runloop::media_time());
}

/// `only`: tick just this layer's animations (a test's fake clock must not
/// end other layers' ones).
pub(crate) fn tick_at(only: Option<Id>, now: f64) {
    let mut started = Vec::new();
    let mut stopped = Vec::new();
    let mut next = f64::INFINITY;
    {
        let mut list = running();
        list.retain_mut(|r| {
            if only.is_some_and(|layer| layer != r.layer) {
                return true;
            }
            let timing = timing_of(r.animation, DEFAULT_DURATION);
            let t = layers::local_time(r.layer, now);
            let state = timing.state(t);
            if state.started && !r.started {
                r.started = true;
                started.push(r.animation);
            }
            if state.ended && !r.stopped {
                r.stopped = true;
                let removed = number(r.animation, "removedOnCompletion", 1.0) != 0.0;
                stopped.push((r.animation, removed));
                if removed {
                    return false;
                }
            }
            let wake = if !r.started { Some(timing.begin) } else if !r.stopped { Some(timing.end()) } else { None };
            if let Some(local) = wake.filter(|w| w.is_finite()) {
                if let Some(at) = layers::global_time(r.layer, local) {
                    next = next.min(at);
                }
            }
            true
        });
    }
    for animation in started {
        tell_started(animation);
    }
    for (animation, removed) in stopped {
        tell_stopped(animation, true);
        crate::fw::smallclasses::animation_finished(animation);
        stopped_now(animation, true);
        if removed {
            let_go(animation);
        }
    }
    if next.is_finite() {
        wake_at(next);
    }
}

/// Whether an animation (a layer's copy) is on a layer and hasn't ended.
pub fn in_progress(animation: Id) -> bool {
    running().iter().any(|r| r.animation == animation && !r.stopped)
}

/// The layer let go of its copy: the copy lets go of its delegate, as its
/// dealloc would on Apple (objects here are never freed).
fn let_go(animation: Id) {
    anim_set_delegate(animation, 0, 0);
}

/// When the clock next wakes (infinity: not set).
static WAKE: Mutex<f64> = Mutex::new(f64::INFINITY);

fn wake_at(at: f64) {
    let mut wake = WAKE.lock().unwrap_or_else(|e| e.into_inner());
    if at < *wake - 1e-6 {
        *wake = at;
        drop(wake);
        runloop::post_after((at - runloop::media_time()).max(0.0), Box::new(|| {
            *WAKE.lock().unwrap_or_else(|e| e.into_inner()) = f64::INFINITY;
            tick();
        }));
    }
}

/// Something changed a layer's time (speed, offset, begin): look again on
/// the next turn of the main run loop.
pub fn reschedule() {
    if !running().is_empty() {
        wake_at(runloop::media_time());
    }
}

/// -addAnimation:forKey:. As on Apple: the animation is copied, one with
/// the same key is removed first, a duration of 0 becomes the transaction's
/// animationDuration, and it starts now unless it has a beginTime; its
/// start is told on the next turn of the main run loop.
pub fn add(layer: Id, animation: Id, key: Id) -> Id {
    if animation == 0 {
        return 0;
    }
    let key = if key == 0 { None } else { Some(string::get(key)) };
    if key.is_some() {
        remove_where(|r| r.layer == layer && r.key == key);
    }
    let copy = runtime::send(animation, "copy", &[]);
    let now = runloop::media_time();
    if number(copy, "beginTime", 0.0) == 0.0 {
        set_number(copy, "beginTime", layers::local_time(layer, now));
    }
    if number(copy, "duration", 0.0) <= 0.0 {
        set_number(copy, "duration", crate::fw::smallclasses::animation_duration());
    }
    crate::fw::smallclasses::animation_added(copy);
    running().push(Running { layer, key, animation: copy, started: false, stopped: false });
    wake_at(now);
    crate::fw::pictures::draw_soon();
    copy
}

/// When the last running animation ends (media time; infinity for one that
/// repeats forever); None when none runs.
pub fn latest_end() -> Option<f64> {
    let list: Vec<(Id, Id)> = running().iter().filter(|r| !r.stopped).map(|r| (r.layer, r.animation)).collect();
    list.into_iter()
        .map(|(layer, animation)| {
            let end = timing_of(animation, DEFAULT_DURATION).end();
            if end.is_finite() { layers::global_time(layer, end).unwrap_or(f64::INFINITY) } else { f64::INFINITY }
        })
        .reduce(f64::max)
}

/// What a UIView animation block asks for (fw/uikit.rs).
#[derive(Clone, Copy, Debug)]
pub struct ViewTiming {
    pub duration: f64,
    pub delay: f64,
    pub curve: Curve,
    pub repeat_count: f64,
    pub autoreverses: bool,
}

/// Animates a change UIKit made to a view's layer, as a UIView animation
/// does: from what showed before (`from`) to the layer's own new value,
/// keyed by its key path (so it replaces an earlier one), the old value
/// showing during a delay. Answers the copy running on the layer.
pub fn animate_view_change(layer: Id, key: &str, from: Value, timing: ViewTiming) -> Id {
    let animation = runtime::send(runtime::send(runtime::class_named("CABasicAnimation"), "alloc", &[]), "init", &[]);
    let key = string::make(key);
    hold(animation, "keyPath", key);
    hold(animation, "fromValue", boxed_value(from));
    set_number(animation, "duration", timing.duration);
    if timing.delay > 0.0 {
        set_number(animation, "beginTime", layers::local_time(layer, runloop::media_time()) + timing.delay);
        hold(animation, "fillMode", string::make("backwards"));
    }
    set_number(animation, "repeatCount", timing.repeat_count);
    set_number(animation, "autoreverses", timing.autoreverses as u8 as f64);
    hold(animation, "timingFunction", make_curve(runtime::class_named("CAMediaTimingFunction"), timing.curve));
    add(layer, animation, key)
}

/// What to do once an animation (a layer's copy) stops: finished or
/// removed before its end (false). Runs on the main thread.
static WHEN_STOPPED: Mutex<Vec<(Id, Box<dyn FnOnce(bool) + Send>)>> = Mutex::new(Vec::new());

pub fn when_stopped(animation: Id, then: Box<dyn FnOnce(bool) + Send>) {
    WHEN_STOPPED.lock().unwrap_or_else(|e| e.into_inner()).push((animation, then));
}

fn stopped_now(animation: Id, finished: bool) {
    let due: Vec<_> = {
        let mut waiting = WHEN_STOPPED.lock().unwrap_or_else(|e| e.into_inner());
        let (due, rest) = std::mem::take(&mut *waiting).into_iter().partition(|(a, _)| *a == animation);
        *waiting = rest;
        due
    };
    for (_, then) in due {
        then(finished);
    }
}

/// Removes animations; those that hadn't ended tell their delegate
/// (finished = NO) on the next turn of the main run loop.
fn remove_where(matches: impl Fn(&Running) -> bool) {
    let mut gone = Vec::new();
    running().retain(|r| {
        if matches(r) {
            gone.push((r.animation, r.stopped));
            false
        } else {
            true
        }
    });
    for (animation, stopped) in gone {
        runloop::post(runloop::Work::Native(Box::new(move || {
            if !stopped {
                tell_stopped(animation, false);
                crate::fw::smallclasses::animation_finished(animation);
                stopped_now(animation, false);
            }
            let_go(animation);
        })));
    }
}

extern "C" fn layer_add_animation(this: Id, _: Sel, animation: Id, key: Id) {
    add(this, animation, key);
}

extern "C" fn layer_remove_animation(this: Id, _: Sel, key: Id) {
    let key = string::get(key);
    remove_where(|r| r.layer == this && r.key.as_deref() == Some(key.as_str()));
}

extern "C" fn layer_remove_all_animations(this: Id, _: Sel) {
    remove_where(|r| r.layer == this);
}

extern "C" fn layer_animation_for_key(this: Id, _: Sel, key: Id) -> Id {
    let key = string::get(key);
    running().iter().find(|r| r.layer == this && r.key.as_deref() == Some(key.as_str())).map_or(0, |r| r.animation)
}

extern "C" fn layer_animation_keys(this: Id, _: Sel) -> Id {
    let keys: Vec<Id> = running().iter().filter(|r| r.layer == this).filter_map(|r| r.key.as_deref().map(string::make)).collect();
    if keys.is_empty() {
        0
    } else {
        collections::array_from(&keys)
    }
}

// ----- key paths on layers -----

/// The layer's own values of what animations change.
fn model_of(layer: Id) -> Shown {
    Shown { opacity: layers::opacity(layer), position: layers::position(layer), transform: layers::affine_of(layers::transform(layer)) }
}

/// A value as an object of the kind valueForKeyPath: hands back.
fn boxed_value(value: Value) -> Id {
    match value {
        Value::Number(v) => collections::number_float(v),
        Value::Point(p) => layers::boxed("valueWithCGPoint:", p),
        Value::Transform(m) => layers::boxed("valueWithCATransform3D:", m),
    }
}

/// valueForKeyPath: on a layer: Core Animation's own sub-paths
/// ("transform.scale", "position.x" and the rest), else key by key.
extern "C" fn layer_value_for_key_path(this: Id, _: Sel, path: Id) -> Id {
    let name = string::get(path);
    if name.contains('.') {
        if let Some(value) = model_of(this).get(&name) {
            return boxed_value(value);
        }
    }
    let Some((first, rest)) = name.split_once('.') else { return runtime::send(this, "valueForKey:", &[path]) };
    let head = runtime::send(this, "valueForKey:", &[string::make(first)]);
    if head == 0 {
        0
    } else {
        runtime::send(head, "valueForKeyPath:", &[string::make(rest)])
    }
}

/// setValue:forKeyPath: on a layer (see `layer_value_for_key_path`).
extern "C" fn layer_set_value_for_key_path(this: Id, _: Sel, value: Id, path: Id) {
    let name = string::get(path);
    if name.contains('.') {
        let mut shown = model_of(this);
        if let (Some(_), Some(new)) = (shown.get(&name), read_value(value)) {
            shown.set(&name, new);
            if name.starts_with("position") {
                layers::send_struct(this, "setPosition:", shown.position);
            } else {
                layers::send_struct(this, "setTransform:", layers::affine_to_3d(shown.transform));
            }
            return;
        }
    }
    match name.split_once('.') {
        None => {
            runtime::send(this, "setValue:forKey:", &[value, path]);
        }
        Some((first, rest)) => {
            let head = runtime::send(this, "valueForKey:", &[string::make(first)]);
            if head != 0 {
                runtime::send(head, "setValue:forKeyPath:", &[value, string::make(rest)]);
            }
        }
    }
}

pub fn register(r: &mut Registry) {
    let function = define_class("CAMediaTimingFunction", "NSObject", 40);
    class_methods(
        function,
        &[("functionWithName:", function_with_name as usize), ("functionWithControlPoints::::", function_with_points as usize)],
    );
    methods(function, &[("initWithControlPoints::::", function_init_with_points as usize), ("getControlPointAtIndex:values:", function_get_point as usize)]);

    let animation = define_class("CAAnimation", "NSObject", 8);
    class_methods(animation, &[("animation", anim_class_animation as usize), ("defaultValueForKey:", zero_class_with as usize), ("needsDisplayForKey:", no_class_with as usize), ("shouldArchiveValueForKey:", anim_should_archive as usize)]);
    methods(
        animation,
        &[
            ("delegate", anim_delegate as usize),
            ("setDelegate:", anim_set_delegate as usize),
            ("duration", anim_duration as usize),
            ("setDuration:", anim_set_duration as usize),
            ("beginTime", anim_begin as usize),
            ("setBeginTime:", anim_set_begin as usize),
            ("speed", anim_speed as usize),
            ("setSpeed:", anim_set_speed as usize),
            ("timeOffset", anim_offset as usize),
            ("setTimeOffset:", anim_set_offset as usize),
            ("repeatCount", anim_repeat_count as usize),
            ("setRepeatCount:", anim_set_repeat_count as usize),
            ("repeatDuration", anim_repeat_duration as usize),
            ("setRepeatDuration:", anim_set_repeat_duration as usize),
            ("autoreverses", anim_autoreverses as usize),
            ("setAutoreverses:", anim_set_autoreverses as usize),
            ("fillMode", anim_fill_mode as usize),
            ("setFillMode:", anim_set_fill_mode as usize),
            ("isRemovedOnCompletion", anim_removed as usize),
            ("setRemovedOnCompletion:", anim_set_removed as usize),
            ("timingFunction", anim_timing as usize),
            ("setTimingFunction:", anim_set_timing as usize),
            ("copyWithZone:", anim_copy as usize),
            ("valueForKey:", anim_value_for_key as usize),
            ("setValue:forKey:", anim_set_value_for_key as usize),
        ],
    );
    let property = define_class("CAPropertyAnimation", "CAAnimation", 8);
    class_methods(property, &[("animationWithKeyPath:", anim_with_key_path as usize)]);
    methods(
        property,
        &[
            ("keyPath", anim_key_path as usize),
            ("setKeyPath:", anim_set_key_path as usize),
            ("isAdditive", anim_additive as usize),
            ("setAdditive:", anim_set_additive as usize),
            ("isCumulative", anim_cumulative as usize),
            ("setCumulative:", anim_set_cumulative as usize),
        ],
    );
    let basic = define_class("CABasicAnimation", "CAPropertyAnimation", 8);
    methods(
        basic,
        &[
            ("fromValue", anim_from as usize),
            ("setFromValue:", anim_set_from as usize),
            ("toValue", anim_to as usize),
            ("setToValue:", anim_set_to as usize),
            ("byValue", anim_by as usize),
            ("setByValue:", anim_set_by as usize),
        ],
    );
    let keyframe = define_class("CAKeyframeAnimation", "CAPropertyAnimation", 8);
    methods(
        keyframe,
        &[
            ("values", anim_values as usize),
            ("setValues:", anim_set_values as usize),
            ("keyTimes", anim_key_times as usize),
            ("setKeyTimes:", anim_set_key_times as usize),
            ("timingFunctions", anim_timings as usize),
            ("setTimingFunctions:", anim_set_timings as usize),
            ("calculationMode", anim_calculation_mode as usize),
            ("setCalculationMode:", anim_set_calculation_mode as usize),
            ("path", anim_path as usize),
            ("setPath:", anim_set_path as usize),
            ("rotationMode", anim_rotation_mode as usize),
            ("setRotationMode:", anim_set_rotation_mode as usize),
        ],
    );
    let group = define_class("CAAnimationGroup", "CAAnimation", 8);
    methods(group, &[("animations", anim_animations as usize), ("setAnimations:", anim_set_animations as usize)]);
    let transition = define_class("CATransition", "CAAnimation", 8);
    methods(
        transition,
        &[
            ("type", transition_type as usize),
            ("setType:", transition_set_type as usize),
            ("subtype", anim_subtype as usize),
            ("setSubtype:", anim_set_subtype as usize),
            ("startProgress", anim_start_progress as usize),
            ("setStartProgress:", anim_set_start_progress as usize),
            ("endProgress", anim_end_progress as usize),
            ("setEndProgress:", anim_set_end_progress as usize),
        ],
    );

    methods(
        runtime::class_named("CALayer"),
        &[
            ("addAnimation:forKey:", layer_add_animation as usize),
            ("removeAnimationForKey:", layer_remove_animation as usize),
            ("removeAllAnimations", layer_remove_all_animations as usize),
            ("animationForKey:", layer_animation_for_key as usize),
            ("animationKeys", layer_animation_keys as usize),
            ("valueForKeyPath:", layer_value_for_key_path as usize),
            ("setValue:forKeyPath:", layer_set_value_for_key_path as usize),
        ],
    );

    // Apple's constant strings, with Apple's values.
    for (name, value) in [
        ("kCAFillModeRemoved", "removed"),
        ("kCAFillModeForwards", "forwards"),
        ("kCAFillModeBackwards", "backwards"),
        ("kCAFillModeBoth", "both"),
        ("kCAMediaTimingFunctionLinear", "linear"),
        ("kCAMediaTimingFunctionEaseIn", "easeIn"),
        ("kCAMediaTimingFunctionEaseOut", "easeOut"),
        ("kCAMediaTimingFunctionEaseInEaseOut", "easeInEaseOut"),
        ("kCAMediaTimingFunctionDefault", "default"),
        ("kCAAnimationLinear", "linear"),
        ("kCAAnimationDiscrete", "discrete"),
        ("kCAAnimationPaced", "paced"),
        ("kCAAnimationCubic", "cubic"),
        ("kCAAnimationCubicPaced", "cubicPaced"),
        ("kCAAnimationRotateAuto", "auto"),
        ("kCAAnimationRotateAutoReverse", "autoReverse"),
        ("kCATransitionFade", "fade"),
        ("kCATransitionMoveIn", "moveIn"),
        ("kCATransitionPush", "push"),
        ("kCATransitionReveal", "reveal"),
        ("kCATransitionFromRight", "fromRight"),
        ("kCATransitionFromLeft", "fromLeft"),
        ("kCATransitionFromTop", "fromTop"),
        ("kCATransitionFromBottom", "fromBottom"),
        ("kCAGravityResize", "resize"),
        ("kCAGravityResizeAspect", "resizeAspect"),
        ("kCAGravityResizeAspectFill", "resizeAspectFill"),
        ("kCAGravityCenter", "center"),
        ("kCAGradientLayerAxial", "axial"),
        ("kCALineCapButt", "butt"),
        ("kCALineCapRound", "round"),
        ("kCALineCapSquare", "square"),
        ("kCALineJoinMiter", "miter"),
        ("kCALineJoinRound", "round"),
        ("kCALineJoinBevel", "bevel"),
        ("kCAFillRuleNonZero", "non-zero"),
        ("kCAFillRuleEvenOdd", "even-odd"),
    ] {
        r.data(name, leak(string::make(value)));
    }
}

/// The Core Animation string constants (for 32-bit games' data imports).
pub const CONSTANT_NAMES: &[&str] = &[
    "kCAFillModeRemoved", "kCAFillModeForwards", "kCAFillModeBackwards", "kCAFillModeBoth", "kCAMediaTimingFunctionLinear",
    "kCAMediaTimingFunctionEaseIn", "kCAMediaTimingFunctionEaseOut", "kCAMediaTimingFunctionEaseInEaseOut",
    "kCAMediaTimingFunctionDefault", "kCAAnimationLinear", "kCAAnimationDiscrete", "kCAAnimationPaced", "kCAAnimationCubic",
    "kCAAnimationCubicPaced", "kCAAnimationRotateAuto", "kCAAnimationRotateAutoReverse", "kCATransitionFade", "kCATransitionMoveIn",
    "kCATransitionPush", "kCATransitionReveal", "kCATransitionFromRight", "kCATransitionFromLeft", "kCATransitionFromTop",
    "kCATransitionFromBottom", "kCAGravityResize", "kCAGravityResizeAspect", "kCAGravityResizeAspectFill", "kCAGravityCenter",
    "kCAGradientLayerAxial", "kCALineCapButt", "kCALineCapRound", "kCALineCapSquare", "kCALineJoinMiter", "kCALineJoinRound",
    "kCALineJoinBevel", "kCAFillRuleNonZero", "kCAFillRuleEvenOdd",
];

extern "C" fn zero_class_with(_: Class, _: Sel, _key: Id) -> Id {
    0
}

extern "C" fn no_class_with(_: Class, _: Sel, _key: Id) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fw::geometry::IDENTITY;

    fn setup() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-3
    }

    /// Apple's named curves: ease-in-ease-out is symmetric and halfway at
    /// half time; ease-in lags, ease-out leads; linear is a straight line.
    #[test]
    fn timing_curves_bend_like_apples() {
        let both = Curve::named("easeInEaseOut").unwrap();
        assert!(close(both.at(0.5), 0.5) && close(both.at(0.0), 0.0) && close(both.at(1.0), 1.0));
        assert!(close(both.at(0.25) + both.at(0.75), 1.0));
        assert!(Curve::named("easeIn").unwrap().at(0.5) < 0.4);
        assert!(Curve::named("easeOut").unwrap().at(0.5) > 0.6);
        assert!(close(Curve::LINEAR.at(0.3), 0.3));
    }

    /// CAMediaTiming: begin, speed, repeats, autoreverse and fill modes
    /// decide what shows when, as Core Animation does.
    #[test]
    fn timing_follows_begin_speed_repeats_and_fill() {
        let plain = Timing { begin: 10.0, duration: 2.0, ..Default::default() };
        assert_eq!(plain.state(9.0), State { started: false, ended: false, progress: None });
        assert_eq!(plain.state(11.0), State { started: true, ended: false, progress: Some(0.5) });
        assert_eq!(plain.state(12.0), State { started: true, ended: true, progress: None }, "removed fill shows nothing after");
        assert_eq!(plain.end(), 12.0);
        let held = Timing { forwards: true, backwards: true, ..plain };
        assert_eq!(held.state(13.0).progress, Some(1.0));
        assert_eq!(held.state(9.0).progress, Some(0.0));
        let fast = Timing { speed: 2.0, ..plain };
        assert_eq!((fast.state(10.5).progress, fast.end()), (Some(0.5), 11.0));
        let back = Timing { autoreverses: true, repeat_count: 2.0, forwards: true, ..plain };
        assert_eq!(back.end(), 18.0);
        assert_eq!(back.state(13.0).progress, Some(0.5), "on the way back");
        assert_eq!(back.state(14.5).progress, Some(0.25), "second lap");
        assert_eq!(back.state(20.0).progress, Some(0.0), "ends where it began");
        let forever = Timing { repeat_count: f32::INFINITY as f64, ..plain };
        assert!(forever.end().is_infinite() && !forever.state(1e6).ended);
        let half = Timing { repeat_count: 2.5, forwards: true, ..plain };
        assert_eq!(half.state(100.0).progress, Some(0.5));
    }

    fn basic(path: &str, from: Id, to: Id) -> Id {
        let animation = anim_with_key_path(runtime::class_named("CABasicAnimation"), 0, string::make(path));
        anim_set_from(animation, 0, from);
        anim_set_to(animation, 0, to);
        animation
    }

    static HEARD: Mutex<Vec<(&'static str, Id, bool)>> = Mutex::new(Vec::new());

    extern "C" fn did_start(_: Id, _: Sel, animation: Id) {
        HEARD.lock().unwrap().push(("start", animation, true));
    }

    extern "C" fn did_stop(_: Id, _: Sel, animation: Id, finished: u8) {
        HEARD.lock().unwrap().push(("stop", animation, finished != 0));
    }

    /// On a layer: a copy runs (with the game's keyed values), its delegate
    /// hears start and stop(YES) at the right times, a held animation keeps
    /// its last value, and a removed one stops with NO. The view's opacity
    /// shown follows the animation while the model value stays.
    #[test]
    fn layer_animations_run_tell_their_delegate_and_hold() {
        setup();
        let class = define_class("CAAnimTestDelegate", "NSObject", 8);
        methods(class, &[("animationDidStart:", did_start as usize), ("animationDidStop:finished:", did_stop as usize)]);
        let delegate = runtime::send(runtime::send(class, "alloc", &[]), "init", &[]);
        let layer = runtime::send(runtime::send(runtime::class_named("CALayer"), "alloc", &[]), "init", &[]);
        let fade = basic("opacity", collections::number_float(0.0), collections::number_float(1.0));
        anim_set_duration(fade, 0, 1.0);
        anim_set_delegate(fade, 0, delegate);
        anim_set_value_for_key(fade, 0, string::make("intro"), string::make("name"));
        let model = Shown { opacity: 0.2, position: Point::default(), transform: IDENTITY };
        add(layer, fade, string::make("fade"));
        let copy = layer_animation_for_key(layer, 0, string::make("fade"));
        assert!(copy != 0 && copy != fade, "the layer holds a copy");
        assert_eq!(string::get(anim_value_for_key(copy, 0, string::make("name"))), "intro");
        let begin = number(copy, "beginTime", 0.0);
        assert!(close(presented_at(layer, model, begin + 0.5).opacity, 0.5));
        // (Another test running the main loop may send a call first: wait for it.)
        let heard_after = |tick: f64, n: usize| {
            tick_at(Some(layer), tick);
            let end = std::time::Instant::now() + std::time::Duration::from_millis(300);
            while HEARD.lock().unwrap().len() < n && std::time::Instant::now() < end {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            HEARD.lock().unwrap().clone()
        };
        assert_eq!(heard_after(begin + 0.1, 1), [("start", copy, true)]);
        assert_eq!(heard_after(begin + 1.5, 2), [("start", copy, true), ("stop", copy, true)]);
        assert_eq!(layer_animation_for_key(layer, 0, string::make("fade")), 0, "removed on completion");
        assert_eq!(presented_at(layer, model, begin + 1.5).opacity, 0.2, "back to the model value");

        // Held at its end (forwards, not removed).
        HEARD.lock().unwrap().clear();
        let slide = basic("position.x", collections::number_float(0.0), collections::number_float(100.0));
        anim_set_duration(slide, 0, 1.0);
        anim_set_fill_mode(slide, 0, string::make("forwards"));
        anim_set_removed(slide, 0, 0);
        add(layer, slide, 0);
        // (A fake clock for one layer must leave other tests' layers alone.)
        let other = runtime::send(runtime::send(runtime::class_named("CALayer"), "alloc", &[]), "init", &[]);
        let wait = basic("opacity", collections::number_float(0.0), collections::number_float(1.0));
        anim_set_duration(wait, 0, 2.0);
        add(other, wait, 0);
        let begin = layers::local_time(layer, runloop::media_time());
        tick_at(Some(layer), begin + 5.0);
        assert!(animating(other), "another layer's animation still runs");
        layer_remove_all_animations(other, 0);
        assert!(close(presented_at(layer, model, begin + 5.0).position.x, 100.0));
        assert!(animating(layer));
        layer_remove_all_animations(layer, 0);
        assert!(!animating(layer));

        // Removed before its end: told NO on the next run-loop turn.
        HEARD.lock().unwrap().clear();
        let long = basic("opacity", 0, collections::number_float(1.0));
        anim_set_duration(long, 0, 100.0);
        anim_set_delegate(long, 0, delegate);
        add(layer, long, string::make("long"));
        let copy = layer_animation_for_key(layer, 0, string::make("long"));
        layer_remove_animation(layer, 0, string::make("long"));
        let end = runloop::media_time() + 0.5;
        while !HEARD.lock().unwrap().contains(&("stop", copy, false)) && runloop::media_time() < end {
            runloop::run_main_loop_briefly();
        }
        let heard = HEARD.lock().unwrap().clone();
        assert!(heard.contains(&("stop", copy, false)), "{heard:?}");
    }

    /// Key frames: values at key times (even without them), per-segment
    /// curves, discrete steps; transforms turn through their angle.
    #[test]
    fn keyframes_and_transforms_interpolate() {
        setup();
        let class = runtime::class_named("CAKeyframeAnimation");
        let hop = anim_with_key_path(class, 0, string::make("transform.scale"));
        let numbers = |list: &[f64]| collections::array_from(&list.iter().map(|&v| collections::number_float(v)).collect::<Vec<_>>());
        anim_set_values(hop, 0, numbers(&[1.0, 2.0, 1.0]));
        let model = Shown { opacity: 1.0, position: Point::default(), transform: IDENTITY };
        assert!(close(value_at(hop, 0.25, 0.0, &model).map(|(_, v)| match v { Value::Number(n) => n, _ => 0.0 }).unwrap(), 1.5));
        anim_set_key_times(hop, 0, numbers(&[0.0, 0.8, 1.0]));
        assert!(close(match value_at(hop, 0.4, 0.0, &model).unwrap().1 { Value::Number(n) => n, _ => 0.0 }, 1.5));
        anim_set_calculation_mode(hop, 0, string::make("discrete"));
        assert_eq!(value_at(hop, 0.9, 0.0, &model).unwrap().1, Value::Number(2.0));
        let mut shown = model;
        shown.set("transform.scale", Value::Number(2.0));
        shown.set("transform.rotation.z", Value::Number(std::f64::consts::FRAC_PI_2));
        let parts = Parts::of(shown.transform);
        assert!(close(parts.sx, 2.0) && close(parts.sy, 2.0) && close(parts.angle, std::f64::consts::FRAC_PI_2));
        // Half way from no turn to a half turn is a quarter turn, not a squash.
        let half = Value::lerp(Value::Transform(layers::affine_to_3d(IDENTITY)), Value::Transform(layers::affine_to_3d(Parts { tx: 0.0, ty: 0.0, sx: 1.0, sy: 1.0, angle: 3.0 }.transform())), 0.5).unwrap();
        let Value::Transform(m) = half else { panic!() };
        assert!(close(Parts::of(layers::affine_of(m)).sx, 1.0) && close(Parts::of(layers::affine_of(m)).angle, 1.5));
    }

    fn number_of(found: Option<(String, Value)>) -> f64 {
        match found {
            Some((_, Value::Number(n))) => n,
            other => panic!("{other:?}"),
        }
    }

    /// A cumulative animation adds each lap's end to the next; paced key
    /// frames ignore the per-segment curves; a path moves a position along
    /// it at even speed (turning with it when asked).
    #[test]
    fn cumulative_paced_and_path_animations() {
        setup();
        let layer = runtime::send(runtime::send(runtime::class_named("CALayer"), "alloc", &[]), "init", &[]);
        let model = Shown { opacity: 1.0, position: Point::default(), transform: IDENTITY };
        let step = basic("position.x", collections::number_float(0.0), collections::number_float(10.0));
        anim_set_duration(step, 0, 100.0); // long: other tests running the main loop end nothing early
        anim_set_repeat_count(step, 0, 2.0);
        anim_set_cumulative(step, 0, 1);
        anim_set_fill_mode(step, 0, string::make("forwards"));
        add(layer, step, string::make("step"));
        let begin = number(layer_animation_for_key(layer, 0, string::make("step")), "beginTime", 0.0);
        assert!(close(presented_at(layer, model, begin + 50.0).position.x, 5.0));
        assert!(close(presented_at(layer, model, begin + 150.0).position.x, 15.0), "second lap starts at the first's end");
        assert!(close(presented_at(layer, model, begin + 900.0).position.x, 20.0), "held at the end of both laps");
        layer_remove_all_animations(layer, 0);

        let class = runtime::class_named("CAKeyframeAnimation");
        let paced = anim_with_key_path(class, 0, string::make("position.x"));
        let numbers = |list: &[f64]| collections::array_from(&list.iter().map(|&v| collections::number_float(v)).collect::<Vec<_>>());
        anim_set_values(paced, 0, numbers(&[0.0, 10.0, 40.0]));
        anim_set_calculation_mode(paced, 0, string::make("paced"));
        let ease_in = function_with_name(runtime::class_named("CAMediaTimingFunction"), 0, string::make("easeIn"));
        anim_set_timings(paced, 0, collections::array_from(&[ease_in, ease_in]));
        assert!(close(number_of(value_at(paced, 0.125, 0.0, &model)), 5.0));
        assert!(close(number_of(value_at(paced, 0.5, 0.0, &model)), 20.0));

        let path = crate::fw::graphics::new_path();
        crate::fw::graphics::path_move(path, Point { x: 0.0, y: 0.0 });
        crate::fw::graphics::path_line(path, Point { x: 100.0, y: 0.0 });
        crate::fw::graphics::path_line(path, Point { x: 100.0, y: 100.0 });
        let along = anim_with_key_path(class, 0, string::make("position"));
        anim_set_path(along, 0, path);
        anim_set_duration(along, 0, 100.0);
        anim_set_rotation_mode(along, 0, string::make("auto"));
        add(layer, along, string::make("along"));
        let begin = number(layer_animation_for_key(layer, 0, string::make("along")), "beginTime", 0.0);
        let quarter = presented_at(layer, model, begin + 25.0);
        assert!(close(quarter.position.x, 50.0) && close(quarter.position.y, 0.0), "{quarter:?}");
        let late = presented_at(layer, model, begin + 75.0);
        assert!(close(late.position.x, 100.0) && close(late.position.y, 50.0), "{late:?}");
        assert!(close(Parts::of(late.transform).angle, std::f64::consts::FRAC_PI_2), "turned down the path");
        layer_remove_all_animations(layer, 0);
    }

    static RELEASED: Mutex<Vec<Id>> = Mutex::new(Vec::new());
    static COMPLETED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    extern "C" fn counted_release(this: Id, _: Sel) {
        RELEASED.lock().unwrap().push(this);
    }

    extern "C" fn completed(_block: usize) {
        COMPLETED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    /// Runs the main run loop a little (at most 0.3 s) until `done`.
    fn run_until(done: impl Fn() -> bool) -> bool {
        let end = runloop::media_time() + 0.3;
        while !done() && runloop::media_time() < end {
            runloop::run_main_loop_briefly();
        }
        done()
    }

    /// A duration of 0 takes the transaction's animationDuration; the
    /// transaction's completion runs once its animations have really ended
    /// or been removed (not on a clock: a paused or endless one holds it);
    /// a layer letting go of its copy lets go of the delegate.
    #[test]
    fn transactions_give_duration_and_wait_for_their_animations() {
        setup();
        let transaction = runtime::class_named("CATransaction");
        let layer = runtime::send(runtime::send(runtime::class_named("CALayer"), "alloc", &[]), "init", &[]);
        let delegate_class = define_class("CAAnimTestCountedDelegate", "NSObject", 8);
        methods(delegate_class, &[("release", counted_release as usize)]);
        let delegate = runtime::send(runtime::send(delegate_class, "alloc", &[]), "init", &[]);
        let descriptor: [usize; 4] = [0, std::mem::size_of::<crate::libsystem::mach::BlockHeader>(), 0, 0];
        let block = crate::libsystem::mach::BlockHeader { isa: 0, flags: 1 << 28, reserved: 0, invoke: completed as usize, descriptor: &descriptor };
        let done = |n: usize| move || COMPLETED.load(std::sync::atomic::Ordering::SeqCst) >= n;

        runtime::send(transaction, "begin", &[]);
        runtime::send(transaction, "setValue:forKey:", &[collections::number_float(2.0), string::make("animationDuration")]);
        runtime::send(transaction, "setCompletionBlock:", &[&block as *const _ as usize]);
        let fade = basic("opacity", collections::number_float(0.0), collections::number_float(1.0));
        anim_set_delegate(fade, 0, delegate);
        add(layer, fade, string::make("fade"));
        let copy = layer_animation_for_key(layer, 0, string::make("fade"));
        assert_eq!(number(copy, "duration", 0.0), 2.0, "the transaction's duration");
        runtime::send(transaction, "commit", &[]);
        layers::layer_set_speed(layer, 0, 0.0);
        assert!(!run_until(done(1)), "a paused animation holds the completion");
        RELEASED.lock().unwrap().clear();
        layer_remove_all_animations(layer, 0);
        assert!(run_until(done(1)), "removed: the completion runs");
        assert!(run_until(|| RELEASED.lock().unwrap().contains(&delegate)), "the copy lets go of its delegate");
        layers::layer_set_speed(layer, 0, 1.0);

        runtime::send(transaction, "begin", &[]);
        runtime::send(transaction, "setCompletionBlock:", &[&block as *const _ as usize]);
        let forever = basic("opacity", collections::number_float(0.0), collections::number_float(1.0));
        anim_set_repeat_count(forever, 0, f32::INFINITY);
        add(layer, forever, 0);
        runtime::send(transaction, "commit", &[]);
        assert!(!run_until(done(2)), "an endless animation holds the completion");
        layer_remove_all_animations(layer, 0);
        assert!(run_until(done(2)));
    }
}
