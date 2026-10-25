//! Touches: UITouch and UIEvent, delivered to the view under the finger
//! (touchesBegan/Moved/Ended/Cancelled:withEvent:) on the main thread.
//! The phone app hands touches over in window pixels; IOS_TAPS lets PC
//! tests tap too ("seconds:x,y;seconds:x,y" in points).

use crate::fw::geometry::{concat, Point, Transform, IDENTITY};
use crate::fw::object::{define_class, methods};
use crate::fw::uikit::{key_window, property_get, property_set, view_state};
use crate::fw::{collections, misc};
use crate::objc::runtime::{self, lock, Id, Sel};
use crate::symbols::Registry;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Phase {
    Began = 0,
    Moved = 1,
    Ended = 3,
    Cancelled = 4,
}

struct Touch {
    object: Id,
    view: Id,
    /// A gesture recognizer took this touch: the view heard touchesCancelled and hears no more.
    taken: bool,
    /// The view heard touchesBegan (so a recognizer taking it cancels it).
    began_sent: bool,
}

static TOUCHES: Mutex<Option<HashMap<i32, Touch>>> = Mutex::new(None);

/// What recognizers hold back from a view (delaysTouchesBegan,
/// delaysTouchesEnded) until they fail or recognize, per finger.
struct Held {
    view: Id,
    object: Id,
    began_sent: bool,
    /// (selector, changed touches, event), in order.
    items: Vec<(&'static str, Id, Id)>,
}

static HELD: Mutex<Option<HashMap<i32, Held>>> = Mutex::new(None);

fn selector_for(phase: Phase) -> &'static str {
    match phase {
        Phase::Began => "touchesBegan:withEvent:",
        Phase::Moved => "touchesMoved:withEvent:",
        Phase::Ended => "touchesEnded:withEvent:",
        Phase::Cancelled => "touchesCancelled:withEvent:",
    }
}

fn mark_touch(finger: i32, f: impl FnOnce(&mut Touch)) {
    if let Some(touch) = TOUCHES.lock().unwrap().as_mut().and_then(|t| t.get_mut(&finger)) {
        f(touch);
    }
}

/// The recognizers holding a finger's touches decided: without `cancel`
/// the view hears all it was held back from, in order; with it (one
/// recognized) the view hears touchesCancelled if it had heard the touch
/// begin, else nothing at all, and nothing more of that touch.
pub(crate) fn release(finger: i32, cancel: bool) {
    let Some(held) = HELD.lock().unwrap().as_mut().and_then(|h| h.remove(&finger)) else { return };
    if cancel {
        mark_touch(finger, |t| t.taken = true);
        if held.began_sent {
            property_set(held.object, "phase", Phase::Cancelled as u64);
            let event = held.items.last().map_or_else(|| make_event(&[held.object], now()), |i| i.2);
            send_to_view(held.view, finger, "touchesCancelled:withEvent:", misc::set_from(&[held.object]), event);
        }
        return;
    }
    for (selector, changed, event) in held.items {
        if selector == "touchesBegan:withEvent:" {
            mark_touch(finger, |t| t.began_sent = true);
        }
        send_to_view(held.view, finger, selector, changed, event);
    }
}

/// A recognizer took a finger's touch outside a touch change (a long press
/// on its timer, one that waited for others to fail): the view's touch is
/// cancelled, as when it is taken during a change.
pub(crate) fn take_from_view(finger: i32) {
    if HELD.lock().unwrap().as_ref().is_some_and(|h| h.contains_key(&finger)) {
        release(finger, true);
        return;
    }
    let touch = TOUCHES.lock().unwrap().as_mut().and_then(|t| t.get_mut(&finger)).filter(|t| !t.taken).map(|t| {
        t.taken = true;
        (t.object, t.view, t.began_sent)
    });
    if let Some((object, view, true)) = touch {
        property_set(object, "phase", Phase::Cancelled as u64);
        send_to_view(view, finger, "touchesCancelled:withEvent:", misc::set_from(&[object]), make_event(&[object], now()));
    }
}

fn make_event(touches: &[Id], stamp: f64) -> Id {
    let event = runtime::alloc_instance(runtime::class_named("UIEvent"));
    property_set(event, "touches", misc::set_from(touches) as u64);
    property_set(event, "timestamp", stamp.to_bits());
    event
}

/// UITouchPhaseStationary.
const STATIONARY: u64 = 2;

fn now() -> f64 {
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) };
    t.tv_sec as f64 + t.tv_nsec as f64 / 1e9
}

fn set_point(object: Id, key: &'static str, point: Point) {
    property_set(object, key, point.x.to_bits());
    let second: &'static str = match key {
        "x" => "y",
        _ => "previousY",
    };
    property_set(object, second, point.y.to_bits());
}

fn point_of(object: Id, x: &'static str, y: &'static str) -> Point {
    Point { x: f64::from_bits(property_get(object, x).unwrap_or(0)), y: f64::from_bits(property_get(object, y).unwrap_or(0)) }
}

/// The view's top-left corner in window points.
pub(crate) fn origin_in_window(view: Id) -> Point {
    let mut point = Point::default();
    let mut current = view;
    for _ in 0..64 {
        if current == 0 {
            break;
        }
        let state = view_state(current);
        if state.superview == 0 {
            break;
        }
        point.x += state.frame.origin.x - state.bounds_origin.x;
        point.y += state.frame.origin.y - state.bounds_origin.y;
        current = state.superview;
    }
    point
}

