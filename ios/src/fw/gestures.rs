//! Gesture recognizers: UITapGestureRecognizer, UIPanGestureRecognizer,
//! UISwipeGestureRecognizer, UILongPressGestureRecognizer,
//! UIPinchGestureRecognizer and UIRotationGestureRecognizer, attached to
//! views (addGestureRecognizer:) and fed every touch that starts on their
//! view or a view inside it (fw/touch.rs), as UIKit does.
//!
//! The rules follow UIKit's: a tap is a press and release that moves less
//! than 10 points (taps of a multi-tap come within 0.35 s); a pan begins
//! once the fingers moved 10 points; a swipe is 50 points mostly one way
//! within half a second; a long press holds still 0.5 s; pinch and rotation
//! follow two fingers. A recognizer that recognizes takes the touch: the
//! others following it fail, and (cancelsTouchesInView, the default) the
//! view gets touchesCancelled:withEvent: and no more of that touch. Its
//! targets get their action with the recognizer. One that must wait for
//! others to fail (requireGestureRecognizerToFail:) recognizes once they
//! have all failed, and fails if one of them recognizes. delaysTouchesBegan
//! and delaysTouchesEnded hold the view's touches back (fw/touch.rs) until
//! the recognizers decide.

use crate::fw::geometry::{apply, invert, Point, IDENTITY};
use crate::fw::object::{define_class, methods};
use crate::fw::touch::Phase;
use crate::fw::uikit::{property, property_get, property_set, view_state};
use crate::fw::{collections, runloop};
use crate::objc::runtime::{self, lock, Id, Sel};
use std::collections::HashMap;
use std::sync::Mutex;

/// UIGestureRecognizerState.
pub const POSSIBLE: isize = 0;
pub const BEGAN: isize = 1;
pub const CHANGED: isize = 2;
pub const ENDED: isize = 3;
pub const CANCELLED: isize = 4;
pub const FAILED: isize = 5;

/// Points a finger may move and still tap or long-press.
const ALLOWED_MOVEMENT: f64 = 10.0;
/// Points the fingers move before a pan begins.
const PAN_START: f64 = 10.0;
const SWIPE_DISTANCE: f64 = 50.0;
const SWIPE_TIME: f64 = 0.5;
/// Longest gap between the taps of a multi-tap.
const TAP_GAP: f64 = 0.35;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Tap,
    Pan,
    Swipe,
    LongPress,
    Pinch,
    Rotation,
    /// A plain UIGestureRecognizer (or the game's own kind): never recognizes by itself.
    Plain,
}

#[derive(Clone, Copy, Debug)]
struct Finger {
    start: Point,
    now: Point,
}

struct Recognizer {
    kind: Kind,
    view: Id,
    targets: Vec<(Id, Sel)>,
    state: isize,
    /// The fingers it follows (window points).
    fingers: HashMap<i32, Finger>,
    /// Most fingers down at once in this sequence.
    most_fingers: usize,
    began_at: f64,
    /// The last centroid and its time, and the speed (points/s).
    last: (Point, f64),
    velocity: Point,
    /// Where translation counts from (the centroid at the start, moved by setTranslation:inView:).
    origin: Point,
    /// Tap: taps so far and when the last one ended.
    taps: usize,
    last_tap: f64,
    /// Pinch and rotation: the two fingers' distance and angle at the start
    /// (or when the game last set the scale or rotation, or a finger came),
    /// and the scale or rotation then.
    start_spread: Option<(f64, f64)>,
    spread_base: f64,
    scale: f64,
    rotation: f64,
    /// When it last recognized (for requireGestureRecognizerToFail:).
    recognized_at: f64,
    /// Recognizers that must fail before this one recognizes.
    requires: Vec<Id>,
    /// Recognized, but waiting for `requires` to fail: the state it enters then.
    waiting: Option<isize>,
    /// When the sequence it waits in began.
    wait_since: f64,
}

impl Recognizer {
    fn new(kind: Kind) -> Recognizer {
        Recognizer {
            kind,
            view: 0,
            targets: Vec::new(),
            state: POSSIBLE,
            fingers: HashMap::new(),
            most_fingers: 0,
            began_at: 0.0,
            last: (Point::default(), 0.0),
            velocity: Point::default(),
            origin: Point::default(),
            taps: 0,
            last_tap: f64::NEG_INFINITY,
            start_spread: None,
            spread_base: 1.0,
            scale: 1.0,
            rotation: 0.0,
            recognized_at: f64::NEG_INFINITY,
            requires: Vec::new(),
            waiting: None,
            wait_since: 0.0,
        }
    }

    fn centroid(&self) -> Point {
        let n = self.fingers.len().max(1) as f64;
        let (x, y) = self.fingers.values().fold((0.0, 0.0), |(x, y), f| (x + f.now.x, y + f.now.y));
        Point { x: x / n, y: y / n }
    }

    /// Distance and angle between the first two fingers (by finger number).
    fn spread(&self) -> Option<(f64, f64)> {
        let mut keys: Vec<&i32> = self.fingers.keys().collect();
        keys.sort();
        let (a, b) = (self.fingers.get(keys.first()?)?.now, self.fingers.get(keys.get(1)?)?.now);
        Some(((b.x - a.x).hypot(b.y - a.y), (b.y - a.y).atan2(b.x - a.x)))
    }

    fn farthest_move(&self) -> f64 {
        self.fingers.values().map(|f| (f.now.x - f.start.x).hypot(f.now.y - f.start.y)).fold(0.0, f64::max)
    }

    fn active(&self) -> bool {
        matches!(self.state, BEGAN | CHANGED)
    }

    /// Still deciding: fingers down, a multi-tap between taps, or waiting
    /// for others to fail.
    fn pending(&self) -> bool {
        !self.fingers.is_empty() || self.taps > 0 || self.waiting.is_some()
    }

    /// Hasn't recognized or failed yet in this sequence.
    fn undecided(&self) -> bool {
        self.state == POSSIBLE && self.pending()
    }

    /// Pinch and rotation count on from here: the fingers as they are now
    /// and the scale or rotation as it is.
    fn rebase_spread(&mut self) {
        self.start_spread = self.spread();
        self.spread_base = if self.kind == Kind::Rotation { self.rotation } else { self.scale };
    }
}

static RECOGNIZERS: Mutex<Option<HashMap<Id, Recognizer>>> = Mutex::new(None);
/// Each view's recognizers, in the order added.
static VIEWS: Mutex<Option<HashMap<Id, Vec<Id>>>> = Mutex::new(None);
/// The recognizers following each finger.
static FINGERS: Mutex<Option<HashMap<i32, Vec<Id>>>> = Mutex::new(None);

fn with<R>(recognizer: Id, f: impl FnOnce(&mut Recognizer) -> R) -> Option<R> {
    RECOGNIZERS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get_mut(&recognizer).map(f)
}

fn kind_of(object: Id) -> Kind {
    let rt = lock();
    let class = rt.class_of(object);
    let is = |name: &str| rt.by_name.get(name).is_some_and(|&c| rt.is_subclass(class, c));
    if is("UITapGestureRecognizer") {
        Kind::Tap
    } else if is("UIPanGestureRecognizer") {
        Kind::Pan
    } else if is("UISwipeGestureRecognizer") {
        Kind::Swipe
    } else if is("UILongPressGestureRecognizer") {
        Kind::LongPress
    } else if is("UIPinchGestureRecognizer") {
        Kind::Pinch
    } else if is("UIRotationGestureRecognizer") {
        Kind::Rotation
    } else {
        Kind::Plain
    }
}

/// The recognizer's record (made for one made with plain alloc/init).
fn ensure(recognizer: Id) {
    let kind = kind_of(recognizer);
    RECOGNIZERS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).entry(recognizer).or_insert_with(|| Recognizer::new(kind));
}

