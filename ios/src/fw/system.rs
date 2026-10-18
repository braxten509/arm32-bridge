//! Smaller system parts the games reach: saved-object archives, time zones,
//! the pasteboard (clipboard) and the motion sensors.

use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::uikit::{property_get, property_set};
use crate::fw::{collections, plist, string};
use crate::objc::runtime::{self, Class, Id, Sel};
use crate::symbols::Registry;
use std::collections::HashMap;
use std::sync::Mutex;

// ----- NSKeyedArchiver / NSKeyedUnarchiver -----
// Objects made of property-list parts (dictionaries, arrays, strings, numbers,
// data, dates) are saved as a property list and read back the same way.
// Game classes with their own saving code are not supported yet: saving
// reports failure and reading finds nothing, so nothing is half-saved.

extern "C" fn archive_root_object(_: Class, _: Sel, object: Id, path: Id) -> bool {
    let path = string::get(path);
    match plist::from_object(object) {
        Some(value) => std::fs::write(&path, plist::to_xml(&value)).is_ok(),
        None => {
            crate::log!("NSKeyedArchiver: a {} can't be saved yet (only property-list objects); {path} not written", string::describe_class_of(object));
            false
        }
    }
}

extern "C" fn unarchive_object_with_file(_: Class, _: Sel, path: Id) -> Id {
    let path = string::get(path);
    match std::fs::read(&path).ok().and_then(|bytes| plist::parse(&bytes)) {
        Some(value) => plist::to_object(&value),
        None => 0,
    }
}

extern "C" fn archived_data_with_root_object(_: Class, _: Sel, object: Id) -> Id {
    match plist::from_object(object) {
        Some(value) => crate::fw::data::make(plist::to_xml(&value).as_bytes()),
        None => {
            crate::log!("NSKeyedArchiver: a {} can't be saved yet (only property-list objects)", string::describe_class_of(object));
            0
        }
    }
}

extern "C" fn unarchive_object_with_data(_: Class, _: Sel, data: Id) -> Id {
    match plist::parse(&crate::fw::data::bytes(data)) {
        Some(value) => plist::to_object(&value),
        None => 0,
    }
}

// ----- NSTimeZone: the phone's own time zone -----

fn local_offset_and_name(at: libc::time_t) -> (i64, String) {
    unsafe {
        let mut parts: libc::tm = std::mem::zeroed();
        libc::localtime_r(&at, &mut parts);
        let name = if parts.tm_zone.is_null() { "GMT".to_string() } else { std::ffi::CStr::from_ptr(parts.tm_zone).to_string_lossy().into_owned() };
        (parts.tm_gmtoff, name)
    }
}

fn zone_name() -> String {
    if let Ok(name) = std::env::var("TZ") {
        if !name.is_empty() {
            return name.trim_start_matches(':').to_string();
        }
    }
    std::fs::read_link("/etc/localtime")
        .ok()
        .and_then(|p| p.to_str().and_then(|p| p.split("zoneinfo/").nth(1).map(String::from)))
        .unwrap_or_else(|| local_offset_and_name(now()).1)
}

fn now() -> libc::time_t {
    unsafe { libc::time(std::ptr::null_mut()) }
}

fn make_zone(class: Class, fixed_offset: Option<i64>) -> Id {
    let zone = runtime::alloc_instance(class);
    if let Some(offset) = fixed_offset {
        property_set(zone, "offset", offset as u64);
    }
    zone
}

extern "C" fn time_zone_local(class: Class, _: Sel) -> Id {
    static ZONE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *ZONE.get_or_init(|| make_zone(class, None))
}

extern "C" fn time_zone_for_seconds(class: Class, _: Sel, seconds: isize) -> Id {
    make_zone(class, Some(seconds as i64))
}

