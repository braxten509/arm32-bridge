//! UIKit basics: device, screen, views, windows, view controllers, layers,
//! colours and images. Nothing is drawn yet; views only keep their state.

use crate::fw::geometry::{Point, Rect, Size, Transform, IDENTITY};
use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::{collections, string};
use crate::objc::runtime::{self, lock, Class, Id, Sel};
use crate::symbols::{leak, Registry};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

// ----- simple stored properties -----

static PROPERTIES: Mutex<Option<HashMap<(Id, &'static str), u64>>> = Mutex::new(None);
static CONTROLLER_CHILDREN: Mutex<Option<HashMap<Id, Vec<Id>>>> = Mutex::new(None);

pub fn property_get(object: Id, key: &'static str) -> Option<u64> {
    PROPERTIES.lock().unwrap().as_ref().and_then(|m| m.get(&(object, key)).copied())
}

pub fn property_set(object: Id, key: &'static str, value: u64) {
    PROPERTIES.lock().unwrap().get_or_insert_with(HashMap::new).insert((object, key), value);
}

/// Sets an object property: like UIKit's, it retains what it holds (this
/// matters for objects with real counts: pictures, fw/counted.rs).
pub fn property_set_object(object: Id, key: &'static str, value: Id) {
    let old = PROPERTIES.lock().unwrap().get_or_insert_with(HashMap::new).insert((object, key), value as u64);
    crate::fw::counted::replace(old.unwrap_or(0) as Id, value);
}

/// Makes a getter and setter pair that keep a value per object.
macro_rules! property {
    ($get:ident, $set:ident, $key:literal, id, $default:expr) => {
        extern "C" fn $get(this: Id, _: Sel) -> Id {
            property_get(this, $key).map(|v| v as Id).unwrap_or($default)
        }
        extern "C" fn $set(this: Id, _: Sel, value: Id) {
            $crate::fw::uikit::property_set_object(this, $key, value);
        }
    };
    ($get:ident, $set:ident, $key:literal, bool, $default:expr) => {
        extern "C" fn $get(this: Id, _: Sel) -> bool {
            property_get(this, $key).map(|v| v != 0).unwrap_or($default)
        }
        extern "C" fn $set(this: Id, _: Sel, value: u8) {
            property_set(this, $key, (value != 0) as u64);
        }
    };
    ($get:ident, $set:ident, $key:literal, f64, $default:expr) => {
        extern "C" fn $get(this: Id, _: Sel) -> f64 {
            property_get(this, $key).map(f64::from_bits).unwrap_or($default)
        }
        extern "C" fn $set(this: Id, _: Sel, value: f64) {
            property_set(this, $key, value.to_bits());
        }
    };
    ($get:ident, $set:ident, $key:literal, isize, $default:expr) => {
        extern "C" fn $get(this: Id, _: Sel) -> isize {
            property_get(this, $key).map(|v| v as isize).unwrap_or($default)
        }
        extern "C" fn $set(this: Id, _: Sel, value: isize) {
            property_set(this, $key, value as u64);
        }
    };
}
pub(crate) use property;

extern "C" fn ignore_bool(_this: Id, _: Sel, _value: u8) {}
extern "C" fn ignore_id(_this: Id, _: Sel, _value: Id) {}
extern "C" fn ignore_id_bool(_this: Id, _: Sel, _value: Id, _flag: u8) {}
extern "C" fn ignore_bool_bool(_this: Id, _: Sel, _value: u8, _flag: u8) {}
extern "C" fn ignore_int(_this: Id, _: Sel, _value: isize) {}
extern "C" fn ignore_int_bool(_this: Id, _: Sel, _value: isize, _flag: u8) {}
extern "C" fn nothing(_this: Id, _: Sel) {}
extern "C" fn yes(_this: Id, _: Sel) -> bool {
    true
}
extern "C" fn no(_this: Id, _: Sel) -> bool {
    false
}
extern "C" fn no_with(_this: Id, _: Sel, _value: Id) -> bool {
    false
}
extern "C" fn zero(_this: Id, _: Sel) -> usize {
    0
}
extern "C" fn return_self(this: Id, _: Sel) -> Id {
    this
}

// ----- screen and device -----

fn app_info() -> Option<&'static crate::fw::plist::Value> {
    static INFO: OnceLock<Option<crate::fw::plist::Value>> = OnceLock::new();
    INFO.get_or_init(|| {
        let app_dir = crate::loader::try_app_dir()?;
        std::fs::read(format!("{app_dir}/Info.plist")).ok().and_then(|bytes| crate::fw::plist::parse(&bytes))
    })
    .as_ref()
}

/// The screen the game is told it has, in the interface's orientation, in
/// points, and its scale. IOS_SCREEN=568x320@2 sets the size outright (an
/// old portrait app still gets it turned upright by the old-iPhone rule).
/// Otherwise, with the real display known (IOS_DISPLAY=2424x1080, set by the
/// phone app), the game gets the shape iOS would give it there, drawn at the
/// display's real pixels. Neither: an iPhone 5s (568x320 points at 2x).
pub fn screen_points() -> (f64, f64, f64) {
    geometry_for(raw_screen_points(), app_info()).oriented
}

/// The screen the app would get in landscape, before the old-iPhone rule
/// turns it to the interface's orientation.
fn raw_screen_points() -> (f64, f64, f64) {
    static SCREEN: OnceLock<(f64, f64, f64)> = OnceLock::new();
    *SCREEN.get_or_init(|| {
        if let Ok(text) = std::env::var("IOS_SCREEN") {
            let (size, scale) = text.split_once('@').unwrap_or((&text, "2"));
            let (w, h) = size.split_once('x').unwrap_or(("568", "320"));
            return (w.parse().unwrap_or(568.0), h.parse().unwrap_or(320.0), scale.parse().unwrap_or(2.0));
        }
        let Some(display) = display() else {
            return (568.0, 320.0, SCALE);
        };
        let app_dir = crate::loader::app_dir();
        let info = app_info();
        let files: Vec<String> = std::fs::read_dir(&app_dir)
            .map(|list| list.filter_map(|e| e.ok()?.file_name().into_string().ok()).collect())
            .unwrap_or_default();
        let unreal = std::path::Path::new(&format!("{app_dir}/CookedIPhone")).is_dir();
        let screen = told_screen(display, info, &files, unreal);
        let (w, h, scale) = screen;
        crate::log!("screen: display {}x{}, told {w}x{h} landscape points at {scale}x", display.0, display.1);
        screen
    })
}

/// The real display's size in pixels (IOS_DISPLAY, set by the phone app).
pub fn display() -> Option<(f64, f64)> {
    let text = std::env::var("IOS_DISPLAY").ok()?;
    let (w, h) = text.split_once('x')?;
    Some((w.trim().parse::<f64>().ok()?, h.trim().parse::<f64>().ok()?)).filter(|&(w, h)| w >= 1.0 && h >= 1.0)
}

/// The screen an app is told it has on this display (points, scale).
/// Apps with a launch storyboard fill the display at its own pixels, and
/// so do Unreal games (`unreal`: the app has CookedIPhone/), which draw well
/// at any size. Other old apps get the screen of a real iPhone of their
/// shape (an iPhone 5's 568x320 for Default-568h), as iOS gives them on
/// newer iPhones: some only know those exact sizes (TownCraft says "unknown
/// device" and draws nothing at others). The window stretches the picture.
fn told_screen(display: (f64, f64), info: Option<&crate::fw::plist::Value>, files: &[String], unreal: bool) -> (f64, f64, f64) {
    let aspect = app_aspect(display, info, files);
    if has_storyboard(info) || unreal {
        let (w, h) = fit_points(display, aspect);
        return (w, h, SCALE);
    }
    let (long, short) = launch_sizes(info, files)
        .into_iter()
        .filter(|&(long, short)| (long / short - aspect).abs() < 1e-9)
        .fold((480.0, 320.0), |best, size| if size.0 > best.0 { size } else { best });
    // The Plus and X iPhones (736 and 812 points long) are 3x screens.
    let scale = if long == 736.0 || long == 812.0 { 3.0 } else { 2.0 };
    (long, short, scale)
}

/// The scale screens are reported at when they fill the display (an
/// iPhone 5s's).
const SCALE: f64 = 2.0;

fn plist_text<'a>(info: Option<&'a crate::fw::plist::Value>, key: &str) -> Option<&'a str> {
    match info.and_then(|value| value.get(key)) {
        Some(crate::fw::plist::Value::String(text)) => Some(text),
        _ => None,
    }
}

fn sdk_major(text: &str) -> Option<u32> {
    text.strip_prefix("iphoneos").unwrap_or(text).split('.').next()?.parse().ok()
}

fn legacy_screen(info: Option<&crate::fw::plist::Value>) -> bool {
    plist_text(info, "DTSDKName")
        .and_then(sdk_major)
        .or_else(|| plist_text(info, "DTPlatformVersion").and_then(sdk_major))
        .is_some_and(|major| major <= 7)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Interface {
    orientation: isize,
    mask: usize,
}

fn interface(info: Option<&crate::fw::plist::Value>, legacy: bool) -> Interface {
    const LANDSCAPE: Interface = Interface { orientation: 3, mask: (1 << 3) | (1 << 4) };
    if !legacy {
        return LANDSCAPE;
    }
    let orientation_number = |name: &str| match name {
        "UIInterfaceOrientationPortrait" => Some(1),
        "UIInterfaceOrientationPortraitUpsideDown" => Some(2),
        "UIInterfaceOrientationLandscapeRight" => Some(3),
        "UIInterfaceOrientationLandscapeLeft" => Some(4),
        _ => None,
    };
    let declared = ["UISupportedInterfaceOrientations~iphone", "UISupportedInterfaceOrientations"].into_iter().find_map(|key| {
        match info.and_then(|value| value.get(key)) {
            Some(crate::fw::plist::Value::Array(values)) => Some(values),
            _ => None,
        }
    });
    let initial = plist_text(info, "UIInterfaceOrientation").and_then(orientation_number);
    let mut orientations: Vec<isize> = declared
        .into_iter()
        .flatten()
        .filter_map(|value| match value {
            crate::fw::plist::Value::String(name) => orientation_number(name),
            _ => None,
        })
        .collect();
    if orientations.is_empty() {
        orientations.extend(initial);
    }
    if orientations.is_empty() {
        return LANDSCAPE;
    }
    let mask = orientations.iter().fold(0, |mask, orientation| mask | (1 << orientation));
    let orientation = initial.filter(|orientation| orientations.contains(orientation)).unwrap_or(orientations[0]);
    Interface { orientation, mask }
}

fn current_interface() -> Interface {
    current_geometry().interface
}

fn oriented_screen(screen: (f64, f64, f64), info: Option<&crate::fw::plist::Value>) -> (f64, f64, f64) {
    if !legacy_screen(info) {
        return screen;
    }
    let (long, short) = (screen.0.max(screen.1), screen.0.min(screen.1));
    match interface(info, true).orientation {
        1 | 2 => (short, long, screen.2),
        _ => (long, short, screen.2),
    }
}

fn reported_screen(screen: (f64, f64, f64), info: Option<&crate::fw::plist::Value>) -> (f64, f64, f64) {
    if legacy_screen(info) { (screen.0.min(screen.1), screen.0.max(screen.1), screen.2) } else { screen }
}

fn screen_pixel_size(screen: (f64, f64, f64)) -> (f64, f64) {
    (screen.0.min(screen.1) * screen.2, screen.0.max(screen.1) * screen.2)
}

/// A game's screen shapes and orientation, decided from its Info.plist and
/// the display.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Geometry {
    /// What the game draws and touches: the screen in the interface's orientation.
    oriented: (f64, f64, f64),
    /// What UIScreen reports. Old iPhones always gave portrait, even in a
    /// landscape game.
    reported: (f64, f64, f64),
    legacy: bool,
    interface: Interface,
}

fn geometry_for(screen: (f64, f64, f64), info: Option<&crate::fw::plist::Value>) -> Geometry {
    let oriented = oriented_screen(screen, info);
    Geometry { oriented, reported: reported_screen(oriented, info), legacy: legacy_screen(info), interface: interface(info, legacy_screen(info)) }
}

fn current_geometry() -> Geometry {
    static GEOMETRY: OnceLock<Geometry> = OnceLock::new();
    *GEOMETRY.get_or_init(|| geometry_for(raw_screen_points(), app_info()))
}

/// A window's bounds in the interface's orientation. A full-screen window
/// made from UIScreen.bounds (portrait in an old app) becomes landscape for
/// a landscape interface, so reading the bounds and setting them back is
/// harmless; any other size stays as the game set it.
fn window_frame(geometry: Geometry, rect: Rect) -> Rect {
    if !geometry.legacy {
        return rect;
    }
    let (wide, high, _) = geometry.oriented;
    let (long, short, _) = geometry.reported;
    let (width, height) = (rect.size.width, rect.size.height);
    if (width == wide && height == high) || (width == long && height == short) {
        Rect { origin: rect.origin, size: Size { width: wide, height: high } }
    } else {
        rect
    }
}

fn has_storyboard(info: Option<&crate::fw::plist::Value>) -> bool {
    let has = |key: &str| info.and_then(|i| i.get(key)).is_some();
    has("UILaunchStoryboardName") || has("UILaunchStoryboardName~iphone")
}

/// The iPhone screens (long side, short side in points) an old app ships
/// launch images for (Default-568h@2x.png, or UILaunchImages sizes), plus
/// the 3.5-inch iPhones' 480x320 every app can run on.
fn launch_sizes(info: Option<&crate::fw::plist::Value>, files: &[String]) -> Vec<(f64, f64)> {
    use crate::fw::plist::Value;
    let mut sizes: Vec<(f64, f64)> = vec![(480.0, 320.0)];
    // Default-568h@2x.png style names: the long side in points, iPhones
    // with that height have these short sides.
    for name in files {
        let Some(rest) = name.strip_prefix("Default-") else { continue };
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !rest[digits.len()..].starts_with('h') {
            continue;
        }
        let long: f64 = digits.parse().unwrap_or(0.0);
        let short = match long as u32 {
            568 => 320.0,
            667 | 812 => 375.0,
            736 | 896 => 414.0,
            _ => continue,
        };
        sizes.push((long, short));
    }
    // UILaunchImages: [{UILaunchImageSize: "{320, 568}", ...}, ...]
    for key in ["UILaunchImages", "UILaunchImages~iphone"] {
        let Some(Value::Array(images)) = info.and_then(|i| i.get(key)) else { continue };
        for image in images {
            let Some(Value::String(size)) = image.get("UILaunchImageSize") else { continue };
            let numbers: Vec<f64> = size
                .trim_matches(|c| c == '{' || c == '}')
                .split(',')
                .filter_map(|n| n.trim().parse().ok())
                .collect();
            if let [a, b] = numbers[..] {
                if a > 0.0 && b > 0.0 {
                    sizes.push((a.max(b), a.min(b)));
                }
            }
        }
    }
    sizes
}

/// The widest landscape shape (long side / short side) this app said it
/// can fill, as iOS decides it: an app with a launch storyboard fills any
/// screen; otherwise it gets the tallest iPhone launch image it ships, and
/// with none a 3:2 screen (the 3.5-inch iPhones), with black bars around it.
fn app_aspect(display: (f64, f64), info: Option<&crate::fw::plist::Value>, files: &[String]) -> f64 {
    let display_aspect = display.0.max(display.1) / display.0.min(display.1);
    if has_storyboard(info) {
        return display_aspect;
    }
    let shapes: Vec<f64> = launch_sizes(info, files).into_iter().map(|(long, short)| long / short).collect();
    // The widest one that fits; on a squarer display, the narrowest one
    // (fitted by width, bars above and below).
    let fits = shapes.iter().copied().filter(|&s| s <= display_aspect + 0.001).fold(0.0, f64::max);
    if fits > 0.0 { fits } else { shapes.into_iter().fold(f64::INFINITY, f64::min) }
}

/// The largest size of this shape that fits the display, in points at
/// SCALE, so the game draws at the display's own pixels.
fn fit_points(display: (f64, f64), aspect: f64) -> (f64, f64) {
    let (long, short) = (display.0.max(display.1) / SCALE, display.0.min(display.1) / SCALE);
    if long / short >= aspect {
        ((short * aspect).floor(), short.floor())
    } else {
        (long.floor(), (long / aspect).floor())
    }
}

fn screen_bounds() -> Rect {
    let (w, h, _) = current_geometry().reported;
    Rect::new(0.0, 0.0, w, h)
}

fn oriented_bounds() -> Rect {
    let (w, h, _) = current_geometry().oriented;
    Rect::new(0.0, 0.0, w, h)
}

static MAIN_SCREEN: OnceLock<Id> = OnceLock::new();
static CURRENT_DEVICE: OnceLock<Id> = OnceLock::new();

extern "C" fn main_screen(_: Class, _: Sel) -> Id {
    *MAIN_SCREEN.get_or_init(|| runtime::alloc_instance(runtime::class_named("UIScreen")))
}

extern "C" fn screens(class: Class, s: Sel) -> Id {
    collections::array_from(&[main_screen(class, s)])
}

extern "C" fn screen_bounds_method(_this: Id, _: Sel) -> Rect {
    screen_bounds()
}

extern "C" fn screen_native_bounds(_this: Id, _: Sel) -> Rect {
    let (w, h) = screen_pixel_size(screen_points());
    Rect::new(0.0, 0.0, w, h)
}

extern "C" fn screen_scale(_this: Id, _: Sel) -> f64 {
    screen_points().2
}

extern "C" fn screen_current_mode(_this: Id, _: Sel) -> Id {
    static MODE: OnceLock<Id> = OnceLock::new();
    *MODE.get_or_init(|| runtime::alloc_instance(runtime::class_named("UIScreenMode")))
}

extern "C" fn screen_mode_size(_this: Id, _: Sel) -> Size {
    let (width, height) = screen_pixel_size(screen_points());
    Size { width, height }
}