// ----- settings -----

property!(enabled, set_enabled_raw, "gestureEnabled", bool, true);
property!(cancels_touches, set_cancels_touches, "cancelsTouchesInView", bool, true);
property!(delays_began, set_delays_began, "delaysTouchesBegan", bool, false);
property!(delays_ended, set_delays_ended, "delaysTouchesEnded", bool, true);
property!(delegate, set_delegate, "gestureDelegate", id, 0);
property!(taps_required, set_taps_required, "numberOfTapsRequired", isize, 1);
property!(touches_required, set_touches_required, "numberOfTouchesRequired", isize, 1);
property!(minimum_touches, set_minimum_touches, "minimumNumberOfTouches", isize, 1);
property!(maximum_touches, set_maximum_touches, "maximumNumberOfTouches", isize, isize::MAX);
property!(direction, set_direction, "direction", isize, 1);
property!(press_duration, set_press_duration, "minimumPressDuration", f64, 0.5);
property!(allowed_movement, set_allowed_movement, "allowableMovement", f64, ALLOWED_MOVEMENT);

fn setting(object: Id, key: &'static str, default: isize) -> isize {
    property_get(object, key).map(|v| v as isize).unwrap_or(default)
}

/// Turning a recognizer off cancels what it is doing; it follows none of
/// the fingers down then, even if turned on again.
extern "C" fn set_enabled(this: Id, s: Sel, value: u8) {
    on_main(|| turn_on_or_off(this, s, value))
}

fn turn_on_or_off(this: Id, s: Sel, value: u8) {
    set_enabled_raw(this, s, value);
    if value == 0 {
        if let Some(fingers) = FINGERS.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            for list in fingers.values_mut() {
                list.retain(|&g| g != this);
            }
        }
        let was_active = with(this, |r| {
            let active = r.active();
            r.state = if active { CANCELLED } else { POSSIBLE };
            r.fingers.clear();
            (r.taps, r.waiting) = (0, None);
            active
        });
        if was_active == Some(true) {
            fire(this);
            with(this, |r| r.state = POSSIBLE);
        }
    }
}

// ----- making them and their targets -----

extern "C" fn init(this: Id, _: Sel) -> Id {
    ensure(this);
    this
}

extern "C" fn init_with_target(this: Id, _: Sel, target: Id, action: Sel) -> Id {
    ensure(this);
    if target != 0 && action != 0 {
        with(this, |r| r.targets.push((target, action)));
    }
    this
}

extern "C" fn add_target(this: Id, _: Sel, target: Id, action: Sel) {
    ensure(this);
    with(this, |r| {
        if target != 0 && action != 0 && !r.targets.contains(&(target, action)) {
            r.targets.push((target, action));
        }
    });
}

/// nil target or NULL action matches every one.
extern "C" fn remove_target(this: Id, _: Sel, target: Id, action: Sel) {
    with(this, |r| r.targets.retain(|&(t, a)| !((target == 0 || t == target) && (action == 0 || a == action))));
}

extern "C" fn require_to_fail(this: Id, _: Sel, other: Id) {
    ensure(this);
    with(this, |r| r.requires.push(other));
}

extern "C" fn state(this: Id, _: Sel) -> isize {
    with(this, |r| r.state).unwrap_or(POSSIBLE)
}

/// Game subclasses set their own state; ending or beginning tells the targets.
extern "C" fn set_state(this: Id, _: Sel, value: isize) {
    ensure(this);
    with(this, |r| r.state = value);
    if matches!(value, BEGAN | CHANGED | ENDED | CANCELLED) {
        fire(this);
    }
}

extern "C" fn view(this: Id, _: Sel) -> Id {
    with(this, |r| r.view).unwrap_or(0)
}

extern "C" fn number_of_touches(this: Id, _: Sel) -> usize {
    with(this, |r| r.fingers.len()).unwrap_or(0)
}

/// A window point in `view`'s own points (nil: the window's), through
/// the transforms of the view and the views it is in.
fn in_view(point: Point, view: Id) -> Point {
    if view == 0 {
        return point;
    }
    let to_view = invert(crate::fw::touch::view_to_window(view)).unwrap_or(IDENTITY);
    let (x, y) = apply(to_view, point.x, point.y);
    Point { x, y }
}

/// A distance or speed in window points as `view`'s points (no moving).
fn along_view(vector: Point, view: Id) -> Point {
    if view == 0 {
        return vector;
    }
    let t = invert(crate::fw::touch::view_to_window(view)).unwrap_or(IDENTITY);
    Point { x: t.a * vector.x + t.c * vector.y, y: t.b * vector.x + t.d * vector.y }
}

/// The fingers' middle; once they're up, where they were last.
extern "C" fn location_in_view(this: Id, _: Sel, view: Id) -> Point {
    let point = with(this, |r| if r.fingers.is_empty() { r.last.0 } else { r.centroid() }).unwrap_or_default();
    in_view(point, view)
}

extern "C" fn location_of_touch(this: Id, _: Sel, index: usize, view: Id) -> Point {
    let point = with(this, |r| {
        let mut keys: Vec<i32> = r.fingers.keys().copied().collect();
        keys.sort();
        keys.get(index).and_then(|k| r.fingers.get(k)).map(|f| f.now).unwrap_or(r.last.0)
    })
    .unwrap_or_default();
    in_view(point, view)
}

/// Translation and speed in the view's own points (a scaled or turned
/// view measures them its way).
extern "C" fn translation_in_view(this: Id, _: Sel, view: Id) -> Point {
    let moved = with(this, |r| {
        let now = if r.fingers.is_empty() { r.last.0 } else { r.centroid() };
        Point { x: now.x - r.origin.x, y: now.y - r.origin.y }
    })
    .unwrap_or_default();
    along_view(moved, view)
}

extern "C" fn set_translation(this: Id, _: Sel, translation: Point, view: Id) {
    // The view's points back to the window's.
    let t = if view == 0 { IDENTITY } else { crate::fw::touch::view_to_window(view) };
    let moved = Point { x: t.a * translation.x + t.c * translation.y, y: t.b * translation.x + t.d * translation.y };
    with(this, |r| {
        let now = if r.fingers.is_empty() { r.last.0 } else { r.centroid() };
        r.origin = Point { x: now.x - moved.x, y: now.y - moved.y };
    });
}

extern "C" fn velocity_in_view(this: Id, _: Sel, view: Id) -> Point {
    along_view(with(this, |r| r.velocity).unwrap_or_default(), view)
}

extern "C" fn scale(this: Id, _: Sel) -> f64 {
    with(this, |r| r.scale).unwrap_or(1.0)
}

/// -setScale: / -setRotation:: later changes count from the value set.
fn set_spread_value(r: &mut Recognizer, value: f64) {
    if r.kind == Kind::Rotation {
        r.rotation = value;
    } else {
        r.scale = value;
    }
    r.rebase_spread();
}

extern "C" fn set_scale(this: Id, _: Sel, value: f64) {
    with(this, |r| set_spread_value(r, value));
}

extern "C" fn rotation(this: Id, _: Sel) -> f64 {
    with(this, |r| r.rotation).unwrap_or(0.0)
}

extern "C" fn set_rotation(this: Id, _: Sel, value: f64) {
    with(this, |r| set_spread_value(r, value));
}

/// Pinch speed (scale per second) and rotation speed (radians per second).
extern "C" fn spread_velocity(this: Id, _: Sel) -> f64 {
    with(this, |r| r.velocity.x).unwrap_or(0.0)
}

extern "C" fn reset(_this: Id, _: Sel) {}

extern "C" fn ignore_touch(_this: Id, _: Sel, _touch: Id, _event: Id) {}

// ----- views -----