extern "C" fn time_zone_with_name(class: Class, _: Sel, name: Id) -> Id {
    let name = string::get(name);
    if name == "GMT" || name == "UTC" {
        return make_zone(class, Some(0));
    }
    if name == zone_name() {
        return time_zone_local(class, 0);
    }
    crate::log!("NSTimeZone {name}: only the phone's own zone and GMT are known; giving nil");
    0
}

fn offset_of(zone: Id, at: libc::time_t) -> i64 {
    match property_get(zone, "offset") {
        Some(fixed) => fixed as i64,
        None => local_offset_and_name(at).0,
    }
}

extern "C" fn time_zone_seconds_from_gmt(this: Id, _: Sel) -> isize {
    offset_of(this, now()) as isize
}

extern "C" fn time_zone_seconds_from_gmt_for_date(this: Id, _: Sel, date: Id) -> isize {
    let at = if date == 0 { now() } else { (runtime_f64(date, "timeIntervalSince1970")) as libc::time_t };
    offset_of(this, at) as isize
}

fn runtime_f64(object: Id, name: &str) -> f64 {
    let s = runtime::sel(name);
    match runtime::imp_for(object, s) {
        Some(imp) => {
            let f: extern "C" fn(Id, Sel) -> f64 = unsafe { std::mem::transmute(imp) };
            f(object, s)
        }
        None => 0.0,
    }
}

extern "C" fn time_zone_name(this: Id, _: Sel) -> Id {
    match property_get(this, "offset") {
        Some(0) => string::make("GMT"),
        Some(offset) => string::make(&format!("GMT{:+03}{:02}", offset as i64 / 3600, (offset as i64).abs() / 60 % 60)),
        None => string::make(&zone_name()),
    }
}

extern "C" fn time_zone_abbreviation(this: Id, _: Sel) -> Id {
    match property_get(this, "offset") {
        Some(_) => time_zone_name(this, 0),
        None => string::make(&local_offset_and_name(now()).1),
    }
}

extern "C" fn time_zone_is_daylight_saving(this: Id, _: Sel) -> bool {
    if property_get(this, "offset").is_some() {
        return false;
    }
    unsafe {
        let mut parts: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now(), &mut parts);
        parts.tm_isdst > 0
    }
}

// ----- UIPasteboard: kept in memory for this run -----

static BOARDS: Mutex<Option<HashMap<String, Id>>> = Mutex::new(None);

fn board(class: Class, name: &str, create: bool) -> Id {
    let mut boards = BOARDS.lock().unwrap();
    let boards = boards.get_or_insert_with(HashMap::new);
    if let Some(&found) = boards.get(name) {
        return found;
    }
    if !create {
        return 0;
    }
    let made = runtime::alloc_instance(class);
    property_set(made, "name", string::make(name) as u64);
    boards.insert(name.to_string(), made);
    made
}

extern "C" fn pasteboard_general(class: Class, _: Sel) -> Id {
    board(class, "com.apple.UIKit.pboard.general", true)
}

extern "C" fn pasteboard_with_name(class: Class, _: Sel, name: Id, create: u8) -> Id {
    board(class, &string::get(name), create != 0)
}