extern "C" fn screen_available_modes(this: Id, s: Sel) -> Id {
    collections::array_from(&[screen_current_mode(this, s)])
}

property!(screen_overscan, screen_set_overscan, "overscanCompensation", isize, 0);

extern "C" fn current_device(_: Class, _: Sel) -> Id {
    *CURRENT_DEVICE.get_or_init(|| runtime::alloc_instance(runtime::class_named("UIDevice")))
}

extern "C" fn device_model(_this: Id, _: Sel) -> Id {
    let model = crate::libsystem::io::device_model();
    string::make(if model.starts_with("iPad") { "iPad" } else if model.starts_with("iPod") { "iPod touch" } else { "iPhone" })
}

extern "C" fn device_system_name(_this: Id, _: Sel) -> Id {
    string::make("iPhone OS")
}

extern "C" fn device_system_version(_this: Id, _: Sel) -> Id {
    string::make("8.2")
}

extern "C" fn device_name(_this: Id, _: Sel) -> Id {
    string::make("Game Bridge")
}

extern "C" fn device_idiom(_this: Id, _: Sel) -> isize {
    if crate::libsystem::io::device_model().starts_with("iPad") { 1 } else { 0 }
}

/// Landscape, home button on the right (matches UIInterfaceOrientationLandscapeRight).
/// UIKit gives the interface's landscape values the device's numbers (its
/// header maps UIInterfaceOrientationLandscapeRight to UIDeviceOrientationLandscapeLeft,
/// both 3), so the same number is right; only the names are the other way round.
extern "C" fn device_orientation(_this: Id, _: Sel) -> isize {
    current_interface().orientation
}

extern "C" fn device_battery_level(_this: Id, _: Sel) -> f32 {
    1.0
}

extern "C" fn device_battery_state(_this: Id, _: Sel) -> isize {
    3
}

fn stable_id() -> String {
    let path = format!("{}/Library/device-id", crate::fw::foundation::home());
    if let Ok(text) = std::fs::read_to_string(&path) {
        if text.trim().len() == 36 {
            return text.trim().to_string();
        }
    }
    let mut bytes = [0u8; 16];
    unsafe { libc::getrandom(bytes.as_mut_ptr() as *mut _, 16, 0) };
    bytes[6] = (bytes[6] & 0x0F) | 0x40;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02X}")).collect();
    let text = format!(
        "{}-{}-{}-{}-{}",
        hex[0..4].concat(),
        hex[4..6].concat(),
        hex[6..8].concat(),
        hex[8..10].concat(),
        hex[10..16].concat()
    );
    let _ = std::fs::write(&path, &text);
    text
}

extern "C" fn device_unique_identifier(_this: Id, _: Sel) -> Id {
    string::make(&stable_id().replace('-', "").to_lowercase())
}

extern "C" fn device_identifier_for_vendor(_this: Id, _: Sel) -> Id {
    crate::fw::misc::uuid_from_string(&stable_id())
}

// ----- view state: { isa, *mut ViewState } -----

pub struct ViewState {
    pub frame: Rect,
    pub bounds_origin: Point,
    pub superview: Id,
    pub subviews: Vec<Id>,
    pub layer: Id,
    pub transform: Transform,
    pub window: Id,
    /// Marked for -layoutSubviews in the run loop's next layout pass.
    pub needs_layout: bool,
}

pub fn view_state(view: Id) -> &'static mut ViewState {
    unsafe {
        let slot = (view + 8) as *mut *mut ViewState;
        if (*slot).is_null() {
            *slot = Box::into_raw(Box::new(ViewState {
                frame: Rect::default(),
                bounds_origin: Point::default(),
                superview: 0,
                subviews: Vec::new(),
                layer: 0,
                transform: IDENTITY,
                window: 0,
                needs_layout: false,
            }));
        }
        &mut **slot
    }
}

property!(view_hidden, view_set_hidden, "hidden", bool, false);
property!(view_alpha, view_set_alpha, "alpha", f64, 1.0);
property!(view_user_interaction, view_set_user_interaction, "userInteraction", bool, true);

extern "C" fn image_view_user_interaction(this: Id, _: Sel) -> bool {
    property_get(this, "userInteraction").is_some_and(|v| v != 0)
}
property!(view_multiple_touch, view_set_multiple_touch, "multipleTouch", bool, false);
property!(view_exclusive_touch, view_set_exclusive_touch, "exclusiveTouch", bool, false);
property!(view_opaque, view_set_opaque, "opaque", bool, true);
property!(view_clips, view_set_clips, "clips", bool, false);
property!(view_tag, view_set_tag, "tag", isize, 0);
property!(view_autoresizing, view_set_autoresizing, "autoresizing", isize, 0);
property!(view_autoresizes, view_set_autoresizes, "autoresizes", bool, true);
property!(view_content_mode, view_set_content_mode, "contentMode", isize, 0);
property!(view_background, view_set_background, "background", id, 0);

extern "C" fn view_content_scale(this: Id, _: Sel) -> f64 {
    property_get(this, "contentScale").map(f64::from_bits).unwrap_or(1.0)
}

extern "C" fn view_set_content_scale(this: Id, _: Sel, value: f64) {
    property_set(this, "contentScale", value.to_bits());
    let layer = view_state(this).layer;
    if layer != 0 {
        property_set(layer, "contentsScale", value.to_bits());
    }
}

extern "C" fn layer_class(_: Class, _: Sel) -> Class {
    runtime::class_named("CALayer")
}

fn make_layer_for(view: Id) {
    let class = lock().class_of(view);
    let layer_class = runtime::send(class, "layerClass", &[]);
    let layer = runtime::send(runtime::send(layer_class, "alloc", &[]), "init", &[]);
    property_set(layer, "delegate", view as u64);
    view_state(view).layer = layer;
}

extern "C" fn view_init_with_frame(this: Id, _: Sel, frame: Rect) -> Id {
    view_state(this).frame = frame;
    make_layer_for(this);
    this
}

extern "C" fn view_init(this: Id, _: Sel) -> Id {
    let frame = Rect::default();
    let s = runtime::sel("initWithFrame:");
    let imp = runtime::imp_for(this, s).unwrap();
    let f: extern "C" fn(Id, Sel, Rect) -> Id = unsafe { std::mem::transmute(imp) };
    f(this, s, frame)
}

extern "C" fn view_frame(this: Id, _: Sel) -> Rect {
    view_state(this).frame
}

extern "C" fn view_set_frame(this: Id, _: Sel, frame: Rect) {
    let state = view_state(this);
    let old = state.frame.size;
    state.frame = frame;
    if old != frame.size {
        resized_view(this, old);
    }
}

/// Sends a method taking a CGRect (setFrame:, setBounds:), so the game's
/// own versions run.
pub(crate) fn send_rect(view: Id, name: &str, rect: Rect) {
    let s = runtime::sel(name);
    if let Some(imp) = runtime::imp_for(view, s) {
        let f: extern "C" fn(Id, Sel, Rect) = unsafe { std::mem::transmute(imp) };
        f(view, s, rect);
    }
}

extern "C" fn view_bounds(this: Id, _: Sel) -> Rect {
    let state = view_state(this);
    Rect { origin: state.bounds_origin, size: state.frame.size }
}

extern "C" fn window_bounds(this: Id, selector: Sel) -> Rect {
    window_reported_bounds(current_geometry(), view_bounds(this, selector))
}

fn window_reported_bounds(geometry: Geometry, bounds: Rect) -> Rect {
    let (wide, high, _) = geometry.oriented;
    let (short, long, _) = geometry.reported;
    if geometry.legacy && bounds.size.width == wide && bounds.size.height == high {
        Rect { origin: bounds.origin, size: Size { width: short, height: long } }
    } else {
        bounds
    }
}

extern "C" fn window_set_bounds(this: Id, selector: Sel, bounds: Rect) {
    view_set_bounds(this, selector, window_frame(current_geometry(), bounds));
}

extern "C" fn window_init_with_frame(this: Id, selector: Sel, frame: Rect) -> Id {
    view_init_with_frame(this, selector, window_frame(current_geometry(), frame))
}

extern "C" fn window_set_frame(this: Id, selector: Sel, frame: Rect) {
    view_set_frame(this, selector, window_frame(current_geometry(), frame));
}

/// New bounds keep the view's center (its layer's position, at the layer's
/// anchor point) where it is.
extern "C" fn view_set_bounds(this: Id, _: Sel, bounds: Rect) {
    let center = view_center(this, 0);
    let anchor = crate::fw::layers::view_anchor(this);
    let state = view_state(this);
    let old = state.frame.size;
    state.bounds_origin = bounds.origin;
    state.frame.size = bounds.size;
    state.frame.origin = Point { x: center.x - bounds.size.width * anchor.x, y: center.y - bounds.size.height * anchor.y };
    if old != bounds.size {
        resized_view(this, old);
    }
}

/// UIKit's center is the layer's position: the anchor point (the middle
/// unless the game moved it) in the superview.
extern "C" fn view_center(this: Id, _: Sel) -> Point {
    let anchor = crate::fw::layers::view_anchor(this);
    let f = view_state(this).frame;
    Point { x: f.origin.x + f.size.width * anchor.x, y: f.origin.y + f.size.height * anchor.y }
}

extern "C" fn view_set_center(this: Id, _: Sel, center: Point) {
    let anchor = crate::fw::layers::view_anchor(this);
    let state = view_state(this);
    state.frame.origin = Point { x: center.x - state.frame.size.width * anchor.x, y: center.y - state.frame.size.height * anchor.y };
}

extern "C" fn view_transform(this: Id, _: Sel) -> Transform {
    view_state(this).transform
}

extern "C" fn view_set_transform(this: Id, _: Sel, transform: Transform) {
    view_state(this).transform = transform;
}

extern "C" fn view_layer(this: Id, _: Sel) -> Id {
    if view_state(this).layer == 0 {
        make_layer_for(this);
    }
    view_state(this).layer
}

// ----- layout: as in UIKit, views marked as needing layout (added to a
// superview, resized, setNeedsLayout) get -layoutSubviews once, superview
// first, in a pass the main run loop makes each turn before drawing (Core
// Animation's commit); -layoutIfNeeded makes it at once. A view's
// controller hears viewWillLayoutSubviews / viewDidLayoutSubviews around it.

