//! UIKit controls and screens: fonts, labels, buttons, text fields, web
//! views, bars, navigation, tables, collections, alerts, action sheets,
//! popovers and the photo picker. They keep their state and answer like
//! UIKit. Labels and buttons are drawn (fw/pictures.rs, from `label_look`
//! and `button_parts`); the rest is not drawn yet, and what it would show
//! is logged. Controls (buttons too) are marked "notDrawn" so they don't
//! catch taps meant for the game: they don't follow touches yet. Screens
//! for things this phone doesn't offer (photo library, camera) answer the
//! way iOS does when the user closes them.

use crate::fw::geometry::{Point, Rect, Size};
use crate::fw::graphics::TextLook;
use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::runloop::{post, Work};
use crate::fw::uikit::{property, property_get, property_set, screen_points, view_state};
use crate::fw::{collections, string};
use crate::objc::runtime::{self, lock, Class, Id, Sel};
use crate::symbols::{leak, Registry};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

// ----- helpers shared by the stand-ins -----

/// Logs something the game would see on screen that isn't drawn yet
/// (the first 60 times; games may set a label's text every frame).
pub fn not_drawn(what: &str) {
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    let n = COUNT.fetch_add(1, Ordering::Relaxed);
    if n < 60 {
        crate::log!("not drawn yet: {what}");
    } else if n == 60 {
        crate::log!("not drawn yet: (more not logged)");
    }
}

fn mark_not_drawn(view: Id) {
    property_set(view, "notDrawn", 1);
}

/// Runs work later on the main run loop (where UIKit calls delegates).
pub fn later(work: impl FnOnce() + Send + 'static) {
    post(Work::Native(Box::new(work)));
}

/// Sends a delegate message if the delegate implements it.
pub fn tell(delegate: Id, name: &str, args: &[usize]) -> Option<usize> {
    if delegate != 0 && runtime::responds(delegate, name) {
        Some(runtime::send(delegate, name, args))
    } else {
        None
    }
}

/// Calls a block later on the main run loop with up to three
/// pointer-sized arguments (a block taking fewer ignores the rest).
pub fn call_block_later(block: usize, args: [usize; 3]) {
    if block == 0 {
        return;
    }
    let block = crate::libsystem::mach::block_copy(block as *mut _) as usize;
    later(move || {
        let header = block as *const crate::libsystem::mach::BlockHeader;
        let invoke: extern "C" fn(usize, usize, usize, usize) = unsafe { std::mem::transmute((*header).invoke) };
        invoke(block, args[0], args[1], args[2]);
    });
}

fn get_id(this: Id, key: &'static str) -> Id {
    property_get(this, key).unwrap_or(0) as Id
}

fn get_f(this: Id, key: &'static str, default: f64) -> f64 {
    property_get(this, key).map(f64::from_bits).unwrap_or(default)
}

fn set_f(this: Id, key: &'static str, value: f64) {
    property_set(this, key, value.to_bits());
}

fn get_flag(this: Id, key: &'static str, default: bool) -> bool {
    property_get(this, key).map(|v| v != 0).unwrap_or(default)
}

/// A copy of a string, as UIKit's copy properties keep.
fn copied(text: Id) -> Id {
    if text == 0 || !string::is_string(text) { text } else { string::make(&string::get(text)) }
}

fn call_rect(this: Id, name: &str, rect: Rect) -> Id {
    let s = runtime::sel(name);
    match runtime::imp_for(this, s) {
        Some(imp) => {
            let f: extern "C" fn(Id, Sel, Rect) -> Id = unsafe { std::mem::transmute(imp) };
            f(this, s, rect)
        }
        None => 0,
    }
}

/// [super initWithFrame:] as seen from one of our view classes.
fn super_init_frame(this: Id, parent: &str, frame: Rect) -> Id {
    let s = runtime::sel("initWithFrame:");
    let parent = runtime::class_named(parent); // before taking the lock: it locks too
    let imp = lock().lookup(parent, s).expect("UIView initWithFrame:");
    let f: extern "C" fn(Id, Sel, Rect) -> Id = unsafe { std::mem::transmute(imp) };
    f(this, s, frame)
}

fn new_view(class: Class, frame: Rect) -> Id {
    let object = runtime::send(class, "alloc", &[]);
    call_rect(object, "initWithFrame:", frame)
}

fn new_view_named(class: &str, frame: Rect) -> Id {
    new_view(runtime::class_named(class), frame)
}

fn new_object(class: &str) -> Id {
    let object = runtime::send(runtime::class_named(class), "alloc", &[]);
    runtime::send(object, "init", &[])
}

fn screen_rect() -> Rect {
    let (w, h, _) = screen_points();
    Rect::new(0.0, 0.0, w, h)
}

fn frame_of(view: Id) -> Rect {
    view_state(view).frame
}

fn class_is(object: Id, name: &str) -> bool {
    let class = runtime::class_named(name);
    object != 0 && class != 0 && {
        let rt = lock();
        rt.is_subclass(rt.class_of(object), class)
    }
}

fn text_of(object: Id) -> String {
    if object == 0 { String::new() } else { string::get(object) }
}

extern "C" fn nothing(_this: Id, _: Sel) {}
extern "C" fn no(_this: Id, _: Sel) -> bool {
    false
}
extern "C" fn class_no_with_int(_: Class, _: Sel, _value: isize) -> bool {
    false
}
extern "C" fn class_nil_with_int(_: Class, _: Sel, _value: isize) -> Id {
    0
}
extern "C" fn zero(_this: Id, _: Sel) -> usize {
    0
}
extern "C" fn ignore_id(_this: Id, _: Sel, _value: Id) {}
extern "C" fn ignore_bool(_this: Id, _: Sel, _value: u8) {}
extern "C" fn ignore_int(_this: Id, _: Sel, _value: isize) {}
extern "C" fn ignore_double(_this: Id, _: Sel, _value: f64) {}
extern "C" fn ignore_id_bool(_this: Id, _: Sel, _value: Id, _flag: u8) {}
extern "C" fn ignore_id_int(_this: Id, _: Sel, _value: Id, _number: isize) {}
extern "C" fn ignore_id_id(_this: Id, _: Sel, _a: Id, _b: Id) {}
extern "C" fn ignore_id_int_int(_this: Id, _: Sel, _value: Id, _a: isize, _b: isize) {}
extern "C" fn nil_with_id(_this: Id, _: Sel, _value: Id) -> Id {
    0
}
extern "C" fn nil_with_id_id(_this: Id, _: Sel, _a: Id, _b: Id) -> Id {
    0
}
extern "C" fn empty_array(_this: Id, _: Sel) -> Id {
    collections::array_from(&[])
}
extern "C" fn empty_string_with_id(_this: Id, _: Sel, _value: Id) -> Id {
    string::make("")
}

/// UIEdgeInsets.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Insets {
    pub top: f64,
    pub left: f64,
    pub bottom: f64,
    pub right: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Range {
    location: usize,
    length: usize,
}

// ----- fonts and text measuring -----

/// How big a piece of text is in the system font, in points, for labels
/// and buttons (NSString's sizeWithFont: measures real fonts in
/// fw/graphics/text.rs): an estimate from Helvetica's average letter width
/// (0.55 em) and line height (1.2 em), wrapped at `max_width` and cut at
/// `max_lines` lines (0: no limit), rounded up like UIKit.
pub fn text_size(text: &str, point_size: f64, max_width: f64, max_lines: usize) -> Size {
    // Written as fractions so whole sizes give whole numbers (no 0.55 drift).
    let letter = point_size * 11.0 / 20.0;
    let line_height = (point_size * 6.0 / 5.0).ceil();
    let mut widest: f64 = 0.0;
    let mut lines = 0usize;
    for paragraph in text.split('\n') {
        let width = paragraph.chars().count() as f64 * letter;
        if max_width.is_finite() && max_width > 0.0 && width > max_width {
            lines += (width / max_width).ceil() as usize;
            widest = widest.max(max_width);
        } else {
            lines += 1;
            widest = widest.max(width);
        }
    }
    if max_lines > 0 {
        lines = lines.min(max_lines);
    }
    if text.is_empty() {
        return Size { width: 0.0, height: 0.0 };
    }
    Size { width: widest.ceil(), height: lines as f64 * line_height }
}

const SYSTEM_FONT: &str = ".HelveticaNeueInterface-Regular";
const BOLD_FONT: &str = ".HelveticaNeueInterface-MediumP4";

/// One UIFont object per name and size (games ask for fonts every frame).
pub fn font(name: &str, size: f64) -> Id {
    static FONTS: Mutex<Option<HashMap<(String, u64), usize>>> = Mutex::new(None);
    let key = (name.to_string(), size.to_bits());
    if let Some(&font) = FONTS.lock().unwrap().get_or_insert_with(HashMap::new).get(&key) {
        return font;
    }
    let object = runtime::alloc_instance(runtime::class_named("UIFont"));
    property_set(object, "fontName", string::make(name) as u64);
    set_f(object, "pointSize", size);
    FONTS.lock().unwrap().get_or_insert_with(HashMap::new).insert(key, object);
    object
}

fn font_size(font: Id) -> f64 {
    if font == 0 { 17.0 } else { get_f(font, "pointSize", 17.0) }
}

/// The system font with UIFontDescriptor's symbolic traits (1 italic, 2 bold).
pub fn system_font(traits: isize, size: f64) -> Id {
    if traits & 2 != 0 {
        font(BOLD_FONT, size)
    } else if traits & 1 != 0 {
        font(".HelveticaNeueInterface-Italic", size)
    } else {
        font(SYSTEM_FONT, size)
    }
}

extern "C" fn font_system(_: Class, _: Sel, size: f64) -> Id {
    font(SYSTEM_FONT, size)
}
extern "C" fn font_bold(_: Class, _: Sel, size: f64) -> Id {
    font(BOLD_FONT, size)
}
extern "C" fn font_italic(_: Class, _: Sel, size: f64) -> Id {
    font(".HelveticaNeueInterface-Italic", size)
}
extern "C" fn font_named(_: Class, _: Sel, name: Id, size: f64) -> Id {
    if name == 0 {
        return 0;
    }
    font(&string::get(name), if size > 0.0 { size } else { 12.0 })
}
extern "C" fn font_system_size(_: Class, _: Sel) -> f64 {
    14.0
}
extern "C" fn font_label_size(_: Class, _: Sel) -> f64 {
    17.0
}
extern "C" fn font_button_size(_: Class, _: Sel) -> f64 {
    18.0
}
extern "C" fn font_small_size(_: Class, _: Sel) -> f64 {
    12.0
}
extern "C" fn font_family_names(_: Class, _: Sel) -> Id {
    collections::array_from(&[string::make("Helvetica Neue"), string::make("Helvetica")])
}
extern "C" fn font_names_for_family(_: Class, _: Sel, _family: Id) -> Id {
    collections::array_from(&[])
}
extern "C" fn font_with_size(this: Id, _: Sel, size: f64) -> Id {
    font(&text_of(get_id(this, "fontName")), size)
}
extern "C" fn font_point_size(this: Id, _: Sel) -> f64 {
    font_size(this)
}
extern "C" fn font_name(this: Id, _: Sel) -> Id {
    get_id(this, "fontName")
}
extern "C" fn font_family(this: Id, _: Sel) -> Id {
    let name = text_of(get_id(this, "fontName"));
    string::make(if name.starts_with('.') { ".Helvetica Neue Interface" } else { name.split('-').next().unwrap_or("") })
}
extern "C" fn font_line_height(this: Id, _: Sel) -> f64 {
    font_size(this) * 1.2
}
/// UIFont's leading is only the extra gap between lines:
/// lineHeight = ascender - descender + leading.
extern "C" fn font_leading(this: Id, s: Sel) -> f64 {
    (font_line_height(this, s) - (font_ascender(this, s) - font_descender(this, s))).max(0.0)
}
extern "C" fn font_ascender(this: Id, _: Sel) -> f64 {
    font_size(this) * 0.952
}
extern "C" fn font_descender(this: Id, _: Sel) -> f64 {
    font_size(this) * -0.213
}
extern "C" fn font_cap_height(this: Id, _: Sel) -> f64 {
    font_size(this) * 0.714
}
extern "C" fn font_x_height(this: Id, _: Sel) -> f64 {
    font_size(this) * 0.517
}

/// A UIFont's name and point size (nil: the 17-point system font), for
/// NSString's measuring and drawing (fw/graphics/text.rs).
pub fn font_name_and_size(font: Id) -> (String, f64) {
    let name = if font == 0 { String::new() } else { text_of(get_id(font, "fontName")) };
    (if name.is_empty() { SYSTEM_FONT.to_string() } else { name }, font_size(font))
}

// ----- shared text properties (labels, text fields, text views) -----

extern "C" fn text_get(this: Id, _: Sel) -> Id {
    get_id(this, "text")
}
extern "C" fn text_or_empty(this: Id, _: Sel) -> Id {
    match get_id(this, "text") {
        0 => string::make(""),
        text => text,
    }
}
extern "C" fn text_set(this: Id, _: Sel, text: Id) {
    property_set(this, "text", copied(text) as u64);
    if text != 0 && !class_is(this, "UILabel") && !class_is(this, "UITextField") {
        not_drawn(&format!("{} text \"{}\"", string::describe_class_of(this), text_of(text)));
    }
}
extern "C" fn text_font(this: Id, _: Sel) -> Id {
    match get_id(this, "font") {
        0 => font(SYSTEM_FONT, 17.0),
        font => font,
    }
}
extern "C" fn text_field_font(this: Id, _: Sel) -> Id {
    match get_id(this, "font") {
        0 => font(SYSTEM_FONT, 12.0),
        font => font,
    }
}
extern "C" fn text_font_set(this: Id, _: Sel, font: Id) {
    property_set(this, "font", font as u64);
}
property!(text_color, text_set_color, "textColor", id, 0);
property!(text_alignment, text_set_alignment, "textAlignment", isize, 0);
property!(text_delegate, text_set_delegate, "delegate", id, 0);
property!(text_placeholder, text_set_placeholder, "placeholder", id, 0);
property!(text_keyboard_type, text_set_keyboard_type, "keyboardType", isize, 0);
property!(text_return_key, text_set_return_key, "returnKeyType", isize, 0);
property!(text_appearance, text_set_appearance, "keyboardAppearance", isize, 0);
property!(text_autocorrection, text_set_autocorrection, "autocorrectionType", isize, 0);
property!(text_autocapitalization, text_set_autocapitalization, "autocapitalizationType", isize, 0);
property!(text_spell_checking, text_set_spell_checking, "spellCheckingType", isize, 0);
property!(text_secure, text_set_secure, "secureTextEntry", bool, false);
property!(text_auto_return, text_set_auto_return, "enablesReturnKeyAutomatically", bool, false);
property!(text_accessory, text_set_accessory, "inputAccessoryView", id, 0);
property!(text_input_view, text_set_input_view, "inputView", id, 0);
property!(text_border, text_set_border, "borderStyle", isize, 0);
property!(text_clear_mode, text_set_clear_mode, "clearButtonMode", isize, 0);
property!(text_clears_on_begin, text_set_clears_on_begin, "clearsOnBeginEditing", bool, false);
property!(text_adjusts, text_set_adjusts, "adjustsFontSizeToFitWidth", bool, false);
property!(text_minimum_font, text_set_minimum_font, "minimumFontSize", f64, 0.0);
property!(text_editable, text_set_editable, "editable", bool, true);
property!(text_selectable, text_set_selectable, "selectable", bool, true);
property!(text_lines, text_set_lines, "numberOfLines", isize, 1);
property!(text_line_break, text_set_line_break, "lineBreakMode", isize, 4);
property!(text_shadow_color, text_set_shadow_color, "shadowColor", id, 0);
property!(text_highlighted_color, text_set_highlighted_color, "highlightedTextColor", id, 0);
property!(text_highlighted, text_set_highlighted, "highlighted", bool, false);
property!(text_enabled, text_set_enabled, "enabled", bool, true);
property!(text_attributed, text_set_attributed, "attributedText", id, 0);

extern "C" fn label_shadow_offset(this: Id, _: Sel) -> Size {
    Size { width: get_f(this, "shadowOffset.w", 0.0), height: get_f(this, "shadowOffset.h", -1.0) }
}
extern "C" fn label_set_shadow_offset(this: Id, _: Sel, size: Size) {
    set_f(this, "shadowOffset.w", size.width);
    set_f(this, "shadowOffset.h", size.height);
}

extern "C" fn text_selected_range(this: Id, _: Sel) -> Range {
    Range {
        location: property_get(this, "selectedRange").unwrap_or(0) as usize,
        length: property_get(this, "selectedRange.length").unwrap_or(0) as usize,
    }
}
extern "C" fn text_set_selected_range(this: Id, _: Sel, range: Range) {
    property_set(this, "selectedRange", range.location as u64);
    property_set(this, "selectedRange.length", range.length as u64);
}
extern "C" fn text_scroll_range(_this: Id, _: Sel, _range: Range) {}

// ----- labels -----

extern "C" fn label_init_with_frame(this: Id, _: Sel, frame: Rect) -> Id {
    super_init_frame(this, "UIView", frame);
    property_set(this, "userInteraction", 0);
    this
}

/// A colour's red, green, blue and alpha (None for no UIColor).
fn color_parts(color: Id) -> Option<[f64; 4]> {
    if class_is(color, "UIColor") { crate::fw::graphics::color_rgba(color) } else { None }
}

/// The text a label (or a text property) holds, plain or attributed.
fn shown_text(this: Id) -> String {
    let text = text_of(get_id(this, "text"));
    let attributed = get_id(this, "attributedText");
    if text.is_empty() && attributed != 0 { text_of(runtime::send(attributed, "string", &[])) } else { text }
}

/// What a UILabel shows (fw/pictures.rs draws it): None with no text.
pub fn label_look(label: Id) -> Option<TextLook> {
    let text = shown_text(label);
    if text.is_empty() {
        return None;
    }
    let size = frame_of(label).size;
    let highlighted = get_flag(label, "highlighted", false);
    let color = match get_id(label, "highlightedTextColor") {
        color if highlighted && color != 0 => color,
        _ => get_id(label, "textColor"),
    };
    let offset = (get_f(label, "shadowOffset.w", 0.0), get_f(label, "shadowOffset.h", -1.0));
    let lines = property_get(label, "numberOfLines").map_or(1, |v| v as isize);
    let minimum = get_f(label, "minimumFontSize", 0.0);
    Some(TextLook {
        text,
        font: font_name_and_size(text_font(label, 0)),
        // UILabel's text is black unless told.
        color: color_parts(color).unwrap_or([0.0, 0.0, 0.0, 1.0]),
        shadow: color_parts(get_id(label, "shadowColor")).map(|shadow| (shadow, offset)),
        alignment: property_get(label, "textAlignment").map_or(0, |v| v as isize),
        line_break: property_get(label, "lineBreakMode").map_or(4, |v| v as isize),
        lines,
        size: (size.width, size.height),
        minimum_font_size: get_flag(label, "adjustsFontSizeToFitWidth", false).then_some(minimum),
    })
}

fn label_fitting_size(this: Id, bounds: Size) -> Size {
    let lines = property_get(this, "numberOfLines").map(|v| v as isize).unwrap_or(1);
    let width = if lines != 1 && bounds.width > 0.0 { bounds.width } else { f64::INFINITY };
    let font = text_font(this, 0);
    text_size(&text_of(get_id(this, "text")), font_size(font), width, lines.max(0) as usize)
}

extern "C" fn label_size_that_fits(this: Id, _: Sel, size: Size) -> Size {
    label_fitting_size(this, size)
}

extern "C" fn label_size_to_fit(this: Id, _: Sel) {
    let state = view_state(this);
    state.frame.size = label_fitting_size(this, state.frame.size);
}

extern "C" fn label_text_rect(this: Id, _: Sel, bounds: Rect, lines: isize) -> Rect {
    let font = text_font(this, 0);
    let size = text_size(&text_of(get_id(this, "text")), font_size(font), bounds.size.width, lines.max(0) as usize);
    Rect { origin: bounds.origin, size }
}

// ----- controls and buttons -----

struct Target {
    target: Id,
    action: Sel,
    events: usize,
}

