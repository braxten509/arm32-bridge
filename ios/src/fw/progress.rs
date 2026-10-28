//! UIProgressView: a track over its bounds with the part done filled
//! from the left (fw/pictures.rs draws it).

use crate::fw::object::{define_class, methods};
use crate::fw::uikit::{property_get, property_set, property_set_object};
use crate::objc::runtime::{Id, Sel};
use crate::symbols::Registry;
use std::collections::HashMap;
use std::sync::Mutex;

/// The track's and the done part's colours when none is set (UIProgressViewStyleDefault).
pub const TRACK: [f64; 4] = [0.72, 0.72, 0.72, 1.0];
pub const DONE: [f64; 4] = [0.0, 122.0 / 255.0, 1.0, 1.0];

/// How far along a progress bar is, 0 to 1.
pub fn progress(bar: Id) -> f64 {
    property_get(bar, "progress").map_or(0.0, |bits| f32::from_bits(bits as u32) as f64)
}

/// How long an animated change of the fill takes.
const SLIDE: f64 = 0.25;

/// Animated changes on their way: bar -> (from, start time).
static SLIDING: Mutex<Option<HashMap<Id, (f64, f64)>>> = Mutex::new(None);

/// How far the fill is drawn at `now`: on its way from the old value while
/// an animated change runs (setProgress:animated:), else the progress.
pub fn shown_progress_at(bar: Id, now: f64) -> f64 {
    let to = progress(bar);
    match SLIDING.lock().unwrap().as_ref().and_then(|m| m.get(&bar).copied()) {
        Some((from, start)) if now - start < SLIDE => {
            let t = ((now - start) / SLIDE).clamp(0.0, 1.0);
            from + (to - from) * (1.0 - (1.0 - t).powi(2))
        }
        _ => to,
    }
}

/// What fw/pictures.rs draws now.
pub fn shown_progress(bar: Id) -> f64 {
    shown_progress_at(bar, crate::fw::runloop::media_time())
}

/// The screen redraws while an animated change runs.
fn redraw_until(bar: Id, end: f64) {
    crate::fw::pictures::draw_soon();
    let now = crate::fw::runloop::media_time();
    if now < end {
        crate::fw::runloop::post_after(1.0 / 60.0, Box::new(move || redraw_until(bar, end)));
    } else {
        slide_over(bar, now);
    }
}

/// A slide's redraws ended: the bar is drawn at its progress again, unless
/// a newer slide started since and is still on its way.
pub(crate) fn slide_over(bar: Id, now: f64) {
    if let Some(m) = SLIDING.lock().unwrap().as_mut() {
        if m.get(&bar).is_some_and(|&(_, start)| now - start >= SLIDE) {
            m.remove(&bar);
        }
    }
}

fn get_id(bar: Id, key: &'static str) -> Id {
    property_get(bar, key).unwrap_or(0) as Id
}

/// What a progress bar shows: (track picture, track colour), (done picture, done colour).
pub fn parts(bar: Id) -> ((Id, Id), (Id, Id)) {
    ((get_id(bar, "trackImage"), get_id(bar, "trackTintColor")), (get_id(bar, "progressImage"), get_id(bar, "progressTintColor")))
}

extern "C" fn get_progress(this: Id, _: Sel) -> f32 {
    progress(this) as f32
}

pub extern "C" fn set_progress(this: Id, _: Sel, value: f32) {
    let value = if value.is_nan() { 0.0 } else { value.clamp(0.0, 1.0) };
    if let Some(m) = SLIDING.lock().unwrap().as_mut() {
        m.remove(&this);
    }
    property_set(this, "progress", value.to_bits() as u64);
}

/// Animated, the fill slides from where it is shown now to the new value
/// (the property is the new value at once), as UIKit's does.
extern "C" fn set_progress_animated(this: Id, s: Sel, value: f32, animated: bool) {
    if animated {
        slide_to(this, value, crate::fw::runloop::media_time());
    } else {
        set_progress(this, s, value);
    }
}

/// The fill starts sliding at `now` from where it is shown to `value`.
pub(crate) fn slide_to(bar: Id, value: f32, now: f64) {
    let from = shown_progress_at(bar, now);
    set_progress(bar, 0, value);
    if from != progress(bar) {
        SLIDING.lock().unwrap().get_or_insert_with(HashMap::new).insert(bar, (from, now));
        redraw_until(bar, now + SLIDE);
    }
}

extern "C" fn init_with_style(this: Id, _: Sel, style: isize) -> Id {
    let this = crate::objc::runtime::send(this, "init", &[]);
    property_set(this, "progressViewStyle", style as u64);
    this
}

extern "C" fn style(this: Id, _: Sel) -> isize {
    property_get(this, "progressViewStyle").unwrap_or(0) as isize
}

extern "C" fn set_style(this: Id, _: Sel, style: isize) {
    property_set(this, "progressViewStyle", style as u64);
}

macro_rules! object_property {
    ($get:ident, $set:ident, $key:literal) => {
        extern "C" fn $get(this: Id, _: Sel) -> Id {
            get_id(this, $key)
        }
        pub extern "C" fn $set(this: Id, _: Sel, value: Id) {
            property_set_object(this, $key, value);
        }
    };
}
object_property!(progress_tint, set_progress_tint, "progressTintColor");
object_property!(track_tint, set_track_tint, "trackTintColor");
object_property!(progress_image, set_progress_image, "progressImage");
object_property!(track_image, set_track_image, "trackImage");

pub fn register(_: &mut Registry) {
    let class = define_class("UIProgressView", "UIView", 8);
    methods(
        class,
        &[
            ("initWithProgressViewStyle:", init_with_style as usize),
            ("progress", get_progress as usize),
            ("setProgress:", set_progress as usize),
            ("setProgress:animated:", set_progress_animated as usize),
            ("progressViewStyle", style as usize),
            ("setProgressViewStyle:", set_style as usize),
            ("progressTintColor", progress_tint as usize),
            ("setProgressTintColor:", set_progress_tint as usize),
            ("trackTintColor", track_tint as usize),
            ("setTrackTintColor:", set_track_tint as usize),
            ("progressImage", progress_image as usize),
            ("setProgressImage:", set_progress_image as usize),
            ("trackImage", track_image as usize),
            ("setTrackImage:", set_track_image as usize),
        ],
    );
}