/// Some view was marked since the last pass.
static LAYOUT_PENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn set_needs_layout(view: Id) {
    if view != 0 {
        view_state(view).needs_layout = true;
        LAYOUT_PENDING.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// A new size lays out the view and its superview, and (as UIKit does at
/// once) moves and sizes its subviews by their autoresizing masks.
fn resized_view(view: Id, old: Size) {
    set_needs_layout(view);
    set_needs_layout(view_state(view).superview);
    if property_get(view, "autoresizes").is_none_or(|v| v != 0) {
        let new = view_state(view).frame.size;
        for child in view_state(view).subviews.clone() {
            let mask = property_get(child, "autoresizing").unwrap_or(0);
            if mask != 0 {
                let frame = autoresized(view_state(child).frame, mask, old, new);
                send_rect(child, "setFrame:", frame);
            }
        }
    }
}

/// A subview's frame after its superview went from `old` to `new`
/// (UIViewAutoresizing: flexible left margin 1, width 2, right margin 4,
/// top margin 8, height 16, bottom margin 32). On each axis the flexible
/// parts share the change by their sizes (evenly when all are 0).
fn autoresized(frame: Rect, mask: u64, old: Size, new: Size) -> Rect {
    fn axis(start: f64, length: f64, old: f64, new: f64, flexible: [bool; 3]) -> (f64, f64) {
        let parts = [start, length, old - start - length];
        let flexible_total: f64 = (0..3).filter(|&i| flexible[i]).map(|i| parts[i]).sum();
        let count = flexible.iter().filter(|&&f| f).count() as f64;
        if count == 0.0 {
            return (start, length);
        }
        let change = new - old;
        let share = |i: usize| match (flexible[i], flexible_total != 0.0) {
            (false, _) => 0.0,
            (true, true) => change * parts[i] / flexible_total,
            (true, false) => change / count,
        };
        (start + share(0), length + share(1))
    }
    let (x, width) = axis(frame.origin.x, frame.size.width, old.width, new.width, [mask & 1 != 0, mask & 2 != 0, mask & 4 != 0]);
    let (y, height) = axis(frame.origin.y, frame.size.height, old.height, new.height, [mask & 8 != 0, mask & 16 != 0, mask & 32 != 0]);
    Rect::new(x, y, width, height)
}

fn lay_out(view: Id) {
    let controller = property_get(view, "viewController").unwrap_or(0) as Id;
    if controller != 0 {
        runtime::send(controller, "viewWillLayoutSubviews", &[]);
    }
    runtime::send(view, "layoutSubviews", &[]);
    if controller != 0 {
        runtime::send(controller, "viewDidLayoutSubviews", &[]);
    }
}

/// Lays out the marked views of a tree, each before its subviews (the
/// list is read after a view's own layout, which may change it).
fn lay_out_tree(view: Id, depth: usize) {
    if view == 0 || depth > 256 {
        return;
    }
    if std::mem::take(&mut view_state(view).needs_layout) {
        lay_out(view);
    }
    for child in view_state(view).subviews.clone() {
        lay_out_tree(child, depth + 1);
    }
}

/// The run loop's layout pass over the shown windows (nothing to do unless
/// a view was marked; views outside them wait until they join one).
pub fn layout_pass() {
    if LAYOUT_PENDING.swap(false, std::sync::atomic::Ordering::SeqCst) {
        for window in windows() {
            lay_out_tree(window, 0);
        }
    }
}

extern "C" fn view_set_needs_layout(this: Id, _: Sel) {
    set_needs_layout(this);
}

extern "C" fn view_layout_if_needed(this: Id, _: Sel) {
    lay_out_tree(this, 0);
}

fn window_of(view: Id) -> Id {
    let mut current = view;
    let window_class = runtime::class_named("UIWindow");
    for _ in 0..256 {
        if current == 0 {
            return 0;
        }
        let class = lock().class_of(current);
        if lock().is_subclass(class, window_class) {
            return current;
        }
        current = view_state(current).superview;
    }
    0
}

/// Tells `view` and every view in it about a move to another window
/// (`window` for willMoveToWindow:, None for didMoveToWindow).
fn tell_window_move(view: Id, window: Option<Id>) {
    match window {
        Some(window) => runtime::send(view, "willMoveToWindow:", &[window]),
        None => runtime::send(view, "didMoveToWindow", &[]),
    };
    for child in view_state(view).subviews.clone() {
        tell_window_move(child, window);
    }
}

fn detach(view: Id) {
    let parent = view_state(view).superview;
    if parent != 0 {
        view_state(parent).subviews.retain(|&v| v != view);
        view_state(view).superview = 0;
        set_needs_layout(parent);
    }
}

fn controller_child_frame(geometry: Geometry, parent_size: Size, child_frame: Rect, child_transform: Transform, linked: bool) -> Option<Rect> {
    let (wide, high, _) = geometry.oriented;
    let (short, long, _) = geometry.reported;
    let landscape_parent = parent_size.height == high && parent_size.width > high && parent_size.width <= wide;
    let full_screen_child = child_frame.origin.x == 0.0 && child_frame.origin.y == 0.0 && child_frame.size.width == short && child_frame.size.height == long;
    if linked && geometry.legacy && wide > high && landscape_parent && full_screen_child && child_transform == IDENTITY {
        Some(Rect::new(0.0, 0.0, wide, high))
    } else {
        None
    }
}

fn attach(parent: Id, view: Id, index: Option<usize>) {
    attach_with_geometry(parent, view, index, current_geometry());
}

fn attach_with_geometry(parent: Id, view: Id, index: Option<usize>, geometry: Geometry) {
    if view == 0 || parent == 0 {
        return;
    }
    // Controllers whose views come on screen or leave it with this one hear
    // it before (will) and after (did), as in UIKit.
    let (was_shown, will_show) = (on_screen(view), on_screen(parent));
    let leaving = if was_shown && !will_show { disappearing(view) } else { Vec::new() };
    let arriving = if !was_shown && will_show { appearing(view) } else { Vec::new() };
    // The move hooks (a view put back in its own superview only changes place).
    let old_parent = view_state(view).superview;
    let moving = old_parent != parent;
    let (old_window, new_window) = (if old_parent != 0 { window_of(old_parent) } else { 0 }, window_of(parent));
    if moving {
        if old_parent != 0 {
            runtime::send(old_parent, "willRemoveSubview:", &[view]);
        }
        runtime::send(view, "willMoveToSuperview:", &[parent]);
        if old_window != new_window {
            tell_window_move(view, Some(new_window));
        }
    }
    detach(view);
    let list = &mut view_state(parent).subviews;
    match index {
        Some(i) => list.insert(i.min(list.len()), view),
        None => list.push(view),
    }
    view_state(view).superview = parent;
    let parent_size = view_state(parent).frame.size;
    let child_frame = view_state(view).frame;
    let child_transform = view_state(view).transform;
    if let Some(frame) = controller_child_frame(geometry, parent_size, child_frame, child_transform, true) {
        let parent_controller = property_get(parent, "viewController").unwrap_or(0) as Id;
        if linked_controller_view(parent_controller, parent, view) {
            send_rect(view, "setFrame:", frame);
        }
    }
    set_needs_layout(view);
    set_needs_layout(parent);
    if moving {
        if old_window != new_window {
            tell_window_move(view, None);
        }
        runtime::send(view, "didMoveToSuperview", &[]);
        runtime::send(parent, "didAddSubview:", &[view]);
    }
    disappeared(leaving);
    appeared(arriving);
}

extern "C" fn view_add_subview(this: Id, _: Sel, view: Id) {
    attach(this, view, None);
}

extern "C" fn view_insert_subview(this: Id, _: Sel, view: Id, index: isize) {
    attach(this, view, Some(index.max(0) as usize));
}

extern "C" fn view_insert_above(this: Id, _: Sel, view: Id, sibling: Id) {
    let index = view_state(this).subviews.iter().position(|&v| v == sibling).map(|i| i + 1);
    attach(this, view, index);
}

extern "C" fn view_insert_below(this: Id, _: Sel, view: Id, sibling: Id) {
    let index = view_state(this).subviews.iter().position(|&v| v == sibling);
    attach(this, view, index);
}

extern "C" fn view_remove_from_superview(this: Id, _: Sel) {
    take_out(this);
    crate::fw::media::view_removed(this);
}

extern "C" fn view_bring_to_front(this: Id, _: Sel, view: Id) {
    let list = &mut view_state(this).subviews;
    if let Some(i) = list.iter().position(|&v| v == view) {
        list.remove(i);
        list.push(view);
    }
}

extern "C" fn view_send_to_back(this: Id, _: Sel, view: Id) {
    let list = &mut view_state(this).subviews;
    if let Some(i) = list.iter().position(|&v| v == view) {
        list.remove(i);
        list.insert(0, view);
    }
}

extern "C" fn view_subviews(this: Id, _: Sel) -> Id {
    collections::array_from(&view_state(this).subviews.clone())
}

extern "C" fn view_superview(this: Id, _: Sel) -> Id {
    view_state(this).superview
}

extern "C" fn view_window(this: Id, _: Sel) -> Id {
    window_of(this)
}

/// Whether the view is `other` or somewhere inside it.
extern "C" fn view_is_descendant(this: Id, _: Sel, other: Id) -> bool {
    let mut current = this;
    for _ in 0..256 {
        if current == 0 || other == 0 {
            return false;
        }
        if current == other {
            return true;
        }
        current = view_state(current).superview;
    }
    false
}

extern "C" fn view_with_tag(this: Id, _: Sel, tag: isize) -> Id {
    fn search(view: Id, tag: isize) -> Id {
        if property_get(view, "tag").map(|v| v as isize).unwrap_or(0) == tag {
            return view;
        }
        for child in view_state(view).subviews.clone() {
            let found = search(child, tag);
            if found != 0 {
                return found;
            }
        }
        0
    }
    search(this, tag)
}

extern "C" fn view_size_that_fits(this: Id, _: Sel, _size: Size) -> Size {
    view_state(this).frame.size
}

extern "C" fn view_convert_point(_this: Id, _: Sel, point: Point, _other: Id) -> Point {
    point
}

extern "C" fn view_convert_rect(_this: Id, _: Sel, rect: Rect, _other: Id) -> Rect {
    rect
}

// ----- block animations: nothing moves on screen here, so the changes the
// animations block makes take effect at once and the completion block is
// told the animation finished (YES) on the next turn of the main run loop,
// as UIKit calls it after the animation. -----

/// Runs `animations` now and hands the completion call to `later`.
fn animate_with(animations: usize, completion: usize, later: impl FnOnce(Box<dyn FnOnce() + Send>)) {
    if animations != 0 {
        unsafe { crate::libsystem::mach::call_block(animations) };
    }
    if completion != 0 {
        let block = crate::libsystem::mach::block_copy(completion as *mut _) as usize;
        later(Box::new(move || {
            let header = block as *const crate::libsystem::mach::BlockHeader;
            let invoke: extern "C" fn(usize, u8) = unsafe { std::mem::transmute((*header).invoke) };
            invoke(block, 1);
        }));
    }
}

fn animate(animations: usize, completion: usize) {
    animate_with(animations, completion, |work| crate::fw::runloop::post(crate::fw::runloop::Work::Native(work)));
}

extern "C" fn animate_simple(_: Class, _: Sel, _duration: f64, animations: usize) {
    animate(animations, 0);
}

extern "C" fn animate_completion(_: Class, _: Sel, _duration: f64, animations: usize, completion: usize) {
    animate(animations, completion);
}

extern "C" fn animate_delay(_: Class, _: Sel, _duration: f64, _delay: f64, _options: usize, animations: usize, completion: usize) {
    animate(animations, completion);
}

// ----- iOS 3 style animations: +beginAnimations:context: opens a block
// (blocks nest), the +setAnimation… calls set up the innermost one and
// +commitAnimations closes it. As with block animations the changes take
// effect at once; the delegate's will-start and did-stop selectors are
// sent on the next turn of the main run loop, did-stop with the block's id,
// finished = YES and its context (animationDidStop:finished:context:). -----

#[derive(Clone, Copy)]
struct OldAnimation {
    id: Id,
    context: usize,
    delegate: Id,
    will_start: Sel,
    did_stop: Sel,
}

static OLD_ANIMATIONS: Mutex<Vec<OldAnimation>> = Mutex::new(Vec::new());

fn with_open_animation(change: impl FnOnce(&mut OldAnimation)) {
    if let Some(open) = OLD_ANIMATIONS.lock().unwrap_or_else(|e| e.into_inner()).last_mut() {
        change(open);
    }
}

extern "C" fn begin_animations(_: Class, _: Sel, id: Id, context: usize) {
    runtime::send(id, "retain", &[]);
    let open = OldAnimation { id, context, delegate: 0, will_start: 0, did_stop: 0 };
    OLD_ANIMATIONS.lock().unwrap_or_else(|e| e.into_inner()).push(open);
}

extern "C" fn set_animation_delegate(_: Class, _: Sel, delegate: Id) {
    with_open_animation(|open| open.delegate = delegate);
}

extern "C" fn set_animation_will_start(_: Class, _: Sel, selector: Sel) {
    with_open_animation(|open| open.will_start = selector);
}

extern "C" fn set_animation_did_stop(_: Class, _: Sel, selector: Sel) {
    with_open_animation(|open| open.did_stop = selector);
}

/// Sends `selector` with as many of `args` as its name has colons.
fn send_to_delegate(delegate: Id, selector: Sel, args: &[usize]) {
    if selector == 0 {
        return;
    }
    let name = lock().sel_name(selector);
    let count = name.matches(':').count().min(args.len());
    runtime::send(delegate, &name, &args[..count]);
}

/// +commitAnimations: closes the innermost block and hands its delegate calls to `later`.
fn commit_old_animation(later: impl FnOnce(Box<dyn FnOnce() + Send>)) {
    let Some(done) = OLD_ANIMATIONS.lock().unwrap_or_else(|e| e.into_inner()).pop() else { return };
    if done.delegate == 0 || (done.will_start == 0 && done.did_stop == 0) {
        runtime::send(done.id, "release", &[]);
        return;
    }
    runtime::send(done.delegate, "retain", &[]);
    later(Box::new(move || {
        send_to_delegate(done.delegate, done.will_start, &[done.id, done.context]);
        send_to_delegate(done.delegate, done.did_stop, &[done.id, collections::number_bool(true), done.context]);
        runtime::send(done.delegate, "release", &[]);
        runtime::send(done.id, "release", &[]);
    }));
}

extern "C" fn commit_animations(_: Class, _: Sel) {
    commit_old_animation(|work| crate::fw::runloop::post(crate::fw::runloop::Work::Native(work)));
}

// Timing and curve settings: nothing moves on screen here.
extern "C" fn ignore_class_double(_: Class, _: Sel, _value: f64) {}
extern "C" fn ignore_class_float(_: Class, _: Sel, _value: f32) {}
extern "C" fn ignore_class_word(_: Class, _: Sel, _value: usize) {}
extern "C" fn ignore_animation_transition(_: Class, _: Sel, _transition: usize, _view: Id, _cache: u8) {}

#[allow(clippy::too_many_arguments)]
extern "C" fn animate_spring(
    _: Class,
    _: Sel,
    _duration: f64,
    _delay: f64,
    _damping: f64,
    _velocity: f64,
    _options: usize,
    animations: usize,
    completion: usize,
) {
    animate(animations, completion);
}

extern "C" fn transition_with_view(_: Class, _: Sel, _view: Id, _duration: f64, _options: usize, animations: usize, completion: usize) {
    animate(animations, completion);
}

/// UIViewAnimationOptionShowHideTransitionViews: hide/show instead of
/// removing/adding.
const SHOW_HIDE_TRANSITION_VIEWS: usize = 1 << 8;

/// transitionFromView:toView:: toView takes fromView's place in its
/// superview (or, with ShowHideTransitionViews, one is hidden and the other shown).
extern "C" fn transition_from_view(_: Class, _: Sel, from: Id, to: Id, _duration: f64, options: usize, completion: usize) {
    if options & SHOW_HIDE_TRANSITION_VIEWS != 0 {
        property_set(from, "hidden", 1);
        property_set(to, "hidden", 0);
    } else {
        let parent = view_state(from).superview;
        if parent != 0 && to != 0 {
            let index = view_state(parent).subviews.iter().position(|&v| v == from);
            attach(parent, to, index.map(|i| i + 1));
        }
        runtime::send(from, "removeFromSuperview", &[]);
    }
    animate(0, completion);
}

extern "C" fn perform_without_animation(_: Class, _: Sel, actions: usize) {
    animate(actions, 0);
}

extern "C" fn ignore_class_bool(_: Class, _: Sel, _value: u8) {}
extern "C" fn class_yes(_: Class, _: Sel) -> bool {
    true
}

// ----- windows -----

static KEY_WINDOW: Mutex<Id> = Mutex::new(0);
static WINDOWS: Mutex<Vec<Id>> = Mutex::new(Vec::new());

/// The windows shown, back to front.
pub fn windows() -> Vec<Id> {
    WINDOWS.lock().unwrap().clone()
}

extern "C" fn window_make_key_and_visible(this: Id, _: Sel) {
    *KEY_WINDOW.lock().unwrap() = this;
    let was_shown = on_screen(this);
    let mut windows = WINDOWS.lock().unwrap();
    if !windows.contains(&this) {
        windows.push(this);
    }
    drop(windows);
    set_needs_layout(this);
    property_set(this, "hidden", 0);
    crate::log!("window {this:#x} shown");
    let root = property_get(this, "rootViewController").unwrap_or(0) as Id;
    let showing_root = will_appear(root, false);
    // The other controllers whose views are in the window come on screen with it.
    let arriving = if was_shown { Vec::new() } else { appearing(this) };
    appeared(arriving);
    if showing_root {
        did_appear(root, false);
    }
}

// ----- appearance: viewWillAppear: / viewDidAppear: as a controller's view
// comes on screen (its window shown, presented) and viewWillDisappear: /
// viewDidDisappear: as it leaves; "appeared" keeps the pairs in turn. -----

fn tell_appearance(controller: Id, name: &str, animated: bool) {
    let s = runtime::sel(name);
    if let Some(imp) = runtime::imp_for(controller, s) {
        let f: extern "C" fn(Id, Sel, bool) = unsafe { std::mem::transmute(imp) };
        f(controller, s, animated);
    }
}

/// viewWillAppear: unless the controller is on screen already or on its
/// way ("appearing": whoever sent viewWillAppear: sends viewDidAppear:);
/// whether it was sent.
fn will_appear(controller: Id, animated: bool) -> bool {
    if controller == 0 || property_get(controller, "appeared") == Some(1) || property_get(controller, "appearing") == Some(1) {
        return false;
    }
    property_set(controller, "appearing", 1);
    tell_appearance(controller, "viewWillAppear:", animated);
    true
}

fn did_appear(controller: Id, animated: bool) {
    property_set(controller, "appearing", 0);
    if property_get(controller, "appeared") == Some(1) {
        return;
    }
    property_set(controller, "appeared", 1);
    tell_appearance(controller, "viewDidAppear:", animated);
    // Tests tap on this line (IOS_TAPS "on=screen shown: <class>"), once it is drawn.
    crate::fw::pictures::on_next_draw(format!("screen shown: {}", crate::fw::string::describe_class_of(controller)));
}

/// viewWillDisappear: if the controller is on screen and not already on its
/// way out; whether it was sent.
fn will_disappear(controller: Id, animated: bool) -> bool {
    if controller == 0 || property_get(controller, "appeared") != Some(1) || property_get(controller, "disappearing") == Some(1) {
        return false;
    }
    property_set(controller, "disappearing", 1);
    tell_appearance(controller, "viewWillDisappear:", animated);
    true
}

fn did_disappear(controller: Id, animated: bool) {
    property_set(controller, "disappearing", 0);
    if property_get(controller, "appeared") != Some(1) {
        return;
    }
    property_set(controller, "appeared", 0);
    tell_appearance(controller, "viewDidDisappear:", animated);
}

/// Whether a view is in a window that is shown.
fn on_screen(view: Id) -> bool {
    let window = window_of(view);
    window != 0 && property_get(window, "hidden").unwrap_or(0) == 0 && WINDOWS.lock().unwrap().contains(&window)
}

/// Whether a container passes appearance on to its children (iOS 5's
/// automaticallyForwardAppearanceAndRotationMethodsToChildViewControllers,
/// iOS 6's shouldAutomaticallyForwardAppearanceMethods; yes unless it says NO).
fn forwards_to_children(controller: Id) -> bool {
    ["automaticallyForwardAppearanceAndRotationMethodsToChildViewControllers", "shouldAutomaticallyForwardAppearanceMethods"]
        .iter()
        .all(|name| !runtime::responds(controller, name) || runtime::send(controller, name, &[]) & 0xFF != 0)
}

/// The controllers whose own views are in `view`'s tree, outer ones first,
/// that UIKit tells about appearance: not those inside a container that
/// passes nothing on.
fn controllers_in(view: Id) -> Vec<Id> {
    fn walk(view: Id, forward: bool, out: &mut Vec<Id>, depth: usize) {
        if view == 0 || depth > 256 {
            return;
        }
        let mut forward_inside = forward;
        let controller = property_get(view, "viewController").unwrap_or(0) as Id;
        if controller != 0 && property_get(controller, "view") == Some(view as u64) {
            if forward {
                out.push(controller);
            }
            forward_inside = forward && forwards_to_children(controller);
        }
        for child in view_state(view).subviews.clone() {
            walk(child, forward_inside, out, depth + 1);
        }
    }
    let mut out = Vec::new();
    walk(view, true, &mut out, 0);
    out
}

/// `view` is about to come on screen: viewWillAppear: to the controllers
/// in it (not those already told by whoever is showing them); the ones told,
/// for `appeared` once it is there.
fn appearing(view: Id) -> Vec<Id> {
    controllers_in(view).into_iter().filter(|&controller| will_appear(controller, false)).collect()
}

fn appeared(controllers: Vec<Id>) {
    for controller in controllers {
        did_appear(controller, false);
    }
}

/// `view` is about to leave the screen: viewWillDisappear: to the controllers in it.
fn disappearing(view: Id) -> Vec<Id> {
    controllers_in(view).into_iter().filter(|&controller| will_disappear(controller, false)).collect()
}

fn disappeared(controllers: Vec<Id>) {
    for controller in controllers {
        did_disappear(controller, false);
    }
}

/// Takes a view out of its superview, with the appearance calls for the
/// controllers that leave the screen with it.
fn take_out(view: Id) {
    let parent = view_state(view).superview;
    if parent == 0 {
        return;
    }
    let window = window_of(parent);
    runtime::send(parent, "willRemoveSubview:", &[view]);
    runtime::send(view, "willMoveToSuperview:", &[0]);
    if window != 0 {
        tell_window_move(view, Some(0));
    }
    let leaving = if on_screen(view) { disappearing(view) } else { Vec::new() };
    detach(view);
    if window != 0 {
        tell_window_move(view, None);
    }
    runtime::send(view, "didMoveToSuperview", &[]);
    disappeared(leaving);
}

/// A shown window hidden or unhidden: what is in it leaves or comes back.
extern "C" fn window_set_hidden(this: Id, s: Sel, hidden: u8) {
    let listed = WINDOWS.lock().unwrap().contains(&this);
    let was_shown = listed && property_get(this, "hidden").unwrap_or(0) == 0;
    let (leaving, arriving) = match (listed, was_shown, hidden != 0) {
        (true, true, true) => (disappearing(this), Vec::new()),
        (true, false, false) => (Vec::new(), appearing(this)),
        _ => (Vec::new(), Vec::new()),
    };
    view_set_hidden(this, s, hidden);
    disappeared(leaving);
    appeared(arriving);
}

extern "C" fn window_make_key(this: Id, _: Sel) {
    *KEY_WINDOW.lock().unwrap() = this;
}

extern "C" fn window_root_controller(this: Id, _: Sel) -> Id {
    property_get(this, "rootViewController").unwrap_or(0) as Id
}

extern "C" fn window_set_root_controller(this: Id, _: Sel, controller: Id) {
    let old = property_get(this, "rootViewController").unwrap_or(0) as Id;
    property_set(this, "rootViewController", controller as u64);
    // A new root takes the old one's place: its view leaves the window.
    let shown = WINDOWS.lock().unwrap().contains(&this);
    if old != 0 && old != controller {
        // What the old root presented goes with it (else dismissing it later
        // would put the old root's view back over the new one).
        close_presented(old, false, false);
        let hiding = shown && will_disappear(old, false);
        if let Some(view) = property_get(old, "view").map(|view| view as Id).filter(|&view| view != 0 && view_state(view).superview == this) {
            take_out(view);
        }
        if hiding {
            did_disappear(old, false);
        }
    }
    if controller != 0 {
        // As UIKit: the view is loaded (viewDidLoad) before viewWillAppear:.
        let view = runtime::send(controller, "view", &[]);
        let showing = shown && will_appear(controller, false);
        if view != 0 {
            send_rect(view, "setFrame:", oriented_bounds());
            attach(this, view, None);
        }
        put_on_screen(controller);
        if showing {
            did_appear(controller, false);
        }
    }
}

/// Tells a controller of ours (fw/controls.rs: navigation controllers) that
/// it is now on screen.
fn put_on_screen(controller: Id) {
    if controller != 0 && runtime::responds(controller, "gamebridgeShown") {
        runtime::send(controller, "gamebridgeShown", &[]);
    }
}

extern "C" fn window_screen(this: Id, _: Sel) -> Id {
    main_screen(this, 0)
}

property!(window_level, window_set_level, "windowLevel", f64, 0.0);

pub fn key_window() -> Id {
    *KEY_WINDOW.lock().unwrap()
}

// ----- view controllers: { isa, ... } with the view kept as a property -----

extern "C" fn controller_init(this: Id, _: Sel) -> Id {
    this
}

extern "C" fn controller_view(this: Id, _: Sel) -> Id {
    if let Some(view) = property_get(this, "view") {
        return view as Id;
    }
    runtime::send(this, "loadView", &[]);
    if property_get(this, "view").is_none() {
        let view = runtime::send(runtime::class_named("UIView"), "alloc", &[]);
        let s = runtime::sel("initWithFrame:");
        let imp = runtime::imp_for(view, s).unwrap();
        let f: extern "C" fn(Id, Sel, Rect) -> Id = unsafe { std::mem::transmute(imp) };
        let view = f(view, s, oriented_bounds());
        property_set(this, "view", view as u64);
        // The view's next responder is its controller, as with setView:.
        property_set(view, "viewController", this as u64);
    }
    runtime::send(this, "viewDidLoad", &[]);
    property_get(this, "view").unwrap_or(0) as Id
}

extern "C" fn controller_set_view(this: Id, _: Sel, view: Id) {
    if let Some(old) = property_get(this, "view").map(|old| old as Id).filter(|&old| old != view && property_get(old, "viewController") == Some(this as u64)) {
        property_set(old, "viewController", 0);
    }
    property_set(this, "view", view as u64);
    if view != 0 {
        property_set(view, "viewController", this as u64);
    }
}

extern "C" fn controller_is_view_loaded(this: Id, _: Sel) -> bool {
    property_get(this, "view").is_some()
}

extern "C" fn controller_orientation(_this: Id, _: Sel) -> isize {
    current_interface().orientation
}

extern "C" fn controller_supported_orientations(_this: Id, _: Sel) -> usize {
    current_interface().mask
}

/// -[UIApplication supportedInterfaceOrientationsForWindow:]: what the app
/// delegate says (application:supportedInterfaceOrientationsForWindow:),
/// else the orientations its Info.plist lists.
extern "C" fn app_supported_orientations(this: Id, _: Sel, window: Id) -> usize {
    let delegate = runtime::send(this, "delegate", &[]);
    let ask = "application:supportedInterfaceOrientationsForWindow:";
    if delegate != 0 && runtime::responds(delegate, ask) {
        return runtime::send(delegate, ask, &[this, window]);
    }
    current_interface().mask
}

extern "C" fn controller_should_autorotate_to(_this: Id, _: Sel, orientation: isize) -> bool {
    matches!(orientation, 1..=4) && current_interface().mask & (1 << orientation) != 0
}

/// Runs a completion block (no arguments) later on the main run loop, the
/// way UIKit calls it once a presentation or dismissal has finished.
fn complete_later(completion: usize) {
    if completion != 0 {
        let block = crate::libsystem::mach::block_copy(completion as *mut _) as usize;
        crate::fw::runloop::post(crate::fw::runloop::Work::Block(block));
    }
}

/// Links `controller` as shown by `this`, and puts its view in the window.
fn present(this: Id, controller: Id, animated: bool) {
    crate::log!("view controller {this:#x} shows {} {controller:#x} on top", crate::fw::string::describe_class_of(controller));
    property_set(this, "presented", controller as u64);
    if controller != 0 {
        property_set(controller, "presenting", this as u64);
        // Screens for services this phone doesn't offer have nothing to show.
        if !runtime::responds(controller, "gamebridgeAnswerShown") {
            show_presented(this, controller, animated);
        }
        put_on_screen(controller);
    }
}

/// A full-screen presentation, as on iPhones: the presented controller's
/// view fills the window, and the presenting controller's view leaves it
/// (kept as "coveredView" for the way back), with the appearance messages.
fn show_presented(presenter: Id, controller: Id, animated: bool) {
    let presenter_view = property_get(presenter, "view").unwrap_or(0) as Id;
    let window = match if presenter_view != 0 { window_of(presenter_view) } else { 0 } {
        0 => key_window(),
        window => window,
    };
    if window == 0 {
        return; // nothing is on screen yet: only linked
    }
    let covered = if presenter_view != 0 && view_state(presenter_view).superview == window { presenter_view } else { 0 };
    let view = runtime::send(controller, "view", &[]);
    if view == 0 {
        return;
    }
    let showing = will_appear(controller, animated);
    let hiding = covered != 0 && will_disappear(presenter, animated);
    // The game draws in the interface's orientation: a window that still
    // holds portrait bounds (an old app made it from UIScreen.bounds) must
    // not squeeze the presented view.
    let s = runtime::sel("setFrame:");
    if let Some(imp) = runtime::imp_for(view, s) {
        let f: extern "C" fn(Id, Sel, Rect) = unsafe { std::mem::transmute(imp) };
        f(view, s, oriented_bounds());
    }
    attach(window, view, None);
    property_set(controller, "presentedInWindow", window as u64);
    if covered != 0 {
        take_out(covered);
        property_set(controller, "coveredView", covered as u64);
    }
    if hiding {
        did_disappear(presenter, animated);
    }
    if showing {
        did_appear(controller, animated);
    }
}

/// Takes a presented controller's view out of its window and, with
/// `restore`, puts back the view it covered; `uncovered` (the controller
/// that view belongs to) hears that it appears again.
fn hide_presented(controller: Id, uncovered: Id, animated: bool, restore: bool) {
    let Some(window) = property_get(controller, "presentedInWindow").map(|window| window as Id).filter(|&window| window != 0) else {
        return;
    };
    let view = property_get(controller, "view").unwrap_or(0) as Id;
    let covered = property_get(controller, "coveredView").unwrap_or(0) as Id;
    let hiding = will_disappear(controller, animated);
    let showing = restore && covered != 0 && will_appear(uncovered, animated);
    if restore && covered != 0 {
        let index = view_state(window).subviews.iter().position(|&v| v == view);
        attach(window, covered, index);
    }
    if view != 0 && view_state(view).superview == window {
        take_out(view);
    }
    property_set(controller, "presentedInWindow", 0);
    property_set(controller, "coveredView", 0);
    if hiding {
        did_disappear(controller, animated);
    }
    if showing {
        did_appear(uncovered, animated);
    }
}

/// Screens for services this phone doesn't offer answer the way iOS does
/// when the user closes them (see fw/controls.rs), once they are shown.
fn answer_shown_later(controller: Id) {
    if controller != 0 && runtime::responds(controller, "gamebridgeAnswerShown") {
        crate::fw::runloop::post(crate::fw::runloop::Work::Native(Box::new(move || {
            runtime::send(controller, "gamebridgeAnswerShown", &[]);
        })));
    }
}

extern "C" fn controller_present(this: Id, _: Sel, controller: Id, animated: u8) {
    present(this, controller, animated != 0);
    answer_shown_later(controller);
}

extern "C" fn controller_present_completion(this: Id, _: Sel, controller: Id, animated: u8, completion: usize) {
    present(this, controller, animated != 0);
    // As on iOS: the presentation finishes (completion block) before the
    // user can close the screen. The run loop keeps this order.
    complete_later(completion);
    answer_shown_later(controller);
}

/// dismissViewControllerAnimated: sent to the presenting controller closes
/// what it shows and everything shown on top of that; sent to the shown
/// controller itself (nothing on top) it is passed to the one that
/// presented it.
fn dismiss(this: Id, animated: bool) {
    let owner = match property_get(this, "presented") {
        Some(shown) if shown != 0 => this,
        _ => property_get(this, "presenting").unwrap_or(0) as Id,
    };
    if owner != 0 {
        close_presented(owner, animated, true);
    }
}

/// Closes everything `owner` presents, top first, and unlinks it. With
/// `restore` (a dismissal) each view put back the view it covered, so
/// `owner`'s comes back on screen; without (`owner` is leaving its window)
/// the presented views just go.
fn close_presented(owner: Id, animated: bool, restore: bool) {
    let mut shown = property_get(owner, "presented").unwrap_or(0) as Id;
    property_set(owner, "presented", 0);
    // Walk up the stack: A shows B shows C; dismissing from A closes B and C.
    let mut closed = Vec::new();
    for _ in 0..64 {
        if shown == 0 {
            break;
        }
        closed.push(shown);
        shown = property_get(shown, "presented").unwrap_or(0) as Id;
    }
    // Their views leave the window top first, each putting back the view it
    // covered; only `owner` comes back on screen for good.
    for (index, &controller) in closed.iter().enumerate().rev() {
        hide_presented(controller, if index == 0 { owner } else { 0 }, animated, restore);
    }
    for shown in closed {
        property_set(shown, "presenting", 0);
        property_set(shown, "presented", 0);
        property_set(shown, "onScreen", 0); // shown again later: announced again
    }
}

extern "C" fn controller_dismiss(this: Id, _: Sel, animated: u8) {
    dismiss(this, animated != 0);
}

extern "C" fn controller_dismiss_completion(this: Id, s: Sel, animated: u8, completion: usize) {
    controller_dismiss(this, s, animated);
    complete_later(completion);
}

property!(controller_presenting, controller_set_presenting, "presenting", id, 0);

/// The navigation controller a controller was pushed onto (fw/controls.rs).
extern "C" fn controller_navigation(this: Id, _: Sel) -> Id {
    property_get(this, "navigationController").unwrap_or(0) as Id
}

property!(controller_presented, controller_set_presented, "presented", id, 0);

fn controller_children_list(parent: Id) -> Vec<Id> {
    CONTROLLER_CHILDREN.lock().unwrap().as_ref().and_then(|children| children.get(&parent)).cloned().unwrap_or_default()
}

fn controller_owns_view(controller: Id, view: Id) -> bool {
    if controller == 0 || view == 0 {
        return false;
    }
    let owned = property_get(controller, "view").unwrap_or(0) as Id;
    let owned = if owned == 0 { runtime::send(controller, "view", &[]) } else { owned };
    owned == view
}

fn linked_controller_view(parent_controller: Id, parent_view: Id, view: Id) -> bool {
    if !controller_owns_view(parent_controller, parent_view) {
        return false;
    }
    let mut fallback = Vec::new();
    for child in controller_children_list(parent_controller) {
        if property_get(child, "parentController") != Some(parent_controller as u64) {
            continue;
        }
        let owned = property_get(child, "view").unwrap_or(0) as Id;
        if owned == view || property_get(view, "viewController") == Some(child as u64) {
            return true;
        }
        if owned == 0 {
            fallback.push(child);
        }
    }
    if fallback.len() == 1 {
        runtime::send(fallback[0], "view", &[]) == view
    } else {
        false
    }
}

extern "C" fn controller_parent(this: Id, _: Sel) -> Id {
    property_get(this, "parentController").unwrap_or(0) as Id
}

extern "C" fn controller_add_child(this: Id, _: Sel, child: Id) {
    if child == 0 {
        return;
    }
    let previous = property_get(child, "parentController").unwrap_or(0) as Id;
    if previous == this {
        return;
    }
    {
        let mut all = CONTROLLER_CHILDREN.lock().unwrap();
        let all = all.get_or_insert_with(HashMap::new);
        if previous != 0 {
            if let Some(children) = all.get_mut(&previous) {
                children.retain(|&controller| controller != child);
            }
        }
        all.entry(this).or_default().push(child);
    }
    property_set(child, "parentController", this as u64);
}

extern "C" fn controller_remove_from_parent(this: Id, _: Sel) {
    let parent = property_get(this, "parentController").unwrap_or(0) as Id;
    if parent != 0 {
        if let Some(children) = CONTROLLER_CHILDREN.lock().unwrap().as_mut().and_then(|all| all.get_mut(&parent)) {
            children.retain(|&controller| controller != this);
        }
    }
    property_set(this, "parentController", 0);
}

extern "C" fn controller_children(this: Id, _: Sel) -> Id {
    collections::array_from(&controller_children_list(this))
}

/// transitionFromViewController:toViewController:duration:options:animations:completion:
/// between two children of the receiver: the new child's view takes the old
/// one's place in its superview (with ShowHideTransitionViews, the old view
/// is hidden instead), the animations block runs, and after `duration` the
/// old view leaves, the appearance calls finish and completion(YES) runs.
#[allow(clippy::too_many_arguments)]
extern "C" fn controller_transition(_this: Id, _: Sel, from: Id, to: Id, duration: f64, options: usize, animations: usize, completion: usize) {
    let from_view = if from == 0 { 0 } else { runtime::send(from, "view", &[]) };
    let to_view = if to == 0 { 0 } else { runtime::send(to, "view", &[]) };
    let on_screen = from != 0 && property_get(from, "appeared") == Some(1);
    let hiding = on_screen && will_disappear(from, true);
    let showing = on_screen && will_appear(to, true);
    let show_hide = options & SHOW_HIDE_TRANSITION_VIEWS != 0;
    if show_hide {
        property_set(to_view, "hidden", 0);
    } else if from_view != 0 && to_view != 0 {
        let parent = view_state(from_view).superview;
        if parent != 0 {
            let index = view_state(parent).subviews.iter().position(|&v| v == from_view);
            attach(parent, to_view, index.map(|i| i + 1));
        }
    }
    let completion = if completion == 0 { 0 } else { crate::libsystem::mach::block_copy(completion as *mut _) as usize };
    animate_with(animations, 0, |_| {});
    crate::fw::runloop::post_after(
        duration.max(0.0),
        Box::new(move || {
            if show_hide {
                property_set(from_view, "hidden", 1);
            } else if from_view != 0 && from_view != to_view {
                runtime::send(from_view, "removeFromSuperview", &[]);
            }
            if hiding {
                did_disappear(from, true);
            }
            if showing {
                did_appear(to, true);
            }
            if completion != 0 {
                let header = completion as *const crate::libsystem::mach::BlockHeader;
                let invoke: extern "C" fn(usize, u8) = unsafe { std::mem::transmute((*header).invoke) };
                invoke(completion, 1);
            }
        }),
    );
}

// ----- layers -----

extern "C" fn layer_init(this: Id, _: Sel) -> Id {
    this
}

property!(layer_opaque, layer_set_opaque, "opaque", bool, false);
property!(layer_drawable_properties, layer_set_drawable_properties, "drawableProperties", id, 0);
property!(layer_contents_scale, layer_set_contents_scale, "contentsScale", f64, 1.0);
property!(layer_delegate, layer_set_delegate, "delegate", id, 0);

// ----- colours: { isa, r, g, b, a } -----

fn color(r: f64, g: f64, b: f64, a: f64) -> Id {
    let object = runtime::alloc_instance(runtime::class_named("UIColor"));
    unsafe { *((object + 8) as *mut [f64; 4]) = [r, g, b, a] };
    object
}

extern "C" fn color_rgba(_: Class, _: Sel, r: f64, g: f64, b: f64, a: f64) -> Id {
    color(r, g, b, a)
}
extern "C" fn color_white_alpha(_: Class, _: Sel, w: f64, a: f64) -> Id {
    color(w, w, w, a)
}
// UIKit's named colours (+blackColor and so on), with Apple's values.
macro_rules! named_colors {
    ($($function:ident $selector:literal [$r:expr, $g:expr, $b:expr, $a:expr]),* $(,)?) => {
        $(extern "C" fn $function(_: Class, _: Sel) -> Id {
            color($r, $g, $b, $a)
        })*
        fn named_colors() -> Vec<(&'static str, usize)> {
            vec![$(($selector, $function as usize)),*]
        }
    };
}
named_colors! {
    color_black "blackColor" [0.0, 0.0, 0.0, 1.0],
    color_dark_gray "darkGrayColor" [1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0, 1.0],
    color_light_gray "lightGrayColor" [2.0 / 3.0, 2.0 / 3.0, 2.0 / 3.0, 1.0],
    color_white "whiteColor" [1.0, 1.0, 1.0, 1.0],
    color_gray "grayColor" [0.5, 0.5, 0.5, 1.0],
    color_red "redColor" [1.0, 0.0, 0.0, 1.0],
    color_green "greenColor" [0.0, 1.0, 0.0, 1.0],
    color_blue "blueColor" [0.0, 0.0, 1.0, 1.0],
    color_cyan "cyanColor" [0.0, 1.0, 1.0, 1.0],
    color_yellow "yellowColor" [1.0, 1.0, 0.0, 1.0],
    color_magenta "magentaColor" [1.0, 0.0, 1.0, 1.0],
    color_orange "orangeColor" [1.0, 0.5, 0.0, 1.0],
    color_purple "purpleColor" [0.5, 0.0, 0.5, 1.0],
    color_brown "brownColor" [0.6, 0.4, 0.2, 1.0],
    color_clear "clearColor" [0.0, 0.0, 0.0, 0.0],
}
extern "C" fn color_cg(this: Id, _: Sel) -> Id {
    this
}

// ----- images: { isa, *mut ImageInfo } -----

#[derive(Default)]
pub struct ImageInfo {
    /// The picture's file ("" for pictures made in memory).
    pub path: String,
    pub pixels: (u32, u32),
    pub scale: f64,
    /// UIImageOrientation (0 = up).
    pub orientation: i64,
    /// A stretchable image's cap insets in points (top, left, bottom, right).
    pub caps: Option<[f64; 4]>,
    /// The stretchable middle is tiled, not stretched (UIImageResizingModeTile).
    pub tile: bool,
    /// Its CGImage (see fw/graphics.rs; read from the file when first asked for).
    pub cg: OnceLock<Id>,
    /// An upright PNG made to show it on the phone screen (see graphics::picture_file).
    pub picture_file: OnceLock<Option<String>>,
    /// Whether making that file has started.
    pub picture_started: std::sync::atomic::AtomicBool,
    /// A stretchable image's nine parts as PNGs (see graphics::picture_part_file).
    pub part_files: OnceLock<Vec<Option<String>>>,
    /// Whether making those has started.
    pub parts_started: std::sync::atomic::AtomicBool,
}

fn png_size(path: &str) -> Option<(u32, u32)> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() < 24 || &bytes[1..4] != b"PNG" {
        return None;
    }
    // Apple's crushed PNGs put a CgBI chunk before the header.
    let at = if &bytes[12..16] == b"CgBI" { 32 } else { 16 };
    let width = u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?);
    let height = u32::from_be_bytes(bytes.get(at + 4..at + 8)?.try_into().ok()?);
    Some((width, height))
}