/// How the view's own points map to window points: each view's bounds
/// origin, its transform around its anchor point (its center unless the
/// layer's anchor moved) and its place in its superview, up to the window.
pub(crate) fn view_to_window(view: Id) -> Transform {
    let mut t = IDENTITY;
    let mut current = view;
    for _ in 0..64 {
        if current == 0 {
            break;
        }
        let state = view_state(current);
        if state.superview == 0 {
            break;
        }
        let anchor = crate::fw::layers::view_anchor(current);
        let size = state.frame.size;
        let (ax, ay) = (size.width * anchor.x, size.height * anchor.y);
        let to_anchor = Transform { tx: -(state.bounds_origin.x + ax), ty: -(state.bounds_origin.y + ay), ..IDENTITY };
        let to_superview = Transform { tx: state.frame.origin.x + ax, ty: state.frame.origin.y + ay, ..IDENTITY };
        t = concat(concat(concat(t, to_anchor), state.transform), to_superview);
        current = state.superview;
    }
    t
}

/// What hit-testing needs to know about one view.
struct HitView {
    /// Hidden, alpha below 0.01 or user interaction off: UIKit skips the
    /// view and everything inside it.
    blocks: bool,
    /// A view we don't draw yet (fw/controls.rs): views inside it can still
    /// be touched, but it never catches a touch itself, so taps go on to
    /// the drawn views under it.
    see_through: bool,
    /// Top-left corner in window points.
    origin: Point,
    size: crate::fw::geometry::Size,
    is_window: bool,
    /// Back to front.
    subviews: Vec<Id>,
}

fn hit_view(view: Id) -> HitView {
    // As in UIKit, image views (and labels) ignore touches unless the game
    // turns them on; other views take them.
    let default = || {
        let plain = ["UIImageView", "UILabel"].iter().any(|name| {
            let class = runtime::class_named(name);
            class != 0 && runtime::send(view, "isKindOfClass:", &[class as Id]) & 0xFF != 0
        });
        !plain
    };
    let hidden = property_get(view, "hidden").unwrap_or(0) != 0;
    let alpha = property_get(view, "alpha").map(f64::from_bits).unwrap_or(1.0);
    // The GL picture is shown whatever the view's alpha (fw/eagl.rs), so a
    // see-through view holding it keeps its touches: what's seen is what's
    // touched. UIKit pictures (fw/pictures.rs) do skip see-through views.
    let invisible = alpha < 0.01 && !holds_gl_picture(view);
    let interaction = property_get(view, "userInteraction").map(|v| v != 0).unwrap_or_else(default);
    let state = view_state(view);
    HitView {
        blocks: hidden || invisible || !interaction,
        see_through: property_get(view, "notDrawn").is_some(),
        origin: origin_in_window(view),
        size: state.frame.size,
        is_window: state.superview == 0,
        subviews: state.subviews.clone(),
    }
}

/// Whether the view or one inside it shows a GL picture (a CAEAGLLayer).
fn holds_gl_picture(view: Id) -> bool {
    let gl_layer = runtime::class_named("CAEAGLLayer");
    fn search(view: Id, gl_layer: crate::objc::runtime::Class, depth: usize) -> bool {
        let state = view_state(view);
        let layer = state.layer;
        if layer != 0 && gl_layer != 0 && {
            let rt = lock();
            rt.is_subclass(rt.class_of(layer), gl_layer)
        } {
            return true;
        }
        depth < 64 && state.subviews.clone().into_iter().any(|child| search(child, gl_layer, depth + 1))
    }
    search(view, gl_layer, 0)
}

/// UIKit's hitTest:withEvent: from `view` down: the front-most, deepest view
/// under the point (window points), skipping views that block and views
/// outside their parent's bounds, as pointInside:withEvent: does.
fn hit_search(view: Id, point: Point, info: &dyn Fn(Id) -> HitView) -> Option<Id> {
    let node = info(view);
    if node.blocks {
        return None;
    }
    let local = Point { x: point.x - node.origin.x, y: point.y - node.origin.y };
    if !node.is_window && (local.x < 0.0 || local.y < 0.0 || local.x >= node.size.width || local.y >= node.size.height) {
        return None;
    }
    for &child in node.subviews.iter().rev() {
        if let Some(found) = hit_search(child, point, info) {
            return Some(found);
        }
    }
    if node.see_through {
        None
    } else {
        Some(view)
    }
}

fn hit_test(window: Id, point: Point) -> Id {
    hit_search(window, point, &hit_view).unwrap_or(window)
}

/// Where a UITouch is in a view's own points (-locationInView:).
pub fn location_in(touch: Id, view: Id) -> Point {
    touch_location_in_view(touch, 0, view)
}

extern "C" fn touch_location_in_view(this: Id, _: Sel, view: Id) -> Point {
    let at = point_of(this, "x", "y");
    if view == 0 {
        return at;
    }
    let origin = origin_in_window(view);
    Point { x: at.x - origin.x, y: at.y - origin.y }
}

extern "C" fn touch_previous_location_in_view(this: Id, _: Sel, view: Id) -> Point {
    let at = point_of(this, "previousX", "previousY");
    if view == 0 {
        return at;
    }
    let origin = origin_in_window(view);
    Point { x: at.x - origin.x, y: at.y - origin.y }
}

extern "C" fn touch_phase(this: Id, _: Sel) -> isize {
    property_get(this, "phase").unwrap_or(0) as isize
}

extern "C" fn touch_tap_count(this: Id, _: Sel) -> usize {
    property_get(this, "tapCount").unwrap_or(1) as usize
}

extern "C" fn touch_timestamp(this: Id, _: Sel) -> f64 {
    f64::from_bits(property_get(this, "timestamp").unwrap_or(0))
}

extern "C" fn touch_view(this: Id, _: Sel) -> Id {
    property_get(this, "view").unwrap_or(0) as Id
}

extern "C" fn touch_window(_this: Id, _: Sel) -> Id {
    key_window()
}

