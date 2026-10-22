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
            property_set_object(this, $key, value);
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

/// The screen the game is told it has, in points, landscape, and its scale.
/// IOS_SCREEN=568x320@2 sets it outright. Otherwise, with the real display
/// known (IOS_DISPLAY=2424x1080, set by the phone app), the game gets the
/// shape iOS would give it there, drawn at the display's real pixels.
/// Neither: an iPhone 5s (568x320 points at 2x).
pub fn screen_points() -> (f64, f64, f64) {
    static SCREEN: OnceLock<(f64, f64, f64)> = OnceLock::new();
    *SCREEN.get_or_init(|| {
        if let Ok(text) = std::env::var("IOS_SCREEN") {
            let (size, scale) = text.split_once('@').unwrap_or((&text, "2"));
            let (w, h) = size.split_once('x').unwrap_or(("568", "320"));
            return (w.parse().unwrap_or(568.0), h.parse().unwrap_or(320.0), scale.parse().unwrap_or(2.0));
        }
        let display = std::env::var("IOS_DISPLAY").ok().and_then(|text| {
            let (w, h) = text.split_once('x')?;
            Some((w.trim().parse::<f64>().ok()?, h.trim().parse::<f64>().ok()?))
        });
        let Some(display) = display.filter(|&(w, h)| w >= 1.0 && h >= 1.0) else {
            return (568.0, 320.0, SCALE);
        };
        let app_dir = crate::loader::loaded().app_dir.clone();
        let info = std::fs::read(format!("{app_dir}/Info.plist")).ok().and_then(|bytes| crate::fw::plist::parse(&bytes));
        let files: Vec<String> = std::fs::read_dir(&app_dir)
            .map(|list| list.filter_map(|e| e.ok()?.file_name().into_string().ok()).collect())
            .unwrap_or_default();
        let aspect = app_aspect(display, info.as_ref(), &files);
        let (w, h) = fit_points(display, aspect);
        crate::log!("screen: display {}x{}, app shape {aspect:.3}, told {w}x{h} points at {SCALE}x", display.0, display.1);
        (w, h, SCALE)
    })
}

/// The scale every screen is reported at (an iPhone 5s, the device the
/// games are told they run on).
const SCALE: f64 = 2.0;

/// The widest landscape shape (long side / short side) this app said it
/// can fill, as iOS decides it: an app with a launch storyboard fills any
/// screen; otherwise it gets the tallest iPhone launch image it ships
/// (Default-568h@2x.png, or a UILaunchImages size), and with none a 3:2
/// screen (the 3.5-inch iPhones), with black bars around it.
fn app_aspect(display: (f64, f64), info: Option<&crate::fw::plist::Value>, files: &[String]) -> f64 {
    use crate::fw::plist::Value;
    let display_aspect = display.0.max(display.1) / display.0.min(display.1);
    let has = |key: &str| info.and_then(|i| i.get(key)).is_some();
    if has("UILaunchStoryboardName") || has("UILaunchStoryboardName~iphone") {
        return display_aspect;
    }
    let mut shapes: Vec<f64> = vec![1.5];
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
        shapes.push(long / short);
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
                    shapes.push(a.max(b) / a.min(b));
                }
            }
        }
    }
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
    let (w, h, _) = screen_points();
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
    let (w, h, scale) = screen_points();
    Rect::new(0.0, 0.0, h * scale, w * scale)
}

extern "C" fn screen_scale(_this: Id, _: Sel) -> f64 {
    screen_points().2
}

extern "C" fn screen_current_mode(_this: Id, _: Sel) -> Id {
    static MODE: OnceLock<Id> = OnceLock::new();
    *MODE.get_or_init(|| runtime::alloc_instance(runtime::class_named("UIScreenMode")))
}

extern "C" fn screen_mode_size(_this: Id, _: Sel) -> Size {
    let (w, h, scale) = screen_points();
    Size { width: h * scale, height: w * scale }
}

extern "C" fn screen_available_modes(this: Id, s: Sel) -> Id {
    collections::array_from(&[screen_current_mode(this, s)])
}

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
extern "C" fn device_orientation(_this: Id, _: Sel) -> isize {
    3
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
    view_state(this).frame = frame;
}

extern "C" fn view_bounds(this: Id, _: Sel) -> Rect {
    let state = view_state(this);
    Rect { origin: state.bounds_origin, size: state.frame.size }
}