extern "C" fn view_add(this: Id, _: Sel, recognizer: Id) {
    if recognizer == 0 {
        return;
    }
    ensure(recognizer);
    let old = with(recognizer, |r| std::mem::replace(&mut r.view, this)).unwrap_or(0);
    let mut views = VIEWS.lock().unwrap_or_else(|e| e.into_inner());
    let views = views.get_or_insert_with(HashMap::new);
    if old != 0 {
        if let Some(list) = views.get_mut(&old) {
            list.retain(|&g| g != recognizer);
        }
    }
    let list = views.entry(this).or_default();
    if !list.contains(&recognizer) {
        list.push(recognizer);
        // The view holds its recognizers.
        runtime::send(recognizer, "retain", &[]);
    }
}

extern "C" fn view_remove(this: Id, _: Sel, recognizer: Id) {
    let removed = {
        let mut views = VIEWS.lock().unwrap_or_else(|e| e.into_inner());
        let list = views.get_or_insert_with(HashMap::new).entry(this).or_default();
        let before = list.len();
        list.retain(|&g| g != recognizer);
        before != list.len()
    };
    if removed {
        with(recognizer, |r| {
            r.view = 0;
            r.fingers.clear();
            r.state = POSSIBLE;
        });
        runtime::send(recognizer, "release", &[]);
    }
}

pub fn recognizers_of(view: Id) -> Vec<Id> {
    VIEWS.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|v| v.get(&view)).cloned().unwrap_or_default()
}

extern "C" fn view_recognizers(this: Id, _: Sel) -> Id {
    let list = recognizers_of(this);
    if list.is_empty() {
        0
    } else {
        collections::array_from(&list)
    }
}

extern "C" fn view_set_recognizers(this: Id, s: Sel, list: Id) {
    for old in recognizers_of(this) {
        view_remove(this, s, old);
    }
    for recognizer in if list == 0 { Vec::new() } else { collections::array_items(list) } {
        view_add(this, s, recognizer);
    }
}

// ----- recognizing -----

/// Tells the targets (each action with as many arguments as it has colons).
fn fire(recognizer: Id) {
    let targets = with(recognizer, |r| r.targets.clone()).unwrap_or_default();
    for (target, action) in targets {
        let name = lock().sel_name(action);
        let args: &[usize] = if name.contains(':') { &[recognizer] } else { &[] };
        runtime::send(target, &name, args);
    }
}

/// Asks the delegate gestureRecognizerShouldBegin: (yes when it doesn't say).
fn may_begin(recognizer: Id) -> bool {
    let delegate = delegate(recognizer, 0);
    if delegate == 0 || !runtime::responds(delegate, "gestureRecognizerShouldBegin:") {
        return true;
    }
    runtime::send(delegate, "gestureRecognizerShouldBegin:", &[recognizer]) & 0xFF != 0
}

fn may_receive(recognizer: Id, touch: Id) -> bool {
    let delegate = delegate(recognizer, 0);
    if delegate == 0 || touch == 0 || !runtime::responds(delegate, "gestureRecognizer:shouldReceiveTouch:") {
        return true;
    }
    runtime::send(delegate, "gestureRecognizer:shouldReceiveTouch:", &[recognizer, touch]) & 0xFF != 0
}

fn simultaneous(a: Id, b: Id) -> bool {
    [(a, b), (b, a)].into_iter().any(|(x, y)| {
        let delegate = delegate(x, 0);
        delegate != 0
            && runtime::responds(delegate, "gestureRecognizer:shouldRecognizeSimultaneouslyWithGestureRecognizer:")
            && runtime::send(delegate, "gestureRecognizer:shouldRecognizeSimultaneouslyWithGestureRecognizer:", &[x, y]) & 0xFF != 0
    })
}

/// What one recognizer does with a finger change: whether it took the
/// touch now (recognized), and whether its targets are told.
#[derive(Default, Debug, PartialEq)]
struct Step {
    recognized: bool,
    tell: bool,
    /// A multi-tap between taps: it fails unless the next comes in time.
    tap_gap: bool,
    /// A long press to check once it has been held long enough.
    hold_check: Option<f64>,
}

/// The rules for one recognizer (settings read before: no messages here).
fn step(r: &mut Recognizer, settings: &Settings, finger: i32, phase: Phase, point: Point, now: f64) -> Step {
    let mut out = Step::default();
    if r.waiting.is_some() {
        // Decided, waiting for others: it only follows the fingers.
        match phase {
            Phase::Began => drop(r.fingers.insert(finger, Finger { start: point, now: point })),
            Phase::Moved => {
                if let Some(f) = r.fingers.get_mut(&finger) {
                    f.now = point;
                }
            }
            Phase::Ended | Phase::Cancelled => drop(r.fingers.remove(&finger)),
        }
        if !r.fingers.is_empty() {
            r.last = (r.centroid(), now);
        }
        return out;
    }
    if matches!(r.state, FAILED | CANCELLED) {
        if phase == Phase::Ended || phase == Phase::Cancelled {
            r.fingers.remove(&finger);
        }
        return out;
    }
    match phase {
        Phase::Began => {
            if r.fingers.is_empty() {
                r.began_at = now;
                r.most_fingers = 0;
                r.velocity = Point::default();
            }
            r.fingers.insert(finger, Finger { start: point, now: point });
            r.most_fingers = r.most_fingers.max(r.fingers.len());
            let c = r.centroid();
            r.last = (c, now);
            if !r.active() {
                r.origin = c;
            }
            if matches!(r.kind, Kind::Pinch | Kind::Rotation) {
                r.rebase_spread();
            }
            if r.kind == Kind::LongPress && r.state == POSSIBLE && r.fingers.len() as isize == settings.touches {
                out.hold_check = Some(settings.press);
            }
            if r.kind == Kind::Tap && r.fingers.len() as isize > settings.touches {
                r.state = FAILED;
            }
        }
        Phase::Moved => {
            if let Some(f) = r.fingers.get_mut(&finger) {
                f.now = point;
            }
            let c = r.centroid();
            let dt = now - r.last.1;
            if dt > 1e-4 {
                let speed = Point { x: (c.x - r.last.0.x) / dt, y: (c.y - r.last.0.y) / dt };
                r.velocity = Point { x: (r.velocity.x + speed.x) / 2.0, y: (r.velocity.y + speed.y) / 2.0 };
                r.last = (c, now);
            }
            match r.kind {
                Kind::Tap | Kind::LongPress => {
                    if r.farthest_move() > settings.movement {
                        if r.active() {
                            r.state = CHANGED;
                            out.tell = true;
                        } else {
                            r.state = FAILED;
                        }
                    } else if r.active() {
                        r.state = CHANGED;
                        out.tell = true;
                    }
                }
                Kind::Pan => {
                    let n = r.fingers.len() as isize;
                    if r.active() {
                        r.state = CHANGED;
                        out.tell = true;
                    } else if n >= settings.min_touches && n <= settings.max_touches && (c.x - r.origin.x).hypot(c.y - r.origin.y) >= PAN_START {
                        r.state = BEGAN;
                        out.recognized = true;
                        out.tell = true;
                    }
                }
                Kind::Swipe => {
                    let (dx, dy) = (c.x - r.origin.x, c.y - r.origin.y);
                    let along = match settings.direction {
                        d if d & 1 != 0 && dx >= SWIPE_DISTANCE && dx.abs() > 2.0 * dy.abs() => true,
                        d if d & 2 != 0 && -dx >= SWIPE_DISTANCE && dx.abs() > 2.0 * dy.abs() => true,
                        d if d & 4 != 0 && -dy >= SWIPE_DISTANCE && dy.abs() > 2.0 * dx.abs() => true,
                        d if d & 8 != 0 && dy >= SWIPE_DISTANCE && dy.abs() > 2.0 * dx.abs() => true,
                        _ => false,
                    };
                    if now - r.began_at > SWIPE_TIME {
                        r.state = FAILED;
                    } else if along && r.most_fingers as isize == settings.touches {
                        r.state = ENDED;
                        out.recognized = true;
                        out.tell = true;
                    }
                }
                Kind::Pinch | Kind::Rotation => {
                    if let (Some((d0, a0)), Some((d, a))) = (r.start_spread, r.spread()) {
                        let old = (r.scale, r.rotation);
                        if r.kind == Kind::Pinch && d0 > 0.0 {
                            r.scale = r.spread_base * d / d0;
                        }
                        let mut turn = a - a0;
                        while turn > std::f64::consts::PI {
                            turn -= std::f64::consts::TAU;
                        }
                        while turn < -std::f64::consts::PI {
                            turn += std::f64::consts::TAU;
                        }
                        if r.kind == Kind::Rotation {
                            r.rotation = r.spread_base + turn;
                        }
                        if dt > 1e-4 {
                            r.velocity.x = if r.kind == Kind::Pinch { (r.scale - old.0) / dt } else { (r.rotation - old.1) / dt };
                        }
                        if r.active() {
                            r.state = CHANGED;
                            out.tell = true;
                        } else if (r.kind == Kind::Pinch && (d - d0).abs() >= PAN_START) || (r.kind == Kind::Rotation && turn.abs() >= 0.1) {
                            r.state = BEGAN;
                            out.recognized = true;
                            out.tell = true;
                        }
                    }
                }
                Kind::Plain => {}
            }
        }
        Phase::Ended | Phase::Cancelled => {
            if let Some(f) = r.fingers.get_mut(&finger) {
                f.now = point;
            }
            let all_up = r.fingers.len() <= 1;
            // Fingers held still before lifting: no speed left.
            let still = now - r.last.1 > 0.1;
            r.last = (r.centroid(), now);
            if still && (all_up || matches!(r.kind, Kind::Pinch | Kind::Rotation)) {
                r.velocity = Point::default();
            }
            if phase == Phase::Cancelled {
                if r.active() {
                    r.state = CANCELLED;
                    out.tell = true;
                } else {
                    r.state = FAILED;
                }
            } else if r.active() {
                if all_up || matches!(r.kind, Kind::Pinch | Kind::Rotation) {
                    r.state = ENDED;
                    out.tell = true;
                }
            } else if r.kind == Kind::Tap && all_up && r.state == POSSIBLE {
                r.taps = if now - r.last_tap <= TAP_GAP { r.taps + 1 } else { 1 };
                r.last_tap = now;
                if r.most_fingers as isize != settings.touches {
                    r.state = FAILED;
                } else if r.taps as isize >= settings.taps {
                    r.taps = 0;
                    r.state = ENDED;
                    out.recognized = true;
                    out.tell = true;
                } else {
                    out.tap_gap = true;
                }
            } else if all_up && r.state == POSSIBLE && r.kind != Kind::Tap {
                r.state = FAILED;
            }
            r.fingers.remove(&finger);
        }
    }
    if out.recognized {
        r.recognized_at = now;
    }
    out
}