extern "C" fn event_all_touches(this: Id, _: Sel) -> Id {
    property_get(this, "touches").unwrap_or(0) as Id
}

extern "C" fn event_touches_for_view(this: Id, _: Sel, view: Id) -> Id {
    let all = event_all_touches(this, 0);
    let list: Vec<Id> = collections::array_items(runtime::send(all, "allObjects", &[]))
        .into_iter()
        .filter(|&t| property_get(t, "view").unwrap_or(0) as Id == view)
        .collect();
    misc::set_from(&list)
}

extern "C" fn event_timestamp(this: Id, _: Sel) -> f64 {
    f64::from_bits(property_get(this, "timestamp").unwrap_or(0))
}

extern "C" fn event_type(_this: Id, _: Sel) -> isize {
    0
}

/// Handles one finger change, on the main thread. `point` is in window points.
fn deliver_now(finger: i32, phase: Phase, point: Point) {
    deliver_in(key_window(), finger, phase, point);
}

/// The same for a given window.
pub(crate) fn deliver_in(window: Id, finger: i32, phase: Phase, point: Point) {
    if window == 0 {
        return;
    }
    let mut guard = TOUCHES.lock().unwrap();
    let touches = guard.get_or_insert_with(HashMap::new);
    let stamp = now();
    let touch = match (phase, touches.get(&finger)) {
        (Phase::Began, _) => {
            let object = runtime::alloc_instance(runtime::class_named("UITouch"));
            let view = hit_test(window, point);
            property_set(object, "view", view as u64);
            property_set(object, "tapCount", 1);
            set_point(object, "previousX", point);
            touches.insert(finger, Touch { object, view, taken: false, began_sent: false });
            touches.get(&finger).unwrap()
        }
        (_, Some(existing)) => existing,
        (_, None) => return,
    };
    let (object, view, taken, began_sent) = (touch.object, touch.view, touch.taken, touch.began_sent);
    if phase != Phase::Began {
        let previous = point_of(object, "x", "y");
        set_point(object, "previousX", previous);
    }
    set_point(object, "x", point);
    property_set(object, "phase", phase as u64);
    property_set(object, "timestamp", stamp.to_bits());
    // Fingers that didn't change in this event are "stationary", as on iPhone.
    for other in touches.values().filter(|t| t.object != object) {
        property_set(other.object, "phase", STATIONARY);
    }
    let active: Vec<Id> = touches.values().map(|t| t.object).collect();
    if matches!(phase, Phase::Ended | Phase::Cancelled) {
        touches.remove(&finger);
    }
    drop(guard);

    let event = make_event(&active, stamp);
    let changed = misc::set_from(&[object]);
    // Gesture recognizers on the view and the views it is in see the touch
    // first (and may take it, or hold it back from the view for now).
    let verdict = crate::fw::gestures::touch_changed(view, object, finger, phase, point);
    if taken {
        return;
    }
    match verdict {
        crate::fw::gestures::Verdict::Took => {
            mark_touch(finger, |t| t.taken = true);
            if began_sent {
                property_set(object, "phase", Phase::Cancelled as u64);
                send_to_view(view, finger, "touchesCancelled:withEvent:", changed, event);
            }
        }
        crate::fw::gestures::Verdict::Hold => {
            let mut held = HELD.lock().unwrap();
            let entry = held.get_or_insert_with(HashMap::new).entry(finger).or_insert(Held { view, object, began_sent, items: Vec::new() });
            entry.items.push((selector_for(phase), changed, event));
        }
        crate::fw::gestures::Verdict::Pass => {
            if phase == Phase::Began {
                mark_touch(finger, |t| t.began_sent = true);
            }
            send_to_view(view, finger, selector_for(phase), changed, event);
        }
    }
}

/// Sends a touch message for the view a finger touched, logging the first few.
fn send_to_view(view: Id, finger: i32, selector: &'static str, changed: Id, event: Id) {
    let point = runtime::send(changed, "anyObject", &[]);
    let point = if point == 0 { Point::default() } else { point_of(point, "x", "y") };
    // Like UIKit's responder chain: the touched view, else the nearest
    // view above it that handles touches.
    let mut handler = view;
    while handler != 0 && !runtime::responds(handler, selector) {
        handler = view_state(handler).superview;
    }
    static SHOWN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    // Tests see every touch; otherwise only the first few are logged.
    if SHOWN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 12 || TAPS_ARMED.load(std::sync::atomic::Ordering::Relaxed) {
        crate::log!(
            "touch {finger} {selector} at {:.0},{:.0}: view {}, handled by {}",
            point.x,
            point.y,
            crate::fw::string::describe_class_of(view),
            if handler == 0 { "nobody".to_string() } else { crate::fw::string::describe_class_of(handler) }
        );
    }
    if handler != 0 {
        runtime::send(handler, selector, &[changed, event]);
    }
}

/// From the phone app: a finger in window pixels, `width` x `height` being the window size.
/// The picture is letterboxed (see `eagl::picture_area`), so window pixels
/// are mapped through the picture's rectangle, not the whole window.
pub fn deliver(finger: i32, phase: Phase, x: f32, y: f32, width: f32, height: f32) {
    let point = window_to_points(x as f64, y as f64, width as i32, height as i32);
    crate::fw::runloop::post(crate::fw::runloop::Work::Native(Box::new(move || deliver_now(finger, phase, point))));
}