static TARGETS: Mutex<Option<HashMap<Id, Vec<Target>>>> = Mutex::new(None);
/// Values a control keeps per state (button titles, images, colours).
static STATE_VALUES: Mutex<Option<HashMap<(Id, &'static str, usize), Id>>> = Mutex::new(None);

fn state_set(this: Id, key: &'static str, state: usize, value: Id) {
    STATE_VALUES.lock().unwrap().get_or_insert_with(HashMap::new).insert((this, key, state), value);
}

/// The same for a picture, which the control holds on to (as UIKit
/// retains it) until it is replaced; nil takes the state's own picture away.
fn state_set_kept(this: Id, key: &'static str, state: usize, value: Id) {
    crate::fw::counted::retain(value);
    let old = {
        let mut values = STATE_VALUES.lock().unwrap();
        let values = values.get_or_insert_with(HashMap::new);
        if value == 0 { values.remove(&(this, key, state)) } else { values.insert((this, key, state), value) }
    };
    // Outside the lock: letting go may clean the picture up.
    if let Some(old) = old {
        crate::fw::counted::release(old);
    }
}

/// The value for a state, or the normal state's, as UIButton falls back.
fn state_get(this: Id, key: &'static str, state: usize) -> Id {
    let values = STATE_VALUES.lock().unwrap();
    let Some(values) = values.as_ref() else { return 0 };
    values.get(&(this, key, state)).or_else(|| values.get(&(this, key, 0))).copied().unwrap_or(0)
}

fn control_state(this: Id) -> usize {
    let mut state = 0;
    if get_flag(this, "highlighted", false) {
        state |= 1;
    }
    if !get_flag(this, "enabled", true) {
        state |= 2;
    }
    if get_flag(this, "selected", false) {
        state |= 4;
    }
    state
}

/// addTarget:action:forControlEvents: on a target/action pair already
/// there adds the events to it, as UIKit does.
fn targets_add(list: &mut Vec<Target>, target: Id, action: Sel, events: usize) {
    match list.iter_mut().find(|t| t.target == target && t.action == action) {
        Some(existing) => existing.events |= events,
        None => list.push(Target { target, action, events }),
    }
}

/// removeTarget:action:forControlEvents: takes only the given events off
/// each matching pair (nil target / NULL action match all); a pair left
/// with no events is gone.
fn targets_remove(list: &mut Vec<Target>, target: Id, action: Sel, events: usize) {
    for t in list.iter_mut() {
        if (target == 0 || t.target == target) && (action == 0 || t.action == action) {
            t.events &= !events;
        }
    }
    list.retain(|t| t.events != 0);
}

extern "C" fn control_add_target(this: Id, _: Sel, target: Id, action: Sel, events: usize) {
    let mut targets = TARGETS.lock().unwrap();
    targets_add(targets.get_or_insert_with(HashMap::new).entry(this).or_default(), target, action, events);
}

extern "C" fn control_remove_target(this: Id, _: Sel, target: Id, action: Sel, events: usize) {
    if let Some(list) = TARGETS.lock().unwrap().get_or_insert_with(HashMap::new).get_mut(&this) {
        targets_remove(list, target, action, events);
    }
}

/// The next responder, as UIKit links them: a controller's view → the
/// controller → the view's superview; a window → the application → the
/// application delegate.
fn next_responder(object: Id) -> Id {
    let app = runtime::send(runtime::class_named("UIApplication"), "sharedApplication", &[]);
    if object == app {
        return runtime::send(app, "delegate", &[]);
    }
    if class_is(object, "UIView") {
        if let Some(controller) = property_get(object, "viewController").filter(|&c| c != 0) {
            return controller as Id;
        }
        let parent = view_state(object).superview;
        return if parent != 0 { parent } else { app };
    }
    if class_is(object, "UIViewController") {
        let view = get_id(object, "view");
        let parent = if view != 0 { view_state(view).superview } else { 0 };
        if parent != 0 {
            return parent;
        }
        let presenting = get_id(object, "presenting");
        return if presenting != 0 { presenting } else { app };
    }
    0
}

/// Sends an action. With no target UIKit asks the first responder and
/// then the responder chain; there's no first responder here (no
/// keyboard), so the search starts at the control itself, as UIKit does
/// then.
fn send_action(sender: Id, action: Sel, target: Id, event: Id) {
    let name = lock().sel_name(action);
    let mut receiver = target;
    if receiver == 0 {
        // The first object up the chain that handles the action; none
        // (or a chain too long to be real) means the action is dropped.
        let mut current = sender;
        for _ in 0..256 {
            if current == 0 {
                break;
            }
            if runtime::responds(current, &name) {
                receiver = current;
                break;
            }
            current = next_responder(current);
        }
    }
    if receiver != 0 {
        runtime::send(receiver, &name, &[sender, event]);
    }
}

extern "C" fn control_all_targets(this: Id, _: Sel) -> Id {
    let targets: Vec<Id> = TARGETS
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|m| m.get(&this))
        .map(|list| list.iter().map(|t| t.target).filter(|&t| t != 0).collect())
        .unwrap_or_default();
    crate::fw::misc::set_from(&targets)
}

extern "C" fn control_all_events(this: Id, _: Sel) -> usize {
    TARGETS.lock().unwrap().as_ref().and_then(|m| m.get(&this)).map(|l| l.iter().fold(0, |a, t| a | t.events)).unwrap_or(0)
}

/// Whether any target listens for one of `events`.
pub(crate) fn listens_for(this: Id, events: usize) -> bool {
    TARGETS.lock().unwrap().as_ref().and_then(|m| m.get(&this)).is_some_and(|list| list.iter().any(|t| t.events & events != 0))
}

/// Each target/action pair for any of `events`, through -sendAction:to:forEvent:
/// (so a game's control can step in), with the event that caused it.
pub(crate) fn send_actions(this: Id, events: usize, event: Id) {
    let calls: Vec<(Id, Sel)> = TARGETS
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|m| m.get(&this))
        .map(|list| list.iter().filter(|t| t.events & events != 0).map(|t| (t.target, t.action)).collect())
        .unwrap_or_default();
    for (target, action) in calls {
        runtime::send(this, "sendAction:to:forEvent:", &[action, target, event]);
    }
}

extern "C" fn control_send_actions(this: Id, _: Sel, events: usize) {
    send_actions(this, events, 0);
}

extern "C" fn control_send_action(this: Id, _: Sel, action: Sel, target: Id, event: Id) {
    send_action(this, action, target, event);
}

property!(control_selected, control_set_selected, "selected", bool, false);
property!(control_horizontal, control_set_horizontal, "contentHorizontalAlignment", isize, 0);
property!(control_vertical, control_set_vertical, "contentVerticalAlignment", isize, 0);
extern "C" fn control_tracking(this: Id, _: Sel) -> bool {
    get_flag(this, "tracking", false)
}
extern "C" fn control_touch_inside(this: Id, _: Sel) -> bool {
    get_flag(this, "touchInside", false)
}

extern "C" fn control_init_with_frame(this: Id, _: Sel, frame: Rect) -> Id {
    super_init_frame(this, "UIView", frame);
    this
}

// ----- following a finger (UIControl's touch tracking) -----

// UIControlEvents
const TOUCH_DOWN: usize = 1;
const TOUCH_DRAG_INSIDE: usize = 1 << 2;
const TOUCH_DRAG_OUTSIDE: usize = 1 << 3;
const TOUCH_DRAG_ENTER: usize = 1 << 4;
const TOUCH_DRAG_EXIT: usize = 1 << 5;
const TOUCH_UP_INSIDE: usize = 1 << 6;
const TOUCH_UP_OUTSIDE: usize = 1 << 7;
const TOUCH_CANCEL: usize = 1 << 8;

/// How far outside its bounds a finger still counts as on a control (UIKit's margin).
const TOUCH_MARGIN: f64 = 70.0;

/// A tracking method that answers YES or NO (the game's own, if it has one).
fn ask_tracking(this: Id, name: &str, touch: Id, event: Id) -> bool {
    let s = runtime::sel(name);
    match runtime::imp_for(this, s) {
        Some(imp) => {
            let f: extern "C" fn(Id, Sel, Id, Id) -> bool = unsafe { std::mem::transmute(imp) };
            f(this, s, touch, event)
        }
        None => true,
    }
}

fn finger_inside(this: Id, touch: Id) -> bool {
    let at = crate::fw::touch::location_in(touch, this);
    let size = frame_of(this).size;
    at.x >= -TOUCH_MARGIN && at.y >= -TOUCH_MARGIN && at.x < size.width + TOUCH_MARGIN && at.y < size.height + TOUCH_MARGIN
}

fn set_pressed(this: Id, pressed: bool) {
    if get_flag(this, "highlighted", false) != pressed {
        runtime::send(this, "setHighlighted:", &[pressed as usize]);
    }
}

/// A disabled control leaves the touch to the next view up that takes touches (UIResponder's way).
fn pass_on(this: Id, name: &str, touches: Id, event: Id) {
    let mut next = view_state(this).superview;
    while next != 0 && !runtime::responds(next, name) {
        next = view_state(next).superview;
    }
    if next != 0 {
        runtime::send(next, name, &[touches, event]);
    }
}

extern "C" fn control_touches_began(this: Id, _: Sel, touches: Id, event: Id) {
    let enabled = get_flag(this, "enabled", true);
    property_set(this, "passingTouches", !enabled as u64);
    if !enabled {
        pass_on(this, "touchesBegan:withEvent:", touches, event);
        return;
    }
    let touch = runtime::send(touches, "anyObject", &[]);
    property_set(this, "touchInside", 1);
    let tracking = ask_tracking(this, "beginTrackingWithTouch:withEvent:", touch, event);
    property_set(this, "tracking", tracking as u64);
    set_pressed(this, true);
    // A control that refuses tracking sends no actions for this touch.
    if tracking {
        send_actions(this, TOUCH_DOWN, event);
    }
}

extern "C" fn control_touches_moved(this: Id, _: Sel, touches: Id, event: Id) {
    if get_flag(this, "passingTouches", false) {
        pass_on(this, "touchesMoved:withEvent:", touches, event);
        return;
    }
    let touch = runtime::send(touches, "anyObject", &[]);
    let was_inside = get_flag(this, "touchInside", false);
    let inside = finger_inside(this, touch);
    property_set(this, "touchInside", inside as u64);
    set_pressed(this, inside);
    if !get_flag(this, "tracking", false) {
        return;
    }
    // continueTracking NO ends the tracking: no more actions for this touch.
    let going_on = ask_tracking(this, "continueTrackingWithTouch:withEvent:", touch, event);
    property_set(this, "tracking", going_on as u64);
    if !going_on {
        return;
    }
    let crossed = match (was_inside, inside) {
        (true, false) => TOUCH_DRAG_EXIT,
        (false, true) => TOUCH_DRAG_ENTER,
        _ => 0,
    };
    send_actions(this, crossed | if inside { TOUCH_DRAG_INSIDE } else { TOUCH_DRAG_OUTSIDE }, event);
}

pub(crate) extern "C" fn control_touches_ended(this: Id, _: Sel, touches: Id, event: Id) {
    if get_flag(this, "passingTouches", false) {
        pass_on(this, "touchesEnded:withEvent:", touches, event);
        return;
    }
    let touch = runtime::send(touches, "anyObject", &[]);
    let inside = finger_inside(this, touch);
    property_set(this, "touchInside", inside as u64);
    set_pressed(this, false);
    let tracking = get_flag(this, "tracking", false);
    property_set(this, "tracking", 0);
    if tracking {
        runtime::send(this, "endTrackingWithTouch:withEvent:", &[touch, event]);
        send_actions(this, if inside { TOUCH_UP_INSIDE } else { TOUCH_UP_OUTSIDE }, event);
    }
}

extern "C" fn control_touches_cancelled(this: Id, _: Sel, touches: Id, event: Id) {
    if get_flag(this, "passingTouches", false) {
        pass_on(this, "touchesCancelled:withEvent:", touches, event);
        return;
    }
    set_pressed(this, false);
    let tracking = get_flag(this, "tracking", false);
    property_set(this, "tracking", 0);
    if tracking {
        runtime::send(this, "cancelTrackingWithEvent:", &[event]);
        send_actions(this, TOUCH_CANCEL, event);
    }
}

extern "C" fn control_begin_tracking(_this: Id, _: Sel, _touch: Id, _event: Id) -> bool {
    true
}

extern "C" fn control_state_method(this: Id, _: Sel) -> usize {
    control_state(this)
}

extern "C" fn button_with_type(class: Class, _: Sel, kind: isize) -> Id {
    let button = new_view(class, Rect::default());
    property_set(button, "buttonType", kind as u64);
    button
}

property!(button_type, button_set_type_unused, "buttonType", isize, 0);
property!(button_adjusts_highlighted, button_set_adjusts_highlighted, "adjustsImageWhenHighlighted", bool, true);
property!(button_shows_touch, button_set_shows_touch, "showsTouchWhenHighlighted", bool, false);
property!(button_adjusts_disabled, button_set_adjusts_disabled, "adjustsImageWhenDisabled", bool, true);

fn button_label(this: Id) -> Id {
    if let Some(label) = property_get(this, "titleLabel") {
        return label as Id;
    }
    let label = new_view_named("UILabel", Rect::default());
    property_set(label, "font", font(SYSTEM_FONT, 15.0) as u64);
    // A button's title is cut in the middle and has no shadow offset.
    property_set(label, "lineBreakMode", 5);
    set_f(label, "shadowOffset.h", 0.0);
    property_set(this, "titleLabel", label as u64);
    label
}

extern "C" fn button_title_label(this: Id, _: Sel) -> Id {
    button_label(this)
}

extern "C" fn button_image_view(this: Id, _: Sel) -> Id {
    if let Some(view) = property_get(this, "imageView") {
        return view as Id;
    }
    let view = new_view_named("UIImageView", Rect::default());
    property_set(this, "imageView", view as u64);
    view
}

extern "C" fn button_set_title(this: Id, _: Sel, title: Id, state: usize) {
    let title = copied(title);
    state_set(this, "title", state, title);
    if state == control_state(this) || state == 0 {
        property_set(button_label(this), "text", state_get(this, "title", control_state(this)) as u64);
    }
}

extern "C" fn button_title_for_state(this: Id, _: Sel, state: usize) -> Id {
    state_get(this, "title", state)
}

extern "C" fn button_current_title(this: Id, _: Sel) -> Id {
    state_get(this, "title", control_state(this))
}

macro_rules! per_state {
    ($get:ident, $set:ident, $current:ident, $key:literal) => {
        per_state!($get, $set, $current, $key, state_set);
    };
    ($get:ident, $set:ident, $current:ident, $key:literal, $store:ident) => {
        extern "C" fn $set(this: Id, _: Sel, value: Id, state: usize) {
            $store(this, $key, state, value);
        }
        extern "C" fn $get(this: Id, _: Sel, state: usize) -> Id {
            state_get(this, $key, state)
        }
        extern "C" fn $current(this: Id, _: Sel) -> Id {
            state_get(this, $key, control_state(this))
        }
    };
}
per_state!(button_title_color, button_set_title_color, button_current_title_color, "titleColor");
per_state!(button_image, button_set_image, button_current_image, "image", state_set_kept);
per_state!(button_background_image, button_set_background_image, button_current_background_image, "backgroundImage", state_set_kept);
per_state!(button_title_shadow, button_set_title_shadow, button_current_title_shadow, "titleShadowColor");
per_state!(button_attributed_title, button_set_attributed_title, button_current_attributed_title, "attributedTitle");

/// What a UIButton shows in its current state (fw/pictures.rs draws it).
pub struct ButtonParts {
    /// The background picture, filling the bounds (0: none).
    pub background: Id,
    /// The picture and where it goes in the bounds.
    pub image: Option<(Id, Rect)>,
    /// The title and where it goes.
    pub title: Option<(TextLook, Rect)>,
    /// How the background and the picture are drawn.
    pub background_shade: Shade,
    pub image_shade: Shade,
    /// The background and picture a press would show that are the pressed
    /// state's own (0: none, or pressed already), so their files can be made
    /// before the press.
    pub pressed_own: [Id; 2],
}

/// How a button's picture is drawn for its state.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Shade {
    /// Pressed, with no picture of its own for that (adjustsImageWhenHighlighted).
    pub darker: bool,
    /// Disabled, with no picture of its own for that (adjustsImageWhenDisabled).
    pub dimmed: bool,
    /// Pressed, it would be drawn darker (so that copy can be made beforehand).
    pub darker_when_pressed: bool,
}

/// How a button draws the picture it keeps under `key` in its current state.
fn shade_of(button: Id, key: &'static str, state: usize) -> Shade {
    let own = |state: usize| STATE_VALUES.lock().unwrap().as_ref().is_some_and(|values| values.contains_key(&(button, key, state)));
    let darkens = get_flag(button, "adjustsImageWhenHighlighted", true);
    Shade {
        darker: state & 1 != 0 && !own(state) && darkens,
        dimmed: state & 2 != 0 && !own(state) && get_flag(button, "adjustsImageWhenDisabled", true),
        darker_when_pressed: !own(state | 1) && darkens,
    }
}

fn insets_of(this: Id, key: &'static str) -> Insets {
    property_get(this, key).map(|p| unsafe { *(p as *const Insets) }).unwrap_or_default()
}

fn inset(rect: Rect, by: Insets) -> Rect {
    Rect::new(rect.origin.x + by.left, rect.origin.y + by.top, rect.size.width - by.left - by.right, rect.size.height - by.top - by.bottom)
}

/// Lays a button out as UIButton does: the content area is the bounds
/// less the content insets; the picture then the title sit side by side
/// in it, placed by the content alignments (UIControlContentHorizontal/
/// VerticalAlignment: centre 0, left/top 1, right/bottom 2, fill 3), each
/// in the content area less its own insets. The picture keeps its size
/// (no bigger than the content) except that Fill stretches it over its
/// area; the title is one line of the title label's font (or the
/// attributed title's own font and colour), cut to the room left.
pub fn button_parts(button: Id) -> ButtonParts {
    let state = control_state(button);
    let bounds = Rect { origin: Point::default(), size: frame_of(button).size };
    let content = inset(bounds, insets_of(button, "contentEdgeInsets"));
    let room = |length: f64| length.max(0.0);
    let image = state_get(button, "image", state);
    let image_size = match image {
        0 => Size::default(),
        image => {
            let size = crate::fw::uikit::image_points(image);
            Size { width: size.width.min(room(content.size.width)), height: size.height.min(room(content.size.height)) }
        }
    };
    // The title label's settings (a button's: 15-point system font, cut in the middle, no shadow offset).
    let label = get_id(button, "titleLabel");
    let mut font = font_name_and_size(if label != 0 { text_font(label, 0) } else { font(SYSTEM_FONT, 15.0) });
    let line_break = if label != 0 { property_get(label, "lineBreakMode").map_or(5, |v| v as isize) } else { 5 };
    let offset = if label != 0 { (get_f(label, "shadowOffset.w", 0.0), get_f(label, "shadowOffset.h", 0.0)) } else { (0.0, 0.0) };
    let mut title = text_of(state_get(button, "title", state));
    // No plain title: the attributed one, with its own font and colour.
    let mut title_color = state_get(button, "titleColor", state);
    let attributed = state_get(button, "attributedTitle", state);
    if title.is_empty() && attributed != 0 {
        title = crate::fw::attributed::text(attributed);
        if let font_object @ 1.. = crate::fw::attributed::attribute(attributed, "NSFont") {
            font = font_name_and_size(font_object);
        }
        if let color @ 1.. = crate::fw::attributed::attribute(attributed, "NSColor") {
            title_color = color;
        }
    }
    let title_size = match title.is_empty() {
        true => Size::default(),
        false => {
            let (width, height) = crate::fw::graphics::text_box(&title, &font, room(content.size.width - image_size.width), 1, line_break);
            Size { width, height }
        }
    };
    let horizontal = property_get(button, "contentHorizontalAlignment").unwrap_or(0);
    let vertical = property_get(button, "contentVerticalAlignment").unwrap_or(0);
    let image_area = inset(content, insets_of(button, "imageEdgeInsets"));
    let title_area = inset(content, insets_of(button, "titleEdgeInsets"));
    // Fill: the picture takes the room its area leaves (across: beside the title).
    let mut image_size = image_size;
    if image != 0 && horizontal == 3 {
        image_size.width = room(image_area.size.width - title_size.width);
    }
    if image != 0 && vertical == 3 {
        image_size.height = room(image_area.size.height);
    }
    let together = image_size.width + title_size.width;
    // Where an item starts across, `before` it in the row; and down, for its height.
    let across = |area: Rect, before: f64| match horizontal {
        1 | 3 => area.origin.x + before,
        2 => area.origin.x + area.size.width - together + before,
        _ => area.origin.x + (area.size.width - together) / 2.0 + before,
    };
    let down = |area: Rect, height: f64| match vertical {
        1 | 3 => area.origin.y,
        2 => area.origin.y + area.size.height - height,
        _ => area.origin.y + (area.size.height - height) / 2.0,
    };
    let image_part =(image != 0).then(|| (image, Rect { origin: Point { x: across(image_area, 0.0), y: down(image_area, image_size.height) }, size: image_size }));
    let title_part = (!title.is_empty()).then(|| {
        let rect = Rect { origin: Point { x: across(title_area, image_size.width), y: down(title_area, title_size.height) }, size: title_size };
        // Custom buttons' titles are white unless told; the others' the system blue.
        let plain = if get_id(button, "buttonType") == 0 { [1.0; 4] } else { [0.0, 122.0 / 255.0, 1.0, 1.0] };
        let look = TextLook {
            text: title,
            font,
            color: color_parts(title_color).unwrap_or(plain),
            shadow: color_parts(state_get(button, "titleShadowColor", state)).map(|shadow| (shadow, offset)),
            alignment: 1,
            line_break,
            lines: 1,
            size: (title_size.width, title_size.height),
            minimum_font_size: None,
        };
        (look, rect)
    });
    ButtonParts {
        background: state_get(button, "backgroundImage", state),
        image: image_part,
        title: title_part,
        background_shade: shade_of(button, "backgroundImage", state),
        image_shade: shade_of(button, "image", state),
        pressed_own: ["backgroundImage", "image"].map(|key| match state & 1 {
            0 => STATE_VALUES.lock().unwrap().as_ref().and_then(|values| values.get(&(button, key, state | 1)).copied()).unwrap_or(0),
            _ => 0,
        }),
    }
}

