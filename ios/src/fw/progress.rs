//! UIProgressView: a track over its bounds with the part done filled
//! from the left (fw/pictures.rs draws it).

use crate::fw::object::{define_class, methods};
use crate::fw::uikit::{property_get, property_set, property_set_object};
use crate::objc::runtime::{Id, Sel};
use crate::symbols::Registry;

/// The track's and the done part's colours when none is set (UIProgressViewStyleDefault).
pub const TRACK: [f64; 4] = [0.72, 0.72, 0.72, 1.0];
pub const DONE: [f64; 4] = [0.0, 122.0 / 255.0, 1.0, 1.0];

/// How far along a progress bar is, 0 to 1.
pub fn progress(bar: Id) -> f64 {
    property_get(bar, "progress").map_or(0.0, |bits| f32::from_bits(bits as u32) as f64)
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
    property_set(this, "progress", value.to_bits() as u64);
}

extern "C" fn set_progress_animated(this: Id, s: Sel, value: f32, _animated: bool) {
    set_progress(this, s, value);
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
