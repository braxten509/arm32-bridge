//! Stand-ins for Apple's Foundation and UIKit classes.

pub mod app;
pub mod audiofile;
pub mod audioqueue;
pub mod coalesced;
pub mod collections;
pub mod crypto;
pub mod data;
pub mod eagl;
pub mod files;
pub mod foundation;
pub mod gamekit;
pub mod openal;
pub mod geometry;
pub mod gl;
pub mod json;
pub mod keychain;
pub mod locks;
pub mod media;
pub mod misc;
pub mod object;
pub mod paths;
pub mod pictures;
pub mod plist;
pub mod png;
pub mod pvrtc;
pub mod regex;
pub mod runloop;
pub mod sqlite;
pub mod storekit;
pub mod services;
pub mod sound;
pub mod string;
pub mod system;
pub mod touch;
pub mod uikit;
pub mod web;

use crate::symbols::Registry;

pub fn register(r: &mut Registry) {
    object::define(r);
    string::register(r);
    collections::register(r);
    data::register(r);
    misc::register(r);
    foundation::register(r);
    app::register(r);
    uikit::register(r);
    runloop::register(r);
    media::register(r);
    keychain::register(r);
    gl::register(r);
    eagl::register(r);
    files::register(r);
    services::register(r);
    web::register(r);
    sqlite::register(r);
    regex::register(r);
    locks::register(r);
    crypto::register(r);
    json::register(r);
    system::register(r);
    touch::register(r);
    sound::register(r);
    gamekit::register(r);
    openal::register(r);
    storekit::register(r);
    audiofile::register(r);
    audioqueue::register(r);
    immutable_copies();
}

/// Objects that never change hand back themselves when copied, as on Apple.
fn immutable_copies() {
    extern "C" fn same(this: crate::objc::runtime::Id, _: crate::objc::runtime::Sel, _zone: usize) -> crate::objc::runtime::Id {
        this
    }
    for name in [
        "NSRegularExpression", "NSTextCheckingResult", "NSDate", "NSURL", "NSUUID", "NSNull", "NSError", "NSLocale",
        "UIColor", "UIImage", "NSURLRequest",
    ] {
        let class = crate::objc::runtime::class_named(name);
        if class != 0 {
            object::methods(class, &[("copyWithZone:", same as usize)]);
        }
    }
}