fn image_from_path(path: &str) -> Id {
    let Some(pixels) = png_size(path).or_else(|| image::image_dimensions(path).ok()) else {
        crate::log!("image {path}: can't read its size");
        return 0;
    };
    let scale = if path.contains("@3x") {
        3.0
    } else if path.contains("@2x") {
        2.0
    } else {
        1.0
    };
    // A photo's EXIF tag turns it, as UIImage reads it (the size is then swapped).
    let orientation = crate::fw::graphics::file_orientation(path);
    make_image(ImageInfo { path: path.into(), pixels, scale, orientation, ..Default::default() })
}

/// A new UIImage with one reference (the caller's); it counts references (fw/counted.rs).
pub fn make_image(info: ImageInfo) -> Id {
    let object = runtime::alloc_instance(runtime::class_named("UIImage"));
    unsafe { *((object + 8) as *mut *mut ImageInfo) = Box::into_raw(Box::new(info)) };
    crate::fw::counted::track(object, crate::fw::graphics::ui_image_clean_up);
    object
}

/// The image's size in points (sideways orientations swap width and height).
pub fn image_points(image: Id) -> Size {
    match image_info(image) {
        Some(info) => {
            let (w, h) = (info.pixels.0 as f64 / info.scale, info.pixels.1 as f64 / info.scale);
            if matches!(info.orientation, 2 | 3 | 6 | 7) {
                Size { width: h, height: w }
            } else {
                Size { width: w, height: h }
            }
        }
        None => Size::default(),
    }
}

