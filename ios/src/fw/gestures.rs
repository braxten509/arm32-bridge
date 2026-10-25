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
//! targets get their action with the recognizer; a tap that must wait for
//! another to fail (requireGestureRecognizerToFail:) is told once that one
//! had its chance.

use crate::fw::geometry::Point;
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
    /// Pinch and rotation: the two fingers' distance and angle at the start.
    start_spread: Option<(f64, f64)>,
    scale: f64,
    rotation: f64,
    /// When it last recognized (for requireGestureRecognizerToFail:).
    recognized_at: f64,
    /// Recognizers that must fail before this one is told.
    requires: Vec<Id>,
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
            scale: 1.0,
            rotation: 0.0,
            recognized_at: f64::NEG_INFINITY,
            requires: Vec::new(),
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

/// Turning a recognizer off cancels what it is doing.
extern "C" fn set_enabled(this: Id, s: Sel, value: u8) {
    set_enabled_raw(this, s, value);
    if value == 0 {
        let was_active = with(this, |r| {
            let active = r.active();
            r.state = if active { CANCELLED } else { POSSIBLE };
            r.fingers.clear();
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

/// A window point in `view`'s own points (nil: the window's).
fn in_view(point: Point, view: Id) -> Point {
    if view == 0 {
        return point;
    }
    let origin = crate::fw::touch::origin_in_window(view);
    Point { x: point.x - origin.x, y: point.y - origin.y }
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

/// Translation and speed are the same in any view without a turn or scale.
extern "C" fn translation_in_view(this: Id, _: Sel, _view: Id) -> Point {
    with(this, |r| {
        let now = if r.fingers.is_empty() { r.last.0 } else { r.centroid() };
        Point { x: now.x - r.origin.x, y: now.y - r.origin.y }
    })
    .unwrap_or_default()
}

extern "C" fn set_translation(this: Id, _: Sel, translation: Point, _view: Id) {
    with(this, |r| {
        let now = if r.fingers.is_empty() { r.last.0 } else { r.centroid() };
        r.origin = Point { x: now.x - translation.x, y: now.y - translation.y };
    });
}

extern "C" fn velocity_in_view(this: Id, _: Sel, _view: Id) -> Point {
    with(this, |r| r.velocity).unwrap_or_default()
}

extern "C" fn scale(this: Id, _: Sel) -> f64 {
    with(this, |r| r.scale).unwrap_or(1.0)
}

extern "C" fn set_scale(this: Id, _: Sel, value: f64) {
    with(this, |r| r.scale = value);
}

extern "C" fn rotation(this: Id, _: Sel) -> f64 {
    with(this, |r| r.rotation).unwrap_or(0.0)
}

extern "C" fn set_rotation(this: Id, _: Sel, value: f64) {
    with(this, |r| r.rotation = value);
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
    /// A tap waiting for others to fail first.
    wait_for: Vec<Id>,
    /// A long press to check once it has been held long enough.
    hold_check: Option<f64>,
}

/// The rules for one recognizer (settings read before: no messages here).
fn step(r: &mut Recognizer, settings: &Settings, finger: i32, phase: Phase, point: Point, now: f64) -> Step {
    let mut out = Step::default();
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
                r.start_spread = r.spread();
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
                        r.scale = if d0 > 0.0 { d / d0 } else { 1.0 };
                        let mut turn = a - a0;
                        while turn > std::f64::consts::PI {
                            turn -= std::f64::consts::TAU;
                        }
                        while turn < -std::f64::consts::PI {
                            turn += std::f64::consts::TAU;
                        }
                        r.rotation = turn;
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
            r.last = (r.centroid(), now);
            if all_up && now - r.last.1 > 0.1 {
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
                    if r.requires.is_empty() {
                        r.state = ENDED;
                        out.recognized = true;
                        out.tell = true;
                    } else {
                        out.wait_for = r.requires.clone();
                    }
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
    let done = with(recognizer, |r| r.fingers.is_empty() && matches!(r.state, ENDED | FAILED | CANCELLED)).unwrap_or(false);
    if done {
        with(recognizer, |r| r.state = POSSIBLE);
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

/// From fw/touch.rs, for each finger change on the main thread, before the
/// view hears it: feeds the recognizers following the finger. Returns
/// whether a recognizer took the touch from the view (cancelsTouchesInView).
pub fn touch_changed(view: Id, touch: Id, finger: i32, phase: Phase, point: Point) -> bool {
    let now = runloop::media_time();
    if phase == Phase::Began {
        let list: Vec<Id> = recognizers_for(view).into_iter().filter(|&g| may_receive(g, touch)).collect();
        if list.is_empty() {
            return false;
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
        if out.recognized {
            // It takes the touch: the others following it fail.
            for &other in &list {
                if other != recognizer && !simultaneous(recognizer, other) {
                    with(other, |r| {
                        if r.state == POSSIBLE {
                            r.state = FAILED;
                        }
                    });
                }
            }
            took |= cancels_touches(recognizer, 0);
        }
        if out.tell {
            fire(recognizer);
        }
        if !out.wait_for.is_empty() {
            wait_then_tap(recognizer, out.wait_for, now);
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
    took
}

/// A tap that waits for others to fail: told after they had their chance,
/// unless one recognized since.
fn wait_then_tap(recognizer: Id, others: Vec<Id>, at: f64) {
    runloop::post_after(
        TAP_GAP,
        Box::new(move || {
            let beaten = others.iter().any(|&o| with(o, |r| r.recognized_at >= at).unwrap_or(false));
            if !beaten {
                with(recognizer, |r| {
                    r.state = ENDED;
                    r.recognized_at = runloop::media_time();
                });
                fire(recognizer);
                with(recognizer, |r| r.state = POSSIBLE);
            }
        }),
    );
}

/// A long press held long enough: it begins if the finger is still down
/// and hasn't moved too far.
fn held(recognizer: Id, finger: i32) {
    let movement = allowed_movement(recognizer, 0);
    let began = with(recognizer, |r| {
        let ok = r.state == POSSIBLE && r.fingers.contains_key(&finger) && r.farthest_move() <= movement;
        if ok {
            r.state = BEGAN;
            r.recognized_at = runloop::media_time();
        }
        ok
    })
    .unwrap_or(false);
    if began && may_begin(recognizer) {
        let others = FINGERS.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|f| f.get(&finger)).cloned().unwrap_or_default();
        for other in others {
            if other != recognizer && !simultaneous(recognizer, other) {
                with(other, |r| {
                    if r.state == POSSIBLE {
                        r.state = FAILED;
                    }
                });
            }
        }
        fire(recognizer);
    } else if began {
        with(recognizer, |r| r.state = FAILED);
    }
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

    fn p(x: f64, y: f64) -> Point {
        Point { x, y }
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
        assert!(!touch_changed(inner, 0, 1, Phase::Began, p(50.0, 50.0)));
        assert!(touch_changed(inner, 0, 1, Phase::Ended, p(51.0, 50.0)), "the tap takes the touch");
        assert_eq!(take(), [(tap, ENDED, p(51.0, 50.0))]);
        assert_eq!((state(tap, 0), state(pan, 0)), (POSSIBLE, POSSIBLE), "both ready again");

        // A drag.
        touch_changed(inner, 0, 2, Phase::Began, p(50.0, 50.0));
        assert!(!touch_changed(inner, 0, 2, Phase::Moved, p(55.0, 50.0)), "not yet 10 points");
        assert!(take().is_empty());
        assert!(touch_changed(inner, 0, 2, Phase::Moved, p(70.0, 50.0)), "the pan takes the touch");
        touch_changed(inner, 0, 2, Phase::Moved, p(80.0, 60.0));
        touch_changed(inner, 0, 2, Phase::Ended, p(90.0, 60.0));
        assert_eq!(take(), [(pan, BEGAN, p(20.0, 0.0)), (pan, CHANGED, p(30.0, 10.0)), (pan, ENDED, p(40.0, 10.0))]);
        assert_eq!(state(tap, 0), POSSIBLE, "the failed tap is ready again");

        // Turned off: nothing.
        set_enabled(pan, 0, 0);
        set_enabled(tap, 0, 0);
        assert!(!touch_changed(inner, 0, 3, Phase::Began, p(1.0, 1.0)));
        touch_changed(inner, 0, 3, Phase::Ended, p(1.0, 1.0));
        assert!(take().is_empty());
        view_remove(outer, 0, tap);
        assert_eq!(recognizers_of(outer), [pan]);
    }

    /// A double tap needs both taps within the gap; a single tap that must
    /// wait for it to fail is told only when no second tap came.
    #[test]
    fn multi_taps_count_their_taps() {
        let settings = |taps| Settings { taps, touches: 1, min_touches: 1, max_touches: isize::MAX, direction: 1, press: 0.5, movement: 10.0 };
        let mut double = Recognizer::new(Kind::Tap);
        let at = p(5.0, 5.0);
        step(&mut double, &settings(2), 1, Phase::Began, at, 0.0);
        assert!(!step(&mut double, &settings(2), 1, Phase::Ended, at, 0.1).tell);
        step(&mut double, &settings(2), 1, Phase::Began, at, 0.2);
        assert!(step(&mut double, &settings(2), 1, Phase::Ended, at, 0.3).recognized);
        let mut slow = Recognizer::new(Kind::Tap);
        step(&mut slow, &settings(2), 1, Phase::Began, at, 0.0);
        step(&mut slow, &settings(2), 1, Phase::Ended, at, 0.1);
        step(&mut slow, &settings(2), 1, Phase::Began, at, 1.0);
        assert!(!step(&mut slow, &settings(2), 1, Phase::Ended, at, 1.1).recognized, "too slow");
        let mut single = Recognizer::new(Kind::Tap);
        single.requires.push(0x1234);
        step(&mut single, &settings(1), 1, Phase::Began, at, 0.0);
        let out = step(&mut single, &settings(1), 1, Phase::Ended, at, 0.1);
        assert_eq!((out.tell, out.wait_for), (false, vec![0x1234]));
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
}