extern "C" fn pasteboard_with_unique_name(class: Class, _: Sel) -> Id {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
    let name = format!("com.apple.UIKit.pboard.unique.{}", NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    board(class, &name, true)
}

extern "C" fn pasteboard_remove(_: Class, _: Sel, name: Id) {
    if let Some(boards) = BOARDS.lock().unwrap().as_mut() {
        boards.remove(&string::get(name));
    }
}

/// Items: an array of dictionaries (type → value).
fn items(this: Id) -> Id {
    property_get(this, "items").unwrap_or(0) as Id
}

fn set_items(this: Id, list: Id) {
    property_set(this, "items", list as u64);
    let count = property_get(this, "changeCount").unwrap_or(0) + 1;
    property_set(this, "changeCount", count);
}

fn first_value(this: Id, kind: Option<&str>) -> Id {
    let list = items(this);
    for item in collections::array_items(list) {
        for (key, value) in collections::dictionary_pairs(item) {
            if kind.is_none_or(|k| string::get(key) == k) {
                return value;
            }
        }
    }
    0
}

fn single(kind: &str, value: Id) -> Id {
    let entry = collections::dictionary_from(&[(string::make(kind), value)]);
    collections::array_from(&[entry])
}

extern "C" fn pasteboard_name(this: Id, _: Sel) -> Id {
    property_get(this, "name").unwrap_or(0) as Id
}

extern "C" fn pasteboard_items(this: Id, _: Sel) -> Id {
    let list = items(this);
    if list == 0 { collections::array_from(&[]) } else { list }
}

extern "C" fn pasteboard_set_items(this: Id, _: Sel, list: Id) {
    set_items(this, list);
}

extern "C" fn pasteboard_string(this: Id, _: Sel) -> Id {
    let value = first_value(this, Some("public.utf8-plain-text"));
    if value != 0 && string::is_string(value) { value } else { 0 }
}

extern "C" fn pasteboard_set_string(this: Id, _: Sel, text: Id) {
    set_items(this, if text == 0 { 0 } else { single("public.utf8-plain-text", runtime::send(text, "copy", &[])) });
}

extern "C" fn pasteboard_data_for_type(this: Id, _: Sel, kind: Id) -> Id {
    first_value(this, Some(&string::get(kind)))
}

extern "C" fn pasteboard_set_data(this: Id, _: Sel, value: Id, kind: Id) {
    set_items(this, if value == 0 { 0 } else { single(&string::get(kind), value) });
}

extern "C" fn pasteboard_change_count(this: Id, _: Sel) -> isize {
    property_get(this, "changeCount").unwrap_or(0) as isize
}

extern "C" fn pasteboard_number_of_items(this: Id, _: Sel) -> isize {
    collections::array_items(items(this)).len() as isize
}

extern "C" fn ignore_bool(_this: Id, _: Sel, _value: u8) {}

// ----- CMMotionManager: no motion sensors offered yet -----

extern "C" fn no(_this: Id, _: Sel) -> bool {
    false
}

extern "C" fn nothing(_this: Id, _: Sel) -> Id {
    0
}

extern "C" fn ignore_interval(_this: Id, _: Sel, _seconds: f64) {}

extern "C" fn motion_start(_this: Id, s: Sel) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    let name = runtime::lock().sel_name(s);
    ONCE.call_once(|| crate::log!("motion: -[CMMotionManager {name}]: no motion sensors offered yet; the game is told none are there"));
}

extern "C" fn motion_start_with_handler(this: Id, s: Sel, _queue: Id, _handler: Id) {
    motion_start(this, s);
}

extern "C" fn motion_start_with_frame(this: Id, s: Sel, _frame: usize) {
    motion_start(this, s);
}

extern "C" fn motion_start_with_frame_handler(this: Id, s: Sel, _frame: usize, _queue: Id, _handler: Id) {
    motion_start(this, s);
}

extern "C" fn ignore(_this: Id, _: Sel) {}