/// A window pixel (top-left origin) to iPhone screen points; places in the
/// black bars go to the nearest edge of the picture.
pub fn window_to_points(x: f64, y: f64, width: i32, height: i32) -> Point {
    let (points_wide, points_high, _) = crate::fw::uikit::screen_points();
    let (x0, y0, x1, y1) = crate::fw::eagl::picture_area(width.max(1), height.max(1));
    // picture_area is centred, so its bottom margin equals its top margin.
    let across = ((x - x0 as f64) / (x1 - x0).max(1) as f64).clamp(0.0, 1.0);
    let down = ((y - y0 as f64) / (y1 - y0).max(1) as f64).clamp(0.0, 1.0);
    Point { x: across * points_wide, y: down * points_high }
}

/// When a test tap goes, one step after another.
#[derive(Clone, Debug, PartialEq)]
enum Trigger {
    /// The moment a log line holds this (a plain part of the line, not a pattern).
    Line(String),
    /// At the first drawn screen where something at the tap's place takes
    /// touches (a button that shows up a while after its screen).
    Ready,
}

#[derive(Clone, Debug, PartialEq)]
struct TestTap {
    on: Trigger,
    from: Point,
    to: Point,
    /// `jump=`: goes as soon as its line comes, dropping the steps still
    /// waiting before it (walks on random maps end at a screen any time).
    jump: bool,
}

/// IOS_TAPS="on=screen shown: IntroViewController=>505,29;ready=>284,297;on=frame 1 shown=>420,250>460,250":
/// each tap goes out the moment a log line holds the text after `on=`
/// (`after=` means the same), or with `ready` once the drawn screen has
/// something at that place to take it; nothing waits or counts seconds. A
/// swipe goes from x,y to x2,y2 over about 150 ms. A `jump=` step goes the
/// moment its line comes even while steps before it still wait (those are
/// dropped). A step that isn't whole is left out.
fn parse_test_taps(text: &str) -> Vec<TestTap> {
    let point = |text: &str| -> Option<Point> {
        let (x, y) = text.split_once(',')?;
        Some(Point { x: x.trim().parse().ok()?, y: y.trim().parse().ok()? })
    };
    text.split(';')
        .filter_map(|step| {
            let (when, place) = step.split_once("=>")?;
            let when = when.trim();
            let jump = when.starts_with("jump=");
            let on = match when {
                "ready" => Trigger::Ready,
                _ => Trigger::Line(
                    when.strip_prefix("on=").or_else(|| when.strip_prefix("after=")).or_else(|| when.strip_prefix("jump=")).filter(|text| !text.is_empty())?.to_string(),
                ),
            };
            let (from, to) = match place.trim().split_once('>') {
                Some((from, to)) => (point(from)?, point(to)?),
                None => (point(place.trim())?, point(place.trim())?),
            };
            Some(TestTap { on, from, to, jump })
        })
        .collect()
}

/// The test taps still to come, in order, and who sends them.
static TEST_TAPS: std::sync::Mutex<std::collections::VecDeque<TestTap>> = std::sync::Mutex::new(std::collections::VecDeque::new());
static TEST_TAP_SENDER: std::sync::Mutex<fn(TestTap)> = std::sync::Mutex::new(send_test_tap);
static TAPS_ARMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static TEST_FINGER: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// IOS_TAPS makes the test taps on the PC (the phone hands touches over its
/// own way, see `deliver`).
pub fn schedule_test_taps() {
    let Ok(text) = std::env::var("IOS_TAPS") else { return };
    let taps = parse_test_taps(&text);
    crate::log!("test taps: {} steps, each sent the moment its line is logged", taps.len());
    arm_test_taps(taps, send_test_tap);
}