extern "C" fn view_set_bounds(this: Id, _: Sel, bounds: Rect) {
    let state = view_state(this);
    let center_x = state.frame.origin.x + state.frame.size.width / 2.0;
    let center_y = state.frame.origin.y + state.frame.size.height / 2.0;
    state.bounds_origin = bounds.origin;
    state.frame.size = bounds.size;
    state.frame.origin = Point { x: center_x - bounds.size.width / 2.0, y: center_y - bounds.size.height / 2.0 };
}

extern "C" fn view_center(this: Id, _: Sel) -> Point {
    let f = view_state(this).frame;
    Point { x: f.origin.x + f.size.width / 2.0, y: f.origin.y + f.size.height / 2.0 }
}

extern "C" fn view_set_center(this: Id, _: Sel, center: Point) {
    let state = view_state(this);
    state.frame.origin = Point { x: center.x - state.frame.size.width / 2.0, y: center.y - state.frame.size.height / 2.0 };
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

fn detach(view: Id) {
    let parent = view_state(view).superview;
    if parent != 0 {
        view_state(parent).subviews.retain(|&v| v != view);
        view_state(view).superview = 0;
    }
}

fn attach(parent: Id, view: Id, index: Option<usize>) {
    if view == 0 || parent == 0 {
        return;
    }
    detach(view);
    let list = &mut view_state(parent).subviews;
    match index {
        Some(i) => list.insert(i.min(list.len()), view),
        None => list.push(view),
    }
    view_state(view).superview = parent;
    for name in ["didMoveToSuperview"] {
        if runtime::responds(view, name) {
            runtime::send(view, name, &[]);
        }
    }
    if window_of(parent) != 0 && runtime::responds(view, "didMoveToWindow") {
        runtime::send(view, "didMoveToWindow", &[]);
    }
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
    detach(this);
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

extern "C" fn view_gesture_recognizers(_this: Id, _: Sel) -> Id {
    collections::array_from(&[])
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
    let mut windows = WINDOWS.lock().unwrap();
    if !windows.contains(&this) {
        windows.push(this);
    }
    drop(windows);
    property_set(this, "hidden", 0);
    crate::log!("window {this:#x} shown");
}

extern "C" fn window_make_key(this: Id, _: Sel) {
    *KEY_WINDOW.lock().unwrap() = this;
}

extern "C" fn window_root_controller(this: Id, _: Sel) -> Id {
    property_get(this, "rootViewController").unwrap_or(0) as Id
}

extern "C" fn window_set_root_controller(this: Id, _: Sel, controller: Id) {
    property_set(this, "rootViewController", controller as u64);
    if controller != 0 {
        let view = runtime::send(controller, "view", &[]);
        if view != 0 {
            let s = runtime::sel("setFrame:");
            if let Some(imp) = runtime::imp_for(view, s) {
                let f: extern "C" fn(Id, Sel, Rect) = unsafe { std::mem::transmute(imp) };
                f(view, s, screen_bounds());
            }
            attach(this, view, None);
        }
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

extern "C" fn controller_init_nib(this: Id, _: Sel, _nib: Id, _bundle: Id) -> Id {
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
        let view = f(view, s, screen_bounds());
        property_set(this, "view", view as u64);
    }
    runtime::send(this, "viewDidLoad", &[]);
    property_get(this, "view").unwrap_or(0) as Id
}

extern "C" fn controller_set_view(this: Id, _: Sel, view: Id) {
    property_set(this, "view", view as u64);
    if view != 0 {
        property_set(view, "viewController", this as u64);
    }
}

extern "C" fn controller_is_view_loaded(this: Id, _: Sel) -> bool {
    property_get(this, "view").is_some()
}

extern "C" fn controller_orientation(_this: Id, _: Sel) -> isize {
    3
}

extern "C" fn controller_supported_orientations(_this: Id, _: Sel) -> usize {
    (1 << 3) | (1 << 4)
}

extern "C" fn controller_should_autorotate_to(_this: Id, _: Sel, orientation: isize) -> bool {
    orientation == 3 || orientation == 4
}

extern "C" fn controller_present(this: Id, _: Sel, controller: Id, _animated: u8) {
    crate::log!("view controller {this:#x} shows {controller:#x} on top (not drawn yet)");
    property_set(this, "presented", controller as u64);
}

extern "C" fn controller_present_completion(this: Id, s: Sel, controller: Id, animated: u8, _completion: usize) {
    controller_present(this, s, controller, animated);
}

extern "C" fn controller_dismiss(this: Id, _: Sel, _animated: u8) {
    property_set(this, "presented", 0);
}

extern "C" fn controller_dismiss_completion(this: Id, s: Sel, animated: u8, _completion: usize) {
    controller_dismiss(this, s, animated);
}

property!(controller_presented, controller_set_presented, "presented", id, 0);

extern "C" fn controller_children(_this: Id, _: Sel) -> Id {
    collections::array_from(&[])
}

// ----- layers -----

extern "C" fn layer_init(this: Id, _: Sel) -> Id {
    this
}

property!(layer_opaque, layer_set_opaque, "opaque", bool, false);
property!(layer_drawable_properties, layer_set_drawable_properties, "drawableProperties", id, 0);
property!(layer_contents_scale, layer_set_contents_scale, "contentsScale", f64, 1.0);
property!(layer_delegate, layer_set_delegate, "delegate", id, 0);

extern "C" fn layer_bounds(this: Id, _: Sel) -> Rect {
    let view = property_get(this, "delegate").unwrap_or(0) as Id;
    if view != 0 {
        return view_bounds(view, 0);
    }
    Rect::default()
}

extern "C" fn layer_frame(this: Id, _: Sel) -> Rect {
    let view = property_get(this, "delegate").unwrap_or(0) as Id;
    if view != 0 {
        return view_state(view).frame;
    }
    Rect::default()
}

extern "C" fn layer_ignore_rect(_this: Id, _: Sel, _rect: Rect) {}
extern "C" fn layer_ignore_point(_this: Id, _: Sel, _point: Point) {}

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
extern "C" fn color_black(_: Class, _: Sel) -> Id {
    color(0.0, 0.0, 0.0, 1.0)
}
extern "C" fn color_white(_: Class, _: Sel) -> Id {
    color(1.0, 1.0, 1.0, 1.0)
}
extern "C" fn color_clear(_: Class, _: Sel) -> Id {
    color(0.0, 0.0, 0.0, 0.0)
}
extern "C" fn color_gray(_: Class, _: Sel) -> Id {
    color(0.5, 0.5, 0.5, 1.0)
}
extern "C" fn color_red(_: Class, _: Sel) -> Id {
    color(1.0, 0.0, 0.0, 1.0)
}
extern "C" fn color_cg(this: Id, _: Sel) -> Id {
    this
}

// ----- images: { isa, *mut ImageInfo } -----

pub struct ImageInfo {
    /// The picture's file ("" for pictures made in memory).
    pub path: String,
    pub pixels: (u32, u32),
    pub scale: f64,
    /// UIImageOrientation (0 = up).
    pub orientation: i64,
    /// Its CGImage (see fw/graphics.rs; read from the file when first asked for).
    pub cg: OnceLock<Id>,
    /// An upright PNG made to show it on the phone screen (see graphics::picture_file).
    pub picture_file: OnceLock<Option<String>>,
    /// Whether making that file has started.
    pub picture_started: std::sync::atomic::AtomicBool,
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
    make_image(ImageInfo { path: path.into(), pixels, scale, orientation, cg: OnceLock::new(), picture_file: OnceLock::new(), picture_started: Default::default() })
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
    let dir = crate::loader::loaded().app_dir.clone();
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), e.to_string()),
        _ => (name.clone(), "png".to_string()),
    };
    for candidate in [format!("{dir}/{stem}@2x.{ext}"), format!("{dir}/{stem}.{ext}"), format!("{dir}/{name}")] {
        if std::path::Path::new(&candidate).exists() {
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
    3
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
    class_methods(view, &[("layerClass", layer_class as usize)]);
    methods(
        view,
        &[
            ("initWithFrame:", view_init_with_frame as usize),
            ("init", view_init as usize),
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
            ("setNeedsLayout", nothing as usize),
            ("layoutIfNeeded", nothing as usize),
            ("layoutSubviews", nothing as usize),
            ("sizeToFit", nothing as usize),
            ("sizeThatFits:", view_size_that_fits as usize),
            ("convertPoint:fromView:", view_convert_point as usize),
            ("convertPoint:toView:", view_convert_point as usize),
            ("convertRect:fromView:", view_convert_rect as usize),
            ("convertRect:toView:", view_convert_rect as usize),
            ("addGestureRecognizer:", ignore_id as usize),
            ("removeGestureRecognizer:", ignore_id as usize),
            ("gestureRecognizers", view_gesture_recognizers as usize),
            ("endEditing:", no_with as usize),
            ("setClearsContextBeforeDrawing:", ignore_bool as usize),
        ],
    );

    let window = define_class("UIWindow", "UIView", 16);
    methods(
        window,
        &[
            ("makeKeyAndVisible", window_make_key_and_visible as usize),
            ("makeKeyWindow", window_make_key as usize),
            ("rootViewController", window_root_controller as usize),
            ("setRootViewController:", window_set_root_controller as usize),
            ("screen", window_screen as usize),
            ("setScreen:", ignore_id as usize),
            ("windowLevel", window_level as usize),
            ("setWindowLevel:", window_set_level as usize),
            ("isKeyWindow", yes as usize),
        ],
    );

    let controller = define_class("UIViewController", "UIResponder", 8);
    methods(
        controller,
        &[
            ("init", controller_init as usize),
            ("initWithNibName:bundle:", controller_init_nib as usize),
            ("view", controller_view as usize),
            ("setView:", controller_set_view as usize),
            ("isViewLoaded", controller_is_view_loaded as usize),
            ("loadView", nothing as usize),
            ("viewDidLoad", nothing as usize),
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
            ("setModalTransitionStyle:", ignore_int as usize),
            ("setEdgesForExtendedLayout:", ignore_int as usize),
            ("setExtendedLayoutIncludesOpaqueBars:", ignore_bool as usize),
            ("setAutomaticallyAdjustsScrollViewInsets:", ignore_bool as usize),
            ("setNeedsStatusBarAppearanceUpdate", nothing as usize),
            ("preferredStatusBarStyle", zero as usize),
            ("childViewControllers", controller_children as usize),
            ("addChildViewController:", ignore_id as usize),
            ("removeFromParentViewController", nothing as usize),
            ("didMoveToParentViewController:", ignore_id as usize),
            ("willMoveToParentViewController:", ignore_id as usize),
            ("parentViewController", zero as usize),
            ("presentingViewController", zero as usize),
            ("navigationController", zero as usize),
            ("setTitle:", ignore_id as usize),
            ("title", zero as usize),
        ],
    );
    let _ = controller_set_presented;
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
            ("bounds", layer_bounds as usize),
            ("frame", layer_frame as usize),
            ("setBounds:", layer_ignore_rect as usize),
            ("setFrame:", layer_ignore_rect as usize),
            ("setPosition:", layer_ignore_point as usize),
            ("setNeedsDisplay", nothing as usize),
            ("setMasksToBounds:", ignore_bool as usize),
            ("setCornerRadius:", ignore_double as usize),
            ("setBorderWidth:", ignore_double as usize),
            ("setBorderColor:", ignore_id as usize),
            ("setShadowOpacity:", ignore_float as usize),
            ("addSublayer:", ignore_id as usize),
            ("removeFromSuperlayer", nothing as usize),
            ("removeAllAnimations", nothing as usize),
            ("addAnimation:forKey:", ignore_id_id as usize),
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
        &[
            ("colorWithRed:green:blue:alpha:", color_rgba as usize),
            ("colorWithWhite:alpha:", color_white_alpha as usize),
            ("blackColor", color_black as usize),
            ("whiteColor", color_white as usize),
            ("clearColor", color_clear as usize),
            ("grayColor", color_gray as usize),
            ("darkGrayColor", color_gray as usize),
            ("lightGrayColor", color_gray as usize),
            ("redColor", color_red as usize),
        ],
    );
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
            ("registerUserNotificationSettings:", ignore_id as usize),
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
    r.data("UIBackgroundTaskInvalid", leak(0usize));
    let _ = ignore_id_bool;
}

extern "C" fn ignore_double(_this: Id, _: Sel, _value: f64) {}
extern "C" fn ignore_float(_this: Id, _: Sel, _value: f32) {}
extern "C" fn ignore_id_id(_this: Id, _: Sel, _a: Id, _b: Id) {}
extern "C" fn ignore_bool_int(_this: Id, _: Sel, _a: u8, _b: isize) {}
extern "C" fn screen_scale_one(_this: Id, _: Sel) -> f64 {
    1.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fw::plist::Value;

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
    fn storyboard_app_fills_the_display() {
        let info = Value::Dict(vec![("UILaunchStoryboardName".into(), Value::String("Launch".into()))]);
        let aspect = app_aspect((2424.0, 1080.0), Some(&info), &[]);
        assert_eq!(fit_points((2424.0, 1080.0), aspect), (1212.0, 540.0));
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
}