pub fn image_info(image: Id) -> Option<&'static ImageInfo> {
    if image == 0 {
        return None;
    }
    unsafe { (*((image + 8) as *const *const ImageInfo)).as_ref() }
}

extern "C" fn image_named(_: Class, _: Sel, name: Id) -> Id {
    let name = string::get(name);
    let dir = crate::loader::app_dir();
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), e.to_string()),
        _ => (name.clone(), "png".to_string()),
    };
    for candidate in [format!("{dir}/{stem}@2x.{ext}"), format!("{dir}/{stem}.{ext}"), format!("{dir}/{name}")] {
        // A file only: an empty name would otherwise find the app's folder.
        if std::path::Path::new(&candidate).is_file() {
            // UIKit keeps named images in its cache (which holds its own reference).
            static CACHE: Mutex<Option<HashMap<String, Id>>> = Mutex::new(None);
            let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
            let cache = cache.get_or_insert_with(HashMap::new);
            if let Some(&image) = cache.get(&candidate) {
                return image;
            }
            let image = image_from_path(&candidate);
            if image != 0 {
                cache.insert(candidate, image);
            }
            return image;
        }
    }
    crate::log!("imageNamed {name}: not in the app");
    0
}

extern "C" fn image_with_contents_of_file(_: Class, _: Sel, path: Id) -> Id {
    crate::fw::counted::autorelease(image_from_path(&string::get(path)))
}

extern "C" fn image_init_with_contents_of_file(_this: Id, _: Sel, path: Id) -> Id {
    image_from_path(&string::get(path))
}

/// A new autoreleased UIImage of the same picture with cap insets (top,
/// left, bottom, right in points): drawn larger, the caps keep their size
/// and the middle stretches (fw/pictures.rs).
fn image_with_caps(image: Id, caps: [f64; 4], tile: bool) -> Id {
    let Some(info) = image_info(image) else { return 0 };
    // Pictures made in memory have no file: the copy holds the same CGImage.
    let cg = OnceLock::new();
    if info.path.is_empty() {
        let picture = crate::fw::graphics::image_cg(image);
        crate::fw::counted::retain(picture);
        let _ = cg.set(picture);
    }
    let copy = ImageInfo { path: info.path.clone(), pixels: info.pixels, scale: info.scale, orientation: info.orientation, caps: Some(caps), tile, cg, ..Default::default() };
    crate::fw::counted::autorelease(make_image(copy))
}

/// -resizableImageWithCapInsets: (UIEdgeInsets: top, left, bottom, right).
extern "C" fn image_resizable(this: Id, _: Sel, top: f64, left: f64, bottom: f64, right: f64) -> Id {
    image_with_caps(this, [top, left, bottom, right], false)
}

/// -resizableImageWithCapInsets:resizingMode: (UIImageResizingModeTile 0, Stretch 1).
extern "C" fn image_resizable_mode(this: Id, _: Sel, top: f64, left: f64, bottom: f64, right: f64, mode: isize) -> Id {
    image_with_caps(this, [top, left, bottom, right], mode == 0)
}

/// -stretchableImageWithLeftCapWidth:topCapHeight: (older): the right and
/// bottom caps leave a one-point middle; a 0 cap stretches that way whole.
extern "C" fn image_stretchable(this: Id, _: Sel, left: isize, top: isize) -> Id {
    let size = image_points(this);
    let (left, top) = (left.max(0) as f64, top.max(0) as f64);
    let right = if left > 0.0 { (size.width - left - 1.0).max(0.0) } else { 0.0 };
    let bottom = if top > 0.0 { (size.height - top - 1.0).max(0.0) } else { 0.0 };
    image_with_caps(this, [top, left, bottom, right], false)
}

extern "C" fn image_left_cap_width(this: Id, _: Sel) -> isize {
    image_info(this).and_then(|info| info.caps).map_or(0, |caps| caps[1] as isize)
}

extern "C" fn image_top_cap_height(this: Id, _: Sel) -> isize {
    image_info(this).and_then(|info| info.caps).map_or(0, |caps| caps[0] as isize)
}

/// -capInsets (UIEdgeInsets: top, left, bottom, right; zero when not stretchable).
extern "C" fn image_cap_insets(this: Id, _: Sel) -> crate::fw::controls::Insets {
    let [top, left, bottom, right] = image_info(this).and_then(|info| info.caps).unwrap_or([0.0; 4]);
    crate::fw::controls::Insets { top, left, bottom, right }
}

extern "C" fn image_size(this: Id, _: Sel) -> Size {
    image_points(this)
}

extern "C" fn image_scale(this: Id, _: Sel) -> f64 {
    image_info(this).map(|i| i.scale).unwrap_or(1.0)
}

extern "C" fn image_view_init_with_image(this: Id, _: Sel, image: Id) -> Id {
    let size = if image != 0 { image_size(image, 0) } else { Size::default() };
    view_init_with_frame(this, 0, Rect { origin: Point::default(), size });
    property_set_object(this, "image", image);
    this
}

property!(image_view_image, image_view_set_image, "image", id, 0);

// ----- the application object (more of it) -----

property!(app_idle_timer_disabled, app_set_idle_timer_disabled, "idleTimerDisabled", bool, false);
property!(app_badge, app_set_badge, "badge", isize, 0);

extern "C" fn app_key_window(_this: Id, _: Sel) -> Id {
    key_window()
}

extern "C" fn app_windows(_this: Id, _: Sel) -> Id {
    collections::array_from(&WINDOWS.lock().unwrap().clone())
}

extern "C" fn app_state(_this: Id, _: Sel) -> isize {
    crate::fw::app::STATE.load(std::sync::atomic::Ordering::SeqCst) as isize
}

extern "C" fn app_status_bar_orientation(_this: Id, _: Sel) -> isize {
    current_interface().orientation
}

extern "C" fn app_status_bar_frame(_this: Id, _: Sel) -> Rect {
    Rect::default()
}

extern "C" fn app_begin_background_task(_this: Id, _: Sel, _handler: usize) -> usize {
    1
}

extern "C" fn app_open_url(_this: Id, _: Sel, url: Id) -> bool {
    crate::log!("game asked to open a link ({}), refused", string::describe(url));
    false
}

// A spinner (UIActivityIndicatorView): a view of the right size that keeps
// its settings; nothing spins on screen.
extern "C" fn spinner_init_with_style(this: Id, _: Sel, style: isize) -> Id {
    // UIActivityIndicatorViewStyleWhiteLarge (0) is 37 points, the others 20.
    let side = if style == 0 { 37.0 } else { 20.0 };
    property_set(this, "style", style as u64);
    view_init_with_frame(this, 0, Rect { origin: Point { x: 0.0, y: 0.0 }, size: Size { width: side, height: side } })
}

extern "C" fn spinner_animating(this: Id, _: Sel) -> bool {
    property_get(this, "animating").is_some_and(|v| v != 0)
}

property!(spinner_hides_when_stopped, spinner_set_hides_when_stopped, "hidesWhenStopped", bool, true);
property!(spinner_style, spinner_set_style, "style", isize, 0);
property!(spinner_color, spinner_set_color, "color", id, 0);

extern "C" fn spinner_start(this: Id, _: Sel) {
    property_set(this, "animating", 1);
}

extern "C" fn spinner_stop(this: Id, _: Sel) {
    property_set(this, "animating", 0);
}

// The accelerometer (UIAccelerometer): it keeps its settings and never
// reports a tilt (the phone's sensors aren't wired to it).
extern "C" fn accelerometer_shared(_: Class, _: Sel) -> Id {
    static SHARED: OnceLock<usize> = OnceLock::new();
    *SHARED.get_or_init(|| runtime::alloc_instance(runtime::class_named("UIAccelerometer")) as usize) as Id
}

property!(accelerometer_delegate, accelerometer_set_delegate, "delegate", id, 0);
property!(accelerometer_interval, accelerometer_set_interval, "updateInterval", f64, 0.0);

extern "C" fn app_scheduled_notifications(_this: Id, _: Sel) -> Id {
    collections::array_from(&[])
}

// A local notification only keeps what the game sets; scheduling one does
// nothing, so it never shows.
property!(notification_fire_date, notification_set_fire_date, "fireDate", id, 0);
property!(notification_time_zone, notification_set_time_zone, "timeZone", id, 0);
property!(notification_alert_body, notification_set_alert_body, "alertBody", id, 0);
property!(notification_alert_action, notification_set_alert_action, "alertAction", id, 0);
property!(notification_sound_name, notification_set_sound_name, "soundName", id, 0);
property!(notification_user_info, notification_set_user_info, "userInfo", id, 0);
property!(notification_category, notification_set_category, "category", id, 0);
property!(notification_badge, notification_set_badge, "applicationIconBadgeNumber", isize, 0);
property!(notification_repeat_interval, notification_set_repeat_interval, "repeatInterval", isize, 0);
property!(notification_has_action, notification_set_has_action, "hasAction", bool, true);