extern "C" fn button_size_that_fits(this: Id, _: Sel, _size: Size) -> Size {
    let title = text_of(state_get(this, "title", control_state(this)));
    let text = text_size(&title, 15.0, f64::INFINITY, 1);
    Size { width: text.width.max(30.0), height: text.height.max(30.0) }
}

extern "C" fn button_size_to_fit(this: Id, s: Sel) {
    let size = button_size_that_fits(this, s, Size::default());
    view_state(this).frame.size = size;
}

macro_rules! insets_property {
    ($get:ident, $set:ident, $key:literal) => {
        extern "C" fn $get(this: Id, _: Sel) -> Insets {
            match property_get(this, $key) {
                Some(p) => unsafe { *(p as *const Insets) },
                None => Insets::default(),
            }
        }
        extern "C" fn $set(this: Id, _: Sel, value: Insets) {
            property_set(this, $key, leak(value) as u64);
        }
    };
}
insets_property!(button_content_insets, button_set_content_insets, "contentEdgeInsets");
insets_property!(button_title_insets, button_set_title_insets, "titleEdgeInsets");
insets_property!(button_image_insets, button_set_image_insets, "imageEdgeInsets");
insets_property!(scroll_content_inset, scroll_set_content_inset, "contentInset");
insets_property!(scroll_indicator_insets, scroll_set_indicator_insets, "scrollIndicatorInsets");
insets_property!(flow_section_inset, flow_set_section_inset, "sectionInset");

// ----- text fields and text views -----

extern "C" fn text_view_init_with_frame(this: Id, _: Sel, frame: Rect) -> Id {
    super_init_frame(this, "UIView", frame);
    mark_not_drawn(this);
    this
}

/// Text fields are drawn (fw/pictures.rs, from typing::text_field_look).
extern "C" fn text_field_init_with_frame(this: Id, _: Sel, frame: Rect) -> Id {
    super_init_frame(this, "UIView", frame);
    this
}

// ----- scroll views -----

macro_rules! pair_property {
    ($get:ident, $set:ident, $kind:ident, $a:ident, $b:ident, $ka:literal, $kb:literal) => {
        extern "C" fn $get(this: Id, _: Sel) -> $kind {
            $kind { $a: get_f(this, $ka, 0.0), $b: get_f(this, $kb, 0.0) }
        }
        extern "C" fn $set(this: Id, _: Sel, value: $kind) {
            set_f(this, $ka, value.$a);
            set_f(this, $kb, value.$b);
        }
    };
}
pair_property!(scroll_content_size, scroll_set_content_size, Size, width, height, "contentSize.w", "contentSize.h");
pair_property!(scroll_content_offset, scroll_set_content_offset, Point, x, y, "contentOffset.x", "contentOffset.y");

extern "C" fn scroll_set_content_offset_animated(this: Id, s: Sel, offset: Point, _animated: u8) {
    scroll_set_content_offset(this, s, offset);
}
extern "C" fn scroll_rect_visible(_this: Id, _: Sel, _rect: Rect, _animated: u8) {}

property!(scroll_delegate, scroll_set_delegate, "delegate", id, 0);
property!(scroll_enabled, scroll_set_enabled, "scrollEnabled", bool, true);
property!(scroll_paging, scroll_set_paging, "pagingEnabled", bool, false);
property!(scroll_bounces, scroll_set_bounces, "bounces", bool, true);
property!(scroll_always_vertical, scroll_set_always_vertical, "alwaysBounceVertical", bool, false);
property!(scroll_always_horizontal, scroll_set_always_horizontal, "alwaysBounceHorizontal", bool, false);
property!(scroll_shows_vertical, scroll_set_shows_vertical, "showsVerticalScrollIndicator", bool, true);
property!(scroll_shows_horizontal, scroll_set_shows_horizontal, "showsHorizontalScrollIndicator", bool, true);
property!(scroll_zoom, scroll_set_zoom, "zoomScale", f64, 1.0);
property!(scroll_min_zoom, scroll_set_min_zoom, "minimumZoomScale", f64, 1.0);
property!(scroll_max_zoom, scroll_set_max_zoom, "maximumZoomScale", f64, 1.0);

extern "C" fn scroll_init_with_frame(this: Id, _: Sel, frame: Rect) -> Id {
    super_init_frame(this, "UIView", frame);
    mark_not_drawn(this);
    this
}

// ----- web views -----

/// Web pages can't be shown here. A web view tells its delegate the load
/// started and then failed, the way an offline iPhone reports it (games
/// are already told there is no network), so nothing waits for a page.
extern "C" fn web_view_init_with_frame(this: Id, _: Sel, frame: Rect) -> Id {
    super_init_frame(this, "UIView", frame);
    mark_not_drawn(this);
    this
}

property!(web_delegate, web_set_delegate, "delegate", id, 0);
property!(web_request, web_set_request_unused, "request", id, 0);
property!(web_scales, web_set_scales, "scalesPageToFit", bool, false);

fn web_fail(view: Id, request: Id, address: String) {
    let network = address.starts_with("http:") || address.starts_with("https:");
    later(move || {
        let delegate = get_id(view, "delegate");
        // UIWebViewNavigationTypeOther
        if request != 0 {
            if let Some(answer) = tell(delegate, "webView:shouldStartLoadWithRequest:navigationType:", &[view, request, 5]) {
                if answer & 0xFF == 0 {
                    return;
                }
            }
        }
        crate::log!("web view {view:#x}: page \"{address}\" can't be shown here; telling the game it failed to load");
        tell(delegate, "webViewDidStartLoad:", &[view]);
        let error = if network {
            crate::fw::misc::error("NSURLErrorDomain", -1009, "The Internet connection appears to be offline.")
        } else {
            crate::fw::misc::error("NSURLErrorDomain", -1008, "The requested resource is unavailable.")
        };
        tell(delegate, "webView:didFailLoadWithError:", &[view, error]);
    });
}

extern "C" fn web_load_request(this: Id, _: Sel, request: Id) {
    property_set(this, "request", request as u64);
    let url = runtime::send(request, "URL", &[]);
    let address = text_of(runtime::send(url, "absoluteString", &[]));
    web_fail(this, request, address);
}

extern "C" fn web_load_html(this: Id, _: Sel, _html: Id, _base: Id) {
    web_fail(this, 0, "(text given by the game)".into());
}

extern "C" fn web_load_data(this: Id, _: Sel, _data: Id, _mime: Id, _encoding: Id, _base: Id) {
    web_fail(this, 0, "(data given by the game)".into());
}

extern "C" fn web_scroll_view(this: Id, _: Sel) -> Id {
    if let Some(view) = property_get(this, "scrollView") {
        return view as Id;
    }
    let view = new_view_named("UIScrollView", Rect { origin: Point::default(), size: frame_of(this).size });
    property_set(this, "scrollView", view as u64);
    view
}

// ----- bars and bar items -----

property!(bar_items, bar_set_items, "items", id, 0);
property!(bar_style, bar_set_style, "barStyle", isize, 0);
property!(bar_tint, bar_set_tint, "tintColor", id, 0);
property!(bar_bar_tint, bar_set_bar_tint, "barTintColor", id, 0);
property!(bar_translucent, bar_set_translucent, "translucent", bool, true);
property!(bar_delegate, bar_set_delegate, "delegate", id, 0);
property!(bar_title_attributes, bar_set_title_attributes, "titleTextAttributes", id, 0);

extern "C" fn bar_init_with_frame(this: Id, _: Sel, frame: Rect) -> Id {
    super_init_frame(this, "UIView", frame);
    mark_not_drawn(this);
    this
}

extern "C" fn bar_set_items_animated(this: Id, _: Sel, items: Id, _animated: u8) {
    property_set(this, "items", items as u64);
}

extern "C" fn bar_size_that_fits(_this: Id, _: Sel, size: Size) -> Size {
    Size { width: size.width, height: 44.0 }
}

extern "C" fn bar_top_item(this: Id, _: Sel) -> Id {
    collections::array_items(get_id(this, "items")).last().copied().unwrap_or(0)
}

extern "C" fn bar_ignore_image_metrics(_this: Id, _: Sel, _image: Id, _metrics: isize) {}
extern "C" fn bar_ignore_image_position_metrics(_this: Id, _: Sel, _image: Id, _position: isize, _metrics: isize) {}

extern "C" fn item_init(this: Id, _: Sel) -> Id {
    property_set(this, "enabled", 1);
    this
}

extern "C" fn item_init_title(this: Id, _: Sel, title: Id, style: isize, target: Id, action: Sel) -> Id {
    property_set(this, "title", copied(title) as u64);
    property_set(this, "style", style as u64);
    property_set(this, "target", target as u64);
    property_set(this, "action", action as u64);
    property_set(this, "enabled", 1);
    this
}

extern "C" fn item_init_image(this: Id, _: Sel, image: Id, style: isize, target: Id, action: Sel) -> Id {
    property_set(this, "image", image as u64);
    property_set(this, "style", style as u64);
    property_set(this, "target", target as u64);
    property_set(this, "action", action as u64);
    property_set(this, "enabled", 1);
    this
}

extern "C" fn item_init_system(this: Id, _: Sel, system: isize, target: Id, action: Sel) -> Id {
    property_set(this, "systemItem", system as u64);
    property_set(this, "target", target as u64);
    property_set(this, "action", action as u64);
    property_set(this, "enabled", 1);
    this
}

extern "C" fn item_init_custom(this: Id, _: Sel, view: Id) -> Id {
    property_set(this, "customView", view as u64);
    property_set(this, "enabled", 1);
    this
}

property!(item_title, item_set_title, "title", id, 0);
property!(item_image, item_set_image, "image", id, 0);
property!(item_style, item_set_style, "style", isize, 0);
property!(item_target, item_set_target, "target", id, 0);
property!(item_action, item_set_action_unused, "action", isize, 0);
property!(item_width, item_set_width, "width", f64, 0.0);
property!(item_custom_view, item_set_custom_view, "customView", id, 0);
property!(item_enabled, item_set_enabled, "enabled", bool, true);
property!(item_tint, item_set_tint, "tintColor", id, 0);
property!(item_possible_titles, item_set_possible_titles, "possibleTitles", id, 0);

extern "C" fn item_set_action(this: Id, _: Sel, action: Sel) {
    property_set(this, "action", action as u64);
}

extern "C" fn navigation_item_init_title(this: Id, _: Sel, title: Id) -> Id {
    property_set(this, "title", copied(title) as u64);
    this
}

property!(nav_item_right, nav_item_set_right, "rightBarButtonItem", id, 0);
property!(nav_item_left, nav_item_set_left, "leftBarButtonItem", id, 0);
property!(nav_item_back, nav_item_set_back, "backBarButtonItem", id, 0);
property!(nav_item_hides_back, nav_item_set_hides_back, "hidesBackButton", bool, false);
property!(nav_item_title_view, nav_item_set_title_view, "titleView", id, 0);
property!(nav_item_prompt, nav_item_set_prompt, "prompt", id, 0);

extern "C" fn nav_item_set_right_animated(this: Id, _: Sel, item: Id, _animated: u8) {
    property_set(this, "rightBarButtonItem", item as u64);
}
extern "C" fn nav_item_set_left_animated(this: Id, _: Sel, item: Id, _animated: u8) {
    property_set(this, "leftBarButtonItem", item as u64);
}
extern "C" fn nav_item_set_hides_back_animated(this: Id, _: Sel, hides: u8, _animated: u8) {
    property_set(this, "hidesBackButton", (hides != 0) as u64);
}
extern "C" fn nav_item_right_items(this: Id, _: Sel) -> Id {
    let item = get_id(this, "rightBarButtonItem");
    collections::array_from(&[item][..usize::from(item != 0)])
}
extern "C" fn nav_item_left_items(this: Id, _: Sel) -> Id {
    let item = get_id(this, "leftBarButtonItem");
    collections::array_from(&[item][..usize::from(item != 0)])
}
extern "C" fn nav_item_set_right_items(this: Id, _: Sel, items: Id) {
    property_set(this, "rightBarButtonItem", collections::array_items(items).first().copied().unwrap_or(0) as u64);
}
extern "C" fn nav_item_set_left_items(this: Id, _: Sel, items: Id) {
    property_set(this, "leftBarButtonItem", collections::array_items(items).first().copied().unwrap_or(0) as u64);
}

// ----- view controller extras -----

extern "C" fn controller_navigation_item(this: Id, _: Sel) -> Id {
    if let Some(item) = property_get(this, "navigationItem") {
        return item as Id;
    }
    let item = new_object("UINavigationItem");
    property_set(item, "title", get_id(this, "title") as u64);
    property_set(this, "navigationItem", item as u64);
    item
}

extern "C" fn controller_title(this: Id, _: Sel) -> Id {
    get_id(this, "title")
}

extern "C" fn controller_set_title(this: Id, _: Sel, title: Id) {
    let title = copied(title);
    property_set(this, "title", title as u64);
    if let Some(item) = property_get(this, "navigationItem") {
        property_set(item as Id, "title", title as u64);
    }
}

property!(controller_toolbar_items, controller_set_toolbar_items, "toolbarItems", id, 0);
property!(controller_hides_bottom_bar, controller_set_hides_bottom_bar, "hidesBottomBarWhenPushed", bool, false);
property!(controller_modal_style, controller_set_modal_style, "modalPresentationStyle", isize, 0);
pair_property!(controller_popover_size, controller_set_popover_size, Size, width, height, "popoverSize.w", "popoverSize.h");

extern "C" fn controller_set_toolbar_items_animated(this: Id, _: Sel, items: Id, _animated: u8) {
    property_set(this, "toolbarItems", items as u64);
}

// ----- navigation controllers -----

fn nav_stack(this: Id) -> Vec<Id> {
    collections::array_items(get_id(this, "viewControllers"))
}

/// The delegate hears navigationController:willShowViewController:animated:
/// now and ...didShowViewController:animated: once the change is done; the
/// same delegate gets both, so they always come in pairs.
fn nav_tell_shown(this: Id, top: Id, animated: bool) {
    let delegate = get_id(this, "delegate");
    if delegate == 0 || top == 0 {
        return;
    }
    let args = [this, top, animated as usize];
    tell(delegate, "navigationController:willShowViewController:animated:", &args);
    later(move || {
        tell(delegate, "navigationController:didShowViewController:animated:", &args);
    });
}

/// Called (from fw/uikit.rs) when the navigation controller is first put on
/// screen, presented or as a window's root: its top controller is shown then.
extern "C" fn nav_shown(this: Id, _: Sel) {
    if !get_flag(this, "onScreen", false) {
        property_set(this, "onScreen", 1);
        nav_tell_shown(this, nav_stack(this).last().copied().unwrap_or(0), false);
    }
}

/// Replaces the stack: controllers no longer on it lose their
/// navigationController (as on iOS). Once the navigation controller is on
/// screen, a new top controller is announced to the delegate.
fn nav_set_stack(this: Id, list: &[Id], animated: bool) {
    let old = nav_stack(this);
    for &controller in old.iter().filter(|c| !list.contains(c)) {
        if get_id(controller, "navigationController") == this {
            property_set(controller, "navigationController", 0);
        }
    }
    for &controller in list {
        property_set(controller, "navigationController", this as u64);
    }
    property_set(this, "viewControllers", collections::array_from(list) as u64);
    let top = list.last().copied().unwrap_or(0);
    if get_flag(this, "onScreen", false) && old.last().copied().unwrap_or(0) != top {
        nav_tell_shown(this, top, animated);
    }
}

extern "C" fn nav_init(this: Id, _: Sel) -> Id {
    nav_set_stack(this, &[], false);
    this
}

extern "C" fn nav_init_with_root(this: Id, _: Sel, root: Id) -> Id {
    nav_set_stack(this, &[root][..usize::from(root != 0)], false);
    this
}

extern "C" fn nav_init_with_classes(this: Id, _: Sel, bar: Class, toolbar: Class) -> Id {
    nav_set_stack(this, &[], false);
    if bar != 0 {
        property_set(this, "navigationBar", new_view(bar, Rect::default()) as u64);
    }
    if toolbar != 0 {
        property_set(this, "toolbar", new_view(toolbar, Rect::default()) as u64);
    }
    this
}

extern "C" fn nav_view_controllers(this: Id, _: Sel) -> Id {
    collections::array_from(&nav_stack(this))
}

extern "C" fn nav_set_view_controllers(this: Id, _: Sel, list: Id) {
    nav_set_stack(this, &collections::array_items(list), false);
}

extern "C" fn nav_set_view_controllers_animated(this: Id, _: Sel, list: Id, animated: u8) {
    nav_set_stack(this, &collections::array_items(list), animated != 0);
}

extern "C" fn nav_push(this: Id, _: Sel, controller: Id, animated: u8) {
    if controller == 0 {
        return;
    }
    let mut stack = nav_stack(this);
    stack.push(controller);
    nav_set_stack(this, &stack, animated != 0);
    crate::log!("navigation controller {this:#x} shows {} (not drawn yet)", string::describe_class_of(controller));
}

extern "C" fn nav_pop(this: Id, _: Sel, animated: u8) -> Id {
    let mut stack = nav_stack(this);
    if stack.len() < 2 {
        return 0;
    }
    let popped = stack.pop().unwrap_or(0);
    nav_set_stack(this, &stack, animated != 0);
    popped
}

fn nav_pop_to(this: Id, keep: usize, animated: u8) -> Id {
    let mut stack = nav_stack(this);
    if keep >= stack.len() {
        return collections::array_from(&[]);
    }
    let popped = stack.split_off(keep);
    nav_set_stack(this, &stack, animated != 0);
    collections::array_from(&popped)
}

extern "C" fn nav_pop_to_root(this: Id, _: Sel, animated: u8) -> Id {
    nav_pop_to(this, 1, animated)
}

extern "C" fn nav_pop_to_controller(this: Id, _: Sel, controller: Id, animated: u8) -> Id {
    match nav_stack(this).iter().position(|&c| c == controller) {
        Some(index) => nav_pop_to(this, index + 1, animated),
        None => 0,
    }
}

extern "C" fn nav_top(this: Id, _: Sel) -> Id {
    nav_stack(this).last().copied().unwrap_or(0)
}

extern "C" fn nav_visible(this: Id, _: Sel) -> Id {
    match get_id(this, "presented") {
        0 => nav_stack(this).last().copied().unwrap_or(0),
        shown => shown,
    }
}