fn arm_test_taps(taps: Vec<TestTap>, sender: fn(TestTap)) {
    *TEST_TAP_SENDER.lock().unwrap_or_else(|e| e.into_inner()) = sender;
    *TEST_TAPS.lock().unwrap_or_else(|e| e.into_inner()) = taps.into();
    TAPS_ARMED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Every log line comes here (see `report::line`): if it is the next test
/// tap's line, the tap is sent right now, from inside the log call.
pub fn line_logged(text: &str) {
    if !TAPS_ARMED.load(std::sync::atomic::Ordering::Relaxed) || text.starts_with("test tap") || text.starts_with("test swipe") {
        return;
    }
    let tap = next_test_tap(&mut TEST_TAPS.lock().unwrap_or_else(|e| e.into_inner()), text);
    if let Some(tap) = tap {
        let sender = *TEST_TAP_SENDER.lock().unwrap_or_else(|e| e.into_inner());
        sender(tap);
    }
}

/// The step this log line sends: the next one if its line is this, else
/// the first `jump=` step whose line it is (the steps before it go away).
fn next_test_tap(taps: &mut std::collections::VecDeque<TestTap>, text: &str) -> Option<TestTap> {
    let at = taps.iter().enumerate().position(|(i, tap)| (i == 0 || tap.jump) && matches!(&tap.on, Trigger::Line(line) if text.contains(line.as_str())))?;
    taps.drain(..at);
    taps.pop_front()
}

/// From the main loop each time the drawn screen has settled (see
/// `pictures::tick`): sends the next tap if it is a `ready` one and
/// something there takes touches now.
pub fn screen_settled() {
    screen_settled_with(|at| {
        let window = key_window();
        let mut handler = if window == 0 { 0 } else { hit_test(window, at) };
        while handler != 0 && !runtime::responds(handler, "touchesBegan:withEvent:") {
            handler = view_state(handler).superview;
        }
        handler != 0
    });
}

fn screen_settled_with(takes_touches: impl Fn(Point) -> bool) {
    if !TAPS_ARMED.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let tap = {
        let mut taps = TEST_TAPS.lock().unwrap_or_else(|e| e.into_inner());
        match taps.front() {
            Some(next) if next.on == Trigger::Ready && takes_touches(next.from) => taps.pop_front(),
            _ => None,
        }
    };
    if let Some(tap) = tap {
        let sender = *TEST_TAP_SENDER.lock().unwrap_or_else(|e| e.into_inner());
        sender(tap);
    }
}

/// Like a finger: a tap is held 80 ms (games that walk while the finger is
/// down, such as cocos2d maps, ignore a press and release in one go); a
/// swipe's moves come 25 ms apart.
fn send_test_tap(tap: TestTap) {
    let (from, to) = (tap.from, tap.to);
    let swipe = from.x != to.x || from.y != to.y;
    let why = match &tap.on {
        Trigger::Line(line) => format!("on the line \"{line}\""),
        Trigger::Ready => "something there takes it now".to_string(),
    };
    crate::log!("test {} at {:.0},{:.0} ({why})", if swipe { "swipe" } else { "tap" }, from.x, from.y);
    let finger = TEST_FINGER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let send = move |phase: Phase, at: Point| {
        crate::fw::runloop::post(crate::fw::runloop::Work::Native(Box::new(move || deliver_now(finger, phase, at))));
    };
    let ended = move || {
        crate::fw::runloop::post(crate::fw::runloop::Work::Native(Box::new(move || {
            deliver_now(finger, Phase::Ended, to);
            // The next tap can go on this line: the screen after this one.
            crate::fw::pictures::on_next_draw(format!("screen drawn after tap at {:.0},{:.0}", to.x, to.y));
        })));
    };
    send(Phase::Began, from);
    std::thread::spawn(move || {
        if !swipe {
            std::thread::sleep(std::time::Duration::from_millis(80));
            ended();
            return;
        }
        for step in 1..=6 {
            std::thread::sleep(std::time::Duration::from_millis(25));
            let f = step as f64 / 6.0;
            send(Phase::Moved, Point { x: from.x + (to.x - from.x) * f, y: from.y + (to.y - from.y) * f });
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
        ended();
    });
}

pub fn register(_: &mut Registry) {
    let touch = define_class("UITouch", "NSObject", 8);
    methods(
        touch,
        &[
            ("locationInView:", touch_location_in_view as usize),
            ("previousLocationInView:", touch_previous_location_in_view as usize),
            ("phase", touch_phase as usize),
            ("tapCount", touch_tap_count as usize),
            ("timestamp", touch_timestamp as usize),
            ("view", touch_view as usize),
            ("window", touch_window as usize),
        ],
    );
    let event = define_class("UIEvent", "NSObject", 8);
    methods(
        event,
        &[
            ("allTouches", event_all_touches as usize),
            ("touchesForView:", event_touches_for_view as usize),
            ("touchesForWindow:", event_all_touches as usize),
            ("timestamp", event_timestamp as usize),
            ("type", event_type as usize),
        ],
    );
    let _ = lock;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touches_reach_views_inside_and_under_undrawn_views() {
        use crate::fw::geometry::Size;
        // Window 1 holds the game view 2 (full screen) and, in front, an
        // undrawn scroll view 3 (0,0 200x200) holding a drawn view 4
        // (10,10 50x50). View 5 sits in front of everything but is hidden.
        let node = |id: Id| {
            let (x, y, w, h, subviews, see_through, blocks) = match id {
                1 => (0.0, 0.0, 568.0, 320.0, vec![2, 3, 5], false, false),
                2 => (0.0, 0.0, 568.0, 320.0, vec![], false, false),
                3 => (0.0, 0.0, 200.0, 200.0, vec![4], true, false),
                4 => (10.0, 10.0, 50.0, 50.0, vec![], false, false),
                _ => (0.0, 0.0, 568.0, 320.0, vec![], false, true),
            };
            HitView { blocks, see_through, origin: Point { x, y }, size: Size { width: w, height: h }, is_window: id == 1, subviews }
        };
        let at = |x, y| hit_search(1, Point { x, y }, &node);
        assert_eq!(at(20.0, 20.0), Some(4), "drawn view inside the undrawn scroll view");
        assert_eq!(at(100.0, 100.0), Some(2), "undrawn scroll view lets the tap through");
        assert_eq!(at(300.0, 100.0), Some(2));
    }

    /// What the touch test's views and targets were told, in order.
    static HEARD: Mutex<Vec<String>> = Mutex::new(Vec::new());

    fn heard() -> Vec<String> {
        std::mem::take(&mut *HEARD.lock().unwrap())
    }

    /// What the gesture test's view heard, in order.
    static GESTURE_HEARD: Mutex<Vec<String>> = Mutex::new(Vec::new());

    /// The view's touches around recognizers, as UIKit holds and cancels
    /// them: a long press recognized on its timer cancels the view's touch;
    /// with delaysTouchesBegan the view hears nothing until the recognizer
    /// fails (then all of it, in order) and nothing at all if it recognizes;
    /// with delaysTouchesEnded (the default) the end waits while a double tap
    /// could still come.
    #[test]
    fn recognizers_hold_back_and_cancel_the_views_touches() {
        use crate::fw::geometry::Rect;
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        extern "C" fn note(_this: Id, s: Sel, _: Id, _: Id) {
            GESTURE_HEARD.lock().unwrap().push(lock().sel_name(s).trim_end_matches(":withEvent:").to_string());
        }
        let heard = || std::mem::take(&mut *GESTURE_HEARD.lock().unwrap());
        let class = define_class("TouchTestGestureView", "UIView", 8);
        let touches = ["touchesBegan:withEvent:", "touchesMoved:withEvent:", "touchesEnded:withEvent:", "touchesCancelled:withEvent:"];
        methods(class, &touches.map(|name| (name, note as usize)));
        let new_view = |class: &str, frame: Rect| {
            let object = runtime::send(runtime::class_named(class), "alloc", &[]);
            crate::fw::uikit::send_rect(object, "initWithFrame:", frame);
            object
        };
        let window = new_view("UIWindow", Rect::new(0.0, 0.0, 568.0, 320.0));
        let view = new_view("TouchTestGestureView", Rect::new(0.0, 0.0, 568.0, 320.0));
        runtime::send(window, "addSubview:", &[view]);
        let finger = 710; // not a finger other tests use
        // As main-loop work: other tests' main loops may let held touches go too.
        let touch = |phase: Phase, x: f64| crate::fw::runloop::as_main(|| deliver_in(window, finger, phase, Point { x, y: 100.0 }));
        let recognizer = |class: &str| {
            let object = runtime::send(runtime::send(runtime::class_named(class), "alloc", &[]), "init", &[]);
            runtime::send(view, "addGestureRecognizer:", &[object]);
            object
        };

        let press = recognizer("UILongPressGestureRecognizer");
        touch(Phase::Began, 100.0);
        crate::fw::runloop::as_main(|| crate::fw::gestures::held(press, finger));
        touch(Phase::Ended, 100.0);
        assert_eq!(heard(), ["touchesBegan", "touchesCancelled"], "the long press took the touch");
        runtime::send(view, "removeGestureRecognizer:", &[press]);

        let pan = recognizer("UIPanGestureRecognizer");
        runtime::send(pan, "setDelaysTouchesBegan:", &[1]);
        touch(Phase::Began, 100.0);
        touch(Phase::Moved, 103.0);
        assert!(heard().is_empty(), "held while the pan could still begin");
        touch(Phase::Ended, 103.0);
        assert_eq!(heard(), ["touchesBegan", "touchesMoved", "touchesEnded"], "the pan failed: all of it, in order");
        touch(Phase::Began, 100.0);
        touch(Phase::Moved, 150.0);
        touch(Phase::Ended, 150.0);
        assert!(heard().is_empty(), "the pan took it before the view heard anything");
        runtime::send(view, "removeGestureRecognizer:", &[pan]);

        let double = recognizer("UITapGestureRecognizer");
        runtime::send(double, "setNumberOfTapsRequired:", &[2]);
        touch(Phase::Began, 100.0);
        touch(Phase::Ended, 100.0);
        assert_eq!(heard(), ["touchesBegan"], "the end waits for a second tap");
        crate::fw::runloop::as_main(|| crate::fw::gestures::tap_gap_passed(double));
        assert_eq!(heard(), ["touchesEnded"], "no second tap: the end comes");
        runtime::send(view, "removeGestureRecognizer:", &[double]);
    }

    /// Controls follow a finger as UIKit's do: pressed (highlighted) while
    /// it is on them, their targets told TouchDown, then TouchUpInside or
    /// (let go far away) TouchUpOutside, or TouchCancel; the game's control
    /// classes hear begin/continue/endTracking. Disabled, hidden, untouchable
    /// controls and places beside them leave the touch to the game's view.
    #[test]
    fn controls_track_the_finger_and_send_their_actions() {
        use crate::fw::geometry::Rect;
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        extern "C" fn note(this: Id, s: Sel, _: Id, event: Id) {
            let name = lock().sel_name(s);
            let class = crate::fw::string::describe_class_of(this);
            assert!(event != 0 || name.starts_with("touches"), "{name}: no event");
            HEARD.lock().unwrap().push(format!("{class} {name}"));
        }
        extern "C" fn tracking(this: Id, s: Sel, _: Id, _: Id) -> bool {
            HEARD.lock().unwrap().push(format!("{} {}", crate::fw::string::describe_class_of(this), lock().sel_name(s)));
            true
        }
        let game_view = define_class("TouchTestGameView", "UIView", 8);
        let touches = ["touchesBegan:withEvent:", "touchesMoved:withEvent:", "touchesEnded:withEvent:", "touchesCancelled:withEvent:"];
        methods(game_view, &touches.map(|name| (name, note as usize)));
        let target = define_class("TouchTestTarget", "NSObject", 8);
        methods(target, &[("down:event:", note as usize), ("upInside:event:", note as usize), ("upOutside:event:", note as usize), ("cancelled:event:", note as usize)]);
        let game_control = define_class("TouchTestControl", "UIControl", 8);
        methods(
            game_control,
            &[
                ("beginTrackingWithTouch:withEvent:", tracking as usize),
                ("continueTrackingWithTouch:withEvent:", tracking as usize),
                ("endTrackingWithTouch:withEvent:", note as usize),
            ],
        );
        let new_view = |class: &str, frame: Rect| {
            let object = runtime::send(runtime::class_named(class), "alloc", &[]);
            let s = runtime::sel("initWithFrame:");
            let init: extern "C" fn(Id, Sel, Rect) -> Id = unsafe { std::mem::transmute(runtime::imp_for(object, s).unwrap()) };
            init(object, s, frame)
        };
        let window = new_view("UIWindow", Rect::new(0.0, 0.0, 568.0, 320.0));
        let game = new_view("TouchTestGameView", Rect::new(0.0, 0.0, 568.0, 320.0));
        runtime::send(window, "addSubview:", &[game]);
        let button = runtime::send(runtime::class_named("UIButton"), "buttonWithType:", &[0]);
        runtime::send(game, "addSubview:", &[button]);
        let s = runtime::sel("setFrame:");
        let set_frame: extern "C" fn(Id, Sel, Rect) = unsafe { std::mem::transmute(runtime::imp_for(button, s).unwrap()) };
        set_frame(button, s, Rect::new(100.0, 100.0, 80.0, 40.0));
        let listener = runtime::send(runtime::send(target, "alloc", &[]), "init", &[]);
        for (name, events) in [("down:event:", 1usize), ("upInside:event:", 1 << 6), ("upOutside:event:", 1 << 7), ("cancelled:event:", 1 << 8)] {
            runtime::send(button, "addTarget:action:forControlEvents:", &[listener, runtime::sel(name), events]);
        }
        let finger = 700; // not a finger other tests use
        let touch = |phase: Phase, x: f64, y: f64| deliver_in(window, finger, phase, Point { x, y });
        let highlighted = || runtime::send(button, "isHighlighted", &[]) & 0xFF != 0;
        let tap = |x: f64, y: f64| {
            touch(Phase::Began, x, y);
            touch(Phase::Ended, x, y);
        };
        let up_inside = vec!["TouchTestTarget down:event:".to_string(), "TouchTestTarget upInside:event:".to_string()];

        // A tap: pressed while held, then down and up inside.
        touch(Phase::Began, 120.0, 110.0);
        assert!(highlighted(), "pressed while the finger is on it");
        touch(Phase::Ended, 120.0, 110.0);
        assert!(!highlighted());
        assert_eq!(heard(), up_inside);
        // A little way off (inside UIKit's 70-point margin) still counts as inside.
        touch(Phase::Began, 120.0, 110.0);
        touch(Phase::Moved, 200.0, 120.0);
        assert!(highlighted());
        touch(Phase::Ended, 200.0, 120.0);
        assert_eq!(heard(), up_inside);
        // Far away: no longer pressed, and let go outside.
        touch(Phase::Began, 120.0, 110.0);
        touch(Phase::Moved, 450.0, 300.0);
        assert!(!highlighted(), "not pressed once the finger left");
        touch(Phase::Ended, 450.0, 300.0);
        assert_eq!(heard(), ["TouchTestTarget down:event:", "TouchTestTarget upOutside:event:"]);
        touch(Phase::Began, 120.0, 110.0);
        touch(Phase::Cancelled, 120.0, 110.0);
        assert_eq!(heard(), ["TouchTestTarget down:event:", "TouchTestTarget cancelled:event:"]);
        assert!(!highlighted());
        // Beside it: the game's view has the touch.
        tap(20.0, 20.0);
        assert_eq!(heard(), ["TouchTestGameView touchesBegan:withEvent:", "TouchTestGameView touchesEnded:withEvent:"]);
        // Disabled: no actions, the touch goes on to the view it is in.
        runtime::send(button, "setEnabled:", &[0]);
        tap(120.0, 110.0);
        assert!(!highlighted());
        assert_eq!(heard(), ["TouchTestGameView touchesBegan:withEvent:", "TouchTestGameView touchesEnded:withEvent:"]);
        runtime::send(button, "setEnabled:", &[1]);
        // Hidden or untouchable: as if it weren't there.
        for (setter, value) in [("setHidden:", 1), ("setUserInteractionEnabled:", 0)] {
            runtime::send(button, setter, &[value]);
            tap(120.0, 110.0);
            assert_eq!(heard(), ["TouchTestGameView touchesBegan:withEvent:", "TouchTestGameView touchesEnded:withEvent:"], "{setter}");
            runtime::send(button, setter, &[1 - value]);
        }
        tap(120.0, 110.0);
        assert_eq!(heard(), up_inside, "back to normal");
        // A game's own control hears the tracking calls.
        let own = new_view("TouchTestControl", Rect::new(300.0, 100.0, 50.0, 50.0));
        runtime::send(game, "addSubview:", &[own]);
        touch(Phase::Began, 310.0, 110.0);
        touch(Phase::Moved, 312.0, 112.0);
        touch(Phase::Ended, 312.0, 112.0);
        assert_eq!(
            heard(),
            [
                "TouchTestControl beginTrackingWithTouch:withEvent:",
                "TouchTestControl continueTrackingWithTouch:withEvent:",
                "TouchTestControl endTrackingWithTouch:withEvent:"
            ]
        );
        // A control that refuses tracking (beginTracking NO) sends no actions;
        // one that stops it (continueTracking NO) sends none after that.
        extern "C" fn refuse(this: Id, s: Sel, touch: Id, event: Id) -> bool {
            tracking(this, s, touch, event);
            false
        }
        let refusing = define_class("TouchTestRefusingControl", "UIControl", 8);
        methods(refusing, &[("beginTrackingWithTouch:withEvent:", refuse as usize), ("endTrackingWithTouch:withEvent:", note as usize)]);
        let stopping = define_class("TouchTestStoppingControl", "UIControl", 8);
        methods(
            stopping,
            &[
                ("beginTrackingWithTouch:withEvent:", tracking as usize),
                ("continueTrackingWithTouch:withEvent:", refuse as usize),
                ("endTrackingWithTouch:withEvent:", note as usize),
            ],
        );
        for (class, x) in [("TouchTestRefusingControl", 400.0), ("TouchTestStoppingControl", 470.0)] {
            let control = new_view(class, Rect::new(x, 100.0, 50.0, 50.0));
            runtime::send(game, "addSubview:", &[control]);
            for (name, events) in [("down:event:", 1usize), ("upInside:event:", 1 << 6), ("upOutside:event:", 1 << 7)] {
                runtime::send(control, "addTarget:action:forControlEvents:", &[listener, runtime::sel(name), events]);
            }
            touch(Phase::Began, x + 10.0, 110.0);
            touch(Phase::Moved, x + 12.0, 112.0);
            touch(Phase::Ended, x + 12.0, 112.0);
        }
        assert_eq!(
            heard(),
            [
                "TouchTestRefusingControl beginTrackingWithTouch:withEvent:",
                "TouchTestStoppingControl beginTrackingWithTouch:withEvent:",
                "TouchTestTarget down:event:",
                "TouchTestStoppingControl continueTrackingWithTouch:withEvent:",
            ]
        );
    }

    #[test]
    fn window_pixels_map_through_the_letterbox() {
        let (wide, high, _) = crate::fw::uikit::screen_points();
        let (x0, _, x1, _) = crate::fw::eagl::picture_area(2400, 1080);
        let centre = window_to_points(1200.0, 540.0, 2400, 1080);
        assert!((centre.x - wide / 2.0).abs() < 0.5 && (centre.y - high / 2.0).abs() < 0.5);
        let left = window_to_points(x0 as f64, 0.0, 2400, 1080);
        assert!(left.x.abs() < 0.01 && left.y.abs() < 0.01);
        let bar = window_to_points(0.0, 1079.0, 2400, 1080);
        assert_eq!(bar.x, 0.0);
        assert!((bar.y - high).abs() < 1.0);
        let right = window_to_points(x1 as f64, 1080.0, 2400, 1080);
        assert!((right.x - wide).abs() < 0.01 && (right.y - high).abs() < 0.01);
    }

    #[test]
    fn test_taps_name_the_line_they_tap_on() {
        let taps = parse_test_taps("on=screen shown: IntroViewController=>505,29;after=Mainmenu.jpg=>283,211;on=-[TileMapLayer init]=>389,297>400,300");
        let at = |x: f64, y: f64| Point { x, y };
        assert_eq!(taps.len(), 3, "three steps: {taps:?}");
        assert_eq!(taps[0].on, Trigger::Line("screen shown: IntroViewController".to_string()));
        assert_eq!((taps[0].from, taps[0].to), (at(505.0, 29.0), at(505.0, 29.0)));
        assert_eq!(taps[1].on, Trigger::Line("Mainmenu.jpg".to_string()), "after= still means the same");
        assert_eq!(taps[2].on, Trigger::Line("-[TileMapLayer init]".to_string()));
        assert_eq!((taps[2].from, taps[2].to), (at(389.0, 297.0), at(400.0, 300.0)), "a swipe still goes from one place to another");
    }

    #[test]
    fn test_tap_steps_that_make_no_sense_are_left_out() {
        // Fixed-time steps ("30:3,4") are gone: a tap only comes from a line.
        let taps = parse_test_taps("after=a line with no place;soon:1,2;30:3,4;on=ready=>;on=go=>3,4");
        assert_eq!(taps.len(), 1, "only the last step is whole: {taps:?}");
        assert_eq!(taps[0].on, Trigger::Line("go".to_string()));
    }

    /// The tests that arm the one shared list of test taps take turns.
    static ARMED_TAPS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn a_ready_tap_goes_once_the_drawn_screen_has_something_there_to_take_it() {
        let _turn = ARMED_TAPS.lock().unwrap_or_else(|e| e.into_inner());
        let taps = parse_test_taps("ready=>505,29;on=after=>1,2");
        assert_eq!(taps.len(), 2, "{taps:?}");
        assert_eq!(taps[0].on, Trigger::Ready);
        static SENT: std::sync::Mutex<Vec<(f64, f64)>> = std::sync::Mutex::new(Vec::new());
        fn record(tap: TestTap) {
            SENT.lock().unwrap().push((tap.from.x, tap.from.y));
        }
        let sent = || SENT.lock().unwrap().clone();
        arm_test_taps(taps, record);
        crate::log!("after (a line step waits its turn behind the ready step)");
        assert_eq!(sent(), []);
        screen_settled_with(|_| false);
        assert_eq!(sent(), [], "nothing at 505,29 takes a tap yet");
        screen_settled_with(|at| at.x == 505.0 && at.y == 29.0);
        assert_eq!(sent(), [(505.0, 29.0)], "sent at the first drawn screen with a taker there");
    }

    #[test]
    fn a_test_tap_is_sent_the_moment_its_line_is_logged() {
        let _turn = ARMED_TAPS.lock().unwrap_or_else(|e| e.into_inner());
        static SENT: std::sync::Mutex<Vec<(f64, f64)>> = std::sync::Mutex::new(Vec::new());
        fn record(tap: TestTap) {
            SENT.lock().unwrap().push((tap.from.x, tap.from.y));
        }
        let sent = || SENT.lock().unwrap().clone();
        arm_test_taps(parse_test_taps("on=tap test: screen one=>10,20;on=tap test: screen two=>30,40"), record);
        crate::log!("tap test: screen two (too early: the first step comes first)");
        assert_eq!(sent(), [], "steps go in order");
        crate::log!("tap test: screen one");
        assert_eq!(sent(), [(10.0, 20.0)], "the tap went out inside the log call, nothing waited");
        crate::log!("tap test: screen two");
        assert_eq!(sent(), [(10.0, 20.0), (30.0, 40.0)]);
    }

    #[test]
    fn a_jump_step_skips_the_steps_still_waiting_before_it() {
        // Walking on a random map: steps repeat on a line that keeps coming,
        // and the fight screen may open after any of them.
        let mut taps: std::collections::VecDeque<TestTap> = parse_test_taps("on=saved=>1,1;on=saved=>2,2;on=saved=>3,3;jump=fight shown=>9,9;on=next=>5,5").into();
        let from = |tap: Option<TestTap>| tap.map(|t| t.from.x);
        assert_eq!(from(next_test_tap(&mut taps, "frame saved")), Some(1.0));
        assert_eq!(from(next_test_tap(&mut taps, "next")), None, "a plain step still waits its turn");
        assert_eq!(from(next_test_tap(&mut taps, "fight shown")), Some(9.0), "the jump step goes now");
        assert_eq!(from(next_test_tap(&mut taps, "frame saved")), None, "the walking steps before it are dropped");
        assert_eq!(from(next_test_tap(&mut taps, "next")), Some(5.0));
        assert!(taps.is_empty());
    }
}
