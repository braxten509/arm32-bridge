//! Touches: UITouch and UIEvent, delivered to the view under the finger
//! (touchesBegan/Moved/Ended/Cancelled:withEvent:) on the main thread.
//! The phone app hands touches over in window pixels; IOS_TAPS lets PC
//! tests tap too ("seconds:x,y;seconds:x,y" in points).

use crate::fw::geometry::Point;
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
}

static TOUCHES: Mutex<Option<HashMap<i32, Touch>>> = Mutex::new(None);

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
fn origin_in_window(view: Id) -> Point {
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

fn takes_touches(view: Id) -> bool {
    property_get(view, "hidden").unwrap_or(0) == 0 && property_get(view, "userInteraction").map(|v| v != 0).unwrap_or(true)
}

/// The deepest visible view under the point (window points), as UIKit's hitTest finds it.
fn hit_test(window: Id, point: Point) -> Id {
    fn inside(view: Id, point: Point) -> Option<Id> {
        if !takes_touches(view) {
            return None;
        }
        let origin = origin_in_window(view);
        let size = view_state(view).frame.size;
        let local = Point { x: point.x - origin.x, y: point.y - origin.y };
        let is_window = view_state(view).superview == 0;
        if !is_window && (local.x < 0.0 || local.y < 0.0 || local.x >= size.width || local.y >= size.height) {
            return None;
        }
        for &child in view_state(view).subviews.iter().rev() {
            if let Some(found) = inside(child, point) {
                return Some(found);
            }
        }
        Some(view)
    }
    inside(window, point).unwrap_or(window)
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
    let window = key_window();
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
            touches.insert(finger, Touch { object, view });
            touches.get(&finger).unwrap()
        }
        (_, Some(existing)) => existing,
        (_, None) => return,
    };
    let (object, view) = (touch.object, touch.view);
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

    let event = runtime::alloc_instance(runtime::class_named("UIEvent"));
    property_set(event, "touches", misc::set_from(&active) as u64);
    property_set(event, "timestamp", stamp.to_bits());
    let changed = misc::set_from(&[object]);
    let selector = match phase {
        Phase::Began => "touchesBegan:withEvent:",
        Phase::Moved => "touchesMoved:withEvent:",
        Phase::Ended => "touchesEnded:withEvent:",
        Phase::Cancelled => "touchesCancelled:withEvent:",
    };
    // Like UIKit's responder chain: the touched view, else the nearest
    // view above it that handles touches.
    let mut handler = view;
    while handler != 0 && !runtime::responds(handler, selector) {
        handler = view_state(handler).superview;
    }
    static SHOWN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    if SHOWN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 12 {
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

/// IOS_TAPS="12:284,160;20:100,50": taps at those times (seconds after start) and points.
/// IOS_TAPS="seconds:x,y;seconds:x,y>x2,y2;..." (points): test taps, or
/// swipes from x,y to x2,y2 over about 150 ms.
pub fn schedule_test_taps() {
    let Ok(text) = std::env::var("IOS_TAPS") else { return };
    let point = |text: &str| -> Option<Point> {
        let (x, y) = text.split_once(',')?;
        Some(Point { x: x.trim().parse().ok()?, y: y.trim().parse().ok()? })
    };
    let touches: Vec<(f64, Point, Point)> = text
        .split(';')
        .filter_map(|part| {
            let (time, place) = part.split_once(':')?;
            let (from, to) = match place.split_once('>') {
                Some((from, to)) => (point(from)?, point(to)?),
                None => (point(place)?, point(place)?),
            };
            Some((time.trim().parse().ok()?, from, to))
        })
        .collect();
    std::thread::spawn(move || {
        let start = now();
        for (finger, (time, from, to)) in touches.into_iter().enumerate() {
            let wait = start + time - now();
            if wait > 0.0 {
                std::thread::sleep(std::time::Duration::from_secs_f64(wait));
            }
            let swipe = from.x != to.x || from.y != to.y;
            crate::log!("test {} at {:.0},{:.0}", if swipe { "swipe" } else { "tap" }, from.x, from.y);
            let finger = finger as i32;
            let send = |phase: Phase, at: Point| {
                crate::fw::runloop::post(crate::fw::runloop::Work::Native(Box::new(move || deliver_now(finger, phase, at))));
            };
            send(Phase::Began, from);
            let steps = if swipe { 6 } else { 0 };
            for step in 1..=steps {
                std::thread::sleep(std::time::Duration::from_millis(25));
                let f = step as f64 / steps as f64;
                send(Phase::Moved, Point { x: from.x + (to.x - from.x) * f, y: from.y + (to.y - from.y) * f });
            }
            std::thread::sleep(std::time::Duration::from_millis(if swipe { 25 } else { 80 }));
            send(Phase::Ended, to);
        }
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
}