extern "C" fn nav_bar(this: Id, _: Sel) -> Id {
    if let Some(bar) = property_get(this, "navigationBar") {
        return bar as Id;
    }
    let (w, _, _) = screen_points();
    let bar = new_view_named("UINavigationBar", Rect::new(0.0, 0.0, w, 44.0));
    property_set(this, "navigationBar", bar as u64);
    bar
}

extern "C" fn nav_toolbar(this: Id, _: Sel) -> Id {
    if let Some(bar) = property_get(this, "toolbar") {
        return bar as Id;
    }
    let (w, h, _) = screen_points();
    let bar = new_view_named("UIToolbar", Rect::new(0.0, h - 44.0, w, 44.0));
    property_set(this, "toolbar", bar as u64);
    bar
}

property!(nav_bar_hidden, nav_set_bar_hidden, "navigationBarHidden", bool, false);
property!(nav_toolbar_hidden, nav_set_toolbar_hidden, "toolbarHidden", bool, true);
property!(nav_delegate, nav_set_delegate, "delegate", id, 0);

extern "C" fn nav_set_bar_hidden_animated(this: Id, _: Sel, hidden: u8, _animated: u8) {
    property_set(this, "navigationBarHidden", (hidden != 0) as u64);
}
extern "C" fn nav_set_toolbar_hidden_animated(this: Id, _: Sel, hidden: u8, _animated: u8) {
    property_set(this, "toolbarHidden", (hidden != 0) as u64);
}

// ----- tables -----

/// Classes registered for reuse identifiers (tables and collections).
static REGISTERED: Mutex<Option<HashMap<(Id, String), Class>>> = Mutex::new(None);

fn register_class(view: Id, key: String, class: Class) {
    REGISTERED.lock().unwrap().get_or_insert_with(HashMap::new).insert((view, key), class);
}

pub(crate) fn registered_class(view: Id, key: &str) -> Option<Class> {
    REGISTERED.lock().unwrap().as_ref().and_then(|m| m.get(&(view, key.to_string())).copied())
}

extern "C" fn table_init_with_frame(this: Id, _: Sel, frame: Rect) -> Id {
    scroll_init_with_frame(this, 0, frame)
}

extern "C" fn table_init_with_style(this: Id, _: Sel, frame: Rect, style: isize) -> Id {
    property_set(this, "style", style as u64);
    call_rect(this, "initWithFrame:", frame)
}

property!(table_style, table_set_style_unused, "style", isize, 0);
property!(table_data_source, table_set_data_source, "dataSource", id, 0);
property!(table_header, table_set_header, "tableHeaderView", id, 0);
property!(table_footer, table_set_footer, "tableFooterView", id, 0);
property!(table_row_height, table_set_row_height, "rowHeight", f64, 44.0);
property!(table_section_header_height, table_set_section_header_height, "sectionHeaderHeight", f64, 22.0);
property!(table_section_footer_height, table_set_section_footer_height, "sectionFooterHeight", f64, 22.0);
property!(table_separator_style, table_set_separator_style, "separatorStyle", isize, 1);
property!(table_separator_color, table_set_separator_color, "separatorColor", id, 0);
property!(table_allows_selection, table_set_allows_selection, "allowsSelection", bool, true);
property!(table_allows_multiple, table_set_allows_multiple, "allowsMultipleSelection", bool, false);
property!(table_background_view, table_set_background_view, "backgroundView", id, 0);
property!(table_editing, table_set_editing, "editing", bool, false);

extern "C" fn table_set_editing_animated(this: Id, _: Sel, editing: u8, _animated: u8) {
    property_set(this, "editing", (editing != 0) as u64);
}

extern "C" fn table_register_class(this: Id, _: Sel, class: Class, identifier: Id) {
    register_class(this, format!("cell:{}", text_of(identifier)), class);
}

extern "C" fn table_number_of_sections(this: Id, _: Sel) -> isize {
    let source = get_id(this, "dataSource");
    tell(source, "numberOfSectionsInTableView:", &[this]).map(|n| n as isize).unwrap_or(1)
}

extern "C" fn table_rows_in_section(this: Id, _: Sel, section: isize) -> isize {
    let source = get_id(this, "dataSource");
    tell(source, "tableView:numberOfRowsInSection:", &[this, section as usize]).map(|n| n as isize).unwrap_or(0)
}

// ----- selected rows / items (tables and collections), set by the game -----

static SELECTED: Mutex<Option<HashMap<Id, Vec<Id>>>> = Mutex::new(None);

pub(crate) fn selected(view: Id) -> Vec<Id> {
    SELECTED.lock().unwrap().as_ref().and_then(|m| m.get(&view).cloned()).unwrap_or_default()
}

/// Two index paths are the same when they name the same section and row
/// (or item), whichever NSIndexPath class made them; else isEqual: decides.
pub(crate) fn same_path(a: Id, b: Id) -> bool {
    if a == b {
        return true;
    }
    if a == 0 || b == 0 {
        return false;
    }
    let parts = |path: Id| -> Option<(usize, usize, usize)> {
        let length = if runtime::responds(path, "length") { runtime::send(path, "length", &[]) } else { 2 };
        if runtime::responds(path, "section") && runtime::responds(path, "row") {
            Some((length, runtime::send(path, "section", &[]), runtime::send(path, "row", &[])))
        } else {
            None
        }
    };
    match (parts(a), parts(b)) {
        (Some(x), Some(y)) => x == y,
        _ => runtime::send(a, "isEqual:", &[b]) & 0xFF != 0,
    }
}

/// select...AtIndexPath: a nil path clears the selection; without multiple
/// selection the new path replaces the old one; an equal path is kept once.
fn select_in(list: &mut Vec<Id>, path: Id, multiple: bool, same: &dyn Fn(Id, Id) -> bool) {
    if path == 0 {
        list.clear();
    } else if !list.iter().any(|&p| same(p, path)) {
        if !multiple {
            list.clear();
        }
        list.push(path); // index paths never change; objects aren't freed here
    }
}

fn deselect_in(list: &mut Vec<Id>, path: Id, same: &dyn Fn(Id, Id) -> bool) {
    list.retain(|&p| !same(p, path));
}

pub(crate) fn select_path(view: Id, path: Id) {
    let mut list = selected(view);
    select_in(&mut list, path, get_flag(view, "allowsMultipleSelection", false), &same_path);
    SELECTED.lock().unwrap().get_or_insert_with(HashMap::new).insert(view, list);
}

pub(crate) fn deselect_path(view: Id, path: Id) {
    let mut list = selected(view);
    deselect_in(&mut list, path, &same_path);
    SELECTED.lock().unwrap().get_or_insert_with(HashMap::new).insert(view, list);
}

/// reloadData forgets the selection, as UIKit does.
pub(crate) fn clear_selection(view: Id) {
    if let Some(map) = SELECTED.lock().unwrap().as_mut() {
        map.remove(&view);
    }
}

extern "C" fn table_selected_row(this: Id, _: Sel) -> Id {
    selected(this).first().copied().unwrap_or(0)
}

/// UITableView answers nil when nothing is selected.
extern "C" fn table_selected_rows(this: Id, _: Sel) -> Id {
    let list = selected(this);
    if list.is_empty() {
        0
    } else {
        collections::array_from(&list)
    }
}

extern "C" fn table_scroll_to(_this: Id, _: Sel, _path: Id, _position: isize, _animated: u8) {}

extern "C" fn table_controller_init(this: Id, _: Sel) -> Id {
    property_set(this, "tableStyle", 0);
    this
}

extern "C" fn table_controller_init_style(this: Id, _: Sel, style: isize) -> Id {
    property_set(this, "tableStyle", style as u64);
    this
}

extern "C" fn table_controller_load_view(this: Id, _: Sel) {
    let style = property_get(this, "tableStyle").unwrap_or(0) as usize;
    let table = runtime::send(runtime::class_named("UITableView"), "alloc", &[]);
    let s = runtime::sel("initWithFrame:style:");
    let imp = runtime::imp_for(table, s).expect("UITableView initWithFrame:style:");
    let f: extern "C" fn(Id, Sel, Rect, usize) -> Id = unsafe { std::mem::transmute(imp) };
    let table = f(table, s, screen_rect(), style);
    property_set(table, "dataSource", this as u64);
    property_set(table, "delegate", this as u64);
    runtime::send(this, "setView:", &[table]);
}

/// tableView is the controller's view (loading it first, as UIKit does).
extern "C" fn table_controller_table(this: Id, _: Sel) -> Id {
    let view = runtime::send(this, "view", &[]);
    if class_is(view, "UITableView") { view } else { 0 }
}

extern "C" fn table_controller_set_table(this: Id, _: Sel, table: Id) {
    runtime::send(this, "setView:", &[table]);
}

property!(controller_clears_selection, controller_set_clears_selection, "clearsSelectionOnViewWillAppear", bool, true);

extern "C" fn cell_init_with_style(this: Id, _: Sel, style: isize, identifier: Id) -> Id {
    property_set(this, "cellStyle", style as u64);
    property_set(this, "reuseIdentifier", copied(identifier) as u64);
    let (w, _, _) = screen_points();
    call_rect(this, "initWithFrame:", Rect::new(0.0, 0.0, w, 44.0))
}

fn cell_content_view(this: Id) -> Id {
    if let Some(view) = property_get(this, "contentView") {
        return view as Id;
    }
    let size = frame_of(this).size;
    let view = new_view_named("UIView", Rect { origin: Point::default(), size });
    property_set(this, "contentView", view as u64);
    runtime::send(this, "addSubview:", &[view]);
    view
}

extern "C" fn cell_content(this: Id, _: Sel) -> Id {
    cell_content_view(this)
}

fn cell_child(this: Id, key: &'static str, class: &str) -> Id {
    if let Some(view) = property_get(this, key) {
        return view as Id;
    }
    let view = new_view_named(class, Rect::default());
    property_set(this, key, view as u64);
    runtime::send(cell_content_view(this), "addSubview:", &[view]);
    view
}

extern "C" fn cell_text_label(this: Id, _: Sel) -> Id {
    cell_child(this, "textLabel", "UILabel")
}

/// The default style has no detail label (nil), as in UIKit.
extern "C" fn cell_detail_label(this: Id, _: Sel) -> Id {
    if property_get(this, "cellStyle").unwrap_or(0) == 0 {
        return 0;
    }
    cell_child(this, "detailTextLabel", "UILabel")
}

extern "C" fn cell_image_view(this: Id, _: Sel) -> Id {
    cell_child(this, "imageView", "UIImageView")
}

property!(cell_reuse_identifier, cell_set_reuse_identifier_unused, "reuseIdentifier", id, 0);
property!(cell_accessory_type, cell_set_accessory_type, "accessoryType", isize, 0);
property!(cell_accessory_view, cell_set_accessory_view, "accessoryView", id, 0);
property!(cell_selection_style, cell_set_selection_style, "selectionStyle", isize, 1);
property!(cell_background_view, cell_set_background_view, "backgroundView", id, 0);
property!(cell_selected_background_view, cell_set_selected_background_view, "selectedBackgroundView", id, 0);
property!(cell_indentation, cell_set_indentation, "indentationLevel", isize, 0);

// ----- collections -----

extern "C" fn collection_init_with_frame(this: Id, _: Sel, frame: Rect) -> Id {
    scroll_init_with_frame(this, 0, frame)
}

extern "C" fn collection_init_with_layout(this: Id, _: Sel, frame: Rect, layout: Id) -> Id {
    let this = call_rect(this, "initWithFrame:", frame);
    collection_set_layout(this, 0, layout);
    this
}

extern "C" fn collection_layout(this: Id, _: Sel) -> Id {
    get_id(this, "collectionViewLayout")
}

extern "C" fn collection_set_layout(this: Id, _: Sel, layout: Id) {
    property_set(this, "collectionViewLayout", layout as u64);
    if layout != 0 {
        property_set(layout, "collectionView", this as u64);
    }
}

extern "C" fn collection_set_layout_animated(this: Id, s: Sel, layout: Id, _animated: u8) {
    collection_set_layout(this, s, layout);
}

property!(collection_data_source, collection_set_data_source, "dataSource", id, 0);
property!(collection_allows_selection, collection_set_allows_selection, "allowsSelection", bool, true);
property!(collection_allows_multiple, collection_set_allows_multiple, "allowsMultipleSelection", bool, false);
property!(collection_background_view, collection_set_background_view, "backgroundView", id, 0);

extern "C" fn collection_register_cell(this: Id, _: Sel, class: Class, identifier: Id) {
    register_class(this, format!("cell:{}", text_of(identifier)), class);
}

extern "C" fn collection_register_supplementary(this: Id, _: Sel, class: Class, kind: Id, identifier: Id) {
    register_class(this, format!("{}:{}", text_of(kind), text_of(identifier)), class);
}

fn flow_size(layout: Id, key_w: &'static str, key_h: &'static str, default: Size) -> Size {
    if layout == 0 {
        return default;
    }
    Size { width: get_f(layout, key_w, default.width), height: get_f(layout, key_h, default.height) }
}

fn collection_make(this: Id, key: String, size: Size) -> Id {
    let Some(class) = registered_class(this, &key) else {
        crate::log!("collection view {this:#x}: no class registered for \"{key}\"");
        return 0;
    };
    let view = new_view(class, Rect { origin: Point::default(), size });
    let identifier = key.split_once(':').map(|(_, id)| id).unwrap_or("");
    property_set(view, "reuseIdentifier", string::make(identifier) as u64);
    view
}

extern "C" fn collection_dequeue_cell(this: Id, _: Sel, identifier: Id, _path: Id) -> Id {
    let layout = get_id(this, "collectionViewLayout");
    let size = flow_size(layout, "itemSize.w", "itemSize.h", Size { width: 50.0, height: 50.0 });
    collection_make(this, format!("cell:{}", text_of(identifier)), size)
}

extern "C" fn collection_dequeue_supplementary(this: Id, _: Sel, kind: Id, identifier: Id, _path: Id) -> Id {
    let layout = get_id(this, "collectionViewLayout");
    let kind_text = text_of(kind);
    let size = if kind_text.ends_with("Footer") {
        flow_size(layout, "footerSize.w", "footerSize.h", Size::default())
    } else {
        flow_size(layout, "headerSize.w", "headerSize.h", Size::default())
    };
    collection_make(this, format!("{kind_text}:{}", text_of(identifier)), size)
}

extern "C" fn collection_reload(this: Id, _: Sel) {
    clear_selection(this);
    not_drawn(&format!("collection view {this:#x} contents"));
}

extern "C" fn collection_number_of_sections(this: Id, _: Sel) -> isize {
    let source = get_id(this, "dataSource");
    tell(source, "numberOfSectionsInCollectionView:", &[this]).map(|n| n as isize).unwrap_or(1)
}

extern "C" fn collection_items_in_section(this: Id, _: Sel, section: isize) -> isize {
    let source = get_id(this, "dataSource");
    tell(source, "collectionView:numberOfItemsInSection:", &[this, section as usize]).map(|n| n as isize).unwrap_or(0)
}

/// performBatchUpdates:completion: runs the changes, then completion(YES).
extern "C" fn collection_batch(_this: Id, _: Sel, updates: usize, completion: usize) {
    if updates != 0 {
        unsafe { crate::libsystem::mach::call_block(updates) };
    }
    call_block_later(completion, [1, 0, 0]);
}

extern "C" fn collection_select(this: Id, _: Sel, path: Id, _animated: u8, _position: usize) {
    select_path(this, path);
}

extern "C" fn collection_deselect(this: Id, _: Sel, path: Id, _animated: u8) {
    deselect_path(this, path);
}

/// UICollectionView answers an empty list when nothing is selected.
extern "C" fn collection_selected_items(this: Id, _: Sel) -> Id {
    collections::array_from(&selected(this))
}
extern "C" fn collection_scroll_to(_this: Id, _: Sel, _path: Id, _position: usize, _animated: u8) {}

extern "C" fn collection_controller_init_layout(this: Id, _: Sel, layout: Id) -> Id {
    property_set(this, "layout", layout as u64);
    this
}

extern "C" fn collection_controller_load_view(this: Id, _: Sel) {
    let layout = get_id(this, "layout");
    let view = runtime::send(runtime::class_named("UICollectionView"), "alloc", &[]);
    let s = runtime::sel("initWithFrame:collectionViewLayout:");
    let imp = runtime::imp_for(view, s).expect("UICollectionView initWithFrame:collectionViewLayout:");
    let f: extern "C" fn(Id, Sel, Rect, Id) -> Id = unsafe { std::mem::transmute(imp) };
    let view = f(view, s, screen_rect(), layout);
    property_set(view, "dataSource", this as u64);
    property_set(view, "delegate", this as u64);
    runtime::send(this, "setView:", &[view]);
}

extern "C" fn collection_controller_view(this: Id, _: Sel) -> Id {
    let view = runtime::send(this, "view", &[]);
    if class_is(view, "UICollectionView") { view } else { 0 }
}

extern "C" fn collection_controller_set_view(this: Id, _: Sel, view: Id) {
    runtime::send(this, "setView:", &[view]);
}

extern "C" fn collection_controller_layout(this: Id, _: Sel) -> Id {
    get_id(this, "layout")
}

extern "C" fn reusable_init_with_frame(this: Id, _: Sel, frame: Rect) -> Id {
    super_init_frame(this, "UIView", frame);
    mark_not_drawn(this);
    this
}

extern "C" fn collection_cell_content(this: Id, _: Sel) -> Id {
    cell_content_view(this)
}

pair_property!(flow_item_size_raw, flow_set_item_size, Size, width, height, "itemSize.w", "itemSize.h");
pair_property!(flow_estimated_size, flow_set_estimated_size, Size, width, height, "estimatedSize.w", "estimatedSize.h");
pair_property!(flow_header_size, flow_set_header_size, Size, width, height, "headerSize.w", "headerSize.h");
pair_property!(flow_footer_size, flow_set_footer_size, Size, width, height, "footerSize.w", "footerSize.h");
property!(flow_line_spacing, flow_set_line_spacing, "minimumLineSpacing", f64, 10.0);
property!(flow_item_spacing, flow_set_item_spacing, "minimumInteritemSpacing", f64, 10.0);
property!(flow_direction, flow_set_direction, "scrollDirection", isize, 0);
property!(layout_collection_view, layout_set_collection_view_unused, "collectionView", id, 0);

/// UICollectionViewFlowLayout starts with 50x50 items.
extern "C" fn flow_item_size(this: Id, _: Sel) -> Size {
    flow_size(this, "itemSize.w", "itemSize.h", Size { width: 50.0, height: 50.0 })
}

extern "C" fn layout_content_size(_this: Id, _: Sel) -> Size {
    Size::default()
}

// ----- alerts and action sheets -----

/// Buttons of an alert or action sheet, with UIKit's special indices
/// (-1 when there is none).
#[derive(Clone, Default, Debug, PartialEq)]
pub struct Buttons {
    pub titles: Vec<String>,
    pub cancel: isize,
    pub destructive: isize,
    pub first_other: isize,
}

impl Buttons {
    fn empty() -> Buttons {
        Buttons { titles: Vec::new(), cancel: -1, destructive: -1, first_other: -1 }
    }

    /// UIAlertView: the cancel button comes first, the others after it.
    pub fn alert(cancel: Option<String>, others: Vec<String>) -> Buttons {
        let mut buttons = Buttons::empty();
        if let Some(cancel) = cancel {
            buttons.cancel = 0;
            buttons.titles.push(cancel);
        }
        for title in others {
            buttons.add(title);
        }
        buttons
    }

    /// UIActionSheet: destructive first, then the others, cancel last.
    pub fn sheet(destructive: Option<String>, cancel: Option<String>, others: Vec<String>) -> Buttons {
        let mut buttons = Buttons::empty();
        if let Some(destructive) = destructive {
            buttons.destructive = 0;
            buttons.titles.push(destructive);
        }
        for title in others {
            buttons.add(title);
        }
        if let Some(cancel) = cancel {
            buttons.cancel = buttons.titles.len() as isize;
            buttons.titles.push(cancel);
        }
        buttons
    }

    /// addButtonWithTitle: gives the new button's index.
    pub fn add(&mut self, title: String) -> isize {
        let index = self.titles.len() as isize;
        self.titles.push(title);
        self.find_first_other();
        index
    }