pub fn register(r: &mut Registry) {
    crate::fw::geometry::register(r);

    let notification = define_class("UILocalNotification", "NSObject", 8);
    methods(
        notification,
        &[
            ("fireDate", notification_fire_date as usize),
            ("setFireDate:", notification_set_fire_date as usize),
            ("timeZone", notification_time_zone as usize),
            ("setTimeZone:", notification_set_time_zone as usize),
            ("alertBody", notification_alert_body as usize),
            ("setAlertBody:", notification_set_alert_body as usize),
            ("alertAction", notification_alert_action as usize),
            ("setAlertAction:", notification_set_alert_action as usize),
            ("soundName", notification_sound_name as usize),
            ("setSoundName:", notification_set_sound_name as usize),
            ("userInfo", notification_user_info as usize),
            ("setUserInfo:", notification_set_user_info as usize),
            ("category", notification_category as usize),
            ("setCategory:", notification_set_category as usize),
            ("applicationIconBadgeNumber", notification_badge as usize),
            ("setApplicationIconBadgeNumber:", notification_set_badge as usize),
            ("repeatInterval", notification_repeat_interval as usize),
            ("setRepeatInterval:", notification_set_repeat_interval as usize),
            ("hasAction", notification_has_action as usize),
            ("setHasAction:", notification_set_has_action as usize),
        ],
    );

    let screen = define_class("UIScreen", "NSObject", 8);
    class_methods(screen, &[("mainScreen", main_screen as usize), ("screens", screens as usize)]);
    methods(
        screen,
        &[
            ("bounds", screen_bounds_method as usize),
            ("applicationFrame", screen_bounds_method as usize),
            ("nativeBounds", screen_native_bounds as usize),
            ("scale", screen_scale as usize),
            ("nativeScale", screen_scale as usize),
            ("currentMode", screen_current_mode as usize),
            ("preferredMode", screen_current_mode as usize),
            ("availableModes", screen_available_modes as usize),
            // The only mode there is (availableModes) stays the current one.
            ("setCurrentMode:", ignore_id as usize),
            ("overscanCompensation", screen_overscan as usize),
            ("setOverscanCompensation:", screen_set_overscan as usize),
            ("mirroredScreen", zero as usize),
        ],
    );
    let mode = define_class("UIScreenMode", "NSObject", 8);
    methods(mode, &[("size", screen_mode_size as usize), ("pixelAspectRatio", screen_scale_one as usize)]);

    let device = define_class("UIDevice", "NSObject", 8);
    class_methods(device, &[("currentDevice", current_device as usize)]);
    methods(
        device,
        &[
            ("model", device_model as usize),
            ("localizedModel", device_model as usize),
            ("systemName", device_system_name as usize),
            ("systemVersion", device_system_version as usize),
            ("name", device_name as usize),
            ("userInterfaceIdiom", device_idiom as usize),
            ("orientation", device_orientation as usize),
            ("isMultitaskingSupported", yes as usize),
            ("isGeneratingDeviceOrientationNotifications", yes as usize),
            ("beginGeneratingDeviceOrientationNotifications", nothing as usize),
            ("endGeneratingDeviceOrientationNotifications", nothing as usize),
            ("setBatteryMonitoringEnabled:", ignore_bool as usize),
            ("isBatteryMonitoringEnabled", no as usize),
            ("batteryLevel", device_battery_level as usize),
            ("batteryState", device_battery_state as usize),
            ("setProximityMonitoringEnabled:", ignore_bool as usize),
            ("uniqueIdentifier", device_unique_identifier as usize),
            ("identifierForVendor", device_identifier_for_vendor as usize),
        ],
    );

    let responder = runtime::class_named("UIResponder");
    methods(
        responder,
        &[
            ("becomeFirstResponder", yes as usize),
            ("resignFirstResponder", yes as usize),
            ("canBecomeFirstResponder", no as usize),
            ("isFirstResponder", no as usize),
            ("nextResponder", zero as usize),
        ],
    );

    let view = define_class("UIView", "UIResponder", 16);
    class_methods(
        view,
        &[
            ("layerClass", layer_class as usize),
            ("animateWithDuration:animations:", animate_simple as usize),
            ("animateWithDuration:animations:completion:", animate_completion as usize),
            ("animateWithDuration:delay:options:animations:completion:", animate_delay as usize),
            ("animateKeyframesWithDuration:delay:options:animations:completion:", animate_delay as usize),
            (
                "animateWithDuration:delay:usingSpringWithDamping:initialSpringVelocity:options:animations:completion:",
                animate_spring as usize,
            ),
            ("transitionWithView:duration:options:animations:completion:", transition_with_view as usize),
            ("transitionFromView:toView:duration:options:completion:", transition_from_view as usize),
            ("performWithoutAnimation:", perform_without_animation as usize),
            ("beginAnimations:context:", begin_animations as usize),
            ("commitAnimations", commit_animations as usize),
            ("setAnimationDelegate:", set_animation_delegate as usize),
            ("setAnimationWillStartSelector:", set_animation_will_start as usize),
            ("setAnimationDidStopSelector:", set_animation_did_stop as usize),
            ("setAnimationDuration:", ignore_class_double as usize),
            ("setAnimationDelay:", ignore_class_double as usize),
            ("setAnimationStartDate:", ignore_class_word as usize),
            ("setAnimationCurve:", ignore_class_word as usize),
            ("setAnimationRepeatCount:", ignore_class_float as usize),
            ("setAnimationRepeatAutoreverses:", ignore_class_bool as usize),
            ("setAnimationBeginsFromCurrentState:", ignore_class_bool as usize),
            ("setAnimationTransition:forView:cache:", ignore_animation_transition as usize),
            ("setAnimationsEnabled:", ignore_class_bool as usize),
            ("areAnimationsEnabled", class_yes as usize),
        ],
    );
    methods(
        view,
        &[
            ("initWithFrame:", view_init_with_frame as usize),
            ("init", view_init as usize),
            // The move hooks: UIView's own do nothing (see `attach`, `take_out`).
            ("willMoveToSuperview:", ignore_id as usize),
            ("didMoveToSuperview", nothing as usize),
            ("willMoveToWindow:", ignore_id as usize),
            ("didMoveToWindow", nothing as usize),
            ("didAddSubview:", ignore_id as usize),
            ("willRemoveSubview:", ignore_id as usize),
            ("frame", view_frame as usize),
            ("setFrame:", view_set_frame as usize),
            ("bounds", view_bounds as usize),
            ("setBounds:", view_set_bounds as usize),
            ("center", view_center as usize),
            ("setCenter:", view_set_center as usize),
            ("transform", view_transform as usize),
            ("setTransform:", view_set_transform as usize),
            ("layer", view_layer as usize),
            ("addSubview:", view_add_subview as usize),
            ("insertSubview:atIndex:", view_insert_subview as usize),
            ("insertSubview:aboveSubview:", view_insert_above as usize),
            ("insertSubview:belowSubview:", view_insert_below as usize),
            ("removeFromSuperview", view_remove_from_superview as usize),
            ("bringSubviewToFront:", view_bring_to_front as usize),
            ("sendSubviewToBack:", view_send_to_back as usize),
            ("subviews", view_subviews as usize),
            ("superview", view_superview as usize),
            ("window", view_window as usize),
            ("isDescendantOfView:", view_is_descendant as usize),
            ("viewWithTag:", view_with_tag as usize),
            ("isHidden", view_hidden as usize),
            ("setHidden:", view_set_hidden as usize),
            ("alpha", view_alpha as usize),
            ("setAlpha:", view_set_alpha as usize),
            ("isUserInteractionEnabled", view_user_interaction as usize),
            ("setUserInteractionEnabled:", view_set_user_interaction as usize),
            ("isMultipleTouchEnabled", view_multiple_touch as usize),
            ("setMultipleTouchEnabled:", view_set_multiple_touch as usize),
            ("isExclusiveTouch", view_exclusive_touch as usize),
            ("setExclusiveTouch:", view_set_exclusive_touch as usize),
            ("isOpaque", view_opaque as usize),
            ("setOpaque:", view_set_opaque as usize),
            ("clipsToBounds", view_clips as usize),
            ("setClipsToBounds:", view_set_clips as usize),
            ("tag", view_tag as usize),
            ("setTag:", view_set_tag as usize),
            ("autoresizingMask", view_autoresizing as usize),
            ("setAutoresizingMask:", view_set_autoresizing as usize),
            ("autoresizesSubviews", view_autoresizes as usize),
            ("setAutoresizesSubviews:", view_set_autoresizes as usize),
            ("contentMode", view_content_mode as usize),
            ("setContentMode:", view_set_content_mode as usize),
            ("backgroundColor", view_background as usize),
            ("setBackgroundColor:", view_set_background as usize),
            ("contentScaleFactor", view_content_scale as usize),
            ("setContentScaleFactor:", view_set_content_scale as usize),
            ("setNeedsDisplay", nothing as usize),
            ("setNeedsLayout", view_set_needs_layout as usize),
            ("layoutIfNeeded", view_layout_if_needed as usize),
            ("layoutSubviews", nothing as usize),
            ("sizeToFit", nothing as usize),
            ("sizeThatFits:", view_size_that_fits as usize),
            ("convertPoint:fromView:", view_convert_point as usize),
            ("convertPoint:toView:", view_convert_point as usize),
            ("convertRect:fromView:", view_convert_rect as usize),
            ("convertRect:toView:", view_convert_rect as usize),
            ("endEditing:", no_with as usize),
            ("setClearsContextBeforeDrawing:", ignore_bool as usize),
        ],
    );

    let window = define_class("UIWindow", "UIView", 16);
    methods(
        window,
        &[
            ("makeKeyAndVisible", window_make_key_and_visible as usize),
            ("setHidden:", window_set_hidden as usize),
            ("makeKeyWindow", window_make_key as usize),
            ("rootViewController", window_root_controller as usize),
            ("setRootViewController:", window_set_root_controller as usize),
            ("screen", window_screen as usize),
            ("setScreen:", ignore_id as usize),
            ("windowLevel", window_level as usize),
            ("setWindowLevel:", window_set_level as usize),
            ("isKeyWindow", yes as usize),
            ("initWithFrame:", window_init_with_frame as usize),
            ("setFrame:", window_set_frame as usize),
            ("bounds", window_bounds as usize),
            ("setBounds:", window_set_bounds as usize),
        ],
    );

    let controller = define_class("UIViewController", "UIResponder", 8);
    // The interface always matches the device's orientation: nothing to rotate.
    class_methods(controller, &[("attemptRotationToDeviceOrientation", nothing as usize)]);
    methods(
        controller,
        &[
            ("init", controller_init as usize),
            ("initWithNibName:bundle:", crate::fw::storyboard::controller_init_with_nib as usize),
            ("view", controller_view as usize),
            ("setView:", controller_set_view as usize),
            ("isViewLoaded", controller_is_view_loaded as usize),
            ("loadView", crate::fw::storyboard::controller_load_view as usize),
            ("viewDidLoad", nothing as usize),
            ("viewWillLayoutSubviews", nothing as usize),
            ("viewDidLayoutSubviews", nothing as usize),
            ("viewWillAppear:", ignore_bool as usize),
            ("viewDidAppear:", ignore_bool as usize),
            ("viewWillDisappear:", ignore_bool as usize),
            ("viewDidDisappear:", ignore_bool as usize),
            ("didReceiveMemoryWarning", nothing as usize),
            ("interfaceOrientation", controller_orientation as usize),
            ("supportedInterfaceOrientations", controller_supported_orientations as usize),
            ("shouldAutorotate", yes as usize),
            ("shouldAutorotateToInterfaceOrientation:", controller_should_autorotate_to as usize),
            ("prefersStatusBarHidden", yes as usize),
            ("setWantsFullScreenLayout:", ignore_bool as usize),
            ("wantsFullScreenLayout", yes as usize),
            ("presentModalViewController:animated:", controller_present as usize),
            ("presentViewController:animated:completion:", controller_present_completion as usize),
            ("dismissModalViewControllerAnimated:", controller_dismiss as usize),
            ("dismissViewControllerAnimated:completion:", controller_dismiss_completion as usize),
            ("presentedViewController", controller_presented as usize),
            ("modalViewController", controller_presented as usize),
            ("setModalPresentationStyle:", ignore_int as usize),
            ("modalTransitionStyle", crate::fw::storyboard::controller_modal_transition_style as usize),
            ("setModalTransitionStyle:", crate::fw::storyboard::controller_set_modal_transition_style as usize),
            ("setEdgesForExtendedLayout:", ignore_int as usize),
            ("setExtendedLayoutIncludesOpaqueBars:", ignore_bool as usize),
            ("setAutomaticallyAdjustsScrollViewInsets:", ignore_bool as usize),
            ("setNeedsStatusBarAppearanceUpdate", nothing as usize),
            ("preferredStatusBarStyle", zero as usize),
            ("childViewControllers", controller_children as usize),
            ("transitionFromViewController:toViewController:duration:options:animations:completion:", controller_transition as usize),
            ("addChildViewController:", controller_add_child as usize),
            ("removeFromParentViewController", controller_remove_from_parent as usize),
            ("didMoveToParentViewController:", ignore_id as usize),
            ("willMoveToParentViewController:", ignore_id as usize),
            ("parentViewController", controller_parent as usize),
            ("presentingViewController", controller_presenting as usize),
            ("navigationController", controller_navigation as usize),
            ("setTitle:", ignore_id as usize),
            ("title", zero as usize),
        ],
    );
    let _ = (controller_set_presented, controller_set_presenting);
    let _ = return_self;

    let layer = define_class("CALayer", "NSObject", 8);
    methods(
        layer,
        &[
            ("init", layer_init as usize),
            ("isOpaque", layer_opaque as usize),
            ("setOpaque:", layer_set_opaque as usize),
            ("contentsScale", layer_contents_scale as usize),
            ("setContentsScale:", layer_set_contents_scale as usize),
            ("delegate", layer_delegate as usize),
            ("setDelegate:", layer_set_delegate as usize),
            ("setNeedsDisplay", nothing as usize),
            // Geometry, looks, sublayers and animations: fw/layers.rs.
        ],
    );
    let eagl_layer = define_class("CAEAGLLayer", "CALayer", 8);
    methods(
        eagl_layer,
        &[
            ("drawableProperties", layer_drawable_properties as usize),
            ("setDrawableProperties:", layer_set_drawable_properties as usize),
        ],
    );

    let color_class = define_class("UIColor", "NSObject", 40);
    class_methods(
        color_class,
        &[("colorWithRed:green:blue:alpha:", color_rgba as usize), ("colorWithWhite:alpha:", color_white_alpha as usize)],
    );
    class_methods(color_class, &named_colors());
    methods(color_class, &[("CGColor", color_cg as usize)]);

    let image = define_class("UIImage", "NSObject", 16);
    class_methods(
        image,
        &[("imageNamed:", image_named as usize), ("imageWithContentsOfFile:", image_with_contents_of_file as usize)],
    );
    methods(
        image,
        &[
            ("initWithContentsOfFile:", image_init_with_contents_of_file as usize),
            ("size", image_size as usize),
            ("scale", image_scale as usize),
            ("resizableImageWithCapInsets:", image_resizable as usize),
            ("resizableImageWithCapInsets:resizingMode:", image_resizable_mode as usize),
            ("stretchableImageWithLeftCapWidth:topCapHeight:", image_stretchable as usize),
            ("leftCapWidth", image_left_cap_width as usize),
            ("topCapHeight", image_top_cap_height as usize),
            ("capInsets", image_cap_insets as usize),
        ],
    );
    let spinner = define_class("UIActivityIndicatorView", "UIView", 16);
    methods(
        spinner,
        &[
            ("initWithActivityIndicatorStyle:", spinner_init_with_style as usize),
            ("startAnimating", spinner_start as usize),
            ("stopAnimating", spinner_stop as usize),
            ("isAnimating", spinner_animating as usize),
            ("hidesWhenStopped", spinner_hides_when_stopped as usize),
            ("setHidesWhenStopped:", spinner_set_hides_when_stopped as usize),
            ("activityIndicatorViewStyle", spinner_style as usize),
            ("setActivityIndicatorViewStyle:", spinner_set_style as usize),
            ("color", spinner_color as usize),
            ("setColor:", spinner_set_color as usize),
        ],
    );
    let accelerometer = define_class("UIAccelerometer", "NSObject", 8);
    class_methods(accelerometer, &[("sharedAccelerometer", accelerometer_shared as usize)]);
    methods(
        accelerometer,
        &[
            ("delegate", accelerometer_delegate as usize),
            ("setDelegate:", accelerometer_set_delegate as usize),
            ("updateInterval", accelerometer_interval as usize),
            ("setUpdateInterval:", accelerometer_set_interval as usize),
        ],
    );
    let image_view = define_class("UIImageView", "UIView", 16);
    methods(
        image_view,
        &[
            ("initWithImage:", image_view_init_with_image as usize),
            ("isUserInteractionEnabled", image_view_user_interaction as usize),
            ("image", image_view_image as usize),
            ("setImage:", image_view_set_image as usize),
        ],
    );

    let app = runtime::class_named("UIApplication");
    methods(
        app,
        &[
            ("keyWindow", app_key_window as usize),
            ("windows", app_windows as usize),
            ("applicationState", app_state as usize),
            ("statusBarOrientation", app_status_bar_orientation as usize),
            ("supportedInterfaceOrientationsForWindow:", app_supported_orientations as usize),
            ("statusBarFrame", app_status_bar_frame as usize),
            ("setStatusBarOrientation:", ignore_int as usize),
            ("setStatusBarOrientation:animated:", ignore_int_bool as usize),
            ("setStatusBarHidden:", ignore_bool as usize),
            ("setStatusBarHidden:animated:", ignore_bool_bool as usize),
            ("setStatusBarHidden:withAnimation:", ignore_bool_int as usize),
            ("setStatusBarStyle:", ignore_int as usize),
            ("isStatusBarHidden", yes as usize),
            ("isIdleTimerDisabled", app_idle_timer_disabled as usize),
            ("setIdleTimerDisabled:", app_set_idle_timer_disabled as usize),
            ("applicationIconBadgeNumber", app_badge as usize),
            ("setApplicationIconBadgeNumber:", app_set_badge as usize),
            ("setApplicationSupportsShakeToEdit:", ignore_bool as usize),
            ("setNetworkActivityIndicatorVisible:", ignore_bool as usize),
            ("beginBackgroundTaskWithExpirationHandler:", app_begin_background_task as usize),
            ("endBackgroundTask:", ignore_int as usize),
            ("registerForRemoteNotificationTypes:", ignore_int as usize),
            ("registerUserNotificationSettings:", crate::fw::controls::app_register_user_notification_settings as usize),
            ("currentUserNotificationSettings", crate::fw::controls::app_current_notification_settings as usize),
            ("registerForRemoteNotifications", nothing as usize),
            ("unregisterForRemoteNotifications", nothing as usize),
            ("openURL:", app_open_url as usize),
            ("canOpenURL:", no_with as usize),
            ("scheduledLocalNotifications", app_scheduled_notifications as usize),
            ("cancelAllLocalNotifications", nothing as usize),
            ("cancelLocalNotification:", ignore_id as usize),
            ("scheduleLocalNotification:", ignore_id as usize),
            ("presentLocalNotificationNow:", ignore_id as usize),
            ("beginReceivingRemoteControlEvents", nothing as usize),
            ("endReceivingRemoteControlEvents", nothing as usize),
            ("beginIgnoringInteractionEvents", nothing as usize),
            ("endIgnoringInteractionEvents", nothing as usize),
        ],
    );
    r.data("UIWindowLevelNormal", leak(0.0f64));
    r.data("UIWindowLevelStatusBar", leak(1000.0f64));
    r.data("UIWindowLevelAlert", leak(2000.0f64));
    // Core Animation's names (fill modes, timing functions, ...) are in fw/animation.rs.
    r.data("UIBackgroundTaskInvalid", leak(0usize));
    let _ = ignore_id_bool;
}