pub fn register(_: &mut Registry) {
    let archiver = define_class("NSKeyedArchiver", "NSObject", 8);
    class_methods(
        archiver,
        &[
            ("archiveRootObject:toFile:", archive_root_object as usize),
            ("archivedDataWithRootObject:", archived_data_with_root_object as usize),
        ],
    );
    let unarchiver = define_class("NSKeyedUnarchiver", "NSObject", 8);
    class_methods(
        unarchiver,
        &[
            ("unarchiveObjectWithFile:", unarchive_object_with_file as usize),
            ("unarchiveObjectWithData:", unarchive_object_with_data as usize),
        ],
    );

    let zone = define_class("NSTimeZone", "NSObject", 8);
    class_methods(
        zone,
        &[
            ("localTimeZone", time_zone_local as usize),
            ("systemTimeZone", time_zone_local as usize),
            ("defaultTimeZone", time_zone_local as usize),
            ("timeZoneForSecondsFromGMT:", time_zone_for_seconds as usize),
            ("timeZoneWithName:", time_zone_with_name as usize),
            ("timeZoneWithAbbreviation:", time_zone_with_name as usize),
        ],
    );
    methods(
        zone,
        &[
            ("secondsFromGMT", time_zone_seconds_from_gmt as usize),
            ("secondsFromGMTForDate:", time_zone_seconds_from_gmt_for_date as usize),
            ("name", time_zone_name as usize),
            ("description", time_zone_name as usize),
            ("abbreviation", time_zone_abbreviation as usize),
            ("isDaylightSavingTime", time_zone_is_daylight_saving as usize),
        ],
    );

    let pasteboard = define_class("UIPasteboard", "NSObject", 8);
    class_methods(
        pasteboard,
        &[
            ("generalPasteboard", pasteboard_general as usize),
            ("pasteboardWithName:create:", pasteboard_with_name as usize),
            ("pasteboardWithUniqueName", pasteboard_with_unique_name as usize),
            ("removePasteboardWithName:", pasteboard_remove as usize),
        ],
    );
    methods(
        pasteboard,
        &[
            ("name", pasteboard_name as usize),
            ("items", pasteboard_items as usize),
            ("setItems:", pasteboard_set_items as usize),
            ("string", pasteboard_string as usize),
            ("setString:", pasteboard_set_string as usize),
            ("dataForPasteboardType:", pasteboard_data_for_type as usize),
            ("valueForPasteboardType:", pasteboard_data_for_type as usize),
            ("setData:forPasteboardType:", pasteboard_set_data as usize),
            ("setValue:forPasteboardType:", pasteboard_set_data as usize),
            ("changeCount", pasteboard_change_count as usize),
            ("numberOfItems", pasteboard_number_of_items as usize),
            ("setPersistent:", ignore_bool as usize),
            ("isPersistent", no as usize),
        ],
    );

    let motion = define_class("CMMotionManager", "NSObject", 8);
    let mut list: Vec<(&str, usize)> = vec![
        ("isDeviceMotionAvailable", no as usize),
        ("isAccelerometerAvailable", no as usize),
        ("isGyroAvailable", no as usize),
        ("isMagnetometerAvailable", no as usize),
        ("isDeviceMotionActive", no as usize),
        ("isAccelerometerActive", no as usize),
        ("isGyroActive", no as usize),
        ("isMagnetometerActive", no as usize),
        ("deviceMotion", nothing as usize),
        ("accelerometerData", nothing as usize),
        ("gyroData", nothing as usize),
        ("magnetometerData", nothing as usize),
        ("setDeviceMotionUpdateInterval:", ignore_interval as usize),
        ("setAccelerometerUpdateInterval:", ignore_interval as usize),
        ("setGyroUpdateInterval:", ignore_interval as usize),
        ("setMagnetometerUpdateInterval:", ignore_interval as usize),
        ("startDeviceMotionUpdatesToQueue:withHandler:", motion_start_with_handler as usize),
        ("startAccelerometerUpdatesToQueue:withHandler:", motion_start_with_handler as usize),
        ("startGyroUpdatesToQueue:withHandler:", motion_start_with_handler as usize),
        ("startDeviceMotionUpdatesUsingReferenceFrame:", motion_start_with_frame as usize),
        ("startDeviceMotionUpdatesUsingReferenceFrame:toQueue:withHandler:", motion_start_with_frame_handler as usize),
    ];
    for name in ["startDeviceMotionUpdates", "startAccelerometerUpdates", "startGyroUpdates", "startMagnetometerUpdates"] {
        list.push((name, motion_start as usize));
    }
    for name in ["stopDeviceMotionUpdates", "stopAccelerometerUpdates", "stopGyroUpdates", "stopMagnetometerUpdates"] {
        list.push((name, ignore as usize));
    }
    methods(motion, &list);
}