    /// The first button that is neither cancel nor destructive (-1: none),
    /// found again when those change, as UIKit's firstOtherButtonIndex is.
    fn find_first_other(&mut self) {
        let (cancel, destructive) = (self.cancel, self.destructive);
        self.first_other = (0..self.titles.len() as isize).find(|&i| i != cancel && i != destructive).unwrap_or(-1);
    }
}

static BUTTONS: Mutex<Option<HashMap<Id, Buttons>>> = Mutex::new(None);

fn with_buttons<T>(this: Id, f: impl FnOnce(&mut Buttons) -> T) -> T {
    let mut all = BUTTONS.lock().unwrap();
    f(all.get_or_insert_with(HashMap::new).entry(this).or_insert_with(Buttons::empty))
}

fn nil_terminated(first: Id, va: *const u64) -> Vec<String> {
    let mut list = Vec::new();
    if first == 0 {
        return list;
    }
    list.push(text_of(first));
    let mut index = 0;
    loop {
        let item = unsafe { *va.add(index) } as Id;
        if item == 0 {
            break;
        }
        list.push(text_of(item));
        index += 1;
    }
    list
}

fn optional_text(object: Id) -> Option<String> {
    if object == 0 { None } else { Some(text_of(object)) }
}

extern "C" fn alert_init_with_frame(this: Id, _: Sel, frame: Rect) -> Id {
    super_init_frame(this, "UIView", frame);
    with_buttons(this, |_| ());
    this
}

crate::libsystem::libc::variadic!(ios_alert_init_with_buttons, 7, ios_alert_init_with_buttons_impl);
crate::libsystem::libc::variadic!(ios_sheet_init_with_buttons, 7, ios_sheet_init_with_buttons_impl);

#[no_mangle]
extern "C" fn ios_alert_init_with_buttons_impl(
    this: Id,
    _: Sel,
    title: Id,
    message: Id,
    delegate: Id,
    cancel: Id,
    first: Id,
    va: *const u64,
) -> Id {
    let this = call_rect(this, "initWithFrame:", Rect::default());
    property_set(this, "title", copied(title) as u64);
    property_set(this, "message", copied(message) as u64);
    property_set(this, "delegate", delegate as u64);
    let buttons = Buttons::alert(optional_text(cancel), nil_terminated(first, va));
    BUTTONS.lock().unwrap().get_or_insert_with(HashMap::new).insert(this, buttons);
    this
}

#[no_mangle]
extern "C" fn ios_sheet_init_with_buttons_impl(
    this: Id,
    _: Sel,
    title: Id,
    delegate: Id,
    cancel: Id,
    destructive: Id,
    first: Id,
    va: *const u64,
) -> Id {
    let this = call_rect(this, "initWithFrame:", Rect::default());
    property_set(this, "title", copied(title) as u64);
    property_set(this, "delegate", delegate as u64);
    let buttons = Buttons::sheet(optional_text(destructive), optional_text(cancel), nil_terminated(first, va));
    BUTTONS.lock().unwrap().get_or_insert_with(HashMap::new).insert(this, buttons);
    this
}

property!(alert_title, alert_set_title, "title", id, 0);
property!(alert_message, alert_set_message, "message", id, 0);
property!(alert_delegate, alert_set_delegate, "delegate", id, 0);
property!(alert_visible, alert_set_visible_unused, "visible", bool, false);
property!(alert_style, alert_set_style_raw, "alertViewStyle", isize, 0);
property!(sheet_style, sheet_set_style, "actionSheetStyle", isize, -1);

extern "C" fn alert_add_button(this: Id, _: Sel, title: Id) -> isize {
    with_buttons(this, |b| b.add(text_of(title)))
}
extern "C" fn alert_number_of_buttons(this: Id, _: Sel) -> isize {
    with_buttons(this, |b| b.titles.len() as isize)
}
extern "C" fn alert_button_title(this: Id, _: Sel, index: isize) -> Id {
    match with_buttons(this, |b| b.titles.get(index.max(0) as usize).cloned().filter(|_| index >= 0)) {
        Some(title) => string::make(&title),
        None => 0,
    }
}
extern "C" fn alert_cancel_index(this: Id, _: Sel) -> isize {
    with_buttons(this, |b| b.cancel)
}
extern "C" fn alert_set_cancel_index(this: Id, _: Sel, index: isize) {
    with_buttons(this, |b| {
        b.cancel = index;
        b.find_first_other();
    });
}
extern "C" fn alert_first_other_index(this: Id, _: Sel) -> isize {
    with_buttons(this, |b| b.first_other)
}
extern "C" fn sheet_destructive_index(this: Id, _: Sel) -> isize {
    with_buttons(this, |b| b.destructive)
}
extern "C" fn sheet_set_destructive_index(this: Id, _: Sel, index: isize) {
    with_buttons(this, |b| {
        b.destructive = index;
        b.find_first_other();
    });
}

/// Text fields of the input styles (secure 1, plain 2, login and password 3).
extern "C" fn alert_set_style(this: Id, s: Sel, style: isize) {
    alert_set_style_raw(this, s, style);
    let count = match style {
        1 | 2 => 1,
        3 => 2,
        _ => 0,
    };
    let fields: Vec<Id> = (0..count).map(|_| new_view_named("UITextField", Rect::default())).collect();
    if style == 1 || style == 3 {
        property_set(*fields.last().unwrap(), "secureTextEntry", 1);
    }
    property_set(this, "textFields", collections::array_from(&fields) as u64);
}

extern "C" fn alert_text_field(this: Id, _: Sel, index: isize) -> Id {
    let fields = collections::array_items(get_id(this, "textFields"));
    if index < 0 { 0 } else { fields.get(index as usize).copied().unwrap_or(0) }
}

/// An alert or sheet is shown (visible, drawn over the key window, its
/// delegate hears it was presented); with no window yet it is only
/// "visible", and the game's own code may still close it with
/// dismissWithClickedButtonIndex:animated:.
fn show(this: Id, kind: &str, present_will: &'static str, present_did: &'static str) {
    let buttons = with_buttons(this, |b| b.clone());
    crate::log!(
        "{kind} shown: title \"{}\", message \"{}\", buttons {:?} (cancel {})",
        text_of(get_id(this, "title")),
        text_of(get_id(this, "message")),
        buttons.titles,
        buttons.cancel
    );
    property_set(this, "visible", 1);
    if !alert_draw(this, kind != "alert") {
        crate::log!("{kind}: no window to show it in, so it is not drawn");
    }
    later(move || {
        let delegate = get_id(this, "delegate");
        tell(delegate, present_will, &[this]);
        tell(delegate, present_did, &[this]);
    });
}

extern "C" fn alert_show(this: Id, _: Sel) {
    show(this, "alert", "willPresentAlertView:", "didPresentAlertView:");
}

extern "C" fn alert_dismiss(this: Id, _: Sel, index: isize, _animated: u8) {
    if !get_flag(this, "visible", false) {
        return;
    }
    property_set(this, "visible", 0);
    for field in collections::array_items(get_id(this, "textFields")) {
        runtime::send(field, "resignFirstResponder", &[]);
    }
    let delegate = get_id(this, "delegate");
    let (will, did) = if class_is(this, "UIActionSheet") {
        ("actionSheet:willDismissWithButtonIndex:", "actionSheet:didDismissWithButtonIndex:")
    } else {
        ("alertView:willDismissWithButtonIndex:", "alertView:didDismissWithButtonIndex:")
    };
    tell(delegate, will, &[this, index as usize]);
    runtime::send(this, "removeFromSuperview", &[]);
    later(move || {
        tell(delegate, did, &[this, index as usize]);
    });
}

fn sheet_show(this: Id) {
    show(this, "action sheet", "willPresentActionSheet:", "didPresentActionSheet:");
}

// ----- drawing alerts and action sheets (iOS 7's look) -----

const ALERT_WIDTH: f64 = 270.0;
const ALERT_MARGIN: f64 = 16.0;
const ALERT_BUTTON_HEIGHT: f64 = 44.0;
const ALERT_FIELD_HEIGHT: f64 = 24.0;
/// iOS 7's blue for buttons.
const TINT: [f64; 4] = [0.0, 0.478, 1.0, 1.0];
const DESTRUCTIVE: [f64; 4] = [1.0, 0.231, 0.188, 1.0];
const DISABLED_TITLE: [f64; 4] = [0.6, 0.6, 0.6, 1.0];
const LINE: [f64; 4] = [0.8, 0.8, 0.8, 1.0];

fn colored(rgba: [f64; 4]) -> Id {
    crate::fw::graphics::make_color(rgba)
}

fn set_frame(view: Id, frame: Rect) {
    let s = runtime::sel("setFrame:");
    if let Some(imp) = runtime::imp_for(view, s) {
        let f: extern "C" fn(Id, Sel, Rect) = unsafe { std::mem::transmute(imp) };
        f(view, s, frame);
    }
}

fn add_view(parent: Id, class: &str, frame: Rect, background: Option<[f64; 4]>) -> Id {
    let view = new_view_named(class, frame);
    if let Some(rgba) = background {
        runtime::send(view, "setBackgroundColor:", &[colored(rgba)]);
    }
    runtime::send(parent, "addSubview:", &[view]);
    view
}

/// A centred label of `text` at `y`, as tall as its lines; gives its height.
fn add_alert_label(parent: Id, text: &str, font: Id, color: [f64; 4], y: f64, width: f64) -> f64 {
    if text.is_empty() {
        return 0.0;
    }
    let height = text_size(text, font_size(font), width, 0).height;
    let label = add_view(parent, "UILabel", Rect::new(ALERT_MARGIN, y, width, height), None);
    runtime::send(label, "setText:", &[string::make(text)]);
    runtime::send(label, "setFont:", &[font]);
    runtime::send(label, "setTextColor:", &[colored(color)]);
    runtime::send(label, "setTextAlignment:", &[1]);
    runtime::send(label, "setNumberOfLines:", &[0]);
    height
}

/// Builds what an alert (or action sheet) shows as its own views, over
/// the whole key window: a dimmed backdrop that keeps taps from the game,
/// and a box with the title, message, text fields and buttons. Alerts
/// with two buttons have them side by side, others one above the other;
/// a sheet's box sits at the bottom. False with no window to show it in.
fn alert_draw(this: Id, sheet: bool) -> bool {
    let window = crate::fw::uikit::key_window();
    if window == 0 {
        return false;
    }
    for child in view_state(this).subviews.clone() {
        runtime::send(child, "removeFromSuperview", &[]);
    }
    let area = frame_of(window).size;
    set_frame(this, Rect::new(0.0, 0.0, area.width, area.height));
    runtime::send(this, "setBackgroundColor:", &[colored([0.0, 0.0, 0.0, 0.4])]);
    let width = if sheet { (area.width - 16.0).min(400.0) } else { ALERT_WIDTH };
    let inner = width - 2.0 * ALERT_MARGIN;
    let panel = add_view(this, "UIView", Rect::default(), Some([0.97, 0.97, 0.97, 1.0]));

    let (title, message) = (text_of(get_id(this, "title")), text_of(get_id(this, "message")));
    let mut y = if title.is_empty() && message.is_empty() { 0.0 } else { 20.0 };
    let (title_font, title_color) = if sheet { (font(SYSTEM_FONT, 13.0), [0.56, 0.56, 0.58, 1.0]) } else { (font(BOLD_FONT, 17.0), [0.0, 0.0, 0.0, 1.0]) };
    y += add_alert_label(panel, &title, title_font, title_color, y, inner);
    if !title.is_empty() && !message.is_empty() {
        y += 4.0;
    }
    y += add_alert_label(panel, &message, font(SYSTEM_FONT, 13.0), [0.0, 0.0, 0.0, 1.0], y, inner);
    let fields = collections::array_items(get_id(this, "textFields"));
    for (index, &field) in fields.iter().enumerate() {
        y += if index == 0 { 14.0 } else { 0.0 };
        let frame = Rect::new(ALERT_MARGIN, y, inner, ALERT_FIELD_HEIGHT);
        // A thin grey edge around the field.
        add_view(panel, "UIView", Rect::new(frame.origin.x - 0.5, y - 0.5, inner + 1.0, ALERT_FIELD_HEIGHT + 1.0), Some(LINE));
        set_frame(field, frame);
        runtime::send(field, "setBackgroundColor:", &[colored([1.0, 1.0, 1.0, 1.0])]);
        runtime::send(field, "setBorderStyle:", &[3]);
        if get_id(field, "font") == 0 {
            runtime::send(field, "setFont:", &[font(SYSTEM_FONT, 13.0)]);
        }
        runtime::send(panel, "addSubview:", &[field]);
        let action = |name: &str| runtime::sel(name);
        runtime::send(field, "addTarget:action:forControlEvents:", &[this, action("gamebridgeAlertTextChanged:"), 1 << 17]);
        runtime::send(field, "addTarget:action:forControlEvents:", &[this, action("gamebridgeAlertReturn:"), 1 << 19]);
        y += ALERT_FIELD_HEIGHT;
    }
    if y > 0.0 {
        y += 20.0;
    }

    let buttons = with_buttons(this, |b| b.clone());
    let side_by_side = !sheet && buttons.titles.len() == 2;
    let mut made = Vec::new();
    for (index, title) in buttons.titles.iter().enumerate() {
        let frame = if side_by_side {
            Rect::new(index as f64 * width / 2.0, y, width / 2.0, ALERT_BUTTON_HEIGHT)
        } else {
            Rect::new(0.0, y + index as f64 * ALERT_BUTTON_HEIGHT, width, ALERT_BUTTON_HEIGHT)
        };
        add_view(panel, "UIView", Rect::new(frame.origin.x, frame.origin.y, frame.size.width, 0.5), Some(LINE));
        if side_by_side && index == 1 {
            add_view(panel, "UIView", Rect::new(frame.origin.x, frame.origin.y, 0.5, frame.size.height), Some(LINE));
        }
        let button = runtime::send(runtime::class_named("UIButton"), "buttonWithType:", &[0]);
        set_frame(button, frame);
        runtime::send(button, "setTitle:forState:", &[string::make(title), 0]);
        let index = index as isize;
        let color = if index == buttons.destructive { DESTRUCTIVE } else { TINT };
        runtime::send(button, "setTitleColor:forState:", &[colored(color), 0]);
        runtime::send(button, "setTitleColor:forState:", &[colored(DISABLED_TITLE), 2]);
        let bold = index == buttons.cancel && !sheet;
        runtime::send(runtime::send(button, "titleLabel", &[]), "setFont:", &[font(if bold { BOLD_FONT } else { SYSTEM_FONT }, 17.0)]);
        runtime::send(button, "setTag:", &[index as usize]);
        runtime::send(button, "addTarget:action:forControlEvents:", &[this, runtime::sel("gamebridgeAlertButton:"), TOUCH_UP_INSIDE]);
        runtime::send(panel, "addSubview:", &[button]);
        made.push(button);
    }
    let rows = if side_by_side { 1 } else { made.len() };
    let height = y + rows as f64 * ALERT_BUTTON_HEIGHT;
    let top = if sheet { area.height - height - 8.0 } else { ((area.height - height) / 2.0).max(0.0) };
    set_frame(panel, Rect::new(((area.width - width) / 2.0).round(), top.round(), width, height));
    property_set(this, "shownButtons", collections::array_from(&made) as u64);
    runtime::send(window, "addSubview:", &[this]);
    if !sheet {
        alert_update_first_other(this);
    }
    if let Some(&first) = fields.first() {
        runtime::send(first, "becomeFirstResponder", &[]);
    }
    true
}

/// The delegate's alertViewShouldEnableFirstOtherButton: turns that button on or off.
fn alert_update_first_other(this: Id) {
    let first = with_buttons(this, |b| b.first_other);
    let Some(&button) = collections::array_items(get_id(this, "shownButtons")).get(first.max(0) as usize).filter(|_| first >= 0) else { return };
    let enable = tell(get_id(this, "delegate"), "alertViewShouldEnableFirstOtherButton:", &[this]).map_or(true, |answer| answer & 0xFF != 0);
    runtime::send(button, "setEnabled:", &[enable as usize]);
}

/// A shown button was tapped: the delegate hears which, and the alert goes away.
extern "C" fn alert_button_tapped(this: Id, _: Sel, button: Id) {
    let index = runtime::send(button, "tag", &[]) as isize;
    let clicked = if class_is(this, "UIActionSheet") { "actionSheet:clickedButtonAtIndex:" } else { "alertView:clickedButtonAtIndex:" };
    tell(get_id(this, "delegate"), clicked, &[this, index as usize]);
    alert_dismiss(this, 0, index, 1);
}

extern "C" fn alert_text_changed(this: Id, _: Sel, _field: Id) {
    alert_update_first_other(this);
}

/// Return in an alert's text field taps the first other button, if it is on.
extern "C" fn alert_return(this: Id, s: Sel, _field: Id) {
    let first = with_buttons(this, |b| b.first_other);
    let shown = collections::array_items(get_id(this, "shownButtons"));
    if let Some(&button) = shown.get(first.max(0) as usize).filter(|_| first >= 0) {
        if runtime::send(button, "isEnabled", &[]) & 0xFF != 0 {
            alert_button_tapped(this, s, button);
        }
    }
}

extern "C" fn sheet_show_in(this: Id, _: Sel, _view: Id) {
    sheet_show(this);
}
extern "C" fn sheet_show_from_rect(this: Id, _: Sel, _rect: Rect, _view: Id, _animated: u8) {
    sheet_show(this);
}
extern "C" fn sheet_show_from_item(this: Id, _: Sel, _item: Id, _animated: u8) {
    sheet_show(this);
}

// ----- popovers -----

extern "C" fn popover_init(this: Id, _: Sel, content: Id) -> Id {
    property_set(this, "contentViewController", content as u64);
    this
}

property!(popover_content, popover_set_content, "contentViewController", id, 0);
property!(popover_delegate, popover_set_delegate, "delegate", id, 0);
property!(popover_visible, popover_set_visible_unused, "visible", bool, false);
property!(popover_passthrough, popover_set_passthrough, "passthroughViews", id, 0);
property!(popover_arrow, popover_set_arrow_unused, "arrowDirection", isize, -1);
pair_property!(popover_size, popover_set_size, Size, width, height, "size.w", "size.h");

extern "C" fn popover_set_content_animated(this: Id, _: Sel, content: Id, _animated: u8) {
    property_set(this, "contentViewController", content as u64);
}
extern "C" fn popover_set_size_animated(this: Id, s: Sel, size: Size, _animated: u8) {
    popover_set_size(this, s, size);
}

/// The popover is "visible" but not drawn; what it holds answers as its
/// screen would when shown (a photo picker says the user cancelled).
fn popover_present(this: Id) {
    let content = get_id(this, "contentViewController");
    crate::log!("popover shows {} (not drawn yet)", string::describe_class_of(content));
    property_set(this, "visible", 1);
    if content != 0 && runtime::responds(content, "gamebridgeAnswerShown") {
        later(move || {
            runtime::send(content, "gamebridgeAnswerShown", &[]);
        });
    }
}

extern "C" fn popover_present_from_rect(this: Id, _: Sel, _rect: Rect, _view: Id, _arrows: usize, _animated: u8) {
    popover_present(this);
}
extern "C" fn popover_present_from_item(this: Id, _: Sel, _item: Id, _arrows: usize, _animated: u8) {
    popover_present(this);
}
extern "C" fn popover_dismiss(this: Id, _: Sel, _animated: u8) {
    property_set(this, "visible", 0);
}

// ----- the photo picker: no camera or photo library here -----

property!(picker_source, picker_set_source_raw, "sourceType", isize, 0);
property!(picker_editing, picker_set_editing, "allowsEditing", bool, false);
property!(picker_media_types, picker_set_media_types, "mediaTypes", id, 0);
property!(picker_camera_device, picker_set_camera_device, "cameraDevice", isize, 0);
property!(picker_shows_controls, picker_set_shows_controls, "showsCameraControls", bool, true);
property!(picker_overlay, picker_set_overlay, "cameraOverlayView", id, 0);
property!(picker_quality, picker_set_quality, "videoQuality", isize, 1);
property!(picker_duration, picker_set_duration, "videoMaximumDuration", f64, 600.0);

extern "C" fn picker_set_source(this: Id, s: Sel, source: isize) {
    crate::log!("photo picker: source {source} isn't available here");
    picker_set_source_raw(this, s, source);
}