/// A recognizer's settings, read with messages before its rules run.
struct Settings {
    taps: isize,
    touches: isize,
    min_touches: isize,
    max_touches: isize,
    direction: isize,
    press: f64,
    movement: f64,
}

fn settings_of(recognizer: Id) -> Settings {
    Settings {
        taps: setting(recognizer, "numberOfTapsRequired", 1).max(1),
        touches: setting(recognizer, "numberOfTouchesRequired", 1).max(1),
        min_touches: setting(recognizer, "minimumNumberOfTouches", 1).max(1),
        max_touches: setting(recognizer, "maximumNumberOfTouches", isize::MAX),
        direction: setting(recognizer, "direction", 1),
        press: press_duration(recognizer, 0),
        movement: allowed_movement(recognizer, 0),
    }
}

/// After a sequence: a recognizer that ended, failed or was cancelled goes
/// back to possible once its fingers are all up.
fn settle(recognizer: Id) {
    let done = with(recognizer, |r| r.fingers.is_empty() && r.waiting.is_none() && matches!(r.state, ENDED | FAILED | CANCELLED)).unwrap_or(false);
    if done {
        with(recognizer, |r| {
            (r.state, r.taps) = (POSSIBLE, 0);
            // A new pinch starts at scale 1 and a new rotation at 0, as UIKit's do.
            (r.start_spread, r.spread_base, r.scale, r.rotation) = (None, 1.0, 1.0, 0.0);
        });
        if runtime::responds(recognizer, "reset") {
            runtime::send(recognizer, "reset", &[]);
        }
    }
}

/// Which view's recognizers see a touch on `view`: its own and those of
/// every view it is in, deepest first (enabled ones).
fn recognizers_for(view: Id) -> Vec<Id> {
    let mut list = Vec::new();
    let mut current = view;
    for _ in 0..256 {
        if current == 0 {
            break;
        }
        list.extend(recognizers_of(current).into_iter().filter(|&g| enabled(g, 0)));
        current = view_state(current).superview;
    }
    list
}

/// What fw/touch.rs does with the view's touch once the recognizers saw a change.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Verdict {
    /// The view hears it.
    Pass,
    /// A recognizer took the touch (cancelsTouchesInView): the view's touch is cancelled.
    Took,
    /// Held back from the view until the recognizers decide (delaysTouchesBegan,
    /// delaysTouchesEnded); then `touch::release`.
    Hold,
}

/// Fingers whose touches are held back from their view: the recognizers
/// that may still take them, and since when.
static HOLDS: Mutex<Option<HashMap<i32, (Vec<Id>, f64)>>> = Mutex::new(None);

fn holding(finger: i32) -> bool {
    HOLDS.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|h| h.contains_key(&finger))
}

/// From fw/touch.rs, for each finger change on the main thread, before the
/// view hears it: feeds the recognizers following the finger, and says
/// what becomes of the view's touch.
pub fn touch_changed(view: Id, touch: Id, finger: i32, phase: Phase, point: Point) -> Verdict {
    on_main(|| decide_touch(view, touch, finger, phase, point))
}

/// Recognizers are main-thread state, decided one change at a time. On the
/// phone only the main thread gets here; in PC tests, where several test
/// threads run the main loop, this takes its turn like any main-loop work,
/// so one test's touches can't decide (or tell) another test's recognizers
/// halfway through its steps.
fn on_main<R>(work: impl FnOnce() -> R) -> R {
    #[cfg(test)]
    return runloop::as_main(work);
    #[cfg(not(test))]
    work()
}

