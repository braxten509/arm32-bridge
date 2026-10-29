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
//! last value stays. What a layer animates (opacity, position, transform
//! and their parts, background colour) is read by fw/pictures.rs each time
//! it lists the screen (`presented`), so views move and fade on screen as on
//! iOS; other key paths keep only the timing and the delegate calls.
//! Transforms are taken apart (fw/geometry.rs `decompose`: scale, skew, a 3D
//! turn, move, perspective) to animate, as Core Animation does. A
//! CATransition takes its layer (and what is inside it) from what it showed
//! to what it shows now, by its type (fw/pictures.rs, `transitions`).

use crate::fw::geometry::{decompose, recompose, Point, Transform, Transform3D};
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

/// A new CAMediaTimingFunction with `curve`'s control points.
pub fn timing_function(curve: Curve) -> Id {
    make_curve(runtime::class_named("CAMediaTimingFunction"), curve)
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

/// CAAction: an animation as a layer's action is added to it for the key.
extern "C" fn anim_run_action(this: Id, _: Sel, key: Id, object: Id, _arguments: Id) {
    if object != 0 && runtime::responds(object, "addAnimation:forKey:") {
        runtime::send(object, "addAnimation:forKey:", &[this, key]);
    }
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

    /// Its own time at the parent's time `t` (inside its active time): the
    /// timeOffset only shifts the phase, wrapping round the active time
    /// (Apple's map_parent_to_active).
    fn local(&self, t: f64) -> f64 {
        let (local, active) = ((t - self.begin) * self.speed + self.offset, self.active());
        if active.is_finite() && active > 0.0 && local >= active { local.rem_euclid(active) } else { local.max(0.0) }
    }

    /// Whether it has run its active time at the parent's time `t`.
    fn over(&self, t: f64) -> bool {
        (t - self.begin) * self.speed >= self.active()
    }

    /// Where it is at the parent's time `t`.
    pub fn state(&self, t: f64) -> State {
        if t < self.begin {
            return State { started: false, ended: false, progress: self.backwards.then(|| self.progress_at(self.offset.max(0.0))) };
        }
        if self.over(t) {
            return State { started: true, ended: true, progress: self.forwards.then(|| self.end_progress()) };
        }
        State { started: true, ended: false, progress: Some(self.progress_at(self.local(t))) }
    }

    /// How many whole cycles (repeats) are behind it at the parent's time
    /// `t`: 0 in the first; held at its end, the laps before the last.
    pub fn lap(&self, t: f64) -> f64 {
        let cycle = if self.autoreverses { 2.0 * self.duration } else { self.duration };
        if t < self.begin || cycle <= 0.0 {
            return 0.0;
        }
        if self.over(t) {
            ((self.active() / cycle).ceil() - 1.0).max(0.0)
        } else {
            (self.local(t) / cycle).floor()
        }
    }

    /// When it ends, in its parent's time (never at speed 0 or repeating
    /// forever): begin + active / speed, whatever its timeOffset.
    pub fn end(&self) -> f64 {
        if self.speed <= 0.0 {
            return f64::INFINITY;
        }
        self.begin + self.active() / self.speed
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
    /// A colour (straight red, green, blue, alpha).
    Color([f64; 4]),
    Rect(crate::fw::geometry::Rect),
}

/// A rect's numbers (x, y, width, height) and back.
fn rect_numbers(r: crate::fw::geometry::Rect) -> [f64; 4] {
    [r.origin.x, r.origin.y, r.size.width, r.size.height]
}

fn numbers_rect([x, y, w, h]: [f64; 4]) -> crate::fw::geometry::Rect {
    crate::fw::geometry::Rect::new(x, y, w, h)
}

impl Value {
    fn lerp(a: Value, b: Value, f: f64) -> Option<Value> {
        let mix = |x: f64, y: f64| x + (y - x) * f;
        Some(match (a, b) {
            (Value::Number(x), Value::Number(y)) => Value::Number(mix(x, y)),
            (Value::Point(p), Value::Point(q)) => Value::Point(Point { x: mix(p.x, q.x), y: mix(p.y, q.y) }),
            (Value::Transform(m), Value::Transform(n)) => {
                // Taken apart and put back together, so turns turn (in 3D,
                // the short way round); one that can't be taken apart jumps half way.
                match (decompose(m), decompose(n)) {
                    (Some(dm), Some(dn)) => Value::Transform(recompose(&dm.lerp(&dn, f))),
                    _ => Value::Transform(if f < 0.5 { m } else { n }),
                }
            }
            (Value::Color(c), Value::Color(d)) => Value::Color(std::array::from_fn(|i| mix(c[i], d[i]))),
            (Value::Rect(r), Value::Rect(q)) => {
                let (r, q) = (rect_numbers(r), rect_numbers(q));
                Value::Rect(numbers_rect(std::array::from_fn(|i| mix(r[i], q[i]))))
            }
            _ => return None,
        })
    }

    fn add(a: Value, b: Value) -> Option<Value> {
        Some(match (a, b) {
            (Value::Number(x), Value::Number(y)) => Value::Number(x + y),
            (Value::Point(p), Value::Point(q)) => Value::Point(Point { x: p.x + q.x, y: p.y + q.y }),
            (Value::Transform(m), Value::Transform(n)) => Value::Transform(crate::fw::geometry::concat_3d(n, m)),
            (Value::Color(c), Value::Color(d)) => Value::Color(std::array::from_fn(|i| c[i] + d[i])),
            (Value::Rect(r), Value::Rect(q)) => {
                let (r, q) = (rect_numbers(r), rect_numbers(q));
                Value::Rect(numbers_rect(std::array::from_fn(|i| r[i] + q[i])))
            }
            _ => return None,
        })
    }

    fn subtract(a: Value, b: Value) -> Option<Value> {
        Some(match (a, b) {
            (Value::Number(x), Value::Number(y)) => Value::Number(x - y),
            (Value::Point(p), Value::Point(q)) => Value::Point(Point { x: p.x - q.x, y: p.y - q.y }),
            (Value::Color(c), Value::Color(d)) => Value::Color(std::array::from_fn(|i| c[i] - d[i])),
            (Value::Rect(r), Value::Rect(q)) => {
                let (r, q) = (rect_numbers(r), rect_numbers(q));
                Value::Rect(numbers_rect(std::array::from_fn(|i| r[i] - q[i])))
            }
            _ => return None,
        })
    }

    /// `n` of a value added up (`n` whole, for cumulative laps).
    fn times(v: Value, n: f64) -> Value {
        match v {
            Value::Number(x) => Value::Number(x * n),
            Value::Point(p) => Value::Point(Point { x: p.x * n, y: p.y * n }),
            Value::Transform(m) => Value::Transform(decompose(m).map_or(m, |d| recompose(&d.times(n)))),
            Value::Color(c) => Value::Color(c.map(|v| v * n)),
            Value::Rect(r) => Value::Rect(numbers_rect(rect_numbers(r).map(|v| v * n))),
        }
    }

    /// How far apart two values are (for paced key frames).
    fn distance(a: Value, b: Value) -> f64 {
        match (a, b) {
            (Value::Number(x), Value::Number(y)) => (y - x).abs(),
            (Value::Point(p), Value::Point(q)) => (q.x - p.x).hypot(q.y - p.y),
            (Value::Color(c), Value::Color(d)) => (0..4).map(|i| (d[i] - c[i]).powi(2)).sum::<f64>().sqrt(),
            (Value::Rect(r), Value::Rect(q)) => (0..4).map(|i| (rect_numbers(q)[i] - rect_numbers(r)[i]).powi(2)).sum::<f64>().sqrt(),
            _ => 1.0,
        }
    }
}

/// A value object as an animation reads it: a number, a colour, or an
/// NSValue holding a point, size (as a point) or transform.
pub fn read_value(object: Id) -> Option<Value> {
    if object == 0 {
        return None;
    }
    if collections::is_number(object) {
        return Some(Value::Number(collections::number_as_f64(object)));
    }
    if layers::is_kind(object, "UIColor") {
        return crate::fw::graphics::color_rgba(object).map(Value::Color);
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
    } else if text.starts_with("{CGRect") {
        unboxed::<crate::fw::geometry::Rect>(object, "CGRectValue").map(Value::Rect)
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

/// What a layer shows of the parts animations can change.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shown {
    pub opacity: f64,
    pub position: Point,
    pub transform: Transform3D,
    /// Its background colour (None: none).
    pub background: Option<[f64; 4]>,
    /// Its bounds, when they can animate (a layer of its own's; None: as they are).
    pub bounds: Option<crate::fw::geometry::Rect>,
}

impl Shown {
    /// What shows with no animation: the layer's own values.
    pub fn new(opacity: f64, position: Point, transform: Transform3D, background: Option<[f64; 4]>) -> Shown {
        Shown { opacity, position, transform, background, bounds: None }
    }

    fn get(&self, path: &str) -> Option<Value> {
        if let Some(part) = path.strip_prefix("transform.") {
            let d = decompose(self.transform)?;
            let [ax, ay, az] = d.angles();
            let [sx, sy, sz] = d.scale;
            let [tx, ty, tz] = d.translate;
            return Some(match part {
                "scale" => Value::Number((sx + sy + sz) / 3.0),
                "scale.x" => Value::Number(sx),
                "scale.y" => Value::Number(sy),
                "scale.z" => Value::Number(sz),
                "rotation" | "rotation.z" => Value::Number(az),
                "rotation.x" => Value::Number(ax),
                "rotation.y" => Value::Number(ay),
                "translation" => Value::Point(Point { x: tx, y: ty }),
                "translation.x" => Value::Number(tx),
                "translation.y" => Value::Number(ty),
                "translation.z" => Value::Number(tz),
                _ => return None,
            });
        }
        Some(match path {
            "opacity" => Value::Number(self.opacity),
            "position" => Value::Point(self.position),
            "position.x" => Value::Number(self.position.x),
            "position.y" => Value::Number(self.position.y),
            "transform" => Value::Transform(self.transform),
            "backgroundColor" => Value::Color(self.background.unwrap_or([0.0; 4])),
            "bounds" => Value::Rect(self.bounds?),
            _ => return None,
        })
    }

    fn set(&mut self, path: &str, value: Value) {
        if let (Some(part), Value::Number(v)) | (Some(part), Value::Point(Point { x: v, .. })) = (path.strip_prefix("transform."), value) {
            let Some(mut d) = decompose(self.transform) else { return };
            let mut angles = d.angles();
            match (part, value) {
                ("scale", _) => d.scale = [v; 3],
                ("scale.x", _) => d.scale[0] = v,
                ("scale.y", _) => d.scale[1] = v,
                ("scale.z", _) => d.scale[2] = v,
                ("rotation" | "rotation.z", _) => angles[2] = v,
                ("rotation.x", _) => angles[0] = v,
                ("rotation.y", _) => angles[1] = v,
                ("translation", Value::Point(p)) => (d.translate[0], d.translate[1]) = (p.x, p.y),
                ("translation.x", _) => d.translate[0] = v,
                ("translation.y", _) => d.translate[1] = v,
                ("translation.z", _) => d.translate[2] = v,
                _ => return,
            }
            if part.starts_with("rotation") {
                d.set_angles(angles);
            }
            self.transform = recompose(&d);
            return;
        }
        match (path, value) {
            ("opacity", Value::Number(v)) => self.opacity = v.clamp(0.0, 1.0),
            ("position", Value::Point(p)) => self.position = p,
            ("position.x", Value::Number(v)) => self.position.x = v,
            ("position.y", Value::Number(v)) => self.position.y = v,
            ("transform", Value::Transform(m)) => self.transform = m,
            ("backgroundColor", Value::Color(c)) => self.background = Some(c.map(|v| v.clamp(0.0, 1.0))),
            ("bounds", Value::Rect(r)) if self.bounds.is_some() => self.bounds = Some(r),
            _ => {}
        }
    }
}

/// A running animation as plain data, read from our copy of it when it is
/// added and again when its turn commits (its begin moves then): what Core
/// Animation's commit hands the render server (encode_add_animation). No
/// objects: it can be shown on any thread, at any moment (`present`).
#[derive(Clone, Debug, PartialEq)]
pub struct Program {
    pub timing: Timing,
    /// removedOnCompletion: the screen drops it at its end (`present`).
    pub removed: bool,
    /// Its timing function (none: linear).
    pub curve: Option<Curve>,
    pub kind: ProgramKind,
    /// When the commit that began it ran (its parent's time), for one the
    /// game gave no beginTime: the screen shows nothing of it from before
    /// then (`apply`). None: the game's own beginTime.
    pub committed: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ProgramKind {
    Property(Property),
    /// A CAAnimationGroup's animations, on its own time.
    Group(Vec<Program>),
}

/// A property animation's key path and values (basic, key frames, a path).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Property {
    pub key: String,
    pub additive: bool,
    pub cumulative: bool,
    pub from: Option<Value>,
    pub to: Option<Value>,
    pub by: Option<Value>,
    /// Key frames' values (None for one that can't be read, which makes the whole animation show nothing).
    pub values: Option<Vec<Option<Value>>>,
    pub key_times: Vec<f64>,
    /// Key frames' timing functions, one per segment.
    pub curves: Option<Vec<Option<Curve>>>,
    /// calculationMode ("linear", "discrete", "paced", ...).
    pub mode: String,
    /// A key frame animation's CGPath, as figures of points.
    pub path: Option<Vec<Vec<Point>>>,
    /// rotationMode ("auto", "autoReverse").
    pub rotation: Option<String>,
}

/// Reads an animation (our copy) into a program; `fallback`: what a
/// duration of 0 stands for (a group's own duration for its animations).
pub fn program_of(animation: Id, fallback: f64) -> Program {
    let timing = timing_of(animation, fallback);
    let removed = number(animation, "removedOnCompletion", 1.0) != 0.0;
    let curve = curve_of(object(animation, "timingFunction"));
    if is_group(animation) {
        let children = object(animation, "animations");
        let children = if children == 0 { Vec::new() } else { collections::array_items(children) };
        let kind = ProgramKind::Group(children.into_iter().map(|child| program_of(child, timing.duration)).collect());
        return Program { timing, removed, curve, kind, committed: None };
    }
    let items = |key: &'static str| match object(animation, key) {
        0 => None,
        list => Some(collections::array_items(list)),
    };
    let text = |key: &'static str| match object(animation, key) {
        0 => None,
        value => Some(string::get(value)),
    };
    let property = Property {
        key: string::get(object(animation, "keyPath")),
        additive: number(animation, "additive", 0.0) != 0.0,
        cumulative: number(animation, "cumulative", 0.0) != 0.0,
        from: read_value(object(animation, "fromValue")),
        to: read_value(object(animation, "toValue")),
        by: read_value(object(animation, "byValue")),
        values: items("values").map(|values| values.into_iter().map(read_value).collect()),
        key_times: items("keyTimes").map(|times| times.into_iter().map(layers::number_value).collect()).unwrap_or_default(),
        curves: items("timingFunctions").map(|curves| curves.into_iter().map(curve_of).collect()),
        mode: string::get(anim_calculation_mode(animation, 0)),
        path: match object(animation, "path") {
            0 => None,
            path => Some(crate::fw::graphics::path_points(path)),
        },
        rotation: text("rotationMode"),
    };
    Program { timing, removed, curve, kind: ProgramKind::Property(property), committed: None }
}

impl Program {
    fn eased(&self, progress: f64) -> f64 {
        self.curve.map_or(progress, |c| c.at(progress))
    }

    /// Applies it, shown at its parent's time `t`, over `shown`. `screen`:
    /// as the screen shows it, where one removed on completion is gone from
    /// its very end (the render server drops it then; the app's own copy
    /// goes at its next tick, holding a forwards fill until then).
    pub fn apply(&self, t: f64, shown: &mut Shown, screen: bool) {
        // The render server draws nothing from before the commit that began
        // it: a frame for a moment before (the phone's present time jitters
        // under the animation clock's lead) shows it as at its commit.
        let t = match self.committed {
            Some(commit) if screen => t.max(commit),
            _ => t,
        };
        let state = self.timing.state(t);
        if screen && state.ended && self.removed {
            return;
        }
        let Some(progress) = state.progress else { return };
        match &self.kind {
            // Its animations run on its own time (clipped to its duration).
            // removedOnCompletion takes an animation off its layer; a group's
            // aren't on one, so theirs counts for nothing on the screen too
            // (one ending inside the group shows its end only by its fill).
            ProgramKind::Group(children) => {
                let eased = self.eased(progress);
                for child in children {
                    child.apply(eased * self.timing.duration, shown, false);
                }
            }
            ProgramKind::Property(property) => {
                let eased = self.eased(progress);
                if let Some(value) = shown.get(&property.key).and_then(|model| property.value_over(eased, self.timing.lap(t), model)) {
                    shown.set(&property.key, value);
                    if let Some(turn) = property.turn(eased) {
                        shown.set("transform.rotation", Value::Number(turn));
                    }
                }
            }
        }
    }
}

impl Property {
    /// Its value `eased` of the way through its simple duration (its timing
    /// function applied) with `lap` cycles behind it, over the value shown
    /// without it (`model`). A path (for a point) wins over key frames;
    /// cumulative laps each start from the end of the ones before.
    fn value_over(&self, eased: f64, lap: f64, model: Value) -> Option<Value> {
        let (value, last) = match (&self.path, &self.values) {
            (Some(path), _) if matches!(model, Value::Point(_)) => (Value::Point(path_sample(path, eased)?.0), Value::Point(path_sample(path, 1.0)?.0)),
            (_, Some(values)) => self.keyframe_value(values, eased)?,
            _ => {
                let (start, end) = match (self.from, self.to, self.by) {
                    (Some(f), Some(t), _) => (f, t),
                    (Some(f), None, Some(b)) => (f, Value::add(f, b)?),
                    (None, Some(t), Some(b)) => (Value::subtract(t, b)?, t),
                    (Some(f), None, None) => (f, model),
                    (None, Some(t), None) => (model, t),
                    (None, None, Some(b)) => (model, Value::add(model, b)?),
                    (None, None, None) => (model, model),
                };
                (Value::lerp(start, end, eased)?, end)
            }
        };
        let value = if lap > 0.0 && self.cumulative { Value::add(Value::times(last, lap), value)? } else { value };
        if self.additive { Value::add(model, value) } else { Some(value) }
    }

    /// A path animation's turn along its path (rotationMode auto or
    /// autoReverse), `eased` of the way along.
    fn turn(&self, eased: f64) -> Option<f64> {
        let heading = path_sample(self.path.as_ref()?, eased)?.1;
        match self.rotation.as_deref()? {
            "auto" => Some(heading),
            "autoReverse" => Some(heading + std::f64::consts::PI),
            _ => None,
        }
    }

    /// The key frames' value at `progress`, and their last value.
    fn keyframe_value(&self, values: &[Option<Value>], progress: f64) -> Option<(Value, Value)> {
        let values: Vec<Value> = values.iter().copied().collect::<Option<_>>()?;
        let n = values.len();
        match n {
            0 => return None,
            1 => return Some((values[0], values[0])),
            _ => {}
        }
        let last = values[n - 1];
        let given = &self.key_times;
        if self.mode == "discrete" {
            let times: Vec<f64> = if given.len() >= n { given.clone() } else { (0..n).map(|i| i as f64 / n as f64).collect() };
            let at = times.iter().take(n).rposition(|&t| t <= progress).unwrap_or(0);
            return Some((values[at], last));
        }
        // Paced modes work out their own times and ignore keyTimes and timingFunctions.
        let paced = self.mode == "paced" || self.mode == "cubicPaced";
        let times: Vec<f64> = if paced {
            let steps: Vec<f64> = values.windows(2).map(|w| Value::distance(w[0], w[1])).collect();
            let total: f64 = steps.iter().sum();
            let mut sum = 0.0;
            std::iter::once(0.0).chain(steps.iter().map(|s| {
                sum += s;
                if total > 0.0 { sum / total } else { 0.0 }
            })).collect()
        } else if given.len() == n {
            given.clone()
        } else {
            (0..n).map(|i| i as f64 / (n - 1) as f64).collect()
        };
        let segment = (0..n - 1).rfind(|&i| times[i] <= progress).unwrap_or(0);
        let span = times[segment + 1] - times[segment];
        let mut f = if span > 0.0 { ((progress - times[segment]) / span).clamp(0.0, 1.0) } else { 1.0 };
        if let Some(curve) = self.curves.as_ref().filter(|_| !paced).and_then(|curves| curves.get(segment).copied().flatten()) {
            f = curve.at(f);
        }
        Some((Value::lerp(values[segment], values[segment + 1], f)?, last))
    }
}

/// The animation's value at `progress` of its simple duration with `lap`
/// cycles behind it, for its key path, over what is shown without it
/// (`under`).
#[cfg(test)]
fn value_at(animation: Id, progress: f64, lap: f64, under: &Shown) -> Option<(String, Value)> {
    let program = program_of(animation, DEFAULT_DURATION);
    let ProgramKind::Property(property) = &program.kind else { return None };
    let model = under.get(&property.key)?;
    Some((property.key.clone(), property.value_over(program.eased(progress), lap, model)?))
}

/// The point `f` (0 to 1) of the way along a path's figures at even speed,
/// and the path's heading there (radians). Lines between figures take no time.
fn path_sample(figures: &[Vec<Point>], f: f64) -> Option<(Point, f64)> {
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

fn is_group(animation: Id) -> bool {
    let class = runtime::class_named("CAAnimationGroup");
    let rt = lock();
    rt.is_subclass(rt.class_of(animation), class)
}

/// Whether the phone can show it by itself (IOS_PHONE_ANIM=1): it changes
/// only where a layer is, how it is turned and how see-through (what the
/// phone moves pictures by); its size, colour or contents need new pictures.
pub fn runs_on_phone(program: &Program) -> bool {
    match &program.kind {
        ProgramKind::Group(children) => children.iter().all(runs_on_phone),
        ProgramKind::Property(property) => {
            matches!(property.key.as_str(), "opacity" | "position" | "position.x" | "position.y" | "transform") || property.key.starts_with("transform.")
        }
    }
}

/// `model` with `programs` (in beginTime order) shown over it at the
/// layer's own time `local`; `screen`: as the screen shows them (see
/// `Program::apply`).
pub fn present(programs: &[std::sync::Arc<Program>], model: Shown, local: f64, screen: bool) -> Shown {
    let mut shown = model;
    for program in programs {
        program.apply(local, &mut shown, screen);
    }
    shown
}

// ----- animations running on layers -----

struct Running {
    layer: Id,
    key: Option<String>,
    /// Our copy of what the game added.
    animation: Id,
    started: bool,
    stopped: bool,
    /// Begun "now" when added (media time, on this thread): moved to when
    /// that thread's main-loop turn is done (`commit_at`).
    begun_on_add: Option<(f64, std::thread::ThreadId)>,
    /// Whether its delegate answers animationDidStart: and
    /// animationDidStop:finished:, looked up when it was added (as Apple's
    /// lookup_delegate_methods).
    tells: (bool, bool),
    /// It as plain data, read when it was added and again when it begun.
    program: std::sync::Arc<Program>,
}

static RUNNING: Mutex<Vec<Running>> = Mutex::new(Vec::new());

fn running() -> std::sync::MutexGuard<'static, Vec<Running>> {
    RUNNING.lock().unwrap_or_else(|e| e.into_inner())
}

/// What the layer shows now, over `model` (its own values), with its
/// animations: in beginTime order (then the order they were added), later
/// ones over earlier ones, as Core Animation stacks them.
pub fn presented(layer: Id, model: Shown) -> Shown {
    presented_at(layer, model, runloop::animation_time())
}

pub fn presented_at(layer: Id, model: Shown, now: f64) -> Shown {
    let list = programs(layer);
    if list.is_empty() {
        return model;
    }
    present(&list, model, layers::local_time(layer, now), false)
}

/// The programs of the animations running on a layer, in beginTime order
/// (then the order they were added): what the screen shows them from.
pub fn programs(layer: Id) -> Vec<std::sync::Arc<Program>> {
    let mut list: Vec<std::sync::Arc<Program>> = running().iter().filter(|r| r.layer == layer).map(|r| r.program.clone()).collect();
    list.sort_by(|a, b| a.timing.begin.total_cmp(&b.timing.begin));
    list
}

/// What the layer shows of a key `Shown` doesn't carry ("bounds",
/// "backgroundColor": a UIView block's size and colour changes) over its
/// own `model` value, with its plain animations of that key path.
pub fn presented_value_at(layer: Id, key: &str, model: Value, now: f64) -> Value {
    let list: Vec<std::sync::Arc<Program>> = running().iter().filter(|r| r.layer == layer).map(|r| r.program.clone()).collect();
    let t = layers::local_time(layer, now);
    let mut shown = model;
    for program in list {
        let ProgramKind::Property(property) = &program.kind else { continue };
        if property.key != key {
            continue;
        }
        if let Some(progress) = program.timing.state(t).progress {
            if let Some(value) = property.value_over(program.eased(progress), program.timing.lap(t), shown) {
                shown = value;
            }
        }
    }
    shown
}

/// The layer's running animations of "contents" (not in groups).
fn contents_animations(layer: Id) -> Vec<Id> {
    let mut list: Vec<Id> = running().iter().filter(|r| r.layer == layer).map(|r| r.animation).collect();
    if !list.is_empty() {
        list.retain(|&a| !is_group(a) && string::get(object(a, "keyPath")) == "contents");
    }
    list
}

/// Whether the layer has a running "contents" animation (its pictures change
/// from frame to frame).
pub fn animates_contents(layer: Id) -> bool {
    !contents_animations(layer).is_empty()
}

/// The picture a layer's running "contents" animations show at `now` (None
/// while none runs), later ones over earlier ones: key frames step from
/// picture to picture at their key times (UIImageView's are discrete); a
/// basic one shows its toValue from halfway.
pub fn presented_contents_at(layer: Id, now: f64) -> Option<Id> {
    let mut list = contents_animations(layer);
    if list.is_empty() {
        return None;
    }
    list.sort_by(|&a, &b| number(a, "beginTime", 0.0).total_cmp(&number(b, "beginTime", 0.0)));
    let t = layers::local_time(layer, now);
    let mut shown = None;
    for animation in list {
        let timing = timing_of(animation, DEFAULT_DURATION);
        let Some(progress) = timing.state(t).progress else { continue };
        let progress = curve_of(object(animation, "timingFunction")).map_or(progress, |c| c.at(progress));
        let values = object(animation, "values");
        let picture = if values != 0 {
            let pictures = collections::array_items(values);
            let n = pictures.len();
            let given = object(animation, "keyTimes");
            let given: Vec<f64> = if given == 0 { Vec::new() } else { collections::array_items(given).into_iter().map(layers::number_value).collect() };
            let times: Vec<f64> = if given.len() >= n { given } else { (0..n).map(|i| i as f64 / n as f64).collect() };
            times.iter().take(n).rposition(|&at| at <= progress).and_then(|at| pictures.get(at).copied()).unwrap_or(0)
        } else {
            object(animation, if progress < 0.5 { "fromValue" } else { "toValue" })
        };
        if picture != 0 {
            shown = Some(picture);
        }
    }
    shown
}

/// The layers with running animations (for a log line).
pub fn running_layers() -> Vec<Id> {
    running().iter().filter(|r| !r.stopped).map(|r| r.layer).collect()
}

/// The key paths of the animations running on a layer (for a log line).
pub fn keys(layer: Id) -> Vec<String> {
    let list: Vec<Id> = running().iter().filter(|r| r.layer == layer).map(|r| r.animation).collect();
    list.into_iter()
        .map(|a| match object(a, "keyPath") {
            _ if is_group(a) => "group".to_string(),
            0 => "-".to_string(),
            path => string::get(path),
        })
        .collect()
}

/// Whether a layer has animations on it now.
pub fn animating(layer: Id) -> bool {
    running().iter().any(|r| r.layer == layer)
}

/// A CATransition running on a layer, as the screen draws it (fw/pictures.rs).
#[derive(Clone, Debug, PartialEq)]
pub struct TransitionLook {
    /// Its type ("fade", "moveIn", "push", "reveal", or UIKit's "oglFlip",
    /// "pageCurl", "pageUnCurl") and subtype ("fromLeft" ...; "" for none).
    pub kind: String,
    pub subtype: String,
    /// Its timing function (linear without one, as Core Animation's).
    pub curve: Curve,
    /// startProgress and endProgress.
    pub progress: (f64, f64),
    /// When it begins (media time) and how long it runs (seconds).
    pub begin: f64,
    pub duration: f64,
}

/// The CATransitions running now, with their layers and our copies.
pub fn transitions() -> Vec<(Id, Id, TransitionLook)> {
    let list: Vec<(Id, Id)> = running().iter().filter(|r| !r.stopped).map(|r| (r.layer, r.animation)).collect();
    list.into_iter()
        .filter(|&(_, animation)| layers::is_kind(animation, "CATransition"))
        .map(|(layer, animation)| {
            let timing = timing_of(animation, DEFAULT_DURATION);
            let begin = layers::global_time(layer, timing.begin).unwrap_or(timing.begin);
            let end = layers::global_time(layer, timing.begin + timing.duration).unwrap_or(begin + timing.duration);
            let text = |key| match object(animation, key) {
                0 => String::new(),
                value => string::get(value),
            };
            let kind = match text("type") {
                kind if kind.is_empty() => "fade".to_string(),
                kind => kind,
            };
            let look = TransitionLook {
                kind,
                subtype: text("subtype"),
                curve: curve_of(object(animation, "timingFunction")).unwrap_or(Curve::LINEAR),
                progress: (number(animation, "startProgress", 0.0), number(animation, "endProgress", 1.0)),
                begin,
                duration: (end - begin).max(0.0),
            };
            (layer, animation, look)
        })
        .collect()
}

/// Whether the animation's delegate answers the start and stop calls.
fn delegate_tells(animation: Id) -> (bool, bool) {
    let delegate = object(animation, "delegate");
    (delegate != 0 && runtime::responds(delegate, "animationDidStart:"), delegate != 0 && runtime::responds(delegate, "animationDidStop:finished:"))
}

fn tell_started(animation: Id, tells: (bool, bool)) {
    let delegate = object(animation, "delegate");
    if delegate != 0 && tells.0 {
        runtime::send(delegate, "animationDidStart:", &[animation]);
    }
}

fn tell_stopped(animation: Id, finished: bool, tells: (bool, bool)) {
    let delegate = object(animation, "delegate");
    if delegate != 0 && tells.1 {
        runtime::send(delegate, "animationDidStop:finished:", &[animation, finished as usize]);
    }
}

/// Sends the delegate calls that are due and drops the animations that
/// ended; then waits for the next start or end. Runs on the main thread.
pub fn tick() {
    tick_at(None, runloop::animation_time());
}

/// `only`: tick just this layer's animations (a test's fake clock must not
/// end other layers' ones).
pub(crate) fn tick_at(only: Option<Id>, now: f64) {
    let mut started = Vec::new();
    let mut stopped = Vec::new();
    let mut next = f64::INFINITY;
    let mut changed = Vec::new();
    // IOS_DRAW_LOG=1: each stopped animation's layer, key and scheduled end (media time).
    let mut seam_ends = Vec::new();
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
                changed.push(r.layer);
                started.push((r.animation, r.tells));
            }
            if state.ended && !r.stopped {
                r.stopped = true;
                changed.push(r.layer);
                let removed = number(r.animation, "removedOnCompletion", 1.0) != 0.0;
                if crate::fw::graphics::draw_log() {
                    let end = layers::global_time(r.layer, timing.end()).unwrap_or(f64::NAN);
                    seam_ends.push((r.animation, r.layer, r.key.clone().unwrap_or_else(|| "-".into()), end));
                }
                stopped.push((r.animation, removed, r.tells));
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
    for layer in changed {
        crate::fw::changes::changed(layer);
    }
    // As Apple's run_animation_callbacks: the starts due, then the stops.
    for (animation, tells) in started {
        tell_started(animation, tells);
    }
    for (animation, removed, tells) in stopped {
        // The seam gauge (IOS_DRAW_LOG=1): when it was due to end, and when its delegate and
        // completion run (the game's own timer, as on Apple).
        if let Some((_, layer, key, end)) = seam_ends.iter().find(|(a, ..)| *a == animation) {
            crate::log!("seam: stop {layer:x} {key} end {end:.6} ran {:.6}", runloop::media_time());
        }
        tell_stopped(animation, true, tells);
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

/// The layer let go of its copy: the copy lets go of its delegate, as its
/// dealloc would on Apple (objects here are never freed).
fn let_go(animation: Id) {
    anim_set_delegate(animation, 0, 0);
}

/// When the clock next wakes (infinity: not set).
static WAKE: Mutex<f64> = Mutex::new(f64::INFINITY);

fn wake_at(at: f64) {
    arm(&WAKE, at, runloop::post_after);
}

/// Sets the clock (`wake`) to wake at animation time `at` unless it wakes
/// sooner, its delay in real seconds given to `post`.
fn arm(wake: &Mutex<f64>, at: f64, post: impl FnOnce(f64, Box<dyn FnOnce() + Send>)) {
    let mut set = wake.lock().unwrap_or_else(|e| e.into_inner());
    if at < *set - 1e-6 {
        *set = at;
        drop(set);
        post((at - runloop::animation_time()).max(0.0), Box::new(|| {
            *WAKE.lock().unwrap_or_else(|e| e.into_inner()) = f64::INFINITY;
            tick();
        }));
    }
}

/// The animation clock's lead changed (runloop::set_animation_lead, posted
/// to the main thread): the clock set to wake is set again for the same
/// animation time, now that many real seconds away.
pub(crate) fn lead_changed() {
    rearm(&WAKE, runloop::post_after);
}

fn rearm(wake: &Mutex<f64>, post: impl FnOnce(f64, Box<dyn FnOnce() + Send>)) {
    // (The old timer still runs at its time: it only ticks once more.)
    let at = std::mem::replace(&mut *wake.lock().unwrap_or_else(|e| e.into_inner()), f64::INFINITY);
    if at.is_finite() {
        arm(wake, at, post);
    }
}

/// Something changed a layer's time (speed, offset, begin): look again on
/// the next turn of the main run loop.
pub fn reschedule() {
    if !running().is_empty() {
        wake_at(runloop::animation_time());
    }
}

/// -addAnimation:forKey:. As on Apple: the animation is copied, one with
/// the same key is removed first (a CATransition's key is "transition"), a
/// duration of 0 becomes the transaction's animationDuration (still 0: it
/// starts and stops at once and isn't added), a property animation without
/// a keyPath takes the key, one without a timing function takes the
/// transaction's, and it starts now unless it has a beginTime; its start is
/// told on the next turn of the main run loop. A CATransition takes the
/// layer from what it showed to what it shows next (fw/pictures.rs draws it).
pub fn add(layer: Id, animation: Id, key: Id) -> Id {
    if animation == 0 {
        return 0;
    }
    let transition = layers::is_kind(animation, "CATransition");
    let key = if transition { Some("transition".to_string()) } else if key == 0 { None } else { Some(string::get(key)) };
    if key.is_some() {
        remove_where(|r| r.layer == layer && r.key == key);
    }
    let copy = runtime::send(animation, "copy", &[]);
    if number(copy, "duration", 0.0) <= 0.0 {
        set_number(copy, "duration", crate::fw::smallclasses::animation_duration());
    }
    if number(copy, "duration", 0.0) <= 0.0 {
        let tells = delegate_tells(copy);
        tell_started(copy, tells);
        tell_stopped(copy, true, tells);
        let_go(copy);
        return copy;
    }
    if let Some(key) = &key {
        if object(copy, "keyPath") == 0 && layers::is_kind(copy, "CAPropertyAnimation") {
            hold(copy, "keyPath", string::make(key));
        }
    }
    if object(copy, "delegate") == 0 {
        let delegate = crate::fw::smallclasses::animation_delegate();
        if delegate != 0 {
            anim_set_delegate(copy, 0, delegate);
        }
    }
    let tells = delegate_tells(copy);
    if object(copy, "timingFunction") == 0 {
        let curve = crate::fw::smallclasses::animation_timing();
        if curve != 0 {
            hold(copy, "timingFunction", curve);
        }
    }
    let now = runloop::animation_time();
    let begun_on_add = (number(copy, "beginTime", 0.0) == 0.0).then_some((now, std::thread::current().id()));
    if begun_on_add.is_some() {
        set_number(copy, "beginTime", layers::local_time(layer, now));
    }
    crate::fw::smallclasses::animation_added(copy);
    let committed = begun_on_add.map(|_| layers::local_time(layer, now));
    let program = std::sync::Arc::new(Program { committed, ..program_of(copy, DEFAULT_DURATION) });
    running().push(Running { layer, key, animation: copy, started: false, stopped: false, begun_on_add, tells, program });
    wake_at(now);
    crate::fw::changes::changed(layer);
    crate::fw::pictures::draw_soon();
    copy
}

/// The main loop's turn is done (Apple's transaction commit): animations
/// added during it without a begin time begin now, as on Apple, so the work
/// the game did after adding them doesn't use up their first part before a
/// frame of them is drawn (SpellCraft's 0.2 s spell panel slide). Layers
/// made in it are committed (their later changes run implicit actions).
pub fn commit() {
    layers::commit_new();
    commit_at(None, runloop::animation_time());
}

/// Whether this thread added animations that haven't begun yet (`commit`).
pub fn uncommitted() -> bool {
    let me = std::thread::current().id();
    running().iter().any(|r| r.begun_on_add.is_some_and(|(_, thread)| thread == me))
}

pub(crate) fn commit_at(only: Option<Id>, now: f64) {
    let mut late = 0.0f64;
    let mut shifted = Vec::new();
    let mut begun = Vec::new();
    let me = std::thread::current().id();
    for r in running().iter_mut().filter(|r| only.is_none_or(|layer| layer == r.layer)) {
        let Some((added, _)) = r.begun_on_add.take_if(|(_, thread)| *thread == me) else { continue };
        let shift = layers::local_time(r.layer, now) - layers::local_time(r.layer, added);
        if shift > 0.0 {
            set_number(r.animation, "beginTime", number(r.animation, "beginTime", 0.0) + shift);
            late = late.max(now - added);
            shifted.push(r.layer);
        }
        // Finished as it begins: what the screen shows it from.
        r.program = std::sync::Arc::new(Program { committed: Some(layers::local_time(r.layer, now)), ..program_of(r.animation, DEFAULT_DURATION) });
        if crate::fw::graphics::draw_log() {
            let begin = layers::global_time(r.layer, number(r.animation, "beginTime", 0.0)).unwrap_or(f64::NAN);
            begun.push((r.layer, r.key.clone().unwrap_or_else(|| "-".into()), begin));
        }
    }
    // The seam gauge (IOS_DRAW_LOG=1): when each begins (media time), committed now.
    for (layer, key, begin) in begun {
        crate::log!("seam: begin {layer:x} {key} at {begin:.6} committed {now:.6}");
        if key == "transform" {
            crate::fw::pictures::seam_layer_began(layer, now);
        }
    }
    for layer in shifted {
        crate::fw::changes::changed(layer);
    }
    if late > 0.03 {
        static LOGGED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        if LOGGED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 20 {
            crate::log!("animations begin at the end of their turn, {:.0} ms after they were added", late * 1000.0);
        }
    }
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
    /// When the block was made (media time): its animations all begin then
    /// (plus the delay), moved together to the end of the turn.
    pub asked: f64,
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
    set_number(animation, "beginTime", layers::local_time(layer, timing.asked) + timing.delay);
    if timing.delay > 0.0 {
        hold(animation, "fillMode", string::make("backwards"));
    }
    set_number(animation, "repeatCount", timing.repeat_count);
    set_number(animation, "autoreverses", timing.autoreverses as u8 as f64);
    hold(animation, "timingFunction", make_curve(runtime::class_named("CAMediaTimingFunction"), timing.curve));
    let copy = add(layer, animation, key);
    if let Some(r) = running().iter_mut().find(|r| r.animation == copy) {
        r.begun_on_add = Some((timing.asked, std::thread::current().id()));
    }
    copy
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
            gone.push((r.layer, r.animation, r.stopped, r.tells));
            false
        } else {
            true
        }
    });
    for &(layer, _, _, _) in &gone {
        crate::fw::changes::changed(layer);
    }
    for (_, animation, stopped, tells) in gone {
        runloop::post(runloop::Work::Native(Box::new(move || {
            if !stopped {
                tell_stopped(animation, false, tells);
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

/// The layer's own values of what animations change (a view's layer's
/// bounds are its view's, which don't animate here).
pub fn model_of(layer: Id) -> Shown {
    let model = Shown::new(layers::opacity(layer), layers::position(layer), layers::transform(layer), crate::fw::graphics::color_rgba(layers::background(layer)));
    Shown { bounds: (layers::view_of(layer) == 0).then(|| layers::bounds(layer)), ..model }
}

/// A value as an object of the kind valueForKeyPath: hands back.
pub fn boxed_value(value: Value) -> Id {
    match value {
        Value::Number(v) => collections::number_float(v),
        Value::Point(p) => layers::boxed("valueWithCGPoint:", p),
        Value::Transform(m) => layers::boxed("valueWithCATransform3D:", m),
        Value::Color(c) => crate::fw::graphics::make_color(c),
        Value::Rect(r) => layers::boxed("valueWithCGRect:", r),
    }
}

/// What a layer shows now of `key` (opacity, position, transform,
/// backgroundColor), its animations included.
pub fn shown_value(layer: Id, key: &str) -> Option<Value> {
    presented(layer, model_of(layer)).get(key)
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
                layers::send_struct(this, "setTransform:", shown.transform);
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
            ("runActionForKey:object:arguments:", anim_run_action as usize),
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
    use crate::fw::geometry::IDENTITY_3D;

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

    /// As on Apple, an animation added without a begin time starts when its
    /// turn of the main loop is done (the transaction commits), not when it
    /// was added: work the game does after asking for a 0.2 s slide must not
    /// use up the slide before its first frame is drawn. One given its own
    /// begin time keeps it.
    #[test]
    fn animations_start_when_their_turn_is_done() {
        setup();
        runloop::as_main(|| {
            let layer = runtime::send(runtime::send(runtime::class_named("CALayer"), "alloc", &[]), "init", &[]);
            let slide = basic("position.x", collections::number_float(0.0), collections::number_float(100.0));
            anim_set_duration(slide, 0, 0.2);
            add(layer, slide, string::make("slide"));
            let copy = layer_animation_for_key(layer, 0, string::make("slide"));
            let asked = number(copy, "beginTime", 0.0);
            let fixed = basic("opacity", 0, collections::number_float(1.0));
            anim_set_begin(fixed, 0, asked + 1.0);
            add(layer, fixed, string::make("fixed"));
            let fixed = layer_animation_for_key(layer, 0, string::make("fixed"));
            // The game is busy for 150 ms more in the same turn.
            commit_at(Some(layer), layers::global_time(layer, asked + 0.15).unwrap());
            assert!(close(number(copy, "beginTime", 0.0), asked + 0.15), "begins at the commit");
            assert!(close(number(fixed, "beginTime", 0.0), asked + 1.0), "its own begin time stays");
            commit_at(Some(layer), layers::global_time(layer, asked + 0.3).unwrap());
            assert!(close(number(copy, "beginTime", 0.0), asked + 0.15), "only the first commit counts");
            layer_remove_all_animations(layer, 0);
        });
    }

    /// IOS_ANIM_LEAD=1: Core Animation's clock is when the frame made now
    /// shows on the phone (media time + the phone's present lead). An
    /// animation added now begins then, at its commit too, and it ends when
    /// its last frame is drawn: `lead` sooner in real time. No lead: as before.
    #[test]
    fn the_animation_clock_leads_by_the_phones_present_lead() {
        setup();
        // IOS_ANIM_LEAD_EXTRA_MS: a margin on the measured lead (0-20 ms), at most 100 ms in all.
        assert!(close(runloop::lead_with_extra(0.026, 0.0), 0.026) && close(runloop::lead_with_extra(0.026, 8.0), 0.034), "the margin adds");
        assert!(close(runloop::lead_with_extra(0.026, 50.0), 0.046) && close(runloop::lead_with_extra(0.095, 20.0), 0.1), "clamped: {}", runloop::lead_with_extra(0.026, 50.0));
        for lead in [0.0, 0.026] {
            runloop::with_animation_lead(lead, || {
                let layer = runtime::send(runtime::send(runtime::class_named("CALayer"), "alloc", &[]), "init", &[]);
                let fade = basic("opacity", collections::number_float(0.0), collections::number_float(1.0));
                anim_set_duration(fade, 0, 0.1);
                let before = runloop::media_time();
                add(layer, fade, string::make("fade"));
                let added = runloop::media_time();
                let copy = layer_animation_for_key(layer, 0, string::make("fade"));
                let begin = number(copy, "beginTime", 0.0);
                assert!(begin >= before + lead - 1e-9 && begin <= added + lead + 1e-9, "lead {lead}: added, begins {:.4} s after now", begin - before);
                // The game is busy for 20 ms more in the same turn: it begins at the commit.
                std::thread::sleep(std::time::Duration::from_millis(20));
                let (before, _) = (runloop::media_time(), commit());
                let after = runloop::media_time();
                let begin = number(copy, "beginTime", 0.0);
                assert!(begin >= before + lead - 1e-9 && begin <= after + lead + 1e-9, "lead {lead}: committed, begins {:.4} s after now", begin - before);
                // Ticked on the animation clock until it is removed: when, in real time.
                let give_up = before + 0.4;
                while layer_animation_for_key(layer, 0, string::make("fade")) != 0 && runloop::media_time() < give_up {
                    tick_at(Some(layer), runloop::animation_time());
                    std::thread::sleep(std::time::Duration::from_micros(500));
                }
                let ended = runloop::media_time() - (begin + 0.1 - lead);
                assert!((-1e-6..0.01).contains(&ended), "lead {lead}: ends {:.4} s after its last frame is drawn", ended);
                layer_remove_all_animations(layer, 0);
            });
        }
    }

    /// The render server never draws a time before the commit that began an
    /// animation. The seam of p-x8a: the first half (removed) ends at E,
    /// the second is committed with B = E + 3.4 ms; the frame for E + 1.8 ms
    /// shows the second half's start (edge-on), not the model (the face-on
    /// card). One with the game's own beginTime still shows the model then.
    #[test]
    fn the_screen_shows_nothing_from_before_a_commit() {
        let (e, b) = (10.25, 10.25 + 0.0034);
        let turn = |begin: f64, from: f64, to: f64, committed: Option<f64>| std::sync::Arc::new(Program {
            timing: Timing { begin, duration: 0.25, ..Default::default() },
            removed: true,
            curve: None,
            kind: ProgramKind::Property(Property { key: "position.x".into(), from: Some(Value::Number(from)), to: Some(Value::Number(to)), ..Default::default() }),
            committed,
        });
        let (first, second) = (turn(e - 0.25, 0.0, 100.0, Some(e - 0.25)), turn(b, 100.0, 200.0, Some(b)));
        let model = Shown::new(1.0, Point { x: 200.0, y: 0.0 }, IDENTITY_3D, None);
        let at = |programs: &[std::sync::Arc<Program>], t: f64| present(programs, model, t, true).position.x;
        assert_eq!(at(&[first.clone(), second], e + 0.0018), 100.0, "the second half's start");
        let given = turn(b, 100.0, 200.0, None);
        assert_eq!(at(&[first.clone(), given.clone()], e + 0.0018), 200.0, "a game's own beginTime: the model, as before");
        assert!(close(at(&[first, given], b + 0.125), 150.0), "from its begin, as before");
    }

    /// The lead grows by 10 ms after the clock was set to wake for an end:
    /// it is set again, to wake at the end less the new lead (real time).
    #[test]
    fn a_lead_change_moves_the_wake_already_set() {
        let wake = Mutex::new(f64::INFINITY);
        let posted = std::cell::RefCell::new(Vec::new());
        let end = runloop::with_animation_lead(0.026, || {
            let end = runloop::animation_time() + 0.1;
            arm(&wake, end, |delay, _| posted.borrow_mut().push(runloop::media_time() + delay));
            end
        });
        runloop::with_animation_lead(0.036, || rearm(&wake, |delay, _| posted.borrow_mut().push(runloop::media_time() + delay)));
        let posted = posted.into_inner();
        assert_eq!(posted.len(), 2, "set again: {posted:?}");
        assert!((posted[0] - (end - 0.026)).abs() < 0.002, "first for the old lead");
        assert!((posted[1] - (end - 0.036)).abs() < 0.002, "then for the new: {:.4} s off", posted[1] - (end - 0.036));
        assert_eq!(*wake.lock().unwrap(), end, "for the same animation time");
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
        let model = Shown::new(0.2, Point::default(), IDENTITY_3D, None);
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
        let model = Shown::new(1.0, Point::default(), IDENTITY_3D, None);
        assert!(close(value_at(hop, 0.25, 0.0, &model).map(|(_, v)| match v { Value::Number(n) => n, _ => 0.0 }).unwrap(), 1.5));
        anim_set_key_times(hop, 0, numbers(&[0.0, 0.8, 1.0]));
        assert!(close(match value_at(hop, 0.4, 0.0, &model).unwrap().1 { Value::Number(n) => n, _ => 0.0 }, 1.5));
        anim_set_calculation_mode(hop, 0, string::make("discrete"));
        assert_eq!(value_at(hop, 0.9, 0.0, &model).unwrap().1, Value::Number(2.0));
        let mut shown = model;
        shown.set("transform.scale", Value::Number(2.0));
        shown.set("transform.rotation.z", Value::Number(std::f64::consts::FRAC_PI_2));
        let (sx, sy, angle) = scale_and_turn(shown.transform);
        assert!(close(sx, 2.0) && close(sy, 2.0) && close(angle, std::f64::consts::FRAC_PI_2));
        // Half way from no turn to a half turn is a quarter turn, not a squash.
        let turn = crate::fw::geometry::transform_3d_make_rotation(3.0, 0.0, 0.0, 1.0);
        let half = Value::lerp(Value::Transform(IDENTITY_3D), Value::Transform(turn), 0.5).unwrap();
        let Value::Transform(m) = half else { panic!() };
        let (sx, _, angle) = scale_and_turn(m);
        assert!(close(sx, 1.0) && close(angle, 1.5));
    }

    /// A transform's x and y scale and its turn about z.
    fn scale_and_turn(m: Transform3D) -> (f64, f64, f64) {
        let d = decompose(m).unwrap();
        (d.scale[0], d.scale[1], d.angles()[2])
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
        let model = Shown::new(1.0, Point::default(), IDENTITY_3D, None);
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
        assert!(close(scale_and_turn(late.transform).2, std::f64::consts::FRAC_PI_2), "turned down the path");
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

    /// addAnimation:forKey: as Apple's: a duration still 0 after the
    /// transaction's is not added (and takes the key's old animation off);
    /// a nil keyPath takes the key; the transaction's timing function is
    /// given to one without its own; animations apply in beginTime order.
    #[test]
    fn adding_follows_apples_rules_for_duration_key_and_order() {
        setup();
        let transaction = runtime::class_named("CATransaction");
        let layer = runtime::send(runtime::send(runtime::class_named("CALayer"), "alloc", &[]), "init", &[]);
        let model = Shown::new(1.0, Point::default(), IDENTITY_3D, None);
        let old = basic("opacity", collections::number_float(0.0), collections::number_float(1.0));
        anim_set_duration(old, 0, 100.0);
        add(layer, old, string::make("k"));
        runtime::send(transaction, "begin", &[]);
        layers::send_struct(transaction, "setAnimationDuration:", 0.0f64);
        add(layer, basic("opacity", 0, collections::number_float(0.5)), string::make("k"));
        runtime::send(transaction, "commit", &[]);
        assert_eq!(layer_animation_for_key(layer, 0, string::make("k")), 0, "no time: not added, the old one gone");

        let unnamed = anim_class_animation(runtime::class_named("CABasicAnimation"), 0);
        anim_set_from(unnamed, 0, collections::number_float(0.0));
        anim_set_to(unnamed, 0, collections::number_float(1.0));
        anim_set_duration(unnamed, 0, 100.0);
        runtime::send(transaction, "begin", &[]);
        let ease_in = function_with_name(runtime::class_named("CAMediaTimingFunction"), 0, string::make("easeIn"));
        runtime::send(transaction, "setAnimationTimingFunction:", &[ease_in]);
        add(layer, unnamed, string::make("opacity"));
        runtime::send(transaction, "commit", &[]);
        let copy = layer_animation_for_key(layer, 0, string::make("opacity"));
        assert!(curve_of(object(copy, "timingFunction")).is_some(), "the transaction's timing function");
        // (Added, it is read-only, as on Apple: it shows half way along its ease-in.)
        let begin = number(copy, "beginTime", 0.0);
        let half = curve_of(ease_in).unwrap().at(0.5);
        assert!(close(presented_at(layer, model, begin + 50.0).opacity, half), "the key is the key path");
        layer_remove_all_animations(layer, 0);

        // Added later but beginning earlier: applied first, under the other.
        let now = layers::local_time(layer, runloop::media_time());
        let late = basic("opacity", collections::number_float(0.1), collections::number_float(0.1));
        anim_set_duration(late, 0, 100.0);
        anim_set_begin(late, 0, now + 1.0);
        add(layer, late, string::make("late"));
        let early = basic("opacity", collections::number_float(0.9), collections::number_float(0.9));
        anim_set_duration(early, 0, 100.0);
        anim_set_begin(early, 0, now);
        add(layer, early, string::make("early"));
        assert!(close(presented_at(layer, model, layers::global_time(layer, now + 2.0).unwrap()).opacity, 0.1));
        layer_remove_all_animations(layer, 0);
    }

    /// What a layer's animations hand the screen is finished when their turn
    /// commits: a UIView-style slide added "now" begins at the commit, and
    /// the program read then shows it from there at any moment (here a
    /// third of a refresh past a refresh), with no objects to ask. A
    /// fromValue alone goes to the model value given when it is shown.
    #[test]
    fn programs_are_finished_at_the_commit_and_shown_at_any_moment() {
        setup();
        runloop::as_main(|| {
            let layer = runtime::send(runtime::send(runtime::class_named("CALayer"), "alloc", &[]), "init", &[]);
            let linear = function_with_name(runtime::class_named("CAMediaTimingFunction"), 0, string::make("linear"));
            let slide = basic("position.x", collections::number_float(0.0), collections::number_float(100.0));
            anim_set_duration(slide, 0, 0.25);
            anim_set_timing(slide, 0, linear);
            add(layer, slide, string::make("slide"));
            let fade = basic("opacity", collections::number_float(0.0), 0);
            anim_set_duration(fade, 0, 0.25);
            anim_set_timing(fade, 0, linear);
            add(layer, fade, string::make("fade"));
            let asked = number(layer_animation_for_key(layer, 0, string::make("slide")), "beginTime", 0.0);
            commit_at(Some(layer), layers::global_time(layer, asked + 0.15).unwrap());
            let programs = programs(layer);
            let refresh = 1.0 / 120.0;
            let t = asked + 0.15 + 0.37 * refresh;
            let model = Shown::new(0.6, Point { x: 7.0, y: 0.0 }, IDENTITY_3D, None);
            let shown = present(&programs, model, t, true);
            assert!(close(shown.position.x, 100.0 * 0.37 * refresh / 0.25), "from the commit: {}", shown.position.x);
            assert!(close(shown.opacity, 0.6 * 0.37 * refresh / 0.25), "to the model's opacity: {}", shown.opacity);
            let end = present(&programs, model, asked + 0.15 + 0.25 + 0.37 * refresh, true);
            assert_eq!((end.position.x, end.opacity), (7.0, 0.6), "past the end: the model");
            layer_remove_all_animations(layer, 0);
        });
    }

    /// At an animation's end the screen's copy (`present`, dropping ended
    /// ones removed on completion, as the render server does) shows the
    /// model at once, while the app's copy keeps a forwards fill until its
    /// next tick removes it; one not removed holds its end only when filled
    /// forwards; a delay with a backwards fill shows the start value; an
    /// autoreversing repeat comes back.
    #[test]
    fn programs_end_as_the_render_server_ends_them() {
        let refresh = 1.0 / 120.0;
        let fade = |removed: bool, forwards: bool, backwards: bool| {
            let timing = Timing { begin: 10.0, duration: 0.25, forwards, backwards, ..Default::default() };
            let property = Property { key: "opacity".into(), from: Some(Value::Number(0.0)), to: Some(Value::Number(1.0)), ..Default::default() };
            std::sync::Arc::new(Program { timing, removed, curve: None, kind: ProgramKind::Property(property), committed: None })
        };
        let model = Shown::new(0.2, Point::default(), IDENTITY_3D, None);
        let at = |program: std::sync::Arc<Program>, t: f64, screen: bool| present(&[program], model, t, screen).opacity;
        let after = 10.25 + 0.37 * refresh;
        assert_eq!(at(fade(true, true, false), after, true), 0.2, "removed at its end on the screen");
        assert_eq!(at(fade(true, true, false), 10.25, true), 0.2, "from its very end");
        assert_eq!(at(fade(true, true, false), after, false), 1.0, "the app's copy until its tick");
        assert_eq!(at(fade(false, true, false), after, true), 1.0, "kept and filled: its end");
        assert_eq!(at(fade(false, false, false), after, true), 0.2, "kept, not filled: the model");
        assert_eq!(at(fade(true, false, true), 10.0 - 0.37 * refresh, true), 0.0, "filled backwards before it begins");
        assert_eq!(at(fade(true, false, false), 10.0 - 0.37 * refresh, true), 0.2, "not begun: the model");
        assert!(close(at(fade(true, false, false), 10.0 + 0.37 * refresh, true), 0.37 * refresh / 0.25), "just begun");
        let mut back = fade(true, false, false);
        std::sync::Arc::make_mut(&mut back).timing = Timing { autoreverses: true, repeat_count: 1.0, ..back.timing };
        assert!(close(at(back.clone(), 10.25 + 0.37 * refresh, true), 1.0 - 0.37 * refresh / 0.25), "on its way back");
        assert_eq!(at(back, 10.5, true), 0.2, "back and done");
    }

    /// A CAAnimationGroup's animations run on the group's time; one that
    /// ends before the group shows its end only while it fills forwards, on
    /// the screen as in the app's copy: removedOnCompletion takes an
    /// animation off its layer, and a group's animations aren't on it. The
    /// group itself goes from its end. Values worked out by hand: a 1 s
    /// linear group holding a 0.5 s slide of x 0 to 100 (no fill) and a
    /// 0.25 s fade in from 0 (filled forwards, removed on completion).
    #[test]
    fn group_animations_end_inside_their_group_as_apple_shows_them() {
        setup();
        runloop::as_main(|| {
            let layer = runtime::send(runtime::send(runtime::class_named("CALayer"), "alloc", &[]), "init", &[]);
            let linear = function_with_name(runtime::class_named("CAMediaTimingFunction"), 0, string::make("linear"));
            let slide = basic("position.x", collections::number_float(0.0), collections::number_float(100.0));
            anim_set_duration(slide, 0, 0.5);
            anim_set_timing(slide, 0, linear);
            let fade = basic("opacity", collections::number_float(0.0), collections::number_float(1.0));
            anim_set_duration(fade, 0, 0.25);
            anim_set_timing(fade, 0, linear);
            anim_set_fill_mode(fade, 0, string::make("forwards"));
            let group = runtime::send(runtime::class_named("CAAnimationGroup"), "animation", &[]);
            runtime::send(group, "setAnimations:", &[collections::array_from(&[slide, fade])]);
            anim_set_duration(group, 0, 1.0);
            anim_set_timing(group, 0, linear);
            let begin = layers::local_time(layer, runloop::media_time()) + 10.0;
            anim_set_begin(group, 0, begin);
            add(layer, group, string::make("group"));
            let model = Shown::new(0.6, Point { x: 7.0, y: 0.0 }, IDENTITY_3D, None);
            let refresh = 1.0 / 120.0;
            let both = |local: f64| {
                let now = layers::global_time(layer, local).unwrap();
                let (app, screen) = (presented_at(layer, model, now), present(&programs(layer), model, local, true));
                assert_eq!((app.position.x, app.opacity), (screen.position.x, screen.opacity), "app and screen agree at {}", local - begin);
                (app.position.x, app.opacity)
            };
            let (x, opacity) = both(begin + 0.2 + 0.37 * refresh);
            assert!(close(x, 40.0 + 100.0 * 0.37 * refresh / 0.5) && close(opacity, 0.8 + 0.37 * refresh / 0.25), "running: {x} {opacity}");
            for at in [0.6, 0.999] {
                let (x, opacity) = both(begin + at);
                assert!(x == 7.0 && close(opacity, 1.0), "at {at}: the slide is over (no fill), the fade holds its end to the group's end: {x} {opacity}");
            }
            assert_eq!(present(&programs(layer), model, begin + 1.0, true).opacity, 0.6, "the group goes at its end on the screen");
            layer_remove_all_animations(layer, 0);
        });
    }

    /// A timeOffset shifts where an animation starts, not when it ends
    /// (Apple's end = begin + active / speed; the phase wraps).
    #[test]
    fn time_offset_shifts_the_phase_only() {
        let shifted = Timing { begin: 10.0, duration: 2.0, offset: 1.0, ..Default::default() };
        assert_eq!(shifted.end(), 12.0);
        assert_eq!(shifted.state(11.5), State { started: true, ended: false, progress: Some(0.25) });
    }

    /// A 3D turn past a quarter (a card flipped about y) turns about y the
    /// whole way, not about z: half way to 170° is 85° about y.
    #[test]
    fn transforms_turn_in_3d() {
        // CATransform3DMakeRotation(170°, 0, 1, 0).
        let (s, c) = 170f64.to_radians().sin_cos();
        let mut flip = IDENTITY_3D;
        (flip.0[0], flip.0[2], flip.0[8], flip.0[10]) = (c, -s, s, c);
        let Some(Value::Transform(half)) = Value::lerp(Value::Transform(IDENTITY_3D), Value::Transform(flip), 0.5) else { panic!() };
        assert!(close(half.0[0], 85f64.to_radians().cos()) && close(half.0[1], 0.0) && close(half.0[2], -(85f64.to_radians().sin())), "{half:?}");
    }

    static INNER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    static OUTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    static REPLACED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    extern "C" fn inner_done(_block: usize) {
        INNER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    extern "C" fn outer_done(_block: usize) {
        OUTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    extern "C" fn replaced_done(_block: usize) {
        REPLACED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    /// Core Animation's continuations: a nested transaction's completion
    /// waits only for the animations added after it was set (not for the
    /// outer transaction's, even one that repeats forever, with an implicit
    /// transaction open under both); the outer one waits for the inner one;
    /// a replaced completion block still runs.
    #[test]
    fn inner_completions_wait_only_for_their_own_animations() {
        use std::sync::atomic::Ordering::SeqCst;
        setup();
        let transaction = runtime::class_named("CATransaction");
        let new_layer = || runtime::send(runtime::send(runtime::class_named("CALayer"), "alloc", &[]), "init", &[]);
        let (a, b) = (new_layer(), new_layer());
        let descriptor: [usize; 4] = [0, std::mem::size_of::<crate::libsystem::mach::BlockHeader>(), 0, 0];
        let block = |invoke: extern "C" fn(usize)| crate::libsystem::mach::BlockHeader { isa: 0, flags: 1 << 28, reserved: 0, invoke: invoke as usize, descriptor: &descriptor };
        let (inner, outer, replaced) = (block(inner_done), block(outer_done), block(replaced_done));

        // A setting outside begin/commit opens the implicit transaction.
        runtime::send(transaction, "setDisableActions:", &[0]);
        runtime::send(transaction, "begin", &[]);
        runtime::send(transaction, "setCompletionBlock:", &[&outer as *const _ as usize]);
        runtime::send(transaction, "begin", &[]);
        runtime::send(transaction, "setCompletionBlock:", &[&inner as *const _ as usize]);
        let short = basic("opacity", collections::number_float(0.0), collections::number_float(1.0));
        anim_set_duration(short, 0, 0.1);
        add(a, short, string::make("short"));
        let begin = number(layer_animation_for_key(a, 0, string::make("short")), "beginTime", 0.0);
        runtime::send(transaction, "commit", &[]);
        let forever = basic("opacity", collections::number_float(0.0), collections::number_float(1.0));
        anim_set_repeat_count(forever, 0, f32::INFINITY);
        add(b, forever, 0);
        runtime::send(transaction, "commit", &[]);
        tick_at(Some(a), begin + 0.2);
        assert!(run_until(|| INNER.load(SeqCst) == 1), "the inner completion runs when its own animation ends");
        assert!(!run_until(|| OUTER.load(SeqCst) == 1), "the outer one waits for its endless animation");
        layer_remove_all_animations(b, 0);
        assert!(run_until(|| OUTER.load(SeqCst) == 1));

        runtime::send(transaction, "begin", &[]);
        runtime::send(transaction, "setCompletionBlock:", &[&replaced as *const _ as usize]);
        runtime::send(transaction, "setCompletionBlock:", &[&inner as *const _ as usize]);
        runtime::send(transaction, "commit", &[]);
        assert!(run_until(|| REPLACED.load(SeqCst) == 1 && INNER.load(SeqCst) == 2), "a replaced block still runs");
        runtime::send(transaction, "flush", &[]);
    }
}