/// Shown, the picker answers as when the user taps Cancel.
extern "C" fn picker_answer_shown(this: Id, _: Sel) {
    crate::log!("photo picker shown: no photos or camera here, telling the game the user cancelled");
    tell(get_id(this, "delegate"), "imagePickerControllerDidCancel:", &[this]);
}

extern "C" fn picker_start_capture(_this: Id, _: Sel) -> bool {
    false
}

// ----- notification settings -----

extern "C" fn settings_for_types(class: Class, _: Sel, types: usize, categories: Id) -> Id {
    let object = runtime::alloc_instance(class);
    property_set(object, "types", types as u64);
    property_set(object, "categories", categories as u64);
    object
}

extern "C" fn settings_types(this: Id, _: Sel) -> usize {
    property_get(this, "types").unwrap_or(0) as usize
}

extern "C" fn settings_categories(this: Id, _: Sel) -> Id {
    get_id(this, "categories")
}

fn no_notification_settings() -> Id {
    settings_for_types(runtime::class_named("UIUserNotificationSettings"), 0, 0, 0)
}

/// Notifications can't be shown here: the app hears it may use none of the
/// types it asked for, as when the user doesn't allow them.
pub extern "C" fn app_register_user_notification_settings(this: Id, _: Sel, settings: Id) {
    crate::log!(
        "notification permission asked for (types {}); answering 'none allowed'",
        if settings != 0 { settings_types(settings, 0) } else { 0 }
    );
    later(move || {
        let delegate = runtime::send(this, "delegate", &[]);
        tell(delegate, "application:didRegisterUserNotificationSettings:", &[this, no_notification_settings()]);
    });
}

pub extern "C" fn app_current_notification_settings(_this: Id, _: Sel) -> Id {
    no_notification_settings()
}

// ----- accessibility (NSObject's informal protocol) -----

property!(access_element, access_set_element, "isAccessibilityElement", bool, false);
property!(access_traits, access_set_traits, "accessibilityTraits", isize, 0);
property!(access_label, access_set_label, "accessibilityLabel", id, 0);
property!(access_hint, access_set_hint, "accessibilityHint", id, 0);
property!(access_value, access_set_value, "accessibilityValue", id, 0);

// ----- registration -----

fn view_class(name: &str, parent: &str) -> Class {
    define_class(name, parent, 16)
}

