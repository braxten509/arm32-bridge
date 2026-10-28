//! Touches: UITouch and UIEvent, delivered to the view under the finger
//! (touchesBegan/Moved/Ended/Cancelled:withEvent:) on the main thread.
//! The phone app hands touches over in window pixels; IOS_TAPS lets PC
//! tests tap too ("seconds:x,y;seconds:x,y" in points).

use crate::fw::geometry::{concat, invert, Point, Transform, IDENTITY};
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
    /// Held back from the view by an enclosing scroll view (delaysContentTouches)
    /// for its first 0.15 s: what the view hasn't heard yet, in order.
    delayed: Option<Vec<Message>>,
    /// A second finger on a view that takes one at a time (multipleTouchEnabled
    /// off): nobody hears it.
    ignored: bool,
    /// Which touch this is (finger numbers are used again), for timers
    /// set for it.
    seq: u64,
}

static NEXT_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Which touch a finger has now (tests fire its timers by hand).
#[cfg(test)]
pub(crate) fn touch_seq(finger: i32) -> u64 {
    TOUCHES.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|t| t.get(&finger)).map_or(0, |t| t.seq)
}

/// How long a scroll view holds a new touch back from its content
/// (-[UIScrollView _touchDelayForScrollDetection]).
const CONTENT_DELAY: f64 = 0.15;

static TOUCHES: Mutex<Option<HashMap<i32, Touch>>> = Mutex::new(None);

/// What recognizers hold back from a view (delaysTouchesBegan,
/// delaysTouchesEnded) until they fail or recognize, per finger.
struct Held {
    view: Id,
    object: Id,
    began_sent: bool,
    /// In order.
    items: Vec<Message>,
}

static HELD: Mutex<Option<HashMap<i32, Held>>> = Mutex::new(None);

/// A touch message a view hasn't heard yet, with the touch and the event as
/// they were then: heard later, the view still sees the phase, places and
/// time it had, and the fingers that were down.
#[derive(Clone, Copy)]
struct Message {
    selector: &'static str,
    changed: Id,
    event: Id,
    state: Snapshot,
}

/// A touch and its event as they were at one moment.
#[derive(Clone, Copy)]
struct Snapshot {
    phase: u64,
    at: Point,
    previous: Point,
    stamp: u64,
    /// The event's touches and timestamp.
    all: u64,
    event_stamp: u64,
}

impl Snapshot {
    fn take(object: Id, event: Id) -> Snapshot {
        Snapshot {
            phase: property_get(object, "phase").unwrap_or(0),
            at: point_of(object, "x", "y"),
            previous: point_of(object, "previousX", "previousY"),
            stamp: property_get(object, "timestamp").unwrap_or(0),
            all: property_get(event, "touches").unwrap_or(0),
            event_stamp: property_get(event, "timestamp").unwrap_or(0),
        }
    }

    fn put_back(self, object: Id, event: Id) {
        property_set(object, "phase", self.phase);
        set_point(object, "x", self.at);
        set_point(object, "previousX", self.previous);
        property_set(object, "timestamp", self.stamp);
        property_set(event, "touches", self.all);
        property_set(event, "timestamp", self.event_stamp);
    }
}

impl Message {
    /// The message for a change of `object`'s touch, as it is now.
    fn now(object: Id, phase: Phase, changed: Id, event: Id) -> Message {
        let mut state = Snapshot::take(object, event);
        state.phase = phase as u64;
        Message { selector: selector_for(phase), changed, event, state }
    }
}

/// Sends a message the view was held back from, with the touch and event
/// put back as they were for it (and then as they are now again).
fn replay(view: Id, finger: i32, object: Id, message: Message) {
    let now = Snapshot::take(object, message.event);
    message.state.put_back(object, message.event);
    send_to_view(view, finger, message.selector, message.changed, message.event);
    now.put_back(object, message.event);
}

fn selector_for(phase: Phase) -> &'static str {
    match phase {
        Phase::Began => "touchesBegan:withEvent:",
        Phase::Moved => "touchesMoved:withEvent:",
        Phase::Ended => "touchesEnded:withEvent:",
        Phase::Cancelled => "touchesCancelled:withEvent:",
    }
}