extern "C" fn ignore_bool_int(_this: Id, _: Sel, _a: u8, _b: isize) {}
extern "C" fn screen_scale_one(_this: Id, _: Sel) -> f64 {
    1.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fw::plist::Value;

    /// Asking to rotate to the device's orientation is fine; the interface is already there.
    #[test]
    fn rotation_request_keeps_the_device_orientation() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let class = runtime::class_named("UIViewController");
        assert!(runtime::responds(class, "attemptRotationToDeviceOrientation"), "+[UIViewController attemptRotationToDeviceOrientation] is missing");
        runtime::send(class, "attemptRotationToDeviceOrientation", &[]);
        let controller = runtime::send(runtime::alloc_instance(class), "init", &[]);
        let device = runtime::send(runtime::class_named("UIDevice"), "currentDevice", &[]);
        assert_eq!(runtime::send(controller, "interfaceOrientation", &[]), runtime::send(device, "orientation", &[]));
    }

    /// UIKit's named colours, with Apple's values. A missing one stops the
    /// game code that asks for it (Dungeon Plunder's fight screen and
    /// +orangeColor), and 32-bit games need a selector row for each.
    #[test]
    fn every_named_colour_has_apples_value() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let (third, two_thirds) = (1.0 / 3.0, 2.0 / 3.0);
        let named: [(&str, [f64; 4]); 15] = [
            ("blackColor", [0.0, 0.0, 0.0, 1.0]),
            ("darkGrayColor", [third, third, third, 1.0]),
            ("lightGrayColor", [two_thirds, two_thirds, two_thirds, 1.0]),
            ("whiteColor", [1.0, 1.0, 1.0, 1.0]),
            ("grayColor", [0.5, 0.5, 0.5, 1.0]),
            ("redColor", [1.0, 0.0, 0.0, 1.0]),
            ("greenColor", [0.0, 1.0, 0.0, 1.0]),
            ("blueColor", [0.0, 0.0, 1.0, 1.0]),
            ("cyanColor", [0.0, 1.0, 1.0, 1.0]),
            ("yellowColor", [1.0, 1.0, 0.0, 1.0]),
            ("magentaColor", [1.0, 0.0, 1.0, 1.0]),
            ("orangeColor", [1.0, 0.5, 0.0, 1.0]),
            ("purpleColor", [0.5, 0.0, 0.5, 1.0]),
            ("brownColor", [0.6, 0.4, 0.2, 1.0]),
            ("clearColor", [0.0, 0.0, 0.0, 0.0]),
        ];
        let class = runtime::class_named("UIColor");
        let table = crate::guest32::objc::selectors::FrameworkSelectors::builtin();
        for (name, rgba) in named {
            assert!(runtime::responds(class, name), "+[UIColor {name}] is missing");
            assert!(table.get(name).is_some(), "no framework_selectors.tsv row for {name}");
            let got = crate::fw::graphics::color_rgba(runtime::send(class, name, &[])).unwrap();
            assert!(got.iter().zip(rgba).all(|(a, b)| (a - b).abs() < 1e-9), "{name}: {got:?}, Apple's is {rgba:?}");
        }
    }

    /// A stretchable copy of a picture (button backgrounds) is a new picture
    /// of the same file and size that keeps its caps (the original has
    /// none); stretchableImageWithLeftCapWidth:topCapHeight: leaves a
    /// one-point middle. Its nine parts are cut into files at the caps.
    #[test]
    fn resizable_images_keep_the_picture_and_their_caps() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let folder = std::env::temp_dir().join(format!("gamebridge-resizable-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("button.png");
        // 6x4, each column its own red (0, 40, ... 200).
        let pixels: Vec<u8> = (0..6 * 4).flat_map(|i| [(i % 6) as u8 * 40, 0, 0, 255]).collect();
        std::fs::write(&path, crate::fw::png::encode(6, 4, &pixels, false)).unwrap();
        let image = image_from_path(&path.to_string_lossy());
        assert_ne!(image, 0);
        let sel = runtime::sel("resizableImageWithCapInsets:");
        let imp = runtime::imp_for(image, sel).expect("-[UIImage resizableImageWithCapInsets:] is missing");
        let resizable: extern "C" fn(Id, Sel, f64, f64, f64, f64) -> Id = unsafe { std::mem::transmute(imp) };
        let stretched = resizable(image, sel, 1.0, 2.0, 1.0, 2.0);
        assert!(stretched != 0 && stretched != image);
        assert_eq!(image_points(stretched), Size { width: 6.0, height: 4.0 });
        assert_eq!(image_info(stretched).map(|info| info.path.clone()), Some(path.to_string_lossy().into_owned()));
        assert_eq!(image_info(stretched).unwrap().caps, Some([1.0, 2.0, 1.0, 2.0]));
        assert_eq!(image_info(image).unwrap().caps, None);
        let sel = runtime::sel("resizableImageWithCapInsets:resizingMode:");
        let imp = runtime::imp_for(image, sel).expect("-[UIImage resizableImageWithCapInsets:resizingMode:] is missing");
        let with_mode: extern "C" fn(Id, Sel, f64, f64, f64, f64, isize) -> Id = unsafe { std::mem::transmute(imp) };
        assert_eq!(image_info(with_mode(image, sel, 1.0, 1.0, 1.0, 1.0, 1)).unwrap().caps, Some([1.0; 4]));
        // UIImageResizingModeTile 0, Stretch 1.
        let tiles = |mode| image_info(with_mode(image, sel, 1.0, 1.0, 1.0, 1.0, mode)).unwrap().tile;
        assert_eq!((tiles(0), tiles(1)), (true, false));
        let sel = runtime::sel("stretchableImageWithLeftCapWidth:topCapHeight:");
        let imp = runtime::imp_for(image, sel).expect("-[UIImage stretchableImageWithLeftCapWidth:topCapHeight:] is missing");
        let stretchable: extern "C" fn(Id, Sel, isize, isize) -> Id = unsafe { std::mem::transmute(imp) };
        let old_style = stretchable(image, sel, 2, 1);
        assert_eq!(image_info(old_style).unwrap().caps, Some([1.0, 2.0, 2.0, 3.0]));
        assert_eq!((runtime::send(old_style, "leftCapWidth", &[]), runtime::send(old_style, "topCapHeight", &[])), (2, 1));
        // No left cap: stretched whole across.
        assert_eq!(image_info(stretchable(image, sel, 0, 1)).unwrap().caps, Some([1.0, 0.0, 2.0, 0.0]));
        // The parts: corners 2x1, the middle 2x2 holding columns 2 and 3.
        let mut parts = Vec::new();
        for _ in 0..500 {
            parts = (0..9).map(|n| crate::fw::graphics::picture_part_file(stretched, n)).collect();
            if parts.iter().all(|p| p.is_some()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let read = |n: usize| image::open(parts[n].as_ref().expect("part file made")).unwrap().to_rgba8();
        let sizes: Vec<(u32, u32)> = (0..9).map(|n| read(n).dimensions()).collect();
        assert_eq!(sizes, [(2, 1), (2, 1), (2, 1), (2, 2), (2, 2), (2, 2), (2, 1), (2, 1), (2, 1)]);
        let middle = read(4);
        assert_eq!((middle.get_pixel(0, 0)[0], middle.get_pixel(1, 1)[0]), (80, 120));
        std::fs::remove_dir_all(&folder).unwrap();
    }

    /// A stretchable copy is an image of its own (+0, autoreleased, as
    /// UIKit gives it): kept, it lives on after the image it was made from
    /// is gone, whether that came from a file or was made in memory.
    #[test]
    fn stretchable_copies_outlive_their_source() {
        use crate::fw::counted;
        use crate::fw::graphics::{Bitmap, Space, PREMULTIPLIED_LAST};
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let sel = runtime::sel("resizableImageWithCapInsets:");
        let stretch = |image: Id| {
            let imp = runtime::imp_for(image, sel).unwrap();
            let resizable: extern "C" fn(Id, Sel, f64, f64, f64, f64) -> Id = unsafe { std::mem::transmute(imp) };
            counted::retain(resizable(image, sel, 2.0, 2.0, 2.0, 2.0))
        };
        // From a file.
        let source = make_image(ImageInfo { path: "/nowhere/frame.png".into(), pixels: (8, 8), scale: 1.0, ..Default::default() });
        let stretched = stretch(source);
        counted::release(source);
        assert_eq!(counted::count(source), None, "the source is gone");
        assert!(counted::count(stretched).is_some(), "the copy is still alive");
        assert_eq!(image_info(stretched).map(|info| (info.path.as_str(), info.caps)), Some(("/nowhere/frame.png", Some([2.0; 4]))));
        // Made in memory: the copy keeps the pixels.
        let bitmap = Bitmap { width: 8, height: 6, bits_per_pixel: 32, bytes_per_row: 32, info: PREMULTIPLIED_LAST, space: Space::Rgb, data: vec![255; 8 * 6 * 4] };
        let pixels = crate::fw::graphics::make_image(bitmap);
        let source = runtime::send(runtime::send(runtime::class_named("UIImage"), "alloc", &[]), "initWithCGImage:", &[pixels]);
        counted::release(pixels);
        let stretched = stretch(source);
        counted::release(source);
        assert_eq!(counted::count(source), None, "the source is gone");
        let kept = crate::fw::graphics::image_cg(stretched);
        assert!(kept != 0 && counted::count(kept).is_some(), "the copy still holds the pixels");
        assert_eq!(image_points(stretched), Size { width: 8.0, height: 6.0 });
    }

    /// The screen takes one of its own modes back, and keeps its overscan setting.
    #[test]
    fn screen_takes_its_mode_and_overscan_setting() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let screen = runtime::send(runtime::class_named("UIScreen"), "mainScreen", &[]);
        let mode = runtime::send(screen, "preferredMode", &[]);
        assert!(runtime::responds(screen, "setCurrentMode:"), "-[UIScreen setCurrentMode:] is missing");
        runtime::send(screen, "setCurrentMode:", &[mode]);
        assert_eq!(runtime::send(screen, "currentMode", &[]), mode);
        assert_eq!(runtime::send(screen, "overscanCompensation", &[]), 0);
        runtime::send(screen, "setOverscanCompensation:", &[2]);
        assert_eq!(runtime::send(screen, "overscanCompensation", &[]), 2);
        runtime::send(screen, "setOverscanCompensation:", &[0]);
    }

    #[test]
    fn old_app_keeps_its_launch_image_shape_at_the_display_height() {
        let files = vec!["Default-568h@2x.png".to_string(), "Default.png".to_string()];
        let aspect = app_aspect((2424.0, 1080.0), Some(&Value::Dict(vec![])), &files);
        assert!((aspect - 568.0 / 320.0).abs() < 1e-9);
        let (w, h) = fit_points((2424.0, 1080.0), aspect);
        assert_eq!((w, h), (958.0, 540.0)); // 1916x1080 pixels, side bars
        assert!(w / h > 1.7 && w / h < 1.8);
    }

    #[test]
    fn dismissing_closes_the_stack_and_animations_complete() {
        // A shows B shows C (stand-in ids; only the stored links are used).
        let (a, b, c) = (0x7a00, 0x7b00, 0x7c00);
        for (from, to) in [(a, b), (b, c)] {
            property_set(from, "presented", to as u64);
            property_set(to, "presenting", from as u64);
        }
        dismiss(a, false);
        for (object, key) in [(a, "presented"), (b, "presented"), (b, "presenting"), (c, "presenting")] {
            assert_eq!(property_get(object, key), Some(0), "{object:#x} {key}");
        }
        // Sent to the top screen (nothing on top), it closes only that one.
        property_set(a, "presented", b as u64);
        property_set(b, "presenting", a as u64);
        property_set(b, "presented", c as u64);
        property_set(c, "presenting", b as u64);
        dismiss(c, false);
        assert_eq!((property_get(b, "presented"), property_get(a, "presented")), (Some(0), Some(b as u64)));

        // Block animations: the changes apply at once; completion(YES) waits
        // for the next run-loop turn.
        static RAN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        static FINISHED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let animations = crate::fw::media::make_callback(5, |n| RAN.store(n, std::sync::atomic::Ordering::SeqCst));
        #[repr(C)]
        struct Completion {
            isa: usize,
            flags: i32,
            reserved: i32,
            invoke: usize,
            descriptor: usize,
        }
        extern "C" fn completion_invoke(_block: usize, finished: u8) {
            FINISHED.store(1 + finished as usize, std::sync::atomic::Ordering::SeqCst);
        }
        static DESCRIPTOR: [usize; 2] = [0, std::mem::size_of::<Completion>()];
        let completion = leak(Completion {
            isa: 0,
            flags: 1 << 28, // a global block: copying keeps it
            reserved: 0,
            invoke: completion_invoke as usize,
            descriptor: DESCRIPTOR.as_ptr() as usize,
        });
        let mut queued: Vec<Box<dyn FnOnce() + Send>> = Vec::new();
        animate_with(animations, completion, |work| queued.push(work));
        assert_eq!(RAN.load(std::sync::atomic::Ordering::SeqCst), 5, "animations block runs at once");
        assert_eq!((FINISHED.load(std::sync::atomic::Ordering::SeqCst), queued.len()), (0, 1), "completion waits");
        queued.pop().unwrap()();
        assert_eq!(FINISHED.load(std::sync::atomic::Ordering::SeqCst), 2, "completion told finished = YES");
    }

    /// iOS 3 style animations (+beginAnimations:context: ... +commitAnimations):
    /// the changes apply at once; the delegate's did-stop selector gets the
    /// animation's id, finished = YES and the context on the next run-loop
    /// turn, each nested block with its own settings.
    #[test]
    fn old_style_animations_tell_their_delegate_when_they_stop() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        static HEARD: Mutex<Vec<(&'static str, Id, Option<bool>, usize)>> = Mutex::new(Vec::new());
        extern "C" fn stopped(_: Id, _: Sel, id: Id, finished: Id, context: usize) {
            HEARD.lock().unwrap().push(("stopped", id, Some(collections::number_as_f64(finished) != 0.0), context));
        }
        extern "C" fn reel_stopped(_: Id, _: Sel, id: Id) {
            HEARD.lock().unwrap().push(("reel", id, None, 0));
        }
        extern "C" fn starting(_: Id, _: Sel, id: Id, context: usize) {
            HEARD.lock().unwrap().push(("starting", id, None, context));
        }
        let class = define_class("G32OldAnimationDelegate", "NSObject", 8);
        methods(
            class,
            &[
                ("animationDidStop:finished:context:", stopped as usize),
                ("reelStopped:", reel_stopped as usize),
                ("animationWillStart:context:", starting as usize),
            ],
        );
        let delegate = runtime::send(runtime::send(class, "alloc", &[]), "init", &[]);
        let view = runtime::class_named("UIView");
        let send = |name: &str, args: &[usize]| runtime::send(view, name, args);
        let (outer, inner) = (string::make("spin"), string::make("reel"));
        let mut queued: Vec<Box<dyn FnOnce() + Send>> = Vec::new();

        send("beginAnimations:context:", &[outer, 0x1234]);
        send("setAnimationDelegate:", &[delegate]);
        send("setAnimationWillStartSelector:", &[runtime::sel("animationWillStart:context:")]);
        send("setAnimationDidStopSelector:", &[runtime::sel("animationDidStop:finished:context:")]);
        send("beginAnimations:context:", &[inner, 0]);
        send("setAnimationDelegate:", &[delegate]);
        send("setAnimationDidStopSelector:", &[runtime::sel("reelStopped:")]);
        commit_old_animation(|work| queued.push(work));
        commit_old_animation(|work| queued.push(work));
        assert!(HEARD.lock().unwrap().is_empty(), "nothing is told before the next run-loop turn");
        assert_eq!(queued.len(), 2);
        for work in queued.drain(..) {
            work();
        }
        assert_eq!(
            *HEARD.lock().unwrap(),
            [("reel", inner, None, 0), ("starting", outer, None, 0x1234), ("stopped", outer, Some(true), 0x1234)],
            "each block keeps its own delegate selectors"
        );
        // No delegate: nothing to tell; a commit with no begin is ignored.
        send("beginAnimations:context:", &[0, 0]);
        commit_old_animation(|work| queued.push(work));
        commit_old_animation(|work| queued.push(work));
        assert!(queued.is_empty());
    }

    /// UIView's move hooks, in UIKit's order: the moving view (and the views
    /// in it, for the window ones) hears will/didMoveToSuperview and
    /// will/didMoveToWindow, the superview didAddSubview: / willRemoveSubview:.
    /// UIView has them all (doing nothing), so a subclass may call super.
    #[test]
    fn views_hear_their_moves_in_uikits_order() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        static EVENTS: Mutex<Vec<(Id, String)>> = Mutex::new(Vec::new());
        fn note(this: Id, what: &str, other: Id) {
            EVENTS.lock().unwrap().push((this, format!("{what} {other:#x}")));
        }
        fn call_super(this: Id, name: &str, argument: Option<Id>) {
            let (uiview, sel) = (runtime::class_named("UIView"), runtime::sel(name));
            let found = lock().lookup(uiview, sel);
            // UIView lacking it is noted (a panic can't leave this C function).
            let Some(imp) = found else { return note(this, &format!("no super {name}"), 0) };
            match argument {
                Some(other) => unsafe { std::mem::transmute::<usize, extern "C" fn(Id, Sel, Id)>(imp)(this, sel, other) },
                None => unsafe { std::mem::transmute::<usize, extern "C" fn(Id, Sel)>(imp)(this, sel) },
            }
        }
        extern "C" fn will_superview(this: Id, _: Sel, other: Id) {
            call_super(this, "willMoveToSuperview:", Some(other));
            note(this, "willMoveToSuperview:", other);
        }
        extern "C" fn did_superview(this: Id, _: Sel) {
            call_super(this, "didMoveToSuperview", None);
            note(this, "didMoveToSuperview", view_state(this).superview);
        }
        extern "C" fn will_window(this: Id, _: Sel, other: Id) {
            call_super(this, "willMoveToWindow:", Some(other));
            note(this, "willMoveToWindow:", other);
        }
        extern "C" fn did_window(this: Id, _: Sel) {
            call_super(this, "didMoveToWindow", None);
            note(this, "didMoveToWindow", window_of(this));
        }
        extern "C" fn did_add(this: Id, _: Sel, other: Id) {
            call_super(this, "didAddSubview:", Some(other));
            note(this, "didAddSubview:", other);
        }
        extern "C" fn will_remove(this: Id, _: Sel, other: Id) {
            call_super(this, "willRemoveSubview:", Some(other));
            note(this, "willRemoveSubview:", other);
        }
        let class = define_class("UIKitTestMovingView", "UIView", 16);
        methods(
            class,
            &[
                ("willMoveToSuperview:", will_superview as usize),
                ("didMoveToSuperview", did_superview as usize),
                ("willMoveToWindow:", will_window as usize),
                ("didMoveToWindow", did_window as usize),
                ("didAddSubview:", did_add as usize),
                ("willRemoveSubview:", will_remove as usize),
            ],
        );
        let new_view = |class: Class| view_init_with_frame(runtime::send(class, "alloc", &[]), 0, Rect::new(0.0, 0.0, 10.0, 10.0));
        let take = || std::mem::take(&mut *EVENTS.lock().unwrap());
        let window = new_view(runtime::class_named("UIWindow"));
        let parent = new_view(class);
        runtime::send(window, "addSubview:", &[parent]);
        let (view, inner) = (new_view(class), new_view(class));
        runtime::send(view, "addSubview:", &[inner]);
        take();
        let e = |object: Id, what: &str, other: Id| (object, format!("{what} {other:#x}"));
        runtime::send(parent, "addSubview:", &[view]);
        assert_eq!(
            take(),
            [
                e(view, "willMoveToSuperview:", parent),
                e(view, "willMoveToWindow:", window),
                e(inner, "willMoveToWindow:", window),
                e(view, "didMoveToWindow", window),
                e(inner, "didMoveToWindow", window),
                e(view, "didMoveToSuperview", parent),
                e(parent, "didAddSubview:", view),
            ]
        );
        runtime::send(view, "removeFromSuperview", &[]);
        assert_eq!(
            take(),
            [
                e(parent, "willRemoveSubview:", view),
                e(view, "willMoveToSuperview:", 0),
                e(view, "willMoveToWindow:", 0),
                e(inner, "willMoveToWindow:", 0),
                e(view, "didMoveToWindow", 0),
                e(inner, "didMoveToWindow", 0),
                e(view, "didMoveToSuperview", 0),
            ]
        );
    }

    /// UIKit's layout pass: a view added to a shown window gets -layoutSubviews
    /// once before the next frame (superview first, its controller's
    /// viewWill/DidLayoutSubviews around it), and not again until something
    /// changes; -layoutIfNeeded lays out at once.
    #[test]
    fn views_are_laid_out_once_per_change_before_the_next_frame() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        static EVENTS: Mutex<Vec<(Id, &'static str)>> = Mutex::new(Vec::new());
        extern "C" fn layout(this: Id, _: Sel) {
            EVENTS.lock().unwrap().push((this, "layoutSubviews"));
        }
        extern "C" fn will_layout(this: Id, _: Sel) {
            EVENTS.lock().unwrap().push((this, "viewWillLayoutSubviews"));
        }
        extern "C" fn did_layout(this: Id, _: Sel) {
            EVENTS.lock().unwrap().push((this, "viewDidLayoutSubviews"));
        }
        let view_class = define_class("G32LayoutTestView", "UIView", 16);
        methods(view_class, &[("layoutSubviews", layout as usize)]);
        let controller_class = define_class("G32LayoutTestController", "UIViewController", 8);
        methods(controller_class, &[("viewWillLayoutSubviews", will_layout as usize), ("viewDidLayoutSubviews", did_layout as usize)]);
        let new_view = |class: Class, frame: Rect| view_init_with_frame(runtime::send(class, "alloc", &[]), 0, frame);
        let take = || std::mem::take(&mut *EVENTS.lock().unwrap());

        let root = new_view(view_class, screen_bounds());
        let controller = runtime::send(runtime::send(controller_class, "alloc", &[]), "init", &[]);
        runtime::send(controller, "setView:", &[root]);
        let window = new_view(runtime::class_named("UIWindow"), screen_bounds());
        window_set_root_controller(window, 0, controller);
        window_make_key_and_visible(window, 0);
        let child = new_view(view_class, Rect::new(0.0, 0.0, 10.0, 10.0));
        runtime::send(root, "addSubview:", &[child]);
        assert!(take().is_empty(), "nothing is laid out before the run loop's pass");
        layout_pass();
        assert_eq!(
            take(),
            [(controller, "viewWillLayoutSubviews"), (root, "layoutSubviews"), (controller, "viewDidLayoutSubviews"), (child, "layoutSubviews")]
        );
        layout_pass();
        assert!(take().is_empty(), "nothing changed: no layout");
        // Moving keeps the size: no layout. A new size lays out the view and its superview.
        view_set_frame(child, 0, Rect::new(5.0, 5.0, 10.0, 10.0));
        layout_pass();
        assert!(take().is_empty());
        view_set_frame(child, 0, Rect::new(5.0, 5.0, 20.0, 10.0));
        layout_pass();
        let events = take();
        assert_eq!(events.iter().filter(|e| *e == &(child, "layoutSubviews")).count(), 1, "{events:x?}");
        assert!(events.iter().position(|e| e == &(root, "layoutSubviews")) < events.iter().position(|e| e == &(child, "layoutSubviews")), "{events:x?}");
        // setNeedsLayout waits for the pass; layoutIfNeeded doesn't.
        runtime::send(child, "setNeedsLayout", &[]);
        assert!(take().is_empty());
        runtime::send(child, "layoutIfNeeded", &[]);
        assert_eq!(take(), [(child, "layoutSubviews")]);
        layout_pass();
        assert!(take().is_empty());
        // Removing a subview lays out the view it left.
        runtime::send(child, "removeFromSuperview", &[]);
        layout_pass();
        assert_eq!(take(), [(controller, "viewWillLayoutSubviews"), (root, "layoutSubviews"), (controller, "viewDidLayoutSubviews")]);
    }

    /// UIKit's autoresizing: a view's new size moves and sizes its subviews
    /// by their masks at once (the flexible parts share the change by their
    /// sizes), down the tree, unless it doesn't autoresize subviews. A nib
    /// laid out 480 wide fills a 568-wide window this way.
    #[test]
    fn subviews_follow_their_autoresizing_masks() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let new_view = |frame: Rect, mask: u64| {
            let view = view_init_with_frame(runtime::send(runtime::class_named("UIView"), "alloc", &[]), 0, frame);
            property_set(view, "autoresizing", mask);
            view
        };
        let root = new_view(Rect::new(0.0, 0.0, 480.0, 320.0), 0);
        let background = new_view(Rect::new(0.0, 0.0, 480.0, 320.0), 2 | 16); // flexible width and height
        let inside = new_view(Rect::new(10.0, 10.0, 460.0, 20.0), 2); // flexible width, in the background
        let centred = new_view(Rect::new(100.0, 200.0, 72.0, 20.0), 1 | 4 | 32); // flexible margins but the top
        let fixed = new_view(Rect::new(10.0, 10.0, 20.0, 20.0), 0);
        let right = new_view(Rect::new(400.0, 10.0, 50.0, 20.0), 1); // flexible left margin
        runtime::send(background, "addSubview:", &[inside]);
        for child in [background, centred, fixed, right] {
            runtime::send(root, "addSubview:", &[child]);
        }
        send_rect(root, "setFrame:", Rect::new(0.0, 0.0, 568.0, 320.0));
        let frame = |view: Id| view_state(view).frame;
        assert_eq!(frame(background), Rect::new(0.0, 0.0, 568.0, 320.0));
        assert_eq!(frame(inside), Rect::new(10.0, 10.0, 548.0, 20.0), "grandchildren follow");
        // 88 more points: left margin 100 of the 408 flexible takes its share.
        let moved = frame(centred);
        assert!((moved.origin.x - (100.0 + 88.0 * 100.0 / 408.0)).abs() < 1e-9 && moved.size.width == 72.0, "{moved:?}");
        assert_eq!(moved.origin.y, 200.0, "same height: nothing moves down");
        assert_eq!(frame(fixed), Rect::new(10.0, 10.0, 20.0, 20.0));
        assert_eq!(frame(right), Rect::new(488.0, 10.0, 50.0, 20.0));
        // Through setBounds: too; and not when the view doesn't autoresize subviews.
        send_rect(root, "setBounds:", Rect::new(0.0, 0.0, 480.0, 320.0));
        assert_eq!(frame(background).size.width, 480.0);
        property_set(root, "autoresizes", 0);
        send_rect(root, "setFrame:", Rect::new(0.0, 0.0, 568.0, 320.0));
        assert_eq!(frame(background).size.width, 480.0);
    }

    #[test]
    fn storyboard_app_fills_the_display() {
        let info = Value::Dict(vec![("UILaunchStoryboardName".into(), Value::String("Launch".into()))]);
        let aspect = app_aspect((2424.0, 1080.0), Some(&info), &[]);
        assert_eq!(fit_points((2424.0, 1080.0), aspect), (1212.0, 540.0));
    }

    #[test]
    fn old_apps_get_a_real_iphone_size_unreal_and_storyboard_apps_the_display() {
        let display = (2424.0, 1080.0);
        let tall = ["Default-568h@2x.png".to_string()];
        // Old app (TownCraft): an iPhone 5's screen, stretched by the window.
        assert_eq!(told_screen(display, None, &tall, false), (568.0, 320.0, 2.0));
        // Nothing declared: the first iPhones' screen.
        assert_eq!(told_screen(display, None, &[], false), (480.0, 320.0, 2.0));
        // Unreal games draw well at any size: the shape at the display's pixels.
        assert_eq!(told_screen(display, None, &tall, true), (958.0, 540.0, 2.0));
        let info = Value::Dict(vec![("UILaunchStoryboardName".into(), Value::String("Launch".into()))]);
        assert_eq!(told_screen(display, Some(&info), &[], false), (1212.0, 540.0, 2.0));
    }

    #[test]
    fn launch_image_list_and_no_images() {
        let image = Value::Dict(vec![("UILaunchImageSize".into(), Value::String("{375, 667}".into()))]);
        let info = Value::Dict(vec![("UILaunchImages".into(), Value::Array(vec![image]))]);
        assert!((app_aspect((2424.0, 1080.0), Some(&info), &[]) - 667.0 / 375.0).abs() < 1e-9);
        // Nothing declared: the 3:2 screen of the first iPhones.
        assert_eq!(app_aspect((2424.0, 1080.0), None, &[]), 1.5);
        // A 4:3 tablet is narrower than 16:9: the shape is fitted by width.
        let aspect = app_aspect((2048.0, 1536.0), None, &["Default-568h@2x.png".into()]);
        assert_eq!(aspect, 1.5);
        assert_eq!(fit_points((2048.0, 1536.0), aspect), (1024.0, 682.0));
    }

    fn screen_info(sdk: &str, orientations: &[&str]) -> Value {
        Value::Dict(vec![
            ("DTSDKName".into(), Value::String(sdk.into())),
            (
                "UISupportedInterfaceOrientations".into(),
                Value::Array(orientations.iter().map(|name| Value::String((*name).into())).collect()),
            ),
        ])
    }

    #[test]
    fn old_landscape_apps_report_portrait_but_keep_landscape_game_geometry() {
        let info = screen_info("iphoneos6.1", &["UIInterfaceOrientationLandscapeRight", "UIInterfaceOrientationLandscapeLeft"]);
        assert!(legacy_screen(Some(&info)));
        assert_eq!(interface(Some(&info), true), Interface { orientation: 3, mask: (1 << 3) | (1 << 4) });
        assert_eq!(oriented_screen((568.0, 320.0, 2.0), Some(&info)), (568.0, 320.0, 2.0));
        assert_eq!(reported_screen((568.0, 320.0, 2.0), Some(&info)), (320.0, 568.0, 2.0));
        assert_eq!(screen_pixel_size((568.0, 320.0, 2.0)), (640.0, 1136.0));
    }

    #[test]
    fn old_portrait_apps_stay_portrait_and_newer_apps_keep_the_new_rules() {
        let portrait = screen_info("iphoneos7.1", &["UIInterfaceOrientationPortrait"]);
        assert_eq!(interface(Some(&portrait), true), Interface { orientation: 1, mask: 1 << 1 });
        assert_eq!(oriented_screen((568.0, 320.0, 2.0), Some(&portrait)), (320.0, 568.0, 2.0));
        assert_eq!(reported_screen((320.0, 568.0, 2.0), Some(&portrait)), (320.0, 568.0, 2.0));
        assert_eq!(screen_pixel_size((320.0, 568.0, 2.0)), (640.0, 1136.0));
        let initial_only = Value::Dict(vec![
            ("DTSDKName".into(), Value::String("iphoneos5.0".into())),
            ("UIInterfaceOrientation".into(), Value::String("UIInterfaceOrientationPortrait".into())),
        ]);
        assert_eq!(interface(Some(&initial_only), true), Interface { orientation: 1, mask: 1 << 1 });

        let mut newer_entries = match screen_info("iphoneos8.2", &["UIInterfaceOrientationPortrait"]) {
            Value::Dict(entries) => entries,
            _ => unreachable!(),
        };
        newer_entries.push(("MinimumOSVersion".into(), Value::String("5.0".into())));
        let newer = Value::Dict(newer_entries);
        assert!(!legacy_screen(Some(&newer)));
        assert_eq!(oriented_screen((568.0, 320.0, 2.0), Some(&newer)), (568.0, 320.0, 2.0));
        assert_eq!(reported_screen((568.0, 320.0, 2.0), Some(&newer)), (568.0, 320.0, 2.0));
        assert_eq!(interface(Some(&newer), false), Interface { orientation: 3, mask: (1 << 3) | (1 << 4) });
        assert!(!legacy_screen(None));
    }

    /// An old landscape game's window is landscape, whichever full-screen
    /// size it was made with; anything else keeps its size. Reading the
    /// bounds and setting them back changes nothing.
    #[test]
    fn old_landscape_windows_keep_landscape_bounds() {
        let info = screen_info("iphoneos6.1", &["UIInterfaceOrientationLandscapeRight", "UIInterfaceOrientationLandscapeLeft"]);
        let geometry = geometry_for((568.0, 320.0, 2.0), Some(&info));
        let landscape = Rect::new(0.0, 0.0, 568.0, 320.0);
        assert_eq!(window_frame(geometry, Rect::new(0.0, 0.0, 320.0, 568.0)), landscape, "a window made from UIScreen.bounds");
        assert_eq!(window_frame(geometry, landscape), landscape);
        assert_eq!(window_frame(geometry, Rect::new(0.0, 0.0, 200.0, 100.0)), Rect::new(0.0, 0.0, 200.0, 100.0));
        let current = geometry_for((568.0, 320.0, 2.0), None);
        assert_eq!(window_frame(current, Rect::new(0.0, 0.0, 320.0, 568.0)), Rect::new(0.0, 0.0, 320.0, 568.0));
    }

    #[test]
    fn old_fullscreen_window_bounds_are_reported_portrait() {
        let old_info = screen_info("iphoneos7.0", &["UIInterfaceOrientationLandscapeRight"]);
        let new_info = screen_info("iphoneos8.0", &["UIInterfaceOrientationLandscapeRight"]);
        let old = geometry_for((568.0, 320.0, 2.0), Some(&old_info));
        let newer = geometry_for((568.0, 320.0, 2.0), Some(&new_info));
        let landscape = Rect::new(0.0, 0.0, 568.0, 320.0);
        let portrait = Rect::new(0.0, 0.0, 320.0, 568.0);
        assert_eq!(window_reported_bounds(old, landscape), portrait);
        assert_eq!(window_frame(old, portrait), landscape);
        assert_eq!(window_reported_bounds(old, window_frame(old, portrait)), portrait);
        assert_eq!(window_reported_bounds(newer, landscape), landscape);
        let custom = Rect::new(5.0, 6.0, 200.0, 100.0);
        assert_eq!(window_reported_bounds(old, custom), custom);
    }

    extern "C" fn overriding_controller_view(this: Id, _: Sel) -> Id {
        let reads = property_get(this, "viewReads").unwrap_or(0);
        property_set(this, "viewReads", reads + 1);
        property_get(this, "overrideView").unwrap_or(0) as Id
    }

    fn overriding_controller_class() -> Class {
        static CLASS: OnceLock<Class> = OnceLock::new();
        *CLASS.get_or_init(|| {
            let class = define_class("UIKitTestOverridingController", "UIViewController", 8);
            methods(class, &[("view", overriding_controller_view as usize)]);
            class
        })
    }

    #[test]
    fn attachment_checks_geometry_before_reading_an_overridden_controller_view() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let info = screen_info("iphoneos7.0", &["UIInterfaceOrientationLandscapeRight"]);
        let geometry = geometry_for((568.0, 320.0, 2.0), Some(&info));
        let portrait = Rect::new(0.0, 0.0, 320.0, 568.0);
        let landscape = Rect::new(0.0, 0.0, 568.0, 320.0);
        let new_view = |frame| view_init_with_frame(runtime::send(runtime::class_named("UIView"), "alloc", &[]), 0, frame);
        let new_controller = |class| runtime::send(runtime::send(class, "alloc", &[]), "init", &[]);
        let parent = new_controller(runtime::class_named("UIViewController"));
        let child = new_controller(overriding_controller_class());
        let parent_view = new_view(landscape);
        let child_view = new_view(portrait);
        controller_set_view(parent, 0, parent_view);
        property_set(child, "overrideView", child_view as u64);
        controller_add_child(parent, 0, child);
        assert_eq!(property_get(child, "view"), None);
        assert_eq!(property_get(child_view, "viewController"), None);

        let small = Rect::new(0.0, 0.0, 20.0, 20.0);
        let unrelated = new_view(small);
        attach_with_geometry(parent_view, unrelated, None, geometry);
        assert_eq!(property_get(child, "viewReads"), None);
        assert_eq!(view_state(unrelated).frame, small);

        attach_with_geometry(parent_view, child_view, None, geometry);
        assert_eq!(view_state(child_view).frame, landscape);
        assert!(property_get(child, "viewReads").is_some_and(|reads| reads > 0));

        let other = new_view(portrait);
        attach_with_geometry(parent_view, other, None, geometry);
        assert_eq!(view_state(other).frame, portrait);
    }

    #[test]
    fn controller_links_identify_a_view_without_a_back_pointer() {
        let parent = 0x1110;
        let child = 0x2120;
        let parent_view = 0x3130;
        let child_view = 0x4140;
        property_set(parent, "view", parent_view as u64);
        property_set(child, "view", child_view as u64);
        controller_add_child(parent, 0, child);
        assert!(linked_controller_view(parent, parent_view, child_view));
        assert!(!linked_controller_view(parent, 0, child_view));
        assert!(!linked_controller_view(parent, parent_view, 0));
        assert!(!linked_controller_view(parent, parent_view, parent_view));
        controller_remove_from_parent(child, 0);
        assert!(!linked_controller_view(parent, parent_view, child_view));
    }

    #[test]
    fn only_a_linked_controller_view_gets_an_old_landscape_frame() {
        let parent = 0x1010;
        let child = 0x2020;
        controller_add_child(parent, 0, child);
        assert_eq!(property_get(child, "parentController"), Some(parent as u64));
        let info = screen_info("iphoneos7.0", &["UIInterfaceOrientationLandscapeRight"]);
        let geometry = geometry_for((568.0, 320.0, 2.0), Some(&info));
        let parent_size = Size { width: 568.0, height: 320.0 };
        let portrait = Rect::new(0.0, 0.0, 320.0, 568.0);
        let landscape = Rect::new(0.0, 0.0, 568.0, 320.0);
        assert_eq!(controller_child_frame(geometry, parent_size, portrait, IDENTITY, true), Some(landscape));
        assert_eq!(controller_child_frame(geometry, parent_size, landscape, IDENTITY, true), None);
        assert_eq!(controller_child_frame(geometry, parent_size, portrait, IDENTITY, false), None);
        assert_eq!(controller_child_frame(geometry, parent_size, Rect::new(1.0, 0.0, 320.0, 568.0), IDENTITY, true), None);
        let mut rotated = IDENTITY;
        rotated.b = 1.0;
        assert_eq!(controller_child_frame(geometry, parent_size, portrait, rotated, true), None);
        assert_eq!(controller_child_frame(geometry, Size { width: 480.0, height: 320.0 }, portrait, IDENTITY, true), Some(landscape));
        assert_eq!(controller_child_frame(geometry, Size { width: 500.0, height: 320.0 }, portrait, IDENTITY, true), Some(landscape));
        assert_eq!(controller_child_frame(geometry, Size { width: 300.0, height: 320.0 }, portrait, IDENTITY, true), None);
        assert_eq!(controller_child_frame(geometry, Size { width: 500.0, height: 300.0 }, portrait, IDENTITY, true), None);
        let portrait_info = screen_info("iphoneos7.0", &["UIInterfaceOrientationPortrait"]);
        let portrait_geometry = geometry_for((320.0, 568.0, 2.0), Some(&portrait_info));
        assert_eq!(controller_child_frame(portrait_geometry, Size { width: 320.0, height: 568.0 }, portrait, IDENTITY, true), None);
        controller_remove_from_parent(child, 0);
        assert_eq!(property_get(child, "parentController"), Some(0));
        let old_view = 0x3030;
        let new_view = 0x4040;
        property_set(old_view, "viewController", child as u64);
        property_set(child, "view", old_view as u64);
        controller_set_view(child, 0, new_view);
        assert_eq!(property_get(old_view, "viewController"), Some(0));
        assert_eq!(property_get(child, "view"), Some(new_view as u64));
        assert_eq!(property_get(new_view, "viewController"), Some(child as u64));
    }

    /// A presented controller's view fills the game's landscape screen even
    /// when its window holds portrait bounds (an old game made the window
    /// from UIScreen.bounds).
    #[test]
    fn presented_views_fill_the_landscape_window() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let new_view = |class: Class, frame: Rect| view_init_with_frame(runtime::send(class, "alloc", &[]), 0, frame);
        let new_controller = || runtime::send(runtime::send(runtime::class_named("UIViewController"), "alloc", &[]), "init", &[]);
        let presenter = new_controller();
        runtime::send(presenter, "setView:", &[new_view(runtime::class_named("UIView"), Rect::new(0.0, 0.0, 320.0, 568.0))]);
        let window = new_view(runtime::class_named("UIWindow"), Rect::new(0.0, 0.0, 320.0, 568.0));
        runtime::send(window, "setRootViewController:", &[presenter]);
        let presented = new_controller();
        let presented_view = new_view(runtime::class_named("UIView"), Rect::default());
        runtime::send(presented, "setView:", &[presented_view]);
        show_presented(presenter, presented, false);
        assert_eq!(view_state(presented_view).frame, Rect::new(0.0, 0.0, 568.0, 320.0));
    }
}