pub fn register(r: &mut Registry) {
    let object = runtime::class_named("NSObject");
    methods(
        object,
        &[
            ("isAccessibilityElement", access_element as usize),
            ("setIsAccessibilityElement:", access_set_element as usize),
            ("accessibilityTraits", access_traits as usize),
            ("setAccessibilityTraits:", access_set_traits as usize),
            ("accessibilityLabel", access_label as usize),
            ("setAccessibilityLabel:", access_set_label as usize),
            ("accessibilityHint", access_hint as usize),
            ("setAccessibilityHint:", access_set_hint as usize),
            ("accessibilityValue", access_value as usize),
            ("setAccessibilityValue:", access_set_value as usize),
        ],
    );
    // UIAccessibilityTraits values from UIAccessibilityConstants.h.
    for (name, value) in [
        ("UIAccessibilityTraitNone", 0u64),
        ("UIAccessibilityTraitButton", 1),
        ("UIAccessibilityTraitLink", 1 << 1),
        ("UIAccessibilityTraitImage", 1 << 2),
        ("UIAccessibilityTraitSelected", 1 << 3),
        ("UIAccessibilityTraitPlaysSound", 1 << 4),
        ("UIAccessibilityTraitKeyboardKey", 1 << 5),
        ("UIAccessibilityTraitStaticText", 1 << 6),
        ("UIAccessibilityTraitSummaryElement", 1 << 7),
        ("UIAccessibilityTraitNotEnabled", 1 << 8),
        ("UIAccessibilityTraitUpdatesFrequently", 1 << 9),
        ("UIAccessibilityTraitSearchField", 1 << 10),
        ("UIAccessibilityTraitStartsMediaSession", 1 << 11),
        ("UIAccessibilityTraitAdjustable", 1 << 12),
        ("UIAccessibilityTraitAllowsDirectInteraction", 1 << 13),
        ("UIAccessibilityTraitCausesPageTurn", 1 << 14),
        ("UIAccessibilityTraitHeader", 1 << 16),
    ] {
        r.data(name, leak(value));
    }

    let font_class = define_class("UIFont", "NSObject", 8);
    class_methods(
        font_class,
        &[
            ("systemFontOfSize:", font_system as usize),
            ("boldSystemFontOfSize:", font_bold as usize),
            ("italicSystemFontOfSize:", font_italic as usize),
            ("fontWithName:size:", font_named as usize),
            ("systemFontSize", font_system_size as usize),
            ("labelFontSize", font_label_size as usize),
            ("buttonFontSize", font_button_size as usize),
            ("smallSystemFontSize", font_small_size as usize),
            ("familyNames", font_family_names as usize),
            ("fontNamesForFamilyName:", font_names_for_family as usize),
        ],
    );
    methods(
        font_class,
        &[
            ("fontWithSize:", font_with_size as usize),
            ("pointSize", font_point_size as usize),
            ("fontName", font_name as usize),
            ("familyName", font_family as usize),
            ("lineHeight", font_line_height as usize),
            ("leading", font_leading as usize),
            ("ascender", font_ascender as usize),
            ("descender", font_descender as usize),
            ("capHeight", font_cap_height as usize),
            ("xHeight", font_x_height as usize),
        ],
    );
    let text_common: &[(&str, usize)] = &[
        ("text", text_get as usize),
        ("setText:", text_set as usize),
        ("setFont:", text_font_set as usize),
        ("textColor", text_color as usize),
        ("setTextColor:", text_set_color as usize),
        ("textAlignment", text_alignment as usize),
        ("setTextAlignment:", text_set_alignment as usize),
        ("attributedText", text_attributed as usize),
        ("setAttributedText:", text_set_attributed as usize),
    ];

    let label = view_class("UILabel", "UIView");
    methods(label, text_common);
    methods(
        label,
        &[
            ("initWithFrame:", label_init_with_frame as usize),
            ("font", text_font as usize),
            ("numberOfLines", text_lines as usize),
            ("setNumberOfLines:", text_set_lines as usize),
            ("lineBreakMode", text_line_break as usize),
            ("setLineBreakMode:", text_set_line_break as usize),
            ("shadowColor", text_shadow_color as usize),
            ("setShadowColor:", text_set_shadow_color as usize),
            ("shadowOffset", label_shadow_offset as usize),
            ("setShadowOffset:", label_set_shadow_offset as usize),
            ("highlightedTextColor", text_highlighted_color as usize),
            ("setHighlightedTextColor:", text_set_highlighted_color as usize),
            ("isHighlighted", text_highlighted as usize),
            ("setHighlighted:", text_set_highlighted as usize),
            ("isEnabled", text_enabled as usize),
            ("setEnabled:", text_set_enabled as usize),
            ("adjustsFontSizeToFitWidth", text_adjusts as usize),
            ("setAdjustsFontSizeToFitWidth:", text_set_adjusts as usize),
            ("minimumFontSize", text_minimum_font as usize),
            ("setMinimumFontSize:", text_set_minimum_font as usize),
            ("minimumScaleFactor", text_minimum_font as usize),
            ("setMinimumScaleFactor:", text_set_minimum_font as usize),
            ("setBaselineAdjustment:", ignore_int as usize),
            ("setPreferredMaxLayoutWidth:", ignore_double as usize),
            ("sizeThatFits:", label_size_that_fits as usize),
            ("sizeToFit", label_size_to_fit as usize),
            ("textRectForBounds:limitedToNumberOfLines:", label_text_rect as usize),
        ],
    );

    let control = view_class("UIControl", "UIView");
    methods(
        control,
        &[
            ("initWithFrame:", control_init_with_frame as usize),
            ("addTarget:action:forControlEvents:", control_add_target as usize),
            ("removeTarget:action:forControlEvents:", control_remove_target as usize),
            ("allTargets", control_all_targets as usize),
            ("allControlEvents", control_all_events as usize),
            ("sendActionsForControlEvents:", control_send_actions as usize),
            ("sendAction:to:forEvent:", control_send_action as usize),
            ("isEnabled", text_enabled as usize),
            ("setEnabled:", text_set_enabled as usize),
            ("isSelected", control_selected as usize),
            ("setSelected:", control_set_selected as usize),
            ("isHighlighted", text_highlighted as usize),
            ("setHighlighted:", text_set_highlighted as usize),
            ("state", control_state_method as usize),
            ("isTracking", control_tracking as usize),
            ("isTouchInside", control_touch_inside as usize),
            ("touchesBegan:withEvent:", control_touches_began as usize),
            ("touchesMoved:withEvent:", control_touches_moved as usize),
            ("touchesEnded:withEvent:", control_touches_ended as usize),
            ("touchesCancelled:withEvent:", control_touches_cancelled as usize),
            ("beginTrackingWithTouch:withEvent:", control_begin_tracking as usize),
            ("continueTrackingWithTouch:withEvent:", control_begin_tracking as usize),
            ("endTrackingWithTouch:withEvent:", ignore_id_id as usize),
            ("cancelTrackingWithEvent:", ignore_id as usize),
            ("contentHorizontalAlignment", control_horizontal as usize),
            ("setContentHorizontalAlignment:", control_set_horizontal as usize),
            ("contentVerticalAlignment", control_vertical as usize),
            ("setContentVerticalAlignment:", control_set_vertical as usize),
        ],
    );

    let button = view_class("UIButton", "UIControl");
    class_methods(button, &[("buttonWithType:", button_with_type as usize)]);
    methods(
        button,
        &[
            ("buttonType", button_type as usize),
            ("titleLabel", button_title_label as usize),
            ("imageView", button_image_view as usize),
            ("setTitle:forState:", button_set_title as usize),
            ("titleForState:", button_title_for_state as usize),
            ("currentTitle", button_current_title as usize),
            ("setTitleColor:forState:", button_set_title_color as usize),
            ("titleColorForState:", button_title_color as usize),
            ("currentTitleColor", button_current_title_color as usize),
            ("setTitleShadowColor:forState:", button_set_title_shadow as usize),
            ("titleShadowColorForState:", button_title_shadow as usize),
            ("currentTitleShadowColor", button_current_title_shadow as usize),
            ("setImage:forState:", button_set_image as usize),
            ("imageForState:", button_image as usize),
            ("currentImage", button_current_image as usize),
            ("setBackgroundImage:forState:", button_set_background_image as usize),
            ("backgroundImageForState:", button_background_image as usize),
            ("currentBackgroundImage", button_current_background_image as usize),
            ("setAttributedTitle:forState:", button_set_attributed_title as usize),
            ("attributedTitleForState:", button_attributed_title as usize),
            ("currentAttributedTitle", button_current_attributed_title as usize),
            ("contentEdgeInsets", button_content_insets as usize),
            ("setContentEdgeInsets:", button_set_content_insets as usize),
            ("titleEdgeInsets", button_title_insets as usize),
            ("setTitleEdgeInsets:", button_set_title_insets as usize),
            ("imageEdgeInsets", button_image_insets as usize),
            ("setImageEdgeInsets:", button_set_image_insets as usize),
            ("adjustsImageWhenHighlighted", button_adjusts_highlighted as usize),
            ("setAdjustsImageWhenHighlighted:", button_set_adjusts_highlighted as usize),
            ("showsTouchWhenHighlighted", button_shows_touch as usize),
            ("setShowsTouchWhenHighlighted:", button_set_shows_touch as usize),
            ("adjustsImageWhenDisabled", button_adjusts_disabled as usize),
            ("setAdjustsImageWhenDisabled:", button_set_adjusts_disabled as usize),
            ("setReversesTitleShadowWhenHighlighted:", ignore_bool as usize),
            ("sizeThatFits:", button_size_that_fits as usize),
            ("sizeToFit", button_size_to_fit as usize),
        ],
    );
    let _ = button_set_type_unused;

    let keyboard_text: &[(&str, usize)] = &[
        ("delegate", text_delegate as usize),
        ("setDelegate:", text_set_delegate as usize),
        ("keyboardType", text_keyboard_type as usize),
        ("setKeyboardType:", text_set_keyboard_type as usize),
        ("returnKeyType", text_return_key as usize),
        ("setReturnKeyType:", text_set_return_key as usize),
        ("keyboardAppearance", text_appearance as usize),
        ("setKeyboardAppearance:", text_set_appearance as usize),
        ("autocorrectionType", text_autocorrection as usize),
        ("setAutocorrectionType:", text_set_autocorrection as usize),
        ("autocapitalizationType", text_autocapitalization as usize),
        ("setAutocapitalizationType:", text_set_autocapitalization as usize),
        ("spellCheckingType", text_spell_checking as usize),
        ("setSpellCheckingType:", text_set_spell_checking as usize),
        ("isSecureTextEntry", text_secure as usize),
        ("setSecureTextEntry:", text_set_secure as usize),
        ("enablesReturnKeyAutomatically", text_auto_return as usize),
        ("setEnablesReturnKeyAutomatically:", text_set_auto_return as usize),
        ("inputAccessoryView", text_accessory as usize),
        ("setInputAccessoryView:", text_set_accessory as usize),
        ("inputView", text_input_view as usize),
        ("setInputView:", text_set_input_view as usize),
        // Editing (becomeFirstResponder and the rest) is in fw/typing.rs.
        ("reloadInputViews", nothing as usize),
        ("selectedTextRange", zero as usize),
        ("setSelectedTextRange:", ignore_id as usize),
        ("beginningOfDocument", zero as usize),
        ("endOfDocument", zero as usize),
        ("textRangeFromPosition:toPosition:", nil_with_id_id as usize),
        ("positionFromPosition:offset:", zero as usize),
    ];

    let field = view_class("UITextField", "UIControl");
    methods(field, text_common);
    methods(field, keyboard_text);
    methods(
        field,
        &[
            ("initWithFrame:", text_field_init_with_frame as usize),
            ("text", text_or_empty as usize),
            ("font", text_field_font as usize),
            ("placeholder", text_placeholder as usize),
            ("setPlaceholder:", text_set_placeholder as usize),
            ("borderStyle", text_border as usize),
            ("setBorderStyle:", text_set_border as usize),
            ("clearButtonMode", text_clear_mode as usize),
            ("setClearButtonMode:", text_set_clear_mode as usize),
            ("clearsOnBeginEditing", text_clears_on_begin as usize),
            ("setClearsOnBeginEditing:", text_set_clears_on_begin as usize),
            ("adjustsFontSizeToFitWidth", text_adjusts as usize),
            ("setAdjustsFontSizeToFitWidth:", text_set_adjusts as usize),
            ("minimumFontSize", text_minimum_font as usize),
            ("setMinimumFontSize:", text_set_minimum_font as usize),
            ("setLeftView:", ignore_id as usize),
            ("setRightView:", ignore_id as usize),
            ("setLeftViewMode:", ignore_int as usize),
            ("setRightViewMode:", ignore_int as usize),
            ("setBackground:", ignore_id as usize),
        ],
    );

    let scroll = view_class("UIScrollView", "UIView");
    methods(
        scroll,
        &[
            ("initWithFrame:", scroll_init_with_frame as usize),
            ("contentSize", scroll_content_size as usize),
            ("setContentSize:", scroll_set_content_size as usize),
            ("contentOffset", scroll_content_offset as usize),
            ("setContentOffset:", scroll_set_content_offset as usize),
            ("setContentOffset:animated:", scroll_set_content_offset_animated as usize),
            ("contentInset", scroll_content_inset as usize),
            ("setContentInset:", scroll_set_content_inset as usize),
            ("scrollIndicatorInsets", scroll_indicator_insets as usize),
            ("setScrollIndicatorInsets:", scroll_set_indicator_insets as usize),
            ("scrollRectToVisible:animated:", scroll_rect_visible as usize),
            ("delegate", scroll_delegate as usize),
            ("setDelegate:", scroll_set_delegate as usize),
            ("isScrollEnabled", scroll_enabled as usize),
            ("setScrollEnabled:", scroll_set_enabled as usize),
            ("isPagingEnabled", scroll_paging as usize),
            ("setPagingEnabled:", scroll_set_paging as usize),
            ("bounces", scroll_bounces as usize),
            ("setBounces:", scroll_set_bounces as usize),
            ("alwaysBounceVertical", scroll_always_vertical as usize),
            ("setAlwaysBounceVertical:", scroll_set_always_vertical as usize),
            ("alwaysBounceHorizontal", scroll_always_horizontal as usize),
            ("setAlwaysBounceHorizontal:", scroll_set_always_horizontal as usize),
            ("showsVerticalScrollIndicator", scroll_shows_vertical as usize),
            ("setShowsVerticalScrollIndicator:", scroll_set_shows_vertical as usize),
            ("showsHorizontalScrollIndicator", scroll_shows_horizontal as usize),
            ("setShowsHorizontalScrollIndicator:", scroll_set_shows_horizontal as usize),
            ("zoomScale", scroll_zoom as usize),
            ("setZoomScale:", scroll_set_zoom as usize),
            ("minimumZoomScale", scroll_min_zoom as usize),
            ("setMinimumZoomScale:", scroll_set_min_zoom as usize),
            ("maximumZoomScale", scroll_max_zoom as usize),
            ("setMaximumZoomScale:", scroll_set_max_zoom as usize),
            ("isDragging", no as usize),
            ("isTracking", no as usize),
            ("isDecelerating", no as usize),
            ("setScrollsToTop:", ignore_bool as usize),
            ("setDelaysContentTouches:", ignore_bool as usize),
            ("setCanCancelContentTouches:", ignore_bool as usize),
            ("setDecelerationRate:", ignore_double as usize),
            ("setIndicatorStyle:", ignore_int as usize),
            ("flashScrollIndicators", nothing as usize),
        ],
    );

    let text_view = view_class("UITextView", "UIScrollView");
    methods(text_view, text_common);
    methods(text_view, keyboard_text);
    methods(
        text_view,
        &[
            ("initWithFrame:", text_view_init_with_frame as usize),
            ("text", text_or_empty as usize),
            ("font", text_field_font as usize),
            ("isEditable", text_editable as usize),
            ("setEditable:", text_set_editable as usize),
            ("isSelectable", text_selectable as usize),
            ("setSelectable:", text_set_selectable as usize),
            ("selectedRange", text_selected_range as usize),
            ("setSelectedRange:", text_set_selected_range as usize),
            ("scrollRangeToVisible:", text_scroll_range as usize),
            ("setDataDetectorTypes:", ignore_int as usize),
            ("setTextContainerInset:", ignore_insets as usize),
        ],
    );

    let web = view_class("UIWebView", "UIView");
    methods(
        web,
        &[
            ("initWithFrame:", web_view_init_with_frame as usize),
            ("delegate", web_delegate as usize),
            ("setDelegate:", web_set_delegate as usize),
            ("request", web_request as usize),
            ("loadRequest:", web_load_request as usize),
            ("loadHTMLString:baseURL:", web_load_html as usize),
            ("loadData:MIMEType:textEncodingName:baseURL:", web_load_data as usize),
            ("stringByEvaluatingJavaScriptFromString:", empty_string_with_id as usize),
            ("scalesPageToFit", web_scales as usize),
            ("setScalesPageToFit:", web_set_scales as usize),
            ("scrollView", web_scroll_view as usize),
            ("isLoading", no as usize),
            ("canGoBack", no as usize),
            ("canGoForward", no as usize),
            ("goBack", nothing as usize),
            ("goForward", nothing as usize),
            ("reload", nothing as usize),
            ("stopLoading", nothing as usize),
            ("setDataDetectorTypes:", ignore_int as usize),
            ("setAllowsInlineMediaPlayback:", ignore_bool as usize),
            ("setMediaPlaybackRequiresUserAction:", ignore_bool as usize),
            ("setSuppressesIncrementalRendering:", ignore_bool as usize),
            ("setKeyboardDisplayRequiresUserAction:", ignore_bool as usize),
        ],
    );
    let _ = web_set_request_unused;

    let bar_common: &[(&str, usize)] = &[
        ("initWithFrame:", bar_init_with_frame as usize),
        ("items", bar_items as usize),
        ("setItems:", bar_set_items as usize),
        ("setItems:animated:", bar_set_items_animated as usize),
        ("barStyle", bar_style as usize),
        ("setBarStyle:", bar_set_style as usize),
        ("tintColor", bar_tint as usize),
        ("setTintColor:", bar_set_tint as usize),
        ("barTintColor", bar_bar_tint as usize),
        ("setBarTintColor:", bar_set_bar_tint as usize),
        ("isTranslucent", bar_translucent as usize),
        ("setTranslucent:", bar_set_translucent as usize),
        ("delegate", bar_delegate as usize),
        ("setDelegate:", bar_set_delegate as usize),
        ("sizeThatFits:", bar_size_that_fits as usize),
        ("setShadowImage:", ignore_id as usize),
    ];
    let toolbar = view_class("UIToolbar", "UIView");
    methods(toolbar, bar_common);
    methods(
        toolbar,
        &[
            ("setBackgroundImage:forToolbarPosition:barMetrics:", bar_ignore_image_position_metrics as usize),
            ("setShadowImage:forToolbarPosition:", ignore_id_int as usize),
        ],
    );
    let nav_bar_class = view_class("UINavigationBar", "UIView");
    methods(nav_bar_class, bar_common);
    methods(
        nav_bar_class,
        &[
            ("topItem", bar_top_item as usize),
            ("titleTextAttributes", bar_title_attributes as usize),
            ("setTitleTextAttributes:", bar_set_title_attributes as usize),
            ("setBackgroundImage:forBarMetrics:", bar_ignore_image_metrics as usize),
            ("pushNavigationItem:animated:", ignore_id_bool as usize),
        ],
    );

    let bar_item = define_class("UIBarItem", "NSObject", 8);
    methods(
        bar_item,
        &[
            ("init", item_init as usize),
            ("title", item_title as usize),
            ("setTitle:", item_set_title as usize),
            ("image", item_image as usize),
            ("setImage:", item_set_image as usize),
            ("isEnabled", item_enabled as usize),
            ("setEnabled:", item_set_enabled as usize),
            ("tag", view_tag_like as usize),
            ("setTag:", view_set_tag_like as usize),
            ("setTitleTextAttributes:forState:", ignore_id_int as usize),
        ],
    );
    let button_item = define_class("UIBarButtonItem", "UIBarItem", 8);
    methods(
        button_item,
        &[
            ("initWithTitle:style:target:action:", item_init_title as usize),
            ("initWithImage:style:target:action:", item_init_image as usize),
            ("initWithBarButtonSystemItem:target:action:", item_init_system as usize),
            ("initWithCustomView:", item_init_custom as usize),
            ("style", item_style as usize),
            ("setStyle:", item_set_style as usize),
            ("target", item_target as usize),
            ("setTarget:", item_set_target as usize),
            ("action", item_action as usize),
            ("setAction:", item_set_action as usize),
            ("width", item_width as usize),
            ("setWidth:", item_set_width as usize),
            ("customView", item_custom_view as usize),
            ("setCustomView:", item_set_custom_view as usize),
            ("tintColor", item_tint as usize),
            ("setTintColor:", item_set_tint as usize),
            ("possibleTitles", item_possible_titles as usize),
            ("setPossibleTitles:", item_set_possible_titles as usize),
            ("setBackgroundImage:forState:barMetrics:", ignore_id_int_int as usize),
        ],
    );
    let _ = item_set_action_unused;

    let nav_item = define_class("UINavigationItem", "NSObject", 8);
    methods(
        nav_item,
        &[
            ("initWithTitle:", navigation_item_init_title as usize),
            ("title", item_title as usize),
            ("setTitle:", item_set_title as usize),
            ("rightBarButtonItem", nav_item_right as usize),
            ("setRightBarButtonItem:", nav_item_set_right as usize),
            ("setRightBarButtonItem:animated:", nav_item_set_right_animated as usize),
            ("leftBarButtonItem", nav_item_left as usize),
            ("setLeftBarButtonItem:", nav_item_set_left as usize),
            ("setLeftBarButtonItem:animated:", nav_item_set_left_animated as usize),
            ("rightBarButtonItems", nav_item_right_items as usize),
            ("setRightBarButtonItems:", nav_item_set_right_items as usize),
            ("leftBarButtonItems", nav_item_left_items as usize),
            ("setLeftBarButtonItems:", nav_item_set_left_items as usize),
            ("backBarButtonItem", nav_item_back as usize),
            ("setBackBarButtonItem:", nav_item_set_back as usize),
            ("hidesBackButton", nav_item_hides_back as usize),
            ("setHidesBackButton:", nav_item_set_hides_back as usize),
            ("setHidesBackButton:animated:", nav_item_set_hides_back_animated as usize),
            ("titleView", nav_item_title_view as usize),
            ("setTitleView:", nav_item_set_title_view as usize),
            ("prompt", nav_item_prompt as usize),
            ("setPrompt:", nav_item_set_prompt as usize),
        ],
    );

    let controller = runtime::class_named("UIViewController");
    methods(
        controller,
        &[
            ("navigationItem", controller_navigation_item as usize),
            ("title", controller_title as usize),
            ("setTitle:", controller_set_title as usize),
            ("toolbarItems", controller_toolbar_items as usize),
            ("setToolbarItems:", controller_set_toolbar_items as usize),
            ("setToolbarItems:animated:", controller_set_toolbar_items_animated as usize),
            ("hidesBottomBarWhenPushed", controller_hides_bottom_bar as usize),
            ("setHidesBottomBarWhenPushed:", controller_set_hides_bottom_bar as usize),
            ("modalPresentationStyle", controller_modal_style as usize),
            ("setModalPresentationStyle:", controller_set_modal_style as usize),
            ("contentSizeForViewInPopover", controller_popover_size as usize),
            ("setContentSizeForViewInPopover:", controller_set_popover_size as usize),
            ("preferredContentSize", controller_popover_size as usize),
            ("setPreferredContentSize:", controller_set_popover_size as usize),
            ("isBeingPresented", no as usize),
            ("isBeingDismissed", no as usize),
            ("isMovingToParentViewController", no as usize),
            ("isMovingFromParentViewController", no as usize),
        ],
    );

    let nav = define_class("UINavigationController", "UIViewController", 8);
    methods(
        nav,
        &[
            ("init", nav_init as usize),
            ("gamebridgeShown", nav_shown as usize),
            ("initWithRootViewController:", nav_init_with_root as usize),
            ("initWithNavigationBarClass:toolbarClass:", nav_init_with_classes as usize),
            ("viewControllers", nav_view_controllers as usize),
            ("setViewControllers:", nav_set_view_controllers as usize),
            ("setViewControllers:animated:", nav_set_view_controllers_animated as usize),
            ("pushViewController:animated:", nav_push as usize),
            ("popViewControllerAnimated:", nav_pop as usize),
            ("popToRootViewControllerAnimated:", nav_pop_to_root as usize),
            ("popToViewController:animated:", nav_pop_to_controller as usize),
            ("topViewController", nav_top as usize),
            ("visibleViewController", nav_visible as usize),
            ("navigationBar", nav_bar as usize),
            ("toolbar", nav_toolbar as usize),
            ("isNavigationBarHidden", nav_bar_hidden as usize),
            ("setNavigationBarHidden:", nav_set_bar_hidden as usize),
            ("setNavigationBarHidden:animated:", nav_set_bar_hidden_animated as usize),
            ("isToolbarHidden", nav_toolbar_hidden as usize),
            ("setToolbarHidden:", nav_set_toolbar_hidden as usize),
            ("setToolbarHidden:animated:", nav_set_toolbar_hidden_animated as usize),
            ("delegate", nav_delegate as usize),
            ("setDelegate:", nav_set_delegate as usize),
        ],
    );

    let table = view_class("UITableView", "UIScrollView");
    methods(
        table,
        &[
            ("initWithFrame:", table_init_with_frame as usize),
            ("initWithFrame:style:", table_init_with_style as usize),
            ("style", table_style as usize),
            ("dataSource", table_data_source as usize),
            ("tableHeaderView", table_header as usize),
            ("setTableHeaderView:", table_set_header as usize),
            ("tableFooterView", table_footer as usize),
            ("setTableFooterView:", table_set_footer as usize),
            ("rowHeight", table_row_height as usize),
            ("setRowHeight:", table_set_row_height as usize),
            ("sectionHeaderHeight", table_section_header_height as usize),
            ("setSectionHeaderHeight:", table_set_section_header_height as usize),
            ("sectionFooterHeight", table_section_footer_height as usize),
            ("setSectionFooterHeight:", table_set_section_footer_height as usize),
            ("separatorStyle", table_separator_style as usize),
            ("setSeparatorStyle:", table_set_separator_style as usize),
            ("separatorColor", table_separator_color as usize),
            ("setSeparatorColor:", table_set_separator_color as usize),
            ("allowsSelection", table_allows_selection as usize),
            ("setAllowsSelection:", table_set_allows_selection as usize),
            ("allowsMultipleSelection", table_allows_multiple as usize),
            ("setAllowsMultipleSelection:", table_set_allows_multiple as usize),
            ("backgroundView", table_background_view as usize),
            ("setBackgroundView:", table_set_background_view as usize),
            ("isEditing", table_editing as usize),
            ("setEditing:", table_set_editing as usize),
            ("setEditing:animated:", table_set_editing_animated as usize),
            ("registerClass:forCellReuseIdentifier:", table_register_class as usize),
            ("reloadRowsAtIndexPaths:withRowAnimation:", ignore_id_int as usize),
            ("reloadSections:withRowAnimation:", ignore_id_int as usize),
            ("insertRowsAtIndexPaths:withRowAnimation:", ignore_id_int as usize),
            ("deleteRowsAtIndexPaths:withRowAnimation:", ignore_id_int as usize),
            ("insertSections:withRowAnimation:", ignore_id_int as usize),
            ("deleteSections:withRowAnimation:", ignore_id_int as usize),
            ("beginUpdates", nothing as usize),
            ("endUpdates", nothing as usize),
            ("numberOfSections", table_number_of_sections as usize),
            ("numberOfRowsInSection:", table_rows_in_section as usize),
            ("indexPathForSelectedRow", table_selected_row as usize),
            ("indexPathsForSelectedRows", table_selected_rows as usize),
            ("scrollToRowAtIndexPath:atScrollPosition:animated:", table_scroll_to as usize),
        ],
    );
    // Rows, cells, selection and taps are in fw/tables.rs.
    let _ = (table_set_style_unused, table_set_data_source);

    let table_controller = define_class("UITableViewController", "UIViewController", 8);
    methods(
        table_controller,
        &[
            ("init", table_controller_init as usize),
            ("initWithStyle:", table_controller_init_style as usize),
            ("loadView", table_controller_load_view as usize),
            ("tableView", table_controller_table as usize),
            ("setTableView:", table_controller_set_table as usize),
            ("clearsSelectionOnViewWillAppear", controller_clears_selection as usize),
            ("setClearsSelectionOnViewWillAppear:", controller_set_clears_selection as usize),
            ("refreshControl", zero as usize),
            ("setRefreshControl:", ignore_id as usize),
        ],
    );

    let cell = view_class("UITableViewCell", "UIView");
    methods(
        cell,
        &[
            ("initWithStyle:reuseIdentifier:", cell_init_with_style as usize),
            ("reuseIdentifier", cell_reuse_identifier as usize),
            ("contentView", cell_content as usize),
            ("textLabel", cell_text_label as usize),
            ("detailTextLabel", cell_detail_label as usize),
            ("imageView", cell_image_view as usize),
            ("accessoryType", cell_accessory_type as usize),
            ("setAccessoryType:", cell_set_accessory_type as usize),
            ("accessoryView", cell_accessory_view as usize),
            ("setAccessoryView:", cell_set_accessory_view as usize),
            ("selectionStyle", cell_selection_style as usize),
            ("setSelectionStyle:", cell_set_selection_style as usize),
            ("backgroundView", cell_background_view as usize),
            ("setBackgroundView:", cell_set_background_view as usize),
            ("selectedBackgroundView", cell_selected_background_view as usize),
            ("setSelectedBackgroundView:", cell_set_selected_background_view as usize),
            ("indentationLevel", cell_indentation as usize),
            ("setIndentationLevel:", cell_set_indentation as usize),
            ("isSelected", control_selected as usize),
            ("isHighlighted", text_highlighted as usize),
            ("prepareForReuse", nothing as usize),
        ],
    );
    let _ = cell_set_reuse_identifier_unused;

    let collection = view_class("UICollectionView", "UIScrollView");
    methods(
        collection,
        &[
            ("initWithFrame:", collection_init_with_frame as usize),
            ("initWithFrame:collectionViewLayout:", collection_init_with_layout as usize),
            ("collectionViewLayout", collection_layout as usize),
            ("setCollectionViewLayout:", collection_set_layout as usize),
            ("setCollectionViewLayout:animated:", collection_set_layout_animated as usize),
            ("dataSource", collection_data_source as usize),
            ("setDataSource:", collection_set_data_source as usize),
            ("allowsSelection", collection_allows_selection as usize),
            ("setAllowsSelection:", collection_set_allows_selection as usize),
            ("allowsMultipleSelection", collection_allows_multiple as usize),
            ("setAllowsMultipleSelection:", collection_set_allows_multiple as usize),
            ("backgroundView", collection_background_view as usize),
            ("setBackgroundView:", collection_set_background_view as usize),
            ("registerClass:forCellWithReuseIdentifier:", collection_register_cell as usize),
            ("registerClass:forSupplementaryViewOfKind:withReuseIdentifier:", collection_register_supplementary as usize),
            ("dequeueReusableCellWithReuseIdentifier:forIndexPath:", collection_dequeue_cell as usize),
            (
                "dequeueReusableSupplementaryViewOfKind:withReuseIdentifier:forIndexPath:",
                collection_dequeue_supplementary as usize,
            ),
            ("reloadData", collection_reload as usize),
            ("reloadItemsAtIndexPaths:", ignore_id as usize),
            ("reloadSections:", ignore_id as usize),
            ("insertItemsAtIndexPaths:", ignore_id as usize),
            ("deleteItemsAtIndexPaths:", ignore_id as usize),
            ("insertSections:", ignore_id as usize),
            ("deleteSections:", ignore_id as usize),
            ("performBatchUpdates:completion:", collection_batch as usize),
            ("numberOfSections", collection_number_of_sections as usize),
            ("numberOfItemsInSection:", collection_items_in_section as usize),
            ("cellForItemAtIndexPath:", nil_with_id as usize),
            ("indexPathForCell:", nil_with_id as usize),
            ("indexPathsForSelectedItems", collection_selected_items as usize),
            ("indexPathsForVisibleItems", empty_array as usize),
            ("visibleCells", empty_array as usize),
            ("selectItemAtIndexPath:animated:scrollPosition:", collection_select as usize),
            ("deselectItemAtIndexPath:animated:", collection_deselect as usize),
            ("scrollToItemAtIndexPath:atScrollPosition:animated:", collection_scroll_to as usize),
        ],
    );

    let collection_controller = define_class("UICollectionViewController", "UIViewController", 8);
    methods(
        collection_controller,
        &[
            ("initWithCollectionViewLayout:", collection_controller_init_layout as usize),
            ("loadView", collection_controller_load_view as usize),
            ("collectionView", collection_controller_view as usize),
            ("setCollectionView:", collection_controller_set_view as usize),
            ("collectionViewLayout", collection_controller_layout as usize),
            ("clearsSelectionOnViewWillAppear", controller_clears_selection as usize),
            ("setClearsSelectionOnViewWillAppear:", controller_set_clears_selection as usize),
        ],
    );

    let reusable = view_class("UICollectionReusableView", "UIView");
    methods(
        reusable,
        &[
            ("initWithFrame:", reusable_init_with_frame as usize),
            ("reuseIdentifier", cell_reuse_identifier as usize),
            ("prepareForReuse", nothing as usize),
            ("applyLayoutAttributes:", ignore_id as usize),
        ],
    );
    let collection_cell = view_class("UICollectionViewCell", "UICollectionReusableView");
    methods(
        collection_cell,
        &[
            ("contentView", collection_cell_content as usize),
            ("backgroundView", cell_background_view as usize),
            ("setBackgroundView:", cell_set_background_view as usize),
            ("selectedBackgroundView", cell_selected_background_view as usize),
            ("setSelectedBackgroundView:", cell_set_selected_background_view as usize),
            ("isSelected", control_selected as usize),
            ("setSelected:", control_set_selected as usize),
            ("isHighlighted", text_highlighted as usize),
            ("setHighlighted:", text_set_highlighted as usize),
        ],
    );

    let layout = define_class("UICollectionViewLayout", "NSObject", 8);
    methods(
        layout,
        &[
            ("collectionView", layout_collection_view as usize),
            ("invalidateLayout", nothing as usize),
            ("collectionViewContentSize", layout_content_size as usize),
        ],
    );
    let _ = layout_set_collection_view_unused;
    let flow = define_class("UICollectionViewFlowLayout", "UICollectionViewLayout", 8);
    methods(
        flow,
        &[
            ("itemSize", flow_item_size as usize),
            ("setItemSize:", flow_set_item_size as usize),
            ("estimatedItemSize", flow_estimated_size as usize),
            ("setEstimatedItemSize:", flow_set_estimated_size as usize),
            ("headerReferenceSize", flow_header_size as usize),
            ("setHeaderReferenceSize:", flow_set_header_size as usize),
            ("footerReferenceSize", flow_footer_size as usize),
            ("setFooterReferenceSize:", flow_set_footer_size as usize),
            ("sectionInset", flow_section_inset as usize),
            ("setSectionInset:", flow_set_section_inset as usize),
            ("minimumLineSpacing", flow_line_spacing as usize),
            ("setMinimumLineSpacing:", flow_set_line_spacing as usize),
            ("minimumInteritemSpacing", flow_item_spacing as usize),
            ("setMinimumInteritemSpacing:", flow_set_item_spacing as usize),
            ("scrollDirection", flow_direction as usize),
            ("setScrollDirection:", flow_set_direction as usize),
        ],
    );
    let _ = flow_item_size_raw;
    for (name, value) in [
        ("UICollectionElementKindSectionHeader", "UICollectionElementKindSectionHeader"),
        ("UICollectionElementKindSectionFooter", "UICollectionElementKindSectionFooter"),
    ] {
        r.data(name, leak(string::make(value)));
    }

    let alert = view_class("UIAlertView", "UIView");
    let alert_common: &[(&str, usize)] = &[
        ("initWithFrame:", alert_init_with_frame as usize),
        ("title", alert_title as usize),
        ("setTitle:", alert_set_title as usize),
        ("delegate", alert_delegate as usize),
        ("setDelegate:", alert_set_delegate as usize),
        ("isVisible", alert_visible as usize),
        ("addButtonWithTitle:", alert_add_button as usize),
        ("numberOfButtons", alert_number_of_buttons as usize),
        ("buttonTitleAtIndex:", alert_button_title as usize),
        ("cancelButtonIndex", alert_cancel_index as usize),
        ("setCancelButtonIndex:", alert_set_cancel_index as usize),
        ("firstOtherButtonIndex", alert_first_other_index as usize),
        ("dismissWithClickedButtonIndex:animated:", alert_dismiss as usize),
        ("gamebridgeAlertButton:", alert_button_tapped as usize),
        ("gamebridgeAlertTextChanged:", alert_text_changed as usize),
        ("gamebridgeAlertReturn:", alert_return as usize),
    ];
    methods(alert, alert_common);
    methods(
        alert,
        &[
            ("initWithTitle:message:delegate:cancelButtonTitle:otherButtonTitles:", ios_alert_init_with_buttons as usize),
            ("message", alert_message as usize),
            ("setMessage:", alert_set_message as usize),
            ("alertViewStyle", alert_style as usize),
            ("setAlertViewStyle:", alert_set_style as usize),
            ("textFieldAtIndex:", alert_text_field as usize),
            ("show", alert_show as usize),
        ],
    );
    let _ = alert_set_visible_unused;

    let sheet = view_class("UIActionSheet", "UIView");
    methods(sheet, alert_common);
    methods(
        sheet,
        &[
            (
                "initWithTitle:delegate:cancelButtonTitle:destructiveButtonTitle:otherButtonTitles:",
                ios_sheet_init_with_buttons as usize,
            ),
            ("destructiveButtonIndex", sheet_destructive_index as usize),
            ("setDestructiveButtonIndex:", sheet_set_destructive_index as usize),
            ("actionSheetStyle", sheet_style as usize),
            ("setActionSheetStyle:", sheet_set_style as usize),
            ("showInView:", sheet_show_in as usize),
            ("showFromToolbar:", sheet_show_in as usize),
            ("showFromTabBar:", sheet_show_in as usize),
            ("showFromRect:inView:animated:", sheet_show_from_rect as usize),
            ("showFromBarButtonItem:animated:", sheet_show_from_item as usize),
        ],
    );

    let popover = define_class("UIPopoverController", "NSObject", 8);
    methods(
        popover,
        &[
            ("initWithContentViewController:", popover_init as usize),
            ("contentViewController", popover_content as usize),
            ("setContentViewController:", popover_set_content as usize),
            ("setContentViewController:animated:", popover_set_content_animated as usize),
            ("delegate", popover_delegate as usize),
            ("setDelegate:", popover_set_delegate as usize),
            ("isPopoverVisible", popover_visible as usize),
            ("popoverArrowDirection", popover_arrow as usize),
            ("passthroughViews", popover_passthrough as usize),
            ("setPassthroughViews:", popover_set_passthrough as usize),
            ("popoverContentSize", popover_size as usize),
            ("setPopoverContentSize:", popover_set_size as usize),
            ("setPopoverContentSize:animated:", popover_set_size_animated as usize),
            ("presentPopoverFromRect:inView:permittedArrowDirections:animated:", popover_present_from_rect as usize),
            ("presentPopoverFromBarButtonItem:permittedArrowDirections:animated:", popover_present_from_item as usize),
            ("dismissPopoverAnimated:", popover_dismiss as usize),
        ],
    );
    let _ = (popover_set_visible_unused, popover_set_arrow_unused);

    let picker = define_class("UIImagePickerController", "UINavigationController", 8);
    class_methods(
        picker,
        &[
            ("isSourceTypeAvailable:", class_no_with_int as usize),
            ("availableMediaTypesForSourceType:", class_nil_with_int as usize),
            ("isCameraDeviceAvailable:", class_no_with_int as usize),
            ("isFlashAvailableForCameraDevice:", class_no_with_int as usize),
            ("availableCaptureModesForCameraDevice:", class_nil_with_int as usize),
        ],
    );
    methods(
        picker,
        &[
            ("sourceType", picker_source as usize),
            ("setSourceType:", picker_set_source as usize),
            ("allowsEditing", picker_editing as usize),
            ("setAllowsEditing:", picker_set_editing as usize),
            ("allowsImageEditing", picker_editing as usize),
            ("setAllowsImageEditing:", picker_set_editing as usize),
            ("mediaTypes", picker_media_types as usize),
            ("setMediaTypes:", picker_set_media_types as usize),
            ("cameraDevice", picker_camera_device as usize),
            ("setCameraDevice:", picker_set_camera_device as usize),
            ("showsCameraControls", picker_shows_controls as usize),
            ("setShowsCameraControls:", picker_set_shows_controls as usize),
            ("cameraOverlayView", picker_overlay as usize),
            ("setCameraOverlayView:", picker_set_overlay as usize),
            ("videoQuality", picker_quality as usize),
            ("setVideoQuality:", picker_set_quality as usize),
            ("videoMaximumDuration", picker_duration as usize),
            ("setVideoMaximumDuration:", picker_set_duration as usize),
            ("takePicture", nothing as usize),
            ("startVideoCapture", picker_start_capture as usize),
            ("stopVideoCapture", nothing as usize),
            ("gamebridgeAnswerShown", picker_answer_shown as usize),
        ],
    );
    for name in [
        "UIImagePickerControllerMediaType",
        "UIImagePickerControllerOriginalImage",
        "UIImagePickerControllerEditedImage",
        "UIImagePickerControllerCropRect",
        "UIImagePickerControllerMediaURL",
        "UIImagePickerControllerReferenceURL",
        "UIImagePickerControllerMediaMetadata",
    ] {
        r.data(name, leak(string::make(name)));
    }

    let settings = define_class("UIUserNotificationSettings", "NSObject", 8);
    class_methods(settings, &[("settingsForTypes:categories:", settings_for_types as usize)]);
    methods(settings, &[("types", settings_types as usize), ("categories", settings_categories as usize)]);
}

