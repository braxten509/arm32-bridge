//! Stand-ins for Apple's Foundation and UIKit classes.

pub mod animation;
pub mod app;
pub mod addressbook;
pub mod attributed;
pub mod bezier;
pub mod cfbridge;
pub mod changes;
pub mod audiofile;
pub mod extaudiofile;
pub mod audioqueue;
pub mod avasset;
pub mod coalesced;
pub mod corevideo;
pub mod collections;
pub mod controls;
pub mod counted;
pub mod crypto;
pub mod data;
pub mod dates;
pub mod eagl;
pub mod exception;
pub mod files;
pub mod foundation;
pub mod gamekit;
pub mod openal;
pub mod resample;
pub mod operations;
pub mod geometry;
pub mod gestures;
pub mod frame_slots;
pub mod vsync;
pub mod gl;
pub mod graphics;
pub mod invocation;
pub mod json;
pub mod keychain;
pub mod layers;
pub mod locks;
pub mod media;
pub mod misc;
pub mod nib;
pub mod numbers;
pub mod object;
pub mod paths;
pub mod photos;
pub mod pickers;
pub mod pictures;
pub mod plist;
pub mod predicate;
pub mod progress;
pub mod png;
pub mod pvrtc;
pub mod texture_cache;
pub mod workers;
pub mod regex;
pub mod scanner;
pub mod runloop;
pub mod seckey;
pub mod smallclasses;
pub mod sqlite;
pub mod storekit;
pub mod storyboard;
pub mod services;
pub mod sound;
pub mod string;
pub mod system;
pub mod tables;
pub mod touch;
pub mod typing;
pub mod uikit;
pub mod unavailable;
pub mod web;
pub mod xml;

use crate::symbols::Registry;

pub fn register(r: &mut Registry) {
    object::define(r);
    object::define_proxy();
    string::register(r);
    addressbook::register(r);
    attributed::register(r);
    collections::register(r);
    data::register(r);
    plist::register(r);
    misc::register(r);
    object::register_collection_coding();
    predicate::register(r);
    foundation::register(r);
    app::register(r);
    uikit::register(r);
    layers::register();
    animation::register(r);
    controls::register(r);
    pickers::register();
    progress::register(r);
    typing::register(r);
    tables::register(r);
    nib::register(r);
    unavailable::register(r);
    storyboard::register(r);
    graphics::register(r);
    bezier::register();
    runloop::register(r);
    media::register(r);
    keychain::register(r);
    seckey::register(r);
    gl::register(r);
    eagl::register(r);
    files::register(r);
    services::register(r);
    web::register(r);
    xml::register(r);
    sqlite::register(r);
    regex::register(r);
    locks::register(r);
    crypto::register(r);
    json::register(r);
    system::register(r);
    touch::register(r);
    gestures::register();
    sound::register(r);
    gamekit::register(r);
    openal::register(r);
    storekit::register(r);
    dates::register(r);
    numbers::register(r);
    scanner::register(r);
    cfbridge::register(r);
    invocation::register(r);
    operations::register(r);
    smallclasses::register(r);
    exception::register(r);
    audiofile::register(r);
    extaudiofile::register(r);
    audioqueue::register(r);
    corevideo::register(r);
    avasset::register(r);
    photos::register(r);
    immutable_copies();
}

/// Objects that never change hand back themselves when copied (retained), as on Apple.
fn immutable_copies() {
    extern "C" fn same(this: crate::objc::runtime::Id, _: crate::objc::runtime::Sel, _zone: usize) -> crate::objc::runtime::Id {
        counted::retain(this)
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