fn decide_touch(view: Id, touch: Id, finger: i32, phase: Phase, point: Point) -> Verdict {
    let now = runloop::media_time();
    if phase == Phase::Began {
        let list: Vec<Id> = recognizers_for(view).into_iter().filter(|&g| may_receive(g, touch)).collect();
        if list.is_empty() {
            return if holding(finger) { Verdict::Hold } else { Verdict::Pass };
        }
        FINGERS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(finger, list);
    }
    let list = FINGERS.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|f| f.get(&finger)).cloned().unwrap_or_default();
    let mut took = false;
    for &recognizer in &list {
        let settings = settings_of(recognizer);
        let Some(mut out) = with(recognizer, |r| step(r, &settings, finger, phase, point, now)) else { continue };
        if out.recognized && !may_begin(recognizer) {
            with(recognizer, |r| r.state = FAILED);
            out = Step::default();
        }
        if out.recognized && with(recognizer, |r| !r.requires.is_empty()).unwrap_or(false) {
            // It must wait for the others to fail first (`resolve`).
            with(recognizer, |r| {
                r.waiting = Some(r.state);
                (r.state, r.wait_since, r.recognized_at) = (POSSIBLE, r.began_at, f64::NEG_INFINITY);
            });
            (out.recognized, out.tell) = (false, false);
        }
        if out.recognized {
            took |= take_touch(recognizer, &[finger]);
        }
        if out.tell {
            fire(recognizer);
        }
        if out.tap_gap {
            let stamp = with(recognizer, |r| r.last_tap).unwrap_or(0.0);
            runloop::post_after(
                TAP_GAP,
                Box::new(move || {
                    if with(recognizer, |r| r.last_tap == stamp) == Some(true) {
                        tap_gap_passed(recognizer);
                    }
                }),
            );
        }
        if let Some(seconds) = out.hold_check {
            runloop::post_after(seconds, Box::new(move || held(recognizer, finger)));
        }
    }
    if matches!(phase, Phase::Ended | Phase::Cancelled) {
        if let Some(fingers) = FINGERS.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            fingers.remove(&finger);
        }
        for &recognizer in &list {
            settle(recognizer);
        }
    }
    resolve();
    if took {
        return Verdict::Took;
    }
    // Held while a recognizer that delays this part of the touch may still take it.
    let delays = |g: Id| if matches!(phase, Phase::Began | Phase::Moved) { delays_began(g, 0) } else { delays_ended(g, 0) };
    if holding(finger) || list.iter().any(|&g| delays(g) && with(g, |r| r.undecided()).unwrap_or(false)) {
        HOLDS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).entry(finger).or_insert((list, now));
        Verdict::Hold
    } else {
        Verdict::Pass
    }
}

/// `recognizer` recognized on these fingers: the others following them
/// fail (unless they may recognize alongside it). Whether it takes the
/// touches from the view (cancelsTouchesInView).
fn take_touch(recognizer: Id, fingers: &[i32]) -> bool {
    let lists: Vec<Id> = {
        let all = FINGERS.lock().unwrap_or_else(|e| e.into_inner());
        fingers.iter().filter_map(|f| all.as_ref().and_then(|a| a.get(f)).cloned()).flatten().collect()
    };
    for other in lists {
        if other != recognizer && !simultaneous(recognizer, other) {
            with(other, |r| {
                if r.state == POSSIBLE {
                    (r.state, r.waiting) = (FAILED, None);
                }
            });
            settle(other);
        }
    }
    cancels_touches(recognizer, 0)
}

/// Recognizers waiting for others to fail: one fails as soon as one of
/// those recognized; it recognizes once none of them is still deciding.
/// Then the touches held back are let go or cancelled.
fn resolve() {
    for _ in 0..16 {
        let waiters: Vec<Id> =
            RECOGNIZERS.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|all| all.iter().filter(|(_, r)| r.waiting.is_some()).map(|(&id, _)| id).collect()).unwrap_or_default();
        let mut changed = false;
        for waiter in waiters {
            // Each step claims the waiter only if it still waits: an earlier
            // one's action may have turned it off or failed it meanwhile.
            let Some((others, since)) = with(waiter, |r| r.waiting.map(|_| (r.requires.clone(), r.wait_since))).flatten() else { continue };
            let beaten = others.iter().any(|&o| with(o, |r| r.recognized_at >= since).unwrap_or(false));
            let deciding = others.iter().any(|&o| enabled(o, 0) && with(o, |r| r.pending()).unwrap_or(false));
            if beaten {
                if with(waiter, |r| r.waiting.take().map(|_| r.state = FAILED)).flatten().is_some() {
                    settle(waiter);
                    changed = true;
                }
            } else if !deciding {
                let now = runloop::media_time();
                let Some((state, fingers)) = with(waiter, |r| {
                    let state = r.waiting.take()?;
                    (r.state, r.recognized_at) = (state, now);
                    Some((state, r.fingers.keys().copied().collect::<Vec<i32>>()))
                })
                .flatten() else {
                    continue;
                };
                if take_touch(waiter, &fingers) {
                    for &finger in &fingers {
                        crate::fw::touch::take_from_view(finger);
                    }
                }
                fire(waiter);
                if fingers.is_empty() {
                    // The fingers left while it waited: a continuous one ends too.
                    if state == BEGAN {
                        with(waiter, |r| r.state = ENDED);
                        fire(waiter);
                    }
                    settle(waiter);
                }
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    release_holds();
}

/// Touches held back from views whose recognizers have decided: cancelled
/// if one of them recognized (and takes touches), else handed on.
fn release_holds() {
    let holds: Vec<(i32, (Vec<Id>, f64))> = HOLDS.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|h| h.iter().map(|(&f, v)| (f, v.clone())).collect()).unwrap_or_default();
    for (finger, (list, since)) in holds {
        let taken = list.iter().any(|&g| with(g, |r| r.recognized_at >= since).unwrap_or(false) && cancels_touches(g, 0));
        let deciding = list.iter().any(|&g| (delays_began(g, 0) || delays_ended(g, 0)) && with(g, |r| r.undecided()).unwrap_or(false));
        if taken || !deciding {
            if let Some(holds) = HOLDS.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                holds.remove(&finger);
            }
            crate::fw::touch::release(finger, taken);
        }
    }
}

/// A multi-tap's time for its next tap ran out: it fails (UIKit's tap
/// recognizers fail when the gap passes), which may let others go on.
pub(crate) fn tap_gap_passed(recognizer: Id) {
    on_main(|| {
        with(recognizer, |r| {
            if r.taps > 0 && r.fingers.is_empty() && r.waiting.is_none() {
                r.taps = 0;
            }
        });
        resolve();
    })
}

/// A long press held long enough: it begins if the finger is still down
/// and hasn't moved too far (after others it must wait for have failed),
/// and takes the touch from the view as any recognizer does.
pub(crate) fn held(recognizer: Id, finger: i32) {
    on_main(|| hold_check(recognizer, finger))
}

fn hold_check(recognizer: Id, finger: i32) {
    let movement = allowed_movement(recognizer, 0);
    let now = runloop::media_time();
    let ready = with(recognizer, |r| r.state == POSSIBLE && r.waiting.is_none() && r.fingers.contains_key(&finger) && r.farthest_move() <= movement).unwrap_or(false);
    if !ready {
        return;
    }
    if !may_begin(recognizer) {
        with(recognizer, |r| r.state = FAILED);
        return;
    }
    let waits = with(recognizer, |r| {
        if r.requires.is_empty() {
            (r.state, r.recognized_at) = (BEGAN, now);
            false
        } else {
            (r.waiting, r.wait_since) = (Some(BEGAN), r.began_at);
            true
        }
    })
    .unwrap_or(false);
    if !waits {
        if take_touch(recognizer, &[finger]) {
            crate::fw::touch::take_from_view(finger);
        }
        fire(recognizer);
    }
    resolve();
}