property!(view_tag_like, view_set_tag_like, "tag", isize, 0);

extern "C" fn ignore_insets(_this: Id, _: Sel, _insets: Insets) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A button holds on to the pictures it is given per state (as UIKit
    /// retains them): the caller may let go of its own reference, and a
    /// picture replaced (or the state cleared with nil) is let go of.
    #[test]
    fn buttons_keep_their_state_pictures() {
        use crate::fw::counted;
        use crate::fw::uikit::{make_image, ImageInfo};
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let picture = || make_image(ImageInfo { path: "/nowhere/frame.png".into(), pixels: (4, 4), scale: 1.0, ..Default::default() });
        let button = runtime::send(runtime::class_named("UIButton"), "buttonWithType:", &[0]);
        let (first, second, icon) = (picture(), picture(), picture());
        runtime::send(button, "setBackgroundImage:forState:", &[first, 0]);
        runtime::send(button, "setImage:forState:", &[icon, 1]);
        counted::release(first);
        counted::release(icon);
        assert_eq!((counted::count(first), counted::count(icon)), (Some(1), Some(1)), "kept by the button");
        assert_eq!(runtime::send(button, "backgroundImageForState:", &[0]), first);
        runtime::send(button, "setBackgroundImage:forState:", &[second, 0]);
        counted::release(second);
        assert_eq!((counted::count(first), counted::count(second)), (None, Some(1)), "the replaced one is let go of");
        runtime::send(button, "setImage:forState:", &[0, 1]);
        assert_eq!(counted::count(icon), None);
    }

    #[test]
    fn control_targets_and_selection_bookkeeping_follow_uikit() {
        const DOWN: usize = 1 << 0;
        const UP_INSIDE: usize = 1 << 6;
        const CHANGED: usize = 1 << 12;
        let (a, b, tap, other) = (0x100, 0x200, 0x10, 0x20);
        let mut list = Vec::new();
        targets_add(&mut list, a, tap, DOWN | UP_INSIDE);
        targets_add(&mut list, a, tap, CHANGED); // same pair: events added
        targets_add(&mut list, b, other, UP_INSIDE);
        assert_eq!(list.len(), 2);
        // Removing one event keeps the pair with its other events.
        targets_remove(&mut list, a, tap, UP_INSIDE);
        assert_eq!(list[0].events, DOWN | CHANGED);
        // nil target and NULL action match every pair.
        targets_remove(&mut list, 0, 0, UP_INSIDE | DOWN);
        assert_eq!(list.iter().map(|t| (t.target, t.events)).collect::<Vec<_>>(), vec![(a, CHANGED)]);

        // Selection: index path objects 1 and 2 both name section 0 row 3,
        // 4 names row 4 (stand-in ids compared like same_path does).
        let index = |path: Id| match path {
            1 | 2 => (0, 3),
            _ => (0, 4),
        };
        let same = |x: Id, y: Id| index(x) == index(y);
        let mut rows = Vec::new();
        select_in(&mut rows, 1, true, &same);
        select_in(&mut rows, 2, true, &same); // an equal path isn't added twice
        select_in(&mut rows, 4, true, &same);
        assert_eq!(rows, vec![1, 4]);
        deselect_in(&mut rows, 2, &same); // removed through an equal path
        assert_eq!(rows, vec![4]);
        select_in(&mut rows, 1, false, &same); // single selection replaces
        assert_eq!(rows, vec![1]);
        select_in(&mut rows, 0, false, &same); // nil clears
        assert!(rows.is_empty());
    }

    #[test]
    fn text_is_measured_wrapped_and_cut_like_uikit() {
        // 10 letters at 20 points: 0.55 em each, rounded up; one 24-point line.
        assert_eq!(text_size("abcdefghij", 20.0, f64::INFINITY, 1), Size { width: 110.0, height: 24.0 });
        // Wrapped at 50 points: three lines, as wide as the limit.
        assert_eq!(text_size("abcdefghij", 20.0, 50.0, 0), Size { width: 50.0, height: 72.0 });
        // ...but a one-line label stays one line; new lines count too.
        assert_eq!(text_size("abcdefghij", 20.0, 50.0, 1).height, 24.0);
        assert_eq!(text_size("ab\ncd", 20.0, f64::INFINITY, 0), Size { width: 22.0, height: 48.0 });
        assert_eq!(text_size("", 20.0, f64::INFINITY, 0), Size::default());
    }

    #[test]
    fn alert_and_sheet_button_indices_follow_uikit() {
        let alert = Buttons::alert(Some("Cancel".into()), vec!["OK".into(), "Rate".into()]);
        assert_eq!((alert.cancel, alert.first_other, alert.destructive), (0, 1, -1));
        let mut plain = Buttons::alert(None, vec![]);
        assert_eq!(plain.add("Only".into()), 0);
        assert_eq!((plain.cancel, plain.first_other), (-1, 0));
        let sheet = Buttons::sheet(Some("Delete".into()), Some("Cancel".into()), vec!["Camera".into(), "Album".into()]);
        assert_eq!(sheet.titles, ["Delete", "Camera", "Album", "Cancel"]);
        assert_eq!((sheet.destructive, sheet.first_other, sheet.cancel), (0, 1, 3));
    }

    /// A table shows the rows its data source gives, one cell under the
    /// other at the delegate's heights, their labels laid out by the cell's
    /// style; a tap on a row selects it (willSelect may change which, then
    /// didSelect), and the selected cell is drawn selected.
    #[test]
    fn tables_show_their_rows_and_take_taps() {
        use crate::fw::geometry::Point;
        use crate::fw::object::define_class;
        use crate::fw::touch::{deliver_in, origin_in_window, Phase};
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        static HEARD: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let heard = || std::mem::take(&mut *HEARD.lock().unwrap());
        extern "C" fn rows(_this: Id, _: Sel, _table: Id, _section: isize) -> isize {
            3
        }
        // A subtitle cell (style 3) for every row.
        extern "C" fn cell_for(_this: Id, _: Sel, _table: Id, path: Id) -> Id {
            let cell = runtime::send(runtime::send(runtime::class_named("UITableViewCell"), "alloc", &[]), "initWithStyle:reuseIdentifier:", &[3, 0]);
            runtime::send(runtime::send(cell, "textLabel", &[]), "setText:", &[string::make(&format!("Row {}", runtime::send(path, "row", &[])))]);
            runtime::send(runtime::send(cell, "detailTextLabel", &[]), "setText:", &[string::make("more")]);
            cell
        }
        extern "C" fn height(_this: Id, _: Sel, _table: Id, _path: Id) -> f64 {
            50.0
        }
        extern "C" fn will_select(_this: Id, _: Sel, _table: Id, path: Id) -> Id {
            HEARD.lock().unwrap().push(format!("will {}", runtime::send(path, "row", &[])));
            path
        }
        extern "C" fn did_select(_this: Id, _: Sel, _table: Id, path: Id) {
            HEARD.lock().unwrap().push(format!("did {}", runtime::send(path, "row", &[])));
        }
        let source_class = define_class("TableTestSource", "NSObject", 8);
        methods(
            source_class,
            &[
                ("tableView:numberOfRowsInSection:", rows as usize),
                ("tableView:cellForRowAtIndexPath:", cell_for as usize),
                ("tableView:heightForRowAtIndexPath:", height as usize),
                ("tableView:willSelectRowAtIndexPath:", will_select as usize),
                ("tableView:didSelectRowAtIndexPath:", did_select as usize),
            ],
        );
        let source = new_object("TableTestSource");
        let window = new_view_named("UIWindow", Rect::new(0.0, 0.0, 568.0, 320.0));
        let table = new_view_named("UITableView", Rect::new(10.0, 20.0, 240.0, 174.0));
        runtime::send(window, "addSubview:", &[table]);
        runtime::send(table, "setDataSource:", &[source]);
        runtime::send(table, "setDelegate:", &[source]);
        runtime::send(table, "reloadData", &[]);

        let cells = collections::array_items(runtime::send(table, "visibleCells", &[]));
        assert_eq!(cells.len(), 3, "a cell per row");
        let frames: Vec<Rect> = cells.iter().map(|&cell| frame_of(cell)).collect();
        assert_eq!(frames.iter().map(|f| (f.origin.y, f.size.height, f.size.width)).collect::<Vec<_>>(), [(0.0, 50.0, 240.0), (50.0, 50.0, 240.0), (100.0, 50.0, 240.0)]);
        let path = runtime::send(runtime::class_named("NSIndexPath"), "indexPathForRow:inSection:", &[1, 0]);
        assert_eq!(runtime::send(table, "cellForRowAtIndexPath:", &[path]), cells[1]);
        let content_size: Size = {
            let s = runtime::sel("contentSize");
            let f: extern "C" fn(Id, Sel) -> Size = unsafe { std::mem::transmute(runtime::imp_for(table, s).unwrap()) };
            f(table, s)
        };
        assert_eq!(content_size.height, 150.0);
        // Subtitle cells: the title above the detail, both inside the cell and wide enough.
        let title = runtime::send(cells[1], "textLabel", &[]);
        let detail = runtime::send(cells[1], "detailTextLabel", &[]);
        let (title_frame, detail_frame) = (frame_of(title), frame_of(detail));
        assert!(title_frame.size.width >= 100.0 && detail_frame.size.width >= 100.0, "{title_frame:?} {detail_frame:?}");
        assert!(title_frame.origin.y + title_frame.size.height <= detail_frame.origin.y + 0.5, "{title_frame:?} {detail_frame:?}");
        assert!(detail_frame.origin.y + detail_frame.size.height <= 50.0);
        assert!(label_look(title).is_some(), "the title is drawn");

        // A tap on the second row selects it.
        let corner = origin_in_window(cells[1]);
        let at = Point { x: corner.x + 100.0, y: corner.y + 25.0 };
        deliver_in(window, 704, Phase::Began, at);
        deliver_in(window, 704, Phase::Ended, at);
        assert_eq!(heard(), ["will 1", "did 1"]);
        let chosen = runtime::send(table, "indexPathForSelectedRow", &[]);
        assert!(chosen != 0 && runtime::send(chosen, "row", &[]) == 1);
        assert!(runtime::send(cells[1], "isSelected", &[]) & 0xFF != 0, "drawn selected");
        assert!(runtime::send(cells[0], "isSelected", &[]) & 0xFF == 0);
        // Selected in code: the cell follows, without the delegate hearing it (as in UIKit).
        let first = runtime::send(runtime::class_named("NSIndexPath"), "indexPathForRow:inSection:", &[0, 0]);
        runtime::send(table, "selectRowAtIndexPath:animated:scrollPosition:", &[first, 0, 0]);
        assert!(runtime::send(cells[0], "isSelected", &[]) & 0xFF != 0);
        assert!(runtime::send(cells[1], "isSelected", &[]) & 0xFF == 0);
        assert!(heard().is_empty());
    }

    /// Alerts are drawn over the whole key window as UIKit draws them: a box
    /// with the title, the text fields (the first one being edited) and the
    /// buttons. alertViewShouldEnableFirstOtherButton: turns the first other
    /// button on and off as text is typed; a tapped button is told to the
    /// delegate (clicked, then willDismiss) and the alert goes away. Action
    /// sheets show their buttons one above the other.
    #[test]
    fn alerts_are_drawn_and_answer_their_buttons() {
        use crate::fw::geometry::Point;
        use crate::fw::object::define_class;
        use crate::fw::touch::{deliver_in, origin_in_window, Phase};
        use crate::fw::typing::{first_responder, type_text};
        let _turn = crate::fw::typing::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        static HEARD: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let heard = || std::mem::take(&mut *HEARD.lock().unwrap());
        extern "C" fn clicked(_this: Id, s: Sel, _alert: Id, index: isize) {
            HEARD.lock().unwrap().push(format!("{} {index}", lock().sel_name(s)));
        }
        extern "C" fn should_enable(_this: Id, _: Sel, alert: Id) -> bool {
            let field = runtime::send(alert, "textFieldAtIndex:", &[0]);
            !text_of(runtime::send(field, "text", &[])).is_empty()
        }
        let delegate_class = define_class("AlertTestDelegate", "NSObject", 8);
        methods(
            delegate_class,
            &[
                ("alertView:clickedButtonAtIndex:", clicked as usize),
                ("alertView:willDismissWithButtonIndex:", clicked as usize),
                ("actionSheet:clickedButtonAtIndex:", clicked as usize),
                ("alertViewShouldEnableFirstOtherButton:", should_enable as usize),
            ],
        );
        let delegate = new_object("AlertTestDelegate");
        let window = new_view_named("UIWindow", Rect::new(0.0, 0.0, 568.0, 320.0));
        runtime::send(window, "makeKeyAndVisible", &[]);
        fn inside(view: Id) -> Vec<Id> {
            view_state(view).subviews.clone().into_iter().flat_map(|child| std::iter::once(child).chain(inside(child))).collect()
        }
        let buttons = |view: Id| inside(view).into_iter().filter(|&v| class_is(v, "UIButton")).collect::<Vec<_>>();
        let title = |button: Id| text_of(runtime::send(button, "titleForState:", &[0]));
        let enabled = |button: Id| runtime::send(button, "isEnabled", &[]) & 0xFF != 0;
        let tap = |host: Id, button: Id, finger: i32| {
            let corner = origin_in_window(button);
            let size = frame_of(button).size;
            let at = Point { x: corner.x + size.width / 2.0, y: corner.y + size.height / 2.0 };
            deliver_in(host, finger, Phase::Began, at);
            deliver_in(host, finger, Phase::Ended, at);
        };

        let alert = new_view_named("UIAlertView", Rect::default());
        runtime::send(alert, "setTitle:", &[string::make("Enter your character's name")]);
        runtime::send(alert, "setDelegate:", &[delegate]);
        runtime::send(alert, "addButtonWithTitle:", &[string::make("Cancel")]);
        runtime::send(alert, "addButtonWithTitle:", &[string::make("CREATE")]);
        runtime::send(alert, "setCancelButtonIndex:", &[0]);
        runtime::send(alert, "setAlertViewStyle:", &[2]);
        runtime::send(alert, "show", &[]);
        let host = view_state(alert).superview;
        assert!(host != 0, "shown in the key window");
        assert_eq!(frame_of(alert).size, frame_of(host).size, "over the whole window");
        let labels: Vec<String> = inside(alert).into_iter().filter(|&v| class_is(v, "UILabel")).map(|v| text_of(get_id(v, "text"))).collect();
        assert!(labels.contains(&"Enter your character's name".to_string()), "{labels:?}");
        let field = runtime::send(alert, "textFieldAtIndex:", &[0]);
        assert!(inside(alert).contains(&field), "its text field is shown");
        assert_eq!(first_responder(), field, "and being edited");
        let shown = buttons(alert);
        assert_eq!(shown.iter().map(|&b| title(b)).collect::<Vec<_>>(), ["Cancel", "CREATE"]);
        assert!(!enabled(shown[1]), "CREATE waits for a name");
        type_text("Bo");
        assert!(enabled(shown[1]));
        tap(host, shown[1], 702);
        assert_eq!(heard(), ["alertView:clickedButtonAtIndex: 1", "alertView:willDismissWithButtonIndex: 1"]);
        assert_eq!(view_state(alert).superview, 0, "the alert went away");
        assert!(runtime::send(alert, "isVisible", &[]) & 0xFF == 0);
        assert_eq!(first_responder(), 0, "its field is no longer edited");
        assert_eq!(text_of(runtime::send(field, "text", &[])), "Bo");

        let sheet = new_view_named("UIActionSheet", Rect::default());
        runtime::send(sheet, "setDelegate:", &[delegate]);
        runtime::send(sheet, "addButtonWithTitle:", &[string::make("Camera")]);
        runtime::send(sheet, "addButtonWithTitle:", &[string::make("Cancel")]);
        runtime::send(sheet, "setCancelButtonIndex:", &[1]);
        runtime::send(sheet, "showInView:", &[host]);
        let host = view_state(sheet).superview;
        assert!(host != 0);
        let shown = buttons(sheet);
        assert_eq!(shown.len(), 2);
        assert!(origin_in_window(shown[1]).y > origin_in_window(shown[0]).y, "one above the other");
        tap(host, shown[1], 703);
        assert_eq!(heard(), ["actionSheet:clickedButtonAtIndex: 1"]);
        assert_eq!(view_state(sheet).superview, 0);
    }
}