/// Whether a finger is on the screen.
pub fn fingers_down() -> bool {
    TOUCHES.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|t| !t.is_empty())
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
            let event = held.items.last().map_or_else(|| make_event(&[held.object], now()), |i| i.event);
            send_to_view(held.view, finger, "touchesCancelled:withEvent:", misc::set_from(&[held.object]), event);
        }
        return;
    }
    // The scroll view's 0.15 s still holds it: the content waits for that too.
    let mut items = Some(held.items);
    mark_touch(finger, |t| {
        if let Some(delayed) = t.delayed.as_mut() {
            delayed.extend(items.take().unwrap_or_default());
        }
    });
    for message in items.unwrap_or_default() {
        if message.selector == "touchesBegan:withEvent:" {
            mark_touch(finger, |t| t.began_sent = true);
        }
        replay(held.view, finger, held.object, message);
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

/// The one touches UIEvent, as UIKit's application keeps (each change updates it).
static EVENT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

fn make_event(touches: &[Id], stamp: f64) -> Id {
    let event = *EVENT.get_or_init(|| runtime::alloc_instance(runtime::class_named("UIEvent")) as usize) as Id;
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
#[cfg(test)]
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
    view_chain(view, false)
}

/// The same up to the screen: the top view's own place too (a window's
/// frame is in screen points), and an old app's window's turn (see
/// uikit::window_turn).
pub(crate) fn view_to_screen(view: Id) -> Transform {
    view_chain(view, true)
}

/// A window's (or other top view's) points to screen points.
pub(crate) fn window_to_screen(window: Id) -> Transform {
    let step = view_step(window);
    match crate::fw::uikit::window_turn() {
        Some(turn) if is_window(window) => concat(step, turn),
        _ => step,
    }
}

fn is_window(view: Id) -> bool {
    let class = runtime::class_named("UIWindow");
    class != 0 && runtime::send(view, "isKindOfClass:", &[class as Id]) & 0xFF != 0
}

/// A screen point to window points, through the window's turn if it has one.
pub(crate) fn screen_to_window_with(turn: Option<Transform>, point: Point) -> Point {
    match turn.and_then(invert) {
        Some(back) => {
            let (x, y) = crate::fw::geometry::apply(back, point.x, point.y);
            Point { x, y }
        }
        None => point,
    }
}

/// A window point in a view's own points (its bounds), through every
/// transform on the way (UIKit's convertPoint:fromView:nil).
pub(crate) fn point_in_view(view: Id, point: Point) -> Point {
    let back = invert(view_to_window(view)).unwrap_or(IDENTITY);
    let (x, y) = crate::fw::geometry::apply(back, point.x, point.y);
    Point { x, y }
}

/// One view's points to its superview's.
pub(crate) fn view_step(view: Id) -> Transform {
    let state = view_state(view);
    let anchor = crate::fw::layers::view_anchor(view);
    let size = state.frame.size;
    let (ax, ay) = (size.width * anchor.x, size.height * anchor.y);
    let to_anchor = Transform { tx: -(state.bounds_origin.x + ax), ty: -(state.bounds_origin.y + ay), ..IDENTITY };
    let to_superview = Transform { tx: state.frame.origin.x + ax, ty: state.frame.origin.y + ay, ..IDENTITY };
    concat(concat(to_anchor, state.transform), to_superview)
}

fn view_chain(view: Id, with_top: bool) -> Transform {
    let mut t = IDENTITY;
    let mut current = view;
    for _ in 0..64 {
        if current == 0 {
            break;
        }
        let superview = view_state(current).superview;
        if superview == 0 && !with_top {
            break;
        }
        t = concat(t, if superview == 0 { window_to_screen(current) } else { view_step(current) });
        current = superview;
    }
    t
}

/// Sends -hitTest:withEvent: or -pointInside:withEvent: (a CGPoint and an
/// event), so a game's own version runs: as runtime::send, nothing while
/// an exception is on its way, and a missing method is forwarded
/// (forwardingTargetForSelector: and the rest); 0 when nothing answers.
fn send_point_query(view: Id, name: &str, point: Point, event: Id) -> usize {
    match method_for(view, name) {
        Some((receiver, s, imp)) => {
            let f: extern "C" fn(Id, Sel, Point, Id) -> usize = unsafe { std::mem::transmute(imp) };
            f(receiver, s, point, event)
        }
        None => 0,
    }
}

/// Where a message `name` to `view` goes, as runtime::send finds it: the
/// receiver (a forwarding target for a missing method), selector and IMP;
/// None for a nil view, while an exception is on its way, or when nothing answers.
fn method_for(view: Id, name: &str) -> Option<(Id, Sel, usize)> {
    if view == 0 || crate::fw::exception::pending() {
        return None;
    }
    let s = runtime::sel(name);
    let mut receiver = view;
    let imp = runtime::imp_for(view, s).or_else(|| {
        let isa = lock().class_of(view);
        crate::objc::forward::after_miss(&mut receiver, isa, s, 0)
    })?;
    Some((receiver, s, imp))
}

/// -alpha, sent the same way (a float answer); 0 while an exception is on
/// its way (as runtime::send gives), 1 when nothing answers.
fn view_alpha_message(view: Id) -> f64 {
    if crate::fw::exception::pending() {
        return 0.0;
    }
    match method_for(view, "alpha") {
        Some((receiver, s, imp)) => {
            let f: extern "C" fn(Id, Sel) -> f64 = unsafe { std::mem::transmute(imp) };
            f(receiver, s)
        }
        None => 1.0,
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

/// UIView's -hitTest:withEvent: as UIKit's (0x4eb01), `point` in the
/// view's own points: nil when the view ignores touches (interaction off,
/// hidden, alpha 0.01 or less: each asked by message) or its
/// -pointInside:withEvent: says no; else the first subview, front first,
/// whose own -hitTest:withEvent: answers; else the view. A top view (a
/// window) takes touches past its bounds, as before. A view we don't draw
/// yet (fw/controls.rs "notDrawn") never catches a touch itself, so taps go
/// on to the drawn views under it.
pub(crate) fn view_hit_test(view: Id, point: Point, event: Id) -> Id {
    let asks = |name: &str| runtime::send(view, name, &[]) & 0xFF != 0;
    // The GL picture is shown whatever the view's alpha (fw/eagl.rs), so a
    // see-through view holding it keeps its touches: what's seen is what's
    // touched. UIKit pictures (fw/pictures.rs) do skip see-through views.
    let invisible = view_alpha_message(view) < 0.01 && !holds_gl_picture(view);
    if !asks("isUserInteractionEnabled") || asks("isHidden") || invisible {
        return 0;
    }
    let state = view_state(view);
    if state.superview != 0 && send_point_query(view, "pointInside:withEvent:", point, event) & 0xFF == 0 {
        return 0;
    }
    for child in state.subviews.clone().into_iter().rev() {
        // [child convertPoint:point fromView:view]
        let back = invert(view_step(child)).unwrap_or(IDENTITY);
        let (x, y) = crate::fw::geometry::apply(back, point.x, point.y);
        let found = send_point_query(child, "hitTest:withEvent:", Point { x, y }, event);
        if found != 0 {
            return found;
        }
    }
    if property_get(view, "notDrawn").is_some() {
        0
    } else {
        view
    }
}

/// UIView's -pointInside:withEvent:: whether the point is in its bounds.
pub(crate) fn view_point_inside(view: Id, point: Point) -> bool {
    let state = view_state(view);
    let (o, s) = (state.bounds_origin, state.frame.size);
    point.x >= o.x && point.y >= o.y && point.x < o.x + s.width && point.y < o.y + s.height
}

/// The view a touch at `point` (window points) begins on: the window's
/// -hitTest:withEvent: (the window itself when that answers nil).
pub(crate) fn hit_test(window: Id, point: Point) -> Id {
    hit_test_with(window, point, 0)
}

/// The same with the touch's event, as UIKit's window passes it.
fn hit_test_with(window: Id, point: Point, event: Id) -> Id {
    match send_point_query(window, "hitTest:withEvent:", point, event) {
        0 => window,
        found => found,
    }
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
    point_in_view(view, at)
}

extern "C" fn touch_previous_location_in_view(this: Id, _: Sel, view: Id) -> Point {
    let at = point_of(this, "previousX", "previousY");
    if view == 0 {
        return at;
    }
    point_in_view(view, at)
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

extern "C" fn touch_window(this: Id, _: Sel) -> Id {
    property_get(this, "window").map_or_else(key_window, |w| w as Id)
}

/// The last finger lifted: when, where and its tap count.
static LAST_TAP: Mutex<Option<(f64, Point, usize)>> = Mutex::new(None);
/// Longest gap and distance between the taps of a multi-tap (UIKit's).
const TAP_GAP: f64 = 0.35;
const TAP_DISTANCE: f64 = 25.0;

/// A new finger's tapCount: one more than the last finger's if it lifted
/// just before, near here; else 1.
fn tap_count(now: f64, at: Point) -> usize {
    match *LAST_TAP.lock().unwrap_or_else(|e| e.into_inner()) {
        Some((when, place, count)) if now - when <= TAP_GAP && (at.x - place.x).hypot(at.y - place.y) <= TAP_DISTANCE => count + 1,
        _ => 1,
    }
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

/// Handles one finger change, on the main thread. `point` is in screen
/// points: an old app's turned window gets it in its own (see uikit::window_turn).
fn deliver_now(finger: i32, phase: Phase, point: Point) {
    deliver_in(key_window(), finger, phase, screen_to_window_with(crate::fw::uikit::window_turn(), point));
}

/// The same for a given window.
pub(crate) fn deliver_in(window: Id, finger: i32, phase: Phase, point: Point) {
    if window == 0 {
        return;
    }
    let stamp = now();
    // Ignored while the app ignores interaction events, or while a modal
    // transition runs in the window: the finger goes nowhere. A new touch is
    // hit-tested with its event (the game's own -hitTest:/-pointInside:
    // run), before the touches are locked.
    let began = match phase {
        Phase::Began if crate::fw::uikit::ignoring_touches() || crate::fw::uikit::modal_transition_running(window) => return,
        Phase::Began => {
            let object = runtime::alloc_instance(runtime::class_named("UITouch"));
            set_point(object, "x", point);
            property_set(object, "phase", Phase::Began as u64);
            property_set(object, "timestamp", stamp.to_bits());
            let mut held: Vec<Id> = TOUCHES.lock().unwrap().as_ref().map(|t| t.values().map(|t| t.object).collect()).unwrap_or_default();
            held.push(object);
            let hit = hit_test_with(window, point, make_event(&held, stamp));
            // So is asking whether the view takes more than one finger.
            let single_touch = hit != 0 && runtime::send(hit, "isMultipleTouchEnabled", &[]) & 0xFF == 0;
            Some((object, hit, single_touch))
        }
        _ => None,
    };
    let mut guard = TOUCHES.lock().unwrap();
    let touches = guard.get_or_insert_with(HashMap::new);
    let touch = match (phase, touches.get(&finger), began) {
        (Phase::Began, _, Some((object, view, single_touch))) => {
            // A view with multipleTouchEnabled off hears only its first
            // finger: another one's touch has no view (it is still in the event).
            let ignored = single_touch && touches.values().any(|t| t.view == view && !t.ignored);
            property_set(object, "view", if ignored { 0 } else { view as u64 });
            property_set(object, "window", window as u64);
            property_set(object, "tapCount", tap_count(stamp, point) as u64);
            set_point(object, "previousX", point);
            let seq = NEXT_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // Held while the touch lasts, as UITouch retains its view.
            crate::fw::counted::retain(view);
            touches.insert(finger, Touch { object, view, taken: false, began_sent: false, delayed: None, ignored, seq });
            touches.get(&finger).unwrap()
        }
        (_, Some(existing), _) => existing,
        _ => return,
    };
    let ignored = touch.ignored;
    let (object, view, taken, began_sent, seq) = (touch.object, touch.view, touch.taken, touch.began_sent, touch.seq);
    let delaying = touch.delayed.is_some();
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
    // What a scroll view still held back from the view when the finger lifted.
    let mut held_back = Vec::new();
    let over = matches!(phase, Phase::Ended | Phase::Cancelled);
    if over {
        held_back = touches.remove(&finger).and_then(|t| t.delayed).unwrap_or_default();
    }
    if phase == Phase::Ended {
        let count = property_get(object, "tapCount").unwrap_or(1) as usize;
        *LAST_TAP.lock().unwrap_or_else(|e| e.into_inner()) = Some((stamp, point, count));
    }
    drop(guard);
    if over {
        // The touch is done: its view is let go of once this delivery is over.
        crate::fw::counted::autorelease(view);
    }

    let event = make_event(&active, stamp);
    if ignored {
        return;
    }
    let changed = misc::set_from(&[object]);
    // A scroll view drags with the finger: past the slop on an axis it can
    // move, it takes the touch (content that heard it gets touchesCancelled,
    // as canCancelContentTouches does, unless -touchesShouldCancelInContentView:
    // says NO); a plain tap still reaches the content.
    let scroll = crate::fw::controls::scroll_touch(view, object, finger, phase, point, taken, began_sent && !delaying);
    // Gesture recognizers on the view and the views it is in see the touch
    // too (and may take it, or hold it back from the view for now).
    let verdict = crate::fw::gestures::touch_changed_in(view, object, event, finger, phase, point);
    if taken {
        return;
    }
    // The scroll view or a recognizer took a touch the content was still
    // waiting for: the content never hears it.
    let took = matches!(scroll, crate::fw::controls::ScrollVerdict::CancelNow | crate::fw::controls::ScrollVerdict::Swallowed)
        || verdict == crate::fw::gestures::Verdict::Took;
    if delaying && (took || phase == Phase::Cancelled) {
        mark_touch(finger, |t| {
            t.delayed = None;
            t.taken |= verdict == crate::fw::gestures::Verdict::Took;
        });
        // Nor what recognizers were holding back with it.
        if let Some(held) = HELD.lock().unwrap().as_mut() {
            held.remove(&finger);
        }
        return;
    }
    match scroll {
        crate::fw::controls::ScrollVerdict::Swallowed => return,
        crate::fw::controls::ScrollVerdict::CancelNow => {
            if began_sent {
                property_set(object, "phase", Phase::Cancelled as u64);
                send_to_view(view, finger, "touchesCancelled:withEvent:", changed, event);
            }
            return;
        }
        crate::fw::controls::ScrollVerdict::Pass => {}
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
            // The recognizers hold it now: what the scroll view held goes to
            // them first. The scroll view's 0.15 s still runs (it starts now
            // for a new touch): the content hears it once both let it go.
            let mut items = held_back;
            if delaying {
                mark_touch(finger, |t| items.extend(t.delayed.as_mut().map(std::mem::take).unwrap_or_default()));
            } else if phase == Phase::Began && delays_content(view) {
                mark_touch(finger, |t| t.delayed = Some(Vec::new()));
                crate::fw::runloop::post_after(CONTENT_DELAY, Box::new(move || content_delay_passed(finger, seq)));
            }
            items.push(Message::now(object, phase, changed, event));
            let mut held = HELD.lock().unwrap();
            let entry = held.get_or_insert_with(HashMap::new).entry(finger).or_insert(Held { view, object, began_sent, items: Vec::new() });
            entry.items.extend(items);
        }
        crate::fw::gestures::Verdict::Pass if delaying => {
            if phase == Phase::Ended {
                // Let go before the delay passed: the content hears all of it now.
                if content_may_begin(view, changed, event) {
                    for message in held_back {
                        replay(view, finger, object, message);
                    }
                    send_to_view(view, finger, selector_for(phase), changed, event);
                }
            } else {
                mark_touch(finger, |t| t.delayed.get_or_insert_with(Vec::new).push(Message::now(object, phase, changed, event)));
            }
        }
        crate::fw::gestures::Verdict::Pass => {
            if phase == Phase::Began {
                if delays_content(view) {
                    // UIScrollView's delayed-touches recognizer: the content
                    // hears this touch only once 0.15 s pass (or it ends) without a drag.
                    mark_touch(finger, |t| t.delayed = Some(vec![Message::now(object, phase, changed, event)]));
                    crate::fw::runloop::post_after(CONTENT_DELAY, Box::new(move || content_delay_passed(finger, seq)));
                    return;
                }
                if !content_may_begin(view, changed, event) {
                    mark_touch(finger, |t| t.taken = true);
                    return;
                }
                mark_touch(finger, |t| t.began_sent = true);
            }
            send_to_view(view, finger, selector_for(phase), changed, event);
        }
    }
}

/// Whether a scroll view holding `view` (or `view` itself) delays its
/// content's touches (delaysContentTouches, YES unless the game turns it off).
fn delays_content(view: Id) -> bool {
    let class = runtime::class_named("UIScrollView");
    let mut current = view;
    for _ in 0..256 {
        if current == 0 || class == 0 {
            return false;
        }
        if runtime::send(current, "isKindOfClass:", &[class as Id]) & 0xFF != 0
            && runtime::send(current, "isScrollEnabled", &[]) & 0xFF != 0
            && runtime::send(current, "delaysContentTouches", &[]) & 0xFF != 0
        {
            return true;
        }
        current = view_state(current).superview;
    }
    false
}

/// UIScrollView's -touchesShouldBegin:withEvent:inContentView: (YES unless a
/// game's scroll view says otherwise), asked of the innermost scroll view
/// holding `view` before its content hears a touch.
fn content_may_begin(view: Id, touches: Id, event: Id) -> bool {
    let class = runtime::class_named("UIScrollView");
    let mut current = view_state(view).superview;
    for _ in 0..256 {
        if current == 0 || class == 0 {
            return true;
        }
        if runtime::send(current, "isKindOfClass:", &[class as Id]) & 0xFF != 0 {
            return runtime::send(current, "touchesShouldBegin:withEvent:inContentView:", &[touches, event, view]) & 0xFF != 0;
        }
        current = view_state(current).superview;
    }
    true
}

fn take_delayed(finger: i32) -> Vec<Message> {
    TOUCHES.lock().unwrap_or_else(|e| e.into_inner()).as_mut().and_then(|t| t.get_mut(&finger)).and_then(|t| t.delayed.take()).unwrap_or_default()
}

/// The 0.15 s a scroll view holds touch `seq` back from its content passed
/// with no drag: the content hears what it was held back from, in order, if
/// the scroll view lets it begin. (A finger number lifted and used again
/// since is another touch, with its own 0.15 s.) While recognizers still
/// hold it, it waits for them instead.
pub(crate) fn content_delay_passed(finger: i32, seq: u64) {
    let touch = TOUCHES.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|t| t.get(&finger)).filter(|t| t.seq == seq && t.delayed.is_some()).map(|t| (t.view, t.object));
    let Some((view, object)) = touch else { return };
    let items = take_delayed(finger);
    if let Some(held) = HELD.lock().unwrap().as_mut().and_then(|h| h.get_mut(&finger)) {
        held.items.splice(0..0, items);
        return;
    }
    let Some(first) = items.first() else { return };
    if !content_may_begin(view, first.changed, first.event) {
        mark_touch(finger, |t| t.taken = true);
        return;
    }
    mark_touch(finger, |t| t.began_sent = true);
    for message in items {
        replay(view, finger, object, message);
    }
}

/// Sends a touch message for the view a finger touched, logging the first few.
fn send_to_view(view: Id, finger: i32, selector: &'static str, changed: Id, event: Id) {
    let point = runtime::send(changed, "anyObject", &[]);
    let point = if point == 0 { Point::default() } else { point_of(point, "x", "y") };
    // UIKit sends the touch to the view it hit; a view that doesn't handle it
    // passes it up the responder chain (UIResponder's default, see uikit).
    let mut handler = view;
    while handler != 0 && !runtime::responds(handler, selector) {
        handler = view_state(handler).superview;
    }
    static SHOWN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    // Tests see every touch; otherwise only the first few are logged.
    if SHOWN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 12 || TAPS_ARMED.load(std::sync::atomic::Ordering::Relaxed) {
        // At the screen point tapped (test taps wait for these lines).
        let (x, y) = match crate::fw::uikit::window_turn() {
            Some(turn) => crate::fw::geometry::apply(turn, point.x, point.y),
            None => (point.x, point.y),
        };
        crate::log!(
            "touch {finger} {selector} at {:.0},{:.0}: view {}, handled by {}",
            x,
            y,
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
    // picture_area's y counts up from the bottom (GL): from the top, the
    // picture starts at height - y1 (an odd spare row is above it).
    let (top, bottom) = (height.max(1) - y1, height.max(1) - y0);
    let across = ((x - x0 as f64) / (x1 - x0).max(1) as f64).clamp(0.0, 1.0);
    let down = ((y - top as f64) / (bottom - top).max(1) as f64).clamp(0.0, 1.0);
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
    /// `x,y until <text>`: the finger stays down until a log line holds the text.
    until: Option<String>,
    /// `every=`: outside the order, goes each time its line comes (a box
    /// that may open at any point), then waits for its own tap's screen
    /// to be drawn before it can go again.
    every: bool,
    armed: bool,
    /// `settled x,y` (with `every=`): its line only readies it; it goes at
    /// the next settled screen with something there to take it (a box that
    /// fades in, whose button can't be touched yet when its line comes).
    settled: bool,
    due: bool,
}

/// IOS_TAPS="on=screen shown: IntroViewController=>505,29;ready=>284,297;on=frame 1 shown=>420,250>460,250":
/// each tap goes out the moment a log line holds the text after `on=`
/// (`after=` means the same), or with `ready` once the drawn screen has
/// something at that place to take it; nothing waits or counts seconds. A
/// swipe goes from x,y to x2,y2 over about 150 ms. A `jump=` step goes the
/// moment its line comes even while steps before it still wait (those are
/// dropped). `x,y until <text>` holds the finger down until a log line holds
/// the text. An `every=` step stands outside the order and goes each time
/// its line comes once its last tap's screen is drawn; with `settled x,y`
/// it waits after its line for the screen to settle with something at x,y
/// to take the tap. A step that isn't whole is left out.
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
            let every = when.starts_with("every=");
            let on = match when {
                "ready" => Trigger::Ready,
                _ => Trigger::Line(
                    ["on=", "after=", "jump=", "every="].iter().find_map(|prefix| when.strip_prefix(prefix)).filter(|text| !text.is_empty())?.to_string(),
                ),
            };
            let (place, until) = match place.split_once(" until ") {
                Some((place, line)) => (place, Some(line.trim().to_string()).filter(|line| !line.is_empty())),
                None => (place, None),
            };
            let place = place.trim();
            let (settled, place) = match place.strip_prefix("settled ") {
                Some(rest) if every => (true, rest.trim()),
                _ => (false, place),
            };
            let (from, to) = match place.split_once('>') {
                Some((from, to)) => (point(from)?, point(to)?),
                None => (point(place)?, point(place)?),
            };
            Some(TestTap { on, from, to, jump, until, every, armed: true, settled, due: false })
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
    if !TAPS_ARMED.load(std::sync::atomic::Ordering::Relaxed) || text.starts_with("test tap") || text.starts_with("test swipe") || text.starts_with("test hold") {
        return;
    }
    for (finger, at) in lifted(&mut TEST_HOLDS.lock().unwrap_or_else(|e| e.into_inner()), text) {
        crate::fw::runloop::post(crate::fw::runloop::Work::Native(Box::new(move || {
            deliver_now(finger, Phase::Ended, at);
            crate::fw::pictures::on_next_draw(format!("screen drawn after hold at {:.0},{:.0}", at.x, at.y));
        })));
    }
    let tap = next_test_tap(&mut TEST_TAPS.lock().unwrap_or_else(|e| e.into_inner()), text);
    if let Some(tap) = tap {
        let sender = *TEST_TAP_SENDER.lock().unwrap_or_else(|e| e.into_inner());
        sender(tap);
    }
}

/// Fingers held down (`until`): the line that lifts each, its finger and place.
static TEST_HOLDS: std::sync::Mutex<Vec<(String, i32, Point)>> = std::sync::Mutex::new(Vec::new());

/// The held fingers this log line lifts (taken off the list).
fn lifted(held: &mut Vec<(String, i32, Point)>, text: &str) -> Vec<(i32, Point)> {
    let mut lifted = Vec::new();
    held.retain(|(line, finger, at)| {
        let lifts = text.contains(line.as_str());
        if lifts {
            lifted.push((*finger, *at));
        }
        !lifts
    });
    lifted
}

/// The line a tap's next drawn screen logs (see send_test_tap).
fn drawn_after(to: Point) -> String {
    format!("screen drawn after tap at {:.0},{:.0}", to.x, to.y)
}

/// The step this log line sends: an armed `every=` step whose line it is,
/// else the next ordered step if its line is this, else the first `jump=`
/// step whose line it is (the ordered steps before it go away).
fn next_test_tap(taps: &mut std::collections::VecDeque<TestTap>, text: &str) -> Option<TestTap> {
    let holds = |tap: &TestTap| matches!(&tap.on, Trigger::Line(line) if text.contains(line.as_str()));
    for tap in taps.iter_mut().filter(|tap| tap.every && !tap.armed) {
        tap.armed = text.contains(&drawn_after(tap.to));
    }
    // A settled step's line counts even before it is armed again (a second
    // box opening while the first one's tap is still being drawn).
    for tap in taps.iter_mut().filter(|tap| tap.every && tap.settled && holds(tap)) {
        tap.due = true;
    }
    if let Some(tap) = taps.iter_mut().find(|tap| tap.every && !tap.settled && tap.armed && holds(tap)) {
        tap.armed = false;
        return Some(tap.clone());
    }
    let ordered: Vec<usize> = (0..taps.len()).filter(|&i| !taps[i].every).collect();
    let at = ordered.iter().enumerate().position(|(n, &i)| (n == 0 || taps[i].jump) && holds(&taps[i]))?;
    let chosen = taps[ordered[at]].clone();
    for &i in ordered[..=at].iter().rev() {
        taps.remove(i);
    }
    Some(chosen)
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
    // Hit-testing runs game code, which may log, and each log line looks at
    // the list (line_logged): the places are copied out and the list let go
    // while they are tried, then the tap is taken if it is still there.
    let (due, next) = {
        let taps = TEST_TAPS.lock().unwrap_or_else(|e| e.into_inner());
        let due: Vec<Point> = taps.iter().filter(|tap| tap.due && tap.armed).map(|tap| tap.from).collect();
        let next = taps.iter().find(|tap| !tap.every).filter(|tap| tap.on == Trigger::Ready).map(|tap| tap.from);
        (due, next)
    };
    let due_at = due.into_iter().find(|&at| takes_touches(at));
    let ready_at = if due_at.is_none() { next.filter(|&at| takes_touches(at)) } else { None };
    let tap = {
        let mut taps = TEST_TAPS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(at) = due_at {
            taps.iter_mut().find(|tap| tap.due && tap.armed && tap.from == at).map(|tap| {
                (tap.due, tap.armed) = (false, false);
                tap.clone()
            })
        } else if let Some(at) = ready_at {
            match taps.iter().position(|tap| !tap.every) {
                Some(next) if taps[next].on == Trigger::Ready && taps[next].from == at => taps.remove(next),
                _ => None,
            }
        } else {
            None
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
    let finger = TEST_FINGER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let send = move |phase: Phase, at: Point| {
        crate::fw::runloop::post(crate::fw::runloop::Work::Native(Box::new(move || deliver_now(finger, phase, at))));
    };
    if let Some(line) = tap.until {
        crate::log!("test hold at {:.0},{:.0} ({why}) until \"{line}\"", from.x, from.y);
        TEST_HOLDS.lock().unwrap_or_else(|e| e.into_inner()).push((line, finger, from));
        send(Phase::Began, from);
        return;
    }
    crate::log!("test {} at {:.0},{:.0} ({why})", if swipe { "swipe" } else { "tap" }, from.x, from.y);
    let ended = move || {
        crate::fw::runloop::post(crate::fw::runloop::Work::Native(Box::new(move || {
            deliver_now(finger, Phase::Ended, to);
            // The next tap can go on this line: the screen after this one.
            crate::fw::pictures::on_next_draw(drawn_after(to));
        })));
    };
    // The GL frame count when the finger went down (usize::MAX: not yet).
    let pressed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(usize::MAX));
    let pressed_at = pressed.clone();
    crate::fw::runloop::post(crate::fw::runloop::Work::Native(Box::new(move || {
        deliver_now(finger, Phase::Began, from);
        pressed_at.store(crate::fw::eagl::frames_shown(), std::sync::atomic::Ordering::SeqCst);
    })));
    std::thread::spawn(move || {
        if swipe {
            for step in 1..=6 {
                std::thread::sleep(std::time::Duration::from_millis(25));
                let f = step as f64 / 6.0;
                send(Phase::Moved, Point { x: from.x + (to.x - from.x) * f, y: from.y + (to.y - from.y) * f });
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        } else {
            std::thread::sleep(std::time::Duration::from_millis(80));
        }
        // A frame takes seconds on a slow PC run: a game that reads touches
        // once a frame must see the finger down (up to 30 s).
        for _ in 0..1500 {
            let at = pressed.load(std::sync::atomic::Ordering::SeqCst);
            if at != usize::MAX && tap_can_lift(at, crate::fw::eagl::frames_shown()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        ended();
    });
}

/// Whether a test finger pressed at GL frame `pressed` may lift now: once a
/// frame has been drawn since (games that don't draw with GL: at once).
fn tap_can_lift(pressed: usize, now: usize) -> bool {
    now == 0 || now > pressed
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
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        use crate::fw::geometry::Rect;
        // A window holds the game view (full screen) and, in front, an
        // undrawn scroll view (0,0 200x200) holding a drawn view (10,10
        // 50x50). A last view sits in front of everything but is hidden.
        let view = |frame: Rect| {
            let view = runtime::send(runtime::class_named("UIView"), "alloc", &[]);
            crate::fw::uikit::send_rect(view, "initWithFrame:", frame);
            view
        };
        let window = runtime::send(runtime::class_named("UIWindow"), "alloc", &[]);
        crate::fw::uikit::send_rect(window, "initWithFrame:", Rect::new(0.0, 0.0, 568.0, 320.0));
        let (game, scroll, drawn, hidden) = (view(Rect::new(0.0, 0.0, 568.0, 320.0)), view(Rect::new(0.0, 0.0, 200.0, 200.0)), view(Rect::new(10.0, 10.0, 50.0, 50.0)), view(Rect::new(0.0, 0.0, 568.0, 320.0)));
        property_set(scroll, "notDrawn", 1);
        runtime::send(hidden, "setHidden:", &[1]);
        for (parent, child) in [(window, game), (window, scroll), (scroll, drawn), (window, hidden)] {
            runtime::send(parent, "addSubview:", &[child]);
        }
        let at = |x, y| hit_test(window, Point { x, y });
        assert_eq!(at(20.0, 20.0), drawn, "drawn view inside the undrawn scroll view");
        assert_eq!(at(100.0, 100.0), game, "undrawn scroll view lets the tap through");
        assert_eq!(at(300.0, 100.0), game);
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
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        // All of it as one piece of main-loop work: no other test's main loop
        // runs the recognizers' timers or lets held touches go between steps.
        crate::fw::runloop::as_main(hold_back_and_cancel);
    }

    fn hold_back_and_cancel() {
        use crate::fw::geometry::Rect;
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
        let touch = |phase: Phase, x: f64| deliver_in(window, finger, phase, Point { x, y: 100.0 });
        let recognizer = |class: &str| {
            let object = runtime::send(runtime::send(runtime::class_named(class), "alloc", &[]), "init", &[]);
            runtime::send(view, "addGestureRecognizer:", &[object]);
            object
        };

        let press = recognizer("UILongPressGestureRecognizer");
        touch(Phase::Began, 100.0);
        crate::fw::gestures::held(press, finger);
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
        crate::fw::gestures::tap_gap_passed(double);
        assert_eq!(heard(), ["touchesEnded"], "no second tap: the end comes");
        runtime::send(view, "removeGestureRecognizer:", &[double]);
    }

    /// Controls follow a finger as UIKit's do: pressed (highlighted) while
    /// it is on them, their targets told TouchDown, then TouchUpInside or
    /// (let go far away) TouchUpOutside while still pressed, or TouchCancel;
    /// a quick tap stays pressed 0.12 s. The game's control classes hear
    /// begin/continue/endTracking; one that refuses tracking isn't pressed
    /// and its touch goes on up the responder chain. Disabled controls let
    /// the tap through to the view behind them; hidden, untouchable controls
    /// and places beside them leave the touch to the game's view.
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
        // TouchUpInside: the button is still pressed while its targets hear it.
        extern "C" fn up_inside(this: Id, s: Sel, sender: Id, event: Id) {
            note(this, s, sender, event);
            if runtime::send(sender, "isHighlighted", &[]) & 0xFF == 0 {
                HEARD.lock().unwrap().push("not pressed during TouchUpInside".to_string());
            }
        }
        let target = define_class("TouchTestTarget", "NSObject", 8);
        methods(target, &[("down:event:", note as usize), ("upInside:event:", up_inside as usize), ("upOutside:event:", note as usize), ("cancelled:event:", note as usize)]);
        let behind_view = define_class("TouchTestBehindView", "UIView", 8);
        methods(behind_view, &touches.map(|name| (name, note as usize)));
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

        // A tap: pressed while held, then down and up inside; a quick one
        // stays pressed until 0.12 s after the finger went down.
        touch(Phase::Began, 120.0, 110.0);
        assert!(highlighted(), "pressed while the finger is on it");
        touch(Phase::Ended, 120.0, 110.0);
        assert_eq!(heard(), up_inside);
        assert!(highlighted(), "a quick tap still shows pressed");
        assert!(crate::fw::runloop::perform_pending(button, runtime::sel("_unhighlight")), "let go of 0.12 s after the press");
        runtime::send(button, "_unhighlight", &[]);
        assert!(!highlighted());
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
        // Disabled: no actions; hit-testing skips it, so the view behind it has the touch.
        let behind = new_view("TouchTestBehindView", Rect::new(100.0, 100.0, 80.0, 40.0));
        runtime::send(game, "insertSubview:belowSubview:", &[behind, button]);
        runtime::send(button, "setEnabled:", &[0]);
        tap(120.0, 110.0);
        assert!(!highlighted());
        assert_eq!(heard(), ["TouchTestBehindView touchesBegan:withEvent:", "TouchTestBehindView touchesEnded:withEvent:"]);
        runtime::send(button, "setEnabled:", &[1]);
        runtime::send(behind, "removeFromSuperview", &[]);
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
            if class == "TouchTestRefusingControl" {
                assert!(runtime::send(control, "isHighlighted", &[]) & 0xFF == 0, "not pressed when tracking is refused");
            }
            touch(Phase::Moved, x + 12.0, 112.0);
            touch(Phase::Ended, x + 12.0, 112.0);
        }
        // Not tracking, a control passes the touch on (UIResponder's way): to the game's view.
        assert_eq!(
            heard(),
            [
                "TouchTestRefusingControl beginTrackingWithTouch:withEvent:",
                "TouchTestGameView touchesMoved:withEvent:",
                "TouchTestGameView touchesEnded:withEvent:",
                "TouchTestStoppingControl beginTrackingWithTouch:withEvent:",
                "TouchTestTarget down:event:",
                "TouchTestStoppingControl continueTrackingWithTouch:withEvent:",
                "TouchTestGameView touchesEnded:withEvent:",
            ]
        );
    }

    /// A touch counts its taps (a second tap soon after, near the first, has
    /// tapCount 2; one far away starts again at 1) and names the window it is in.
    #[test]
    fn touches_count_their_taps_and_know_their_window() {
        use crate::fw::geometry::Rect;
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        static COUNTS: Mutex<Vec<(usize, Id)>> = Mutex::new(Vec::new());
        extern "C" fn began(_this: Id, _: Sel, touches: Id, _event: Id) {
            let touch = runtime::send(touches, "anyObject", &[]);
            COUNTS.lock().unwrap().push((runtime::send(touch, "tapCount", &[]), runtime::send(touch, "window", &[])));
        }
        let class = define_class("TouchTestTapCountView", "UIView", 8);
        methods(class, &[("touchesBegan:withEvent:", began as usize)]);
        let window = runtime::send(runtime::class_named("UIWindow"), "alloc", &[]);
        crate::fw::uikit::send_rect(window, "initWithFrame:", Rect::new(0.0, 0.0, 568.0, 320.0));
        let view = runtime::send(class, "alloc", &[]);
        crate::fw::uikit::send_rect(view, "initWithFrame:", Rect::new(0.0, 0.0, 568.0, 320.0));
        runtime::send(window, "addSubview:", &[view]);
        crate::fw::runloop::as_main(|| {
            // Places no other test taps (the last tap is shared).
            for (finger, x) in [(730, 337.0), (731, 339.0), (732, 537.0)] {
                deliver_in(window, finger, Phase::Began, Point { x, y: 187.0 });
                deliver_in(window, finger, Phase::Ended, Point { x, y: 187.0 });
            }
        });
        assert_eq!(std::mem::take(&mut *COUNTS.lock().unwrap()), [(1, window), (2, window), (1, window)]);
    }

    /// Hit-testing asks the views themselves (-isHidden, -alpha,
    /// -isUserInteractionEnabled, which a game's class may answer its own
    /// way) and skips one whose alpha is below 0.01, as UIKit does (its
    /// __UIViewIgnoresTouchEvents compares alpha < 0.01).
    #[test]
    fn hit_testing_asks_the_views() {
        use crate::fw::geometry::Rect;
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        static HEARD: Mutex<Vec<String>> = Mutex::new(Vec::new());
        extern "C" fn note(this: Id, _: Sel, _touches: Id, _event: Id) {
            HEARD.lock().unwrap().push(crate::fw::string::describe_class_of(this));
        }
        extern "C" fn yes(_this: Id, _: Sel) -> bool {
            true
        }
        let back = define_class("TouchTestBackView", "UIView", 8);
        methods(back, &[("touchesBegan:withEvent:", note as usize)]);
        let hidden = define_class("TouchTestSaysHidden", "UIView", 8);
        methods(hidden, &[("touchesBegan:withEvent:", note as usize), ("isHidden", yes as usize)]);
        let new_view = |class: crate::objc::runtime::Class| {
            let view = runtime::send(class, "alloc", &[]);
            crate::fw::uikit::send_rect(view, "initWithFrame:", Rect::new(0.0, 0.0, 568.0, 320.0));
            view
        };
        let window = new_view(runtime::class_named("UIWindow"));
        let behind = new_view(back);
        runtime::send(window, "addSubview:", &[behind]);
        let front = new_view(hidden);
        runtime::send(window, "addSubview:", &[front]);
        assert_eq!(hit_test(window, Point { x: 30.0, y: 30.0 }), behind, "the view that says it is hidden is skipped");
        runtime::send(front, "removeFromSuperview", &[]);
        let faint = new_view(back);
        let s = runtime::sel("setAlpha:");
        let set_alpha: extern "C" fn(Id, Sel, f64) = unsafe { std::mem::transmute(runtime::imp_for(faint, s).unwrap()) };
        set_alpha(faint, s, 0.01);
        runtime::send(window, "addSubview:", &[faint]);
        assert_eq!(hit_test(window, Point { x: 30.0, y: 30.0 }), faint, "alpha 0.01 still takes touches (Apple skips only below 0.01)");
        set_alpha(faint, s, 0.009);
        assert_eq!(hit_test(window, Point { x: 30.0, y: 30.0 }), behind, "below 0.01 counts as see-through");
        HEARD.lock().unwrap().clear();
    }

    /// A view hears only its first finger unless it turns
    /// multipleTouchEnabled on (UIKit's default is off).
    #[test]
    fn a_single_touch_view_hears_only_its_first_finger() {
        use crate::fw::geometry::Rect;
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        static FINGERS: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
        extern "C" fn note(_this: Id, s: Sel, _touches: Id, _event: Id) {
            FINGERS.lock().unwrap().push(if lock().sel_name(s).starts_with("touchesBegan") { "began" } else { "ended" });
        }
        let class = define_class("TouchTestSingleTouchView", "UIView", 8);
        methods(class, &[("touchesBegan:withEvent:", note as usize), ("touchesEnded:withEvent:", note as usize)]);
        let window = runtime::send(runtime::class_named("UIWindow"), "alloc", &[]);
        crate::fw::uikit::send_rect(window, "initWithFrame:", Rect::new(0.0, 0.0, 568.0, 320.0));
        let view = runtime::send(class, "alloc", &[]);
        crate::fw::uikit::send_rect(view, "initWithFrame:", Rect::new(0.0, 0.0, 568.0, 320.0));
        runtime::send(window, "addSubview:", &[view]);
        let two_fingers = |a: i32, b: i32| {
            crate::fw::runloop::as_main(|| {
                deliver_in(window, a, Phase::Began, Point { x: 50.0, y: 50.0 });
                deliver_in(window, b, Phase::Began, Point { x: 300.0, y: 50.0 });
                deliver_in(window, b, Phase::Ended, Point { x: 300.0, y: 50.0 });
                deliver_in(window, a, Phase::Ended, Point { x: 50.0, y: 50.0 });
            });
            std::mem::take(&mut *FINGERS.lock().unwrap())
        };
        assert_eq!(two_fingers(740, 741), ["began", "ended"], "the second finger isn't delivered");
        // As UIKit's (_UIApplicationSetTouchViewNilWithGestureCheck): the
        // second finger's touch has no view, but is still in the event.
        let (second_view, in_event) = crate::fw::runloop::as_main(|| {
            deliver_in(window, 744, Phase::Began, Point { x: 50.0, y: 50.0 });
            deliver_in(window, 745, Phase::Began, Point { x: 300.0, y: 50.0 });
            let second = TOUCHES.lock().unwrap().as_ref().and_then(|t| t.get(&745)).map(|t| t.object).unwrap();
            let event = *EVENT.get().unwrap() as Id;
            let found = (runtime::send(second, "view", &[]), runtime::send(runtime::send(event, "allTouches", &[]), "count", &[]));
            deliver_in(window, 745, Phase::Ended, Point { x: 300.0, y: 50.0 });
            deliver_in(window, 744, Phase::Ended, Point { x: 50.0, y: 50.0 });
            found
        });
        assert_eq!(second_view, 0, "the second finger's touch has no view");
        assert_eq!(in_event, 2, "both fingers are in the event");
        FINGERS.lock().unwrap().clear();
        runtime::send(view, "setMultipleTouchEnabled:", &[1]);
        assert_eq!(two_fingers(742, 743), ["began", "began", "ended", "ended"]);
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
    fn touches_land_where_the_picture_is_drawn_with_an_odd_spare_row() {
        // The picture fills the width with an odd number of rows to spare;
        // GL counts picture_area's y from the bottom, so the spare row is on top.
        let (wide, high, _) = crate::fw::uikit::screen_points();
        let width = (wide * 3.0).round() as i32;
        let height = (high * 3.0).round() as i32 + 507;
        let (x0, y0, x1, y1) = crate::fw::eagl::picture_area(width, height);
        assert_eq!((height - (y1 - y0)) % 2, 1, "an odd number of spare rows");
        let (top, bottom) = (height - y1, height - y0);
        let first = window_to_points(x0 as f64, top as f64, width, height);
        assert!(first.y.abs() < 0.01, "the picture's top row is its top: {first:?}");
        let last = window_to_points(x1 as f64, bottom as f64, width, height);
        assert!((last.y - high).abs() < 0.01, "its bottom row is its bottom: {last:?}");
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

    /// Hit-testing a settled screen runs game code (hitTest:withEvent:,
    /// pointInside:...), which may log, and every log line looks at the test
    /// taps: the list is let go meanwhile (else the log line hangs).
    #[test]
    fn settling_hit_tests_without_holding_the_test_taps() {
        let _turn = ARMED_TAPS.lock().unwrap_or_else(|e| e.into_inner());
        static SENT: std::sync::Mutex<Vec<f64>> = std::sync::Mutex::new(Vec::new());
        static FREE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        fn record(tap: TestTap) {
            SENT.lock().unwrap().push(tap.from.x);
        }
        arm_test_taps(parse_test_taps("ready=>3,4"), record);
        screen_settled_with(|_| {
            FREE.store(TEST_TAPS.try_lock().is_ok(), std::sync::atomic::Ordering::SeqCst);
            true
        });
        TAPS_ARMED.store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(FREE.load(std::sync::atomic::Ordering::SeqCst), "the test taps were held while hit-testing");
        assert_eq!(*SENT.lock().unwrap(), [3.0]);
    }

    /// A new touch is hit-tested with its event, as UIKit's window does.
    #[test]
    fn new_touches_are_hit_tested_with_their_event() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        static EVENT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        extern "C" fn hit(_: Id, _: Sel, _point: Point, event: Id) -> Id {
            EVENT.store(event, std::sync::atomic::Ordering::SeqCst);
            0
        }
        let class = crate::fw::object::define_class("TouchTestEventWindow", "UIWindow", 16);
        crate::fw::object::methods(class, &[("hitTest:withEvent:", hit as usize)]);
        let window = runtime::send(class, "alloc", &[]);
        crate::fw::uikit::send_rect(window, "initWithFrame:", crate::fw::geometry::Rect::new(0.0, 0.0, 100.0, 100.0));
        deliver_in(window, 9901, Phase::Began, Point { x: 5.0, y: 5.0 });
        deliver_in(window, 9901, Phase::Ended, Point { x: 5.0, y: 5.0 });
        let event = EVENT.load(std::sync::atomic::Ordering::SeqCst);
        assert_ne!(event, 0, "hitTest:withEvent: got the touch's event");
        assert_eq!(crate::fw::string::describe_class_of(event), "UIEvent");
    }

    /// -alpha is asked as -hitTest: and -pointInside: are: nothing is sent
    /// while an exception raised for a 32-bit game is on its way.
    #[test]
    fn hit_testing_asks_no_alpha_while_an_exception_is_on_its_way() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        static ASKED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        extern "C" fn alpha(_: Id, _: Sel) -> f64 {
            ASKED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            1.0
        }
        let class = crate::fw::object::define_class("TouchTestAlphaView", "UIView", 16);
        crate::fw::object::methods(class, &[("alpha", alpha as usize)]);
        let view = runtime::send(class, "alloc", &[]);
        crate::fw::uikit::send_rect(view, "initWithFrame:", crate::fw::geometry::Rect::new(0.0, 0.0, 10.0, 10.0));
        let (found, raised) = crate::fw::exception::catching(|| {
            crate::fw::exception::raise(view);
            view_hit_test(view, Point { x: 1.0, y: 1.0 }, 0)
        });
        assert_eq!((found, raised), (0, Some(view)));
        assert_eq!(ASKED.load(std::sync::atomic::Ordering::SeqCst), 0, "alpha wasn't sent");
    }

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
    fn a_test_tap_stays_down_for_a_drawn_frame() {
        // A game that reads touches once a frame (a frame takes seconds on a
        // PC run) must see the finger down; games not drawing with GL don't wait.
        assert!(!tap_can_lift(41, 41), "no frame drawn since the press");
        assert!(tap_can_lift(41, 42));
        assert!(tap_can_lift(0, 0), "no GL frames at all");
    }

    #[test]
    fn a_held_finger_lifts_when_its_line_is_logged() {
        // Monster Life warms an egg while the finger stays on it.
        let taps = parse_test_taps("on=egg shown=>290,200 until frame 660 shown;on=next=>1,2");
        assert_eq!(taps.len(), 2, "{taps:?}");
        assert_eq!(taps[0].until.as_deref(), Some("frame 660 shown"));
        assert_eq!((taps[0].from, taps[0].to), (Point { x: 290.0, y: 200.0 }, Point { x: 290.0, y: 200.0 }));
        assert_eq!(taps[1].until, None);
        let mut held = vec![("frame 660 shown".to_string(), 7, Point { x: 290.0, y: 200.0 })];
        assert_eq!(lifted(&mut held, "graphics: frame 630 shown"), [], "still held");
        assert_eq!(lifted(&mut held, "graphics: frame 660 shown (640x1136)"), [(7, Point { x: 290.0, y: 200.0 })]);
        assert!(held.is_empty());
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

    #[test]
    fn an_every_step_answers_its_line_whenever_it_comes() {
        // A help box that may open after any round of a fight: closed each
        // time it opens, while the ordered steps go on around it.
        let mut taps: std::collections::VecDeque<TestTap> = parse_test_taps("every=help shown=>7,7;on=round=>1,1;on=round=>2,2").into();
        let from = |tap: Option<TestTap>| tap.map(|t| t.from.x);
        assert_eq!(from(next_test_tap(&mut taps, "help shown")), Some(7.0));
        assert_eq!(from(next_test_tap(&mut taps, "help shown")), None, "not again before its own tap's screen is drawn");
        assert_eq!(from(next_test_tap(&mut taps, "round")), Some(1.0), "the ordered steps go on");
        assert_eq!(from(next_test_tap(&mut taps, "screen drawn after tap at 7,7")), None);
        assert_eq!(from(next_test_tap(&mut taps, "help shown")), Some(7.0), "again after that");
        assert_eq!(from(next_test_tap(&mut taps, "round")), Some(2.0));
        assert_eq!(taps.len(), 1, "the every step stays");
    }

    #[test]
    fn a_settled_every_step_waits_for_the_screen_to_settle() {
        // A box that fades in: its line comes before its button takes
        // touches, so the tap waits for the screen to settle after the line.
        let _turn = ARMED_TAPS.lock().unwrap_or_else(|e| e.into_inner());
        static SENT: std::sync::Mutex<Vec<f64>> = std::sync::Mutex::new(Vec::new());
        fn sender(tap: TestTap) {
            SENT.lock().unwrap().push(tap.from.x);
        }
        let sent = || std::mem::take(&mut *SENT.lock().unwrap());
        arm_test_taps(parse_test_taps("every=box shown=>settled 7,7"), sender);
        screen_settled_with(|_| true);
        assert_eq!(sent(), [] as [f64; 0], "nothing before its line");
        line_logged("box shown");
        assert_eq!(sent(), [] as [f64; 0], "not on the line itself");
        screen_settled_with(|_| false);
        assert_eq!(sent(), [] as [f64; 0], "not while nothing there takes touches");
        screen_settled_with(|_| true);
        assert_eq!(sent(), [7.0]);
        screen_settled_with(|_| true);
        line_logged("box shown");
        screen_settled_with(|_| true);
        assert_eq!(sent(), [] as [f64; 0], "not again before its own tap's screen is drawn");
        line_logged("screen drawn after tap at 7,7");
        screen_settled_with(|_| true);
        assert_eq!(sent(), [7.0], "then for the box that opened meanwhile");
        TAPS_ARMED.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// A touch on a plain view bubbles up the responder chain, so the view
    /// controller whose view it is in hears touches it didn't handle itself
    /// (as UIKit's UIResponder default does).
    #[test]
    fn touches_reach_a_view_controllers_touches_began() {
        use crate::fw::geometry::Rect;
        use crate::fw::object::define_class;
        use crate::objc::runtime::Class;
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        static HEARD: Mutex<Vec<String>> = Mutex::new(Vec::new());
        extern "C" fn note(_this: Id, s: Sel, _touches: Id, _event: Id) {
            let name = lock().sel_name(s);
            HEARD.lock().unwrap().push(name.trim_end_matches(":withEvent:").to_string());
        }
        let controller_class = define_class("TouchTestResponderController", "UIViewController", 8);
        methods(
            controller_class,
            &[("touchesBegan:withEvent:", note as usize), ("touchesEnded:withEvent:", note as usize)],
        );
        let new_view = |class: Class, frame: Rect| {
            let object = runtime::send(class, "alloc", &[]);
            crate::fw::uikit::send_rect(object, "initWithFrame:", frame);
            object
        };
        let controller = runtime::send(runtime::send(controller_class, "alloc", &[]), "init", &[]);
        let view = runtime::send(controller, "view", &[]);
        crate::fw::uikit::send_rect(view, "setFrame:", Rect::new(0.0, 0.0, 568.0, 320.0));
        let window = new_view(runtime::class_named("UIWindow"), Rect::new(0.0, 0.0, 568.0, 320.0));
        let child = new_view(runtime::class_named("UIView"), Rect::new(10.0, 10.0, 100.0, 50.0));
        runtime::send(window, "addSubview:", &[view]);
        runtime::send(view, "addSubview:", &[child]);
        let finger = 720; // not a finger other tests use
        deliver_in(window, finger, Phase::Began, Point { x: 20.0, y: 20.0 });
        deliver_in(window, finger, Phase::Ended, Point { x: 20.0, y: 20.0 });
        assert_eq!(std::mem::take(&mut *HEARD.lock().unwrap()), ["touchesBegan", "touchesEnded"], "once each, through the chain");
    }
}