pub fn register() {
    let base = define_class("UIGestureRecognizer", "NSObject", 8);
    methods(
        base,
        &[
            ("init", init as usize),
            ("initWithTarget:action:", init_with_target as usize),
            ("addTarget:action:", add_target as usize),
            ("removeTarget:action:", remove_target as usize),
            ("requireGestureRecognizerToFail:", require_to_fail as usize),
            ("state", state as usize),
            ("setState:", set_state as usize),
            ("view", view as usize),
            ("isEnabled", enabled as usize),
            ("setEnabled:", set_enabled as usize),
            ("delegate", delegate as usize),
            ("setDelegate:", set_delegate as usize),
            ("cancelsTouchesInView", cancels_touches as usize),
            ("setCancelsTouchesInView:", set_cancels_touches as usize),
            ("delaysTouchesBegan", delays_began as usize),
            ("setDelaysTouchesBegan:", set_delays_began as usize),
            ("delaysTouchesEnded", delays_ended as usize),
            ("setDelaysTouchesEnded:", set_delays_ended as usize),
            ("numberOfTouches", number_of_touches as usize),
            ("locationInView:", location_in_view as usize),
            ("locationOfTouch:inView:", location_of_touch as usize),
            ("reset", reset as usize),
            ("touchesBegan:withEvent:", ignore_touch as usize),
            ("touchesMoved:withEvent:", ignore_touch as usize),
            ("touchesEnded:withEvent:", ignore_touch as usize),
            ("touchesCancelled:withEvent:", ignore_touch as usize),
        ],
    );
    let tap = define_class("UITapGestureRecognizer", "UIGestureRecognizer", 8);
    methods(
        tap,
        &[
            ("numberOfTapsRequired", taps_required as usize),
            ("setNumberOfTapsRequired:", set_taps_required as usize),
            ("numberOfTouchesRequired", touches_required as usize),
            ("setNumberOfTouchesRequired:", set_touches_required as usize),
        ],
    );
    let pan = define_class("UIPanGestureRecognizer", "UIGestureRecognizer", 8);
    methods(
        pan,
        &[
            ("minimumNumberOfTouches", minimum_touches as usize),
            ("setMinimumNumberOfTouches:", set_minimum_touches as usize),
            ("maximumNumberOfTouches", maximum_touches as usize),
            ("setMaximumNumberOfTouches:", set_maximum_touches as usize),
            ("translationInView:", translation_in_view as usize),
            ("setTranslation:inView:", set_translation as usize),
            ("velocityInView:", velocity_in_view as usize),
        ],
    );
    let swipe = define_class("UISwipeGestureRecognizer", "UIGestureRecognizer", 8);
    methods(
        swipe,
        &[
            ("direction", direction as usize),
            ("setDirection:", set_direction as usize),
            ("numberOfTouchesRequired", touches_required as usize),
            ("setNumberOfTouchesRequired:", set_touches_required as usize),
        ],
    );
    let press = define_class("UILongPressGestureRecognizer", "UIGestureRecognizer", 8);
    methods(
        press,
        &[
            ("minimumPressDuration", press_duration as usize),
            ("setMinimumPressDuration:", set_press_duration as usize),
            ("allowableMovement", allowed_movement as usize),
            ("setAllowableMovement:", set_allowed_movement as usize),
            ("numberOfTouchesRequired", touches_required as usize),
            ("setNumberOfTouchesRequired:", set_touches_required as usize),
            ("numberOfTapsRequired", taps_required as usize),
            ("setNumberOfTapsRequired:", set_taps_required as usize),
        ],
    );
    let pinch = define_class("UIPinchGestureRecognizer", "UIGestureRecognizer", 8);
    methods(pinch, &[("scale", scale as usize), ("setScale:", set_scale as usize), ("velocity", spread_velocity as usize)]);
    let rotate = define_class("UIRotationGestureRecognizer", "UIGestureRecognizer", 8);
    methods(rotate, &[("rotation", rotation as usize), ("setRotation:", set_rotation as usize), ("velocity", spread_velocity as usize)]);
    let screen_edge = define_class("UIScreenEdgePanGestureRecognizer", "UIPanGestureRecognizer", 8);
    let _ = (screen_edge, delays_began, set_delays_began);

    methods(
        runtime::class_named("UIView"),
        &[
            ("addGestureRecognizer:", view_add as usize),
            ("removeGestureRecognizer:", view_remove as usize),
            ("gestureRecognizers", view_recognizers as usize),
            ("setGestureRecognizers:", view_set_recognizers as usize),
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fw::geometry::Rect;

    static HEARD: Mutex<Vec<(Id, isize, Point)>> = Mutex::new(Vec::new());

    extern "C" fn handle(_this: Id, _: Sel, recognizer: Id) {
        let at = if kind_of(recognizer) == Kind::Pan { translation_in_view(recognizer, 0, 0) } else { location_in_view(recognizer, 0, 0) };
        HEARD.lock().unwrap().push((recognizer, state(recognizer, 0), at));
    }

    fn take() -> Vec<(Id, isize, Point)> {
        std::mem::take(&mut *HEARD.lock().unwrap())
    }

    /// The waiting test's own log (tests run side by side).
    static WAIT_HEARD: Mutex<Vec<(Id, isize)>> = Mutex::new(Vec::new());

    extern "C" fn handle_wait(_this: Id, _: Sel, recognizer: Id) {
        WAIT_HEARD.lock().unwrap().push((recognizer, state(recognizer, 0)));
    }

    fn p(x: f64, y: f64) -> Point {
        Point { x, y }
    }

    /// The crossing test's log, and each recognizer's partner its action turns off.
    static CROSS_HEARD: Mutex<Vec<Id>> = Mutex::new(Vec::new());
    static PARTNERS: Mutex<Vec<(Id, Id)>> = Mutex::new(Vec::new());

    extern "C" fn handle_cross(_this: Id, _: Sel, recognizer: Id) {
        CROSS_HEARD.lock().unwrap().push(recognizer);
        let partner = PARTNERS.lock().unwrap().iter().find(|pair| pair.0 == recognizer).map(|pair| pair.1);
        if let Some(partner) = partner {
            set_enabled(partner, 0, 0);
        }
    }

    /// A recognizer that stops waiting while the waiters are being decided
    /// (turned off by an earlier one's action) is not told: two single taps
    /// waiting for one double tap, each turning the other off when told, so
    /// only the first is told.
    #[test]
    fn a_waiter_that_stopped_waiting_is_not_told() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let target_class = define_class("GestureTestCrossTarget", "NSObject", 8);
        methods(target_class, &[("handle:", handle_cross as usize)]);
        let target = runtime::send(runtime::send(target_class, "alloc", &[]), "init", &[]);
        let view = runtime::send(runtime::class_named("UIView"), "alloc", &[]);
        crate::fw::uikit::send_rect(view, "initWithFrame:", Rect::new(0.0, 0.0, 300.0, 300.0));
        let make = |class: &str| init_with_target(runtime::alloc_instance(runtime::class_named(class)), 0, target, runtime::sel("handle:"));
        let (first, second, double) = (make("UITapGestureRecognizer"), make("UITapGestureRecognizer"), make("UITapGestureRecognizer"));
        set_taps_required(double, 0, 2);
        for single in [first, second] {
            require_to_fail(single, 0, double);
            view_add(view, 0, single);
        }
        view_add(view, 0, double);
        PARTNERS.lock().unwrap().extend([(first, second), (second, first)]);
        let heard = crate::fw::runloop::as_main(|| {
            touch_changed(view, 0, 811, Phase::Began, p(50.0, 50.0));
            touch_changed(view, 0, 811, Phase::Ended, p(50.0, 50.0));
            assert!(CROSS_HEARD.lock().unwrap().is_empty(), "both wait for the double tap");
            tap_gap_passed(double);
            std::mem::take(&mut *CROSS_HEARD.lock().unwrap())
        });
        assert_eq!(heard.len(), 1, "only the one decided first: {heard:?}");
        for recognizer in [first, second, double] {
            view_remove(view, 0, recognizer);
        }
    }

    /// Taps and pans on one view, as UIKit recognizes them: a still press
    /// and release taps (and the pan fails); a drag begins the pan after 10
    /// points (the tap fails, the view's touch is cancelled), changes, ends,
    /// with the translation from where the finger went down; a recognizer
    /// on the superview sees touches on views inside.
    #[test]
    fn taps_and_pans_are_recognized_and_take_the_touch() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        // As the main thread would, so no other test's main loop decides recognizers meanwhile.
        crate::fw::runloop::as_main(taps_and_pans);
    }

    fn taps_and_pans() {
        let target_class = define_class("GestureTestTarget", "NSObject", 8);
        methods(target_class, &[("handle:", handle as usize)]);
        let target = runtime::send(runtime::send(target_class, "alloc", &[]), "init", &[]);
        let outer = runtime::send(runtime::class_named("UIView"), "alloc", &[]);
        crate::fw::uikit::send_rect(outer, "initWithFrame:", Rect::new(0.0, 0.0, 300.0, 300.0));
        let inner = runtime::send(runtime::class_named("UIView"), "alloc", &[]);
        crate::fw::uikit::send_rect(inner, "initWithFrame:", Rect::new(10.0, 10.0, 100.0, 100.0));
        runtime::send(outer, "addSubview:", &[inner]);
        let make = |class: &str| init_with_target(runtime::alloc_instance(runtime::class_named(class)), 0, target, runtime::sel("handle:"));
        let (tap, pan) = (make("UITapGestureRecognizer"), make("UIPanGestureRecognizer"));
        view_add(outer, 0, tap);
        view_add(outer, 0, pan);
        assert_eq!(view(tap, 0), outer);

        // A tap on the inner view.
        assert_eq!(touch_changed(inner, 0, 1, Phase::Began, p(50.0, 50.0)), Verdict::Pass);
        assert_eq!(touch_changed(inner, 0, 1, Phase::Ended, p(51.0, 50.0)), Verdict::Took, "the tap takes the touch");
        assert_eq!(take(), [(tap, ENDED, p(51.0, 50.0))]);
        assert_eq!((state(tap, 0), state(pan, 0)), (POSSIBLE, POSSIBLE), "both ready again");

        // A drag.
        touch_changed(inner, 0, 2, Phase::Began, p(50.0, 50.0));
        assert_eq!(touch_changed(inner, 0, 2, Phase::Moved, p(55.0, 50.0)), Verdict::Pass, "not yet 10 points");
        assert!(take().is_empty());
        assert_eq!(touch_changed(inner, 0, 2, Phase::Moved, p(70.0, 50.0)), Verdict::Took, "the pan takes the touch");
        touch_changed(inner, 0, 2, Phase::Moved, p(80.0, 60.0));
        touch_changed(inner, 0, 2, Phase::Ended, p(90.0, 60.0));
        assert_eq!(take(), [(pan, BEGAN, p(20.0, 0.0)), (pan, CHANGED, p(30.0, 10.0)), (pan, ENDED, p(40.0, 10.0))]);
        assert_eq!(state(tap, 0), POSSIBLE, "the failed tap is ready again");

        // Turned off: nothing.
        set_enabled(pan, 0, 0);
        set_enabled(tap, 0, 0);
        assert_eq!(touch_changed(inner, 0, 3, Phase::Began, p(1.0, 1.0)), Verdict::Pass);
        touch_changed(inner, 0, 3, Phase::Ended, p(1.0, 1.0));
        assert!(take().is_empty());
        view_remove(outer, 0, tap);
        assert_eq!(recognizers_of(outer), [pan]);
    }

    /// A single tap that must wait for a double tap to fail is told once the
    /// double tap really failed (its gap passed), never when it recognized;
    /// a recognizer turned off mid-touch stays out of that touch; a pan
    /// measures in the asked view's points (a view scaled 2x halves them).
    #[test]
    fn recognizers_wait_for_failure_stay_off_and_measure_in_their_view() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        // All of it as one piece of main-loop work: no other test's main loop
        // decides these recognizers or runs their tap-gap timers between the
        // steps, however slow the machine is.
        crate::fw::runloop::as_main(wait_for_failure_stay_off_and_measure);
    }

    fn wait_for_failure_stay_off_and_measure() {
        let target_class = define_class("GestureTestWaitTarget", "NSObject", 8);
        methods(target_class, &[("handle:", handle_wait as usize)]);
        let target = runtime::send(runtime::send(target_class, "alloc", &[]), "init", &[]);
        let take = || std::mem::take(&mut *WAIT_HEARD.lock().unwrap());
        let new_view = |frame: Rect| {
            let view = runtime::send(runtime::class_named("UIView"), "alloc", &[]);
            crate::fw::uikit::send_rect(view, "initWithFrame:", frame);
            view
        };
        let window = new_view(Rect::new(0.0, 0.0, 400.0, 400.0));
        let view = new_view(Rect::new(0.0, 0.0, 300.0, 300.0));
        runtime::send(window, "addSubview:", &[view]);
        let make = |class: &str| init_with_target(runtime::alloc_instance(runtime::class_named(class)), 0, target, runtime::sel("handle:"));
        let (single, double) = (make("UITapGestureRecognizer"), make("UITapGestureRecognizer"));
        set_taps_required(double, 0, 2);
        require_to_fail(single, 0, double);
        view_add(view, 0, single);
        view_add(view, 0, double);
        let touch = |finger: i32, phase: Phase, at: Point| touch_changed(view, 0, finger, phase, at);
        let tap = |finger: i32| {
            touch(finger, Phase::Began, p(50.0, 50.0));
            touch(finger, Phase::Ended, p(50.0, 50.0));
        };
        tap(801);
        assert!(take().is_empty(), "the single tap waits for the double tap");
        tap_gap_passed(double);
        assert_eq!(take(), [(single, ENDED)], "the double tap failed: the single one is told");
        tap(802);
        tap(803);
        assert_eq!(take(), [(double, ENDED)], "the double tap won");
        tap_gap_passed(double);
        assert!(take().is_empty(), "the single tap failed with it");
        view_remove(view, 0, single);
        view_remove(view, 0, double);

        // Turned off mid-drag, then on again: nothing from that drag.
        let pan = make("UIPanGestureRecognizer");
        view_add(view, 0, pan);
        touch(804, Phase::Began, p(50.0, 50.0));
        set_enabled(pan, 0, 0);
        touch(804, Phase::Moved, p(90.0, 50.0));
        set_enabled(pan, 0, 1);
        touch(804, Phase::Moved, p(120.0, 50.0));
        touch(804, Phase::Ended, p(120.0, 50.0));
        assert!(take().iter().all(|h| h.0 != pan), "off during the touch");

        // Scaled 2x: 30 window points are 15 of the view's.
        let s = runtime::sel("setTransform:");
        let set_transform: extern "C" fn(Id, Sel, crate::fw::geometry::Transform) = unsafe { std::mem::transmute(runtime::imp_for(view, s).unwrap()) };
        set_transform(view, s, crate::fw::geometry::Transform { a: 2.0, b: 0.0, c: 0.0, d: 2.0, tx: 0.0, ty: 0.0 });
        touch(805, Phase::Began, p(50.0, 50.0));
        touch(805, Phase::Moved, p(80.0, 50.0));
        assert_eq!(translation_in_view(pan, 0, view), p(15.0, 0.0));
        assert_eq!(translation_in_view(pan, 0, 0), p(30.0, 0.0), "the window's points");
        set_translation(pan, 0, p(0.0, 0.0), view);
        touch(805, Phase::Moved, p(90.0, 50.0));
        assert_eq!(translation_in_view(pan, 0, view), p(5.0, 0.0));
        touch(805, Phase::Ended, p(90.0, 50.0));
        view_remove(view, 0, pan);
        take();
    }

    /// A double tap needs both taps within the gap (between them it waits
    /// for the next); swipes and pinches follow their fingers.
    #[test]
    fn multi_taps_count_their_taps() {
        let settings = |taps| Settings { taps, touches: 1, min_touches: 1, max_touches: isize::MAX, direction: 1, press: 0.5, movement: 10.0 };
        let mut double = Recognizer::new(Kind::Tap);
        let at = p(5.0, 5.0);
        step(&mut double, &settings(2), 1, Phase::Began, at, 0.0);
        let first = step(&mut double, &settings(2), 1, Phase::Ended, at, 0.1);
        assert!(!first.tell && first.tap_gap, "waits for the next tap");
        step(&mut double, &settings(2), 1, Phase::Began, at, 0.2);
        assert!(step(&mut double, &settings(2), 1, Phase::Ended, at, 0.3).recognized);
        let mut slow = Recognizer::new(Kind::Tap);
        step(&mut slow, &settings(2), 1, Phase::Began, at, 0.0);
        step(&mut slow, &settings(2), 1, Phase::Ended, at, 0.1);
        step(&mut slow, &settings(2), 1, Phase::Began, at, 1.0);
        assert!(!step(&mut slow, &settings(2), 1, Phase::Ended, at, 1.1).recognized, "too slow");
        // A swipe right: 50 points mostly across, quickly.
        let mut swipe = Recognizer::new(Kind::Swipe);
        step(&mut swipe, &settings(1), 1, Phase::Began, p(0.0, 0.0), 0.0);
        assert!(!step(&mut swipe, &settings(1), 1, Phase::Moved, p(30.0, 2.0), 0.1).recognized);
        assert!(step(&mut swipe, &settings(1), 1, Phase::Moved, p(60.0, 5.0), 0.2).recognized);
        // A pinch: two fingers spreading to twice as far apart.
        let mut pinch = Recognizer::new(Kind::Pinch);
        step(&mut pinch, &settings(1), 1, Phase::Began, p(0.0, 0.0), 0.0);
        step(&mut pinch, &settings(1), 2, Phase::Began, p(20.0, 0.0), 0.0);
        assert!(step(&mut pinch, &settings(1), 2, Phase::Moved, p(40.0, 0.0), 0.1).recognized);
        assert!((pinch.scale - 2.0).abs() < 1e-9);
    }

    /// Games zoom step by step: they apply `scale` and set it back to 1, so
    /// the next change counts from there (UIKit's own sample does this);
    /// a third finger doesn't make it jump; it goes still once the fingers
    /// stop before lifting. Rotation works the same way.
    #[test]
    fn pinch_and_rotation_count_from_a_scale_the_game_set() {
        let settings = Settings { taps: 1, touches: 1, min_touches: 1, max_touches: isize::MAX, direction: 1, press: 0.5, movement: 10.0 };
        let mut pinch = Recognizer::new(Kind::Pinch);
        step(&mut pinch, &settings, 1, Phase::Began, p(0.0, 0.0), 0.0);
        step(&mut pinch, &settings, 2, Phase::Began, p(20.0, 0.0), 0.0);
        step(&mut pinch, &settings, 2, Phase::Moved, p(40.0, 0.0), 0.1);
        assert!((pinch.scale - 2.0).abs() < 1e-9);
        set_spread_value(&mut pinch, 1.0);
        step(&mut pinch, &settings, 2, Phase::Moved, p(60.0, 0.0), 0.2);
        assert!((pinch.scale - 1.5).abs() < 1e-9, "40 to 60 points apart since it was set: {}", pinch.scale);
        step(&mut pinch, &settings, 3, Phase::Began, p(100.0, 100.0), 0.3);
        step(&mut pinch, &settings, 2, Phase::Moved, p(60.0, 0.0), 0.35);
        assert!((pinch.scale - 1.5).abs() < 1e-9, "a third finger: {}", pinch.scale);
        step(&mut pinch, &settings, 2, Phase::Ended, p(60.0, 0.0), 0.8);
        assert_eq!(pinch.state, ENDED);
        assert_eq!(pinch.velocity.x, 0.0, "held still before lifting");

        let mut turn = Recognizer::new(Kind::Rotation);
        step(&mut turn, &settings, 1, Phase::Began, p(0.0, 0.0), 0.0);
        step(&mut turn, &settings, 2, Phase::Began, p(10.0, 0.0), 0.0);
        step(&mut turn, &settings, 2, Phase::Moved, p(0.0, 10.0), 0.1);
        assert!((turn.rotation - std::f64::consts::FRAC_PI_2).abs() < 1e-9);
        set_spread_value(&mut turn, 0.0);
        step(&mut turn, &settings, 2, Phase::Moved, p(-10.0, 0.0), 0.2);
        assert!((turn.rotation - std::f64::consts::FRAC_PI_2).abs() < 1e-9, "a quarter turn since it was set: {}", turn.rotation);
    }

    /// UIKit starts every new pinch at scale 1.0: a second pinch on the same
    /// recognizer counts from 1, not from where the first one left the scale.
    #[test]
    fn a_new_pinch_starts_from_scale_one() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        // One piece of main-loop work, as the phone's main thread would.
        crate::fw::runloop::as_main(new_pinch_starts_from_scale_one);
    }

    fn new_pinch_starts_from_scale_one() {
        let view = runtime::send(runtime::class_named("UIView"), "alloc", &[]);
        crate::fw::uikit::send_rect(view, "initWithFrame:", Rect::new(0.0, 0.0, 300.0, 300.0));
        let pinch = runtime::send(runtime::alloc_instance(runtime::class_named("UIPinchGestureRecognizer")), "init", &[]);
        view_add(view, 0, pinch);
        let touch = |finger: i32, phase: Phase, at: Point| touch_changed(view, 0, finger, phase, at);
        // First pinch: fingers 100 points apart, spreading to 300.
        touch(901, Phase::Began, p(100.0, 100.0));
        touch(902, Phase::Began, p(200.0, 100.0));
        touch(902, Phase::Moved, p(300.0, 100.0));
        assert!((scale(pinch, 0) - 2.0).abs() < 1e-9, "100 to 200 points: {}", scale(pinch, 0));
        touch(902, Phase::Moved, p(400.0, 100.0));
        assert!((scale(pinch, 0) - 3.0).abs() < 1e-9, "100 to 300 points: {}", scale(pinch, 0));
        touch(901, Phase::Ended, p(100.0, 100.0));
        touch(902, Phase::Ended, p(400.0, 100.0));
        assert_eq!(state(pinch, 0), POSSIBLE, "ready for the next pinch");
        // Second pinch: again 100 points apart, then 50 more.
        touch(903, Phase::Began, p(100.0, 100.0));
        touch(904, Phase::Began, p(200.0, 100.0));
        touch(904, Phase::Moved, p(250.0, 100.0));
        assert!((scale(pinch, 0) - 1.5).abs() < 1e-9, "100 to 150 points since this pinch began: {}", scale(pinch, 0));
        touch(903, Phase::Ended, p(100.0, 100.0));
        touch(904, Phase::Ended, p(250.0, 100.0));
        view_remove(view, 0, pinch);
    }

    /// 32-bit games reach these methods only through a framework_selectors.tsv row.
    #[test]
    fn every_gesture_method_has_a_32_bit_row() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let table = crate::guest32::objc::selectors::FrameworkSelectors::builtin();
        let classes = [
            "UIGestureRecognizer", "UITapGestureRecognizer", "UIPanGestureRecognizer", "UISwipeGestureRecognizer",
            "UILongPressGestureRecognizer", "UIPinchGestureRecognizer", "UIRotationGestureRecognizer",
        ];
        let mut missing = Vec::new();
        let rt = lock();
        for name in classes {
            let class = rt.by_name[name];
            for &sel in rt.classes[&class].methods.keys() {
                let selector = rt.sel_name(sel);
                if table.get(&selector).is_none() {
                    missing.push(format!("-[{name} {selector}]"));
                }
            }
        }
        for selector in ["addGestureRecognizer:", "removeGestureRecognizer:", "gestureRecognizers", "setGestureRecognizers:"] {
            if table.get(selector).is_none() {
                missing.push(format!("-[UIView {selector}]"));
            }
        }
        missing.sort();
        assert!(missing.is_empty(), "no framework_selectors.tsv row for {missing:?}");
    }
}
