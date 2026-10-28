//! UIKit controls and screens: fonts, labels, buttons, text fields, web
//! views, bars, navigation, tables, collections, alerts, action sheets,
//! popovers and the photo picker. They keep their state and answer like
//! UIKit. Labels and buttons are drawn (fw/pictures.rs, from `label_look`;
//! a button's parts are its subviews); the rest is not drawn yet, and what it would show
//! is logged. Controls (buttons too) are marked "notDrawn" so they don't
//! catch taps meant for the game: they don't follow touches yet. Screens
//! for things this phone doesn't offer (photo library, camera) answer the
//! way iOS does when the user closes them.

use crate::fw::geometry::{Point, Rect, Size};
use crate::fw::graphics::TextLook;
use crate::fw::layers::send_struct;
use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::runloop::{post, Work};
use crate::fw::touch::Phase;
use crate::fw::uikit::{next_responder, property, property_get, property_set, screen_points, view_state, view_state_mut};
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
/// pointer-sized arguments (a block taking fewer ignores the rest). Our
/// copy goes once it has run, as the frameworks' do.
pub fn call_block_later(block: usize, args: [usize; 3]) {
    if block == 0 {
        return;
    }
    let block = crate::libsystem::mach::block_copy(block as *mut _) as usize;
    later(move || {
        let header = block as *const crate::libsystem::mach::BlockHeader;
        let invoke: extern "C" fn(usize, usize, usize, usize) = unsafe { std::mem::transmute((*header).invoke) };
        invoke(block, args[0], args[1], args[2]);
        crate::libsystem::mach::block_release(block as *mut _);
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
    runtime::send(this, "setNeedsDisplay", &[]);
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
    runtime::send(this, "setNeedsDisplay", &[]);
}
property!(text_color, text_set_color, "textColor", id, 0);
property!(text_alignment, text_set_alignment, "textAlignment", isize, 0);
property!(text_delegate, text_set_delegate, "delegate", assign, 0);
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
// UIBaselineAdjustment (0: align baselines, the default).
property!(label_baseline, label_set_baseline, "baselineAdjustment", isize, 0);
property!(text_shadow_color, text_set_shadow_color, "shadowColor", id, 0);
property!(text_highlighted_color, text_set_highlighted_color, "highlightedTextColor", id, 0);
property!(text_highlighted, text_set_highlighted, "highlighted", bool, false);
property!(text_enabled, text_set_enabled, "enabled", bool, true);
property!(text_attributed, text_set_attributed, "attributedText", copy, 0);

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
    // Attributed text is drawn in its own font and colour, where it gives
    // them. Its colour is the one at its start, else the default black, as
    // iOS 6's -[UILabel textColor] reads it: an earlier textColor no longer counts.
    let attributed = get_id(label, "attributedText");
    let in_force = attributed != 0 && crate::fw::attributed::text(attributed) == text;
    let attribute = |name: &str| if in_force { crate::fw::attributed::attribute(attributed, name) } else { 0 };
    let font = match attribute("NSFont") {
        0 => text_font(label, 0),
        font => font,
    };
    let color = match (in_force, attribute("NSColor")) {
        (true, own) if !(highlighted && get_id(label, "highlightedTextColor") != 0) => own,
        _ => color,
    };
    Some(TextLook {
        text,
        font: font_name_and_size(font),
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
    let state = view_state_mut(this);
    state.frame.size = label_fitting_size(this, state.frame.size);
}

/// UIKit's drawRect: for a label: its text over its bounds.
extern "C" fn label_draw_rect(this: Id, _: Sel, _: Rect) {
    crate::fw::uikit::send_rect(this, "drawTextInRect:", Rect { origin: Point::default(), size: view_state(this).frame.size });
}

extern "C" fn label_draw_text_in_rect(this: Id, _: Sel, rect: Rect) {
    if let Some(mut look) = label_look(this) {
        look.size = (rect.size.width, rect.size.height);
        crate::fw::graphics::draw_text_here(&look, rect);
    }
}

/// A label left as UILabel draws (neither drawRect: nor drawTextInRect:
/// replaced): its text is a picture of its own (fw/pictures.rs), not
/// drawn into its layer. False for any other view.
pub(crate) fn label_draws_plainly(view: Id) -> bool {
    let own = |name: &str| runtime::imp_for(view, runtime::sel(name));
    own("drawRect:") == Some(label_draw_rect as usize) && own("drawTextInRect:") == Some(label_draw_text_in_rect as usize)
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

const TOUCH_DOWN_REPEAT: usize = 1 << 1;

/// How far outside its bounds a finger still counts as on a control (UIKit's margin).
const TOUCH_MARGIN: f64 = 70.0;
/// A press shorter than this keeps the control pressed until this long
/// after the finger went down, so even a quick tap shows (UIKit's 0.12 s).
const MIN_PRESS: f64 = 0.12;

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

/// -pointMostlyInside:withEvent:: the bounds grown by UIKit's margin.
extern "C" fn control_point_mostly_inside(this: Id, _: Sel, at: Point, _event: Id) -> bool {
    let size = frame_of(this).size;
    at.x >= -TOUCH_MARGIN && at.y >= -TOUCH_MARGIN && at.x < size.width + TOUCH_MARGIN && at.y < size.height + TOUCH_MARGIN
}

/// Whether the finger is (mostly) on the control, through
/// -pointMostlyInside:withEvent: so a game's control can answer.
fn finger_inside(this: Id, touch: Id, event: Id) -> bool {
    let at = crate::fw::touch::location_in(touch, this);
    let s = runtime::sel("pointMostlyInside:withEvent:");
    match runtime::imp_for(this, s) {
        Some(imp) => {
            let f: extern "C" fn(Id, Sel, Point, Id) -> bool = unsafe { std::mem::transmute(imp) };
            f(this, s, at, event)
        }
        None => control_point_mostly_inside(this, s, at, event),
    }
}

fn set_pressed(this: Id, pressed: bool) {
    if get_flag(this, "highlighted", false) != pressed {
        runtime::send(this, "setHighlighted:", &[pressed as usize]);
    }
}

fn is_tracking(this: Id) -> bool {
    runtime::send(this, "isTracking", &[]) & 0xFF != 0
}

fn set_tracking(this: Id, tracking: bool) {
    property_set(this, "tracking", tracking as u64);
}

/// The control follows touches only while enabled and touchable (-shouldTrack).
fn should_track(this: Id) -> bool {
    runtime::send(this, "isEnabled", &[]) & 0xFF != 0 && runtime::send(this, "isUserInteractionEnabled", &[]) & 0xFF != 0
}

/// A touch the control doesn't follow goes on up the responder chain
/// (UIControl calls super, UIResponder's default).
fn pass_on(this: Id, name: &'static str, touches: Id, event: Id) {
    crate::fw::uikit::pass_touch(this, name, touches, event);
}

/// UIKit's -touchesBegan:withEvent: for controls: tracking starts only if
/// -beginTrackingWithTouch:withEvent: agrees; then the control is pressed
/// and its targets hear TouchDown (and TouchDownRepeat for a second tap).
extern "C" fn control_touches_began(this: Id, _: Sel, touches: Id, event: Id) {
    if !should_track(this) {
        pass_on(this, "touchesBegan:withEvent:", touches, event);
        return;
    }
    let touch = runtime::send(touches, "anyObject", &[]);
    if touch == 0 {
        return;
    }
    let tracking = ask_tracking(this, "beginTrackingWithTouch:withEvent:", touch, event);
    runtime::send(this, "setTracking:", &[tracking as usize]);
    if !is_tracking(this) {
        return;
    }
    // A press that is still being let go of (its quick unhighlight pending) starts again.
    cancel_unhighlight(this);
    property_set(this, "touchInside", 1);
    set_f(this, "downTime", crate::fw::runloop::media_time());
    set_pressed(this, true);
    let repeat = if runtime::send(touch, "tapCount", &[]) >= 2 { TOUCH_DOWN_REPEAT } else { 0 };
    send_actions(this, TOUCH_DOWN | repeat, event);
}

/// -continueTrackingWithTouch:withEvent: first; still tracking, the control
/// is pressed while the finger is (mostly) on it and its targets hear the drag.
extern "C" fn control_touches_moved(this: Id, _: Sel, touches: Id, event: Id) {
    if !is_tracking(this) {
        pass_on(this, "touchesMoved:withEvent:", touches, event);
        return;
    }
    let touch = runtime::send(touches, "anyObject", &[]);
    if touch == 0 {
        return;
    }
    let going_on = ask_tracking(this, "continueTrackingWithTouch:withEvent:", touch, event);
    runtime::send(this, "setTracking:", &[going_on as usize]);
    if !is_tracking(this) {
        return;
    }
    let was_inside = get_flag(this, "touchInside", false);
    let inside = finger_inside(this, touch, event);
    property_set(this, "touchInside", inside as u64);
    set_pressed(this, inside);
    let crossed = match (was_inside, inside) {
        (true, false) => TOUCH_DRAG_EXIT,
        (false, true) => TOUCH_DRAG_ENTER,
        _ => 0,
    };
    send_actions(this, crossed | if inside { TOUCH_DRAG_INSIDE } else { TOUCH_DRAG_OUTSIDE }, event);
}

/// UIKit's order: -endTrackingWithTouch:withEvent:, tracking off, then the
/// TouchUpInside/Outside actions (the control still pressed), and only then
/// the control lets go, at once or, for a press under 0.12 s, that long
/// after the finger went down (-_unhighlight, performed after a delay).
pub(crate) extern "C" fn control_touches_ended(this: Id, _: Sel, touches: Id, event: Id) {
    if !is_tracking(this) {
        pass_on(this, "touchesEnded:withEvent:", touches, event);
        return;
    }
    let touch = runtime::send(touches, "anyObject", &[]);
    let inside = touch != 0 && finger_inside(this, touch, event);
    let now = crate::fw::runloop::media_time();
    // When the press counts from: a quick one shows for MIN_PRESS from there.
    let pressed_since = if get_flag(this, "highlighted", false) {
        let down = get_f(this, "downTime", now);
        (now - down < MIN_PRESS).then_some(down)
    } else if inside {
        // Pressed again for the touch's end (a subclass let go of it), shown for MIN_PRESS.
        set_pressed(this, true);
        Some(now)
    } else {
        None
    };
    if get_flag(this, "touchInside", false) != inside {
        property_set(this, "touchInside", inside as u64);
        send_actions(this, if inside { TOUCH_DRAG_ENTER } else { TOUCH_DRAG_EXIT }, event);
    }
    runtime::send(this, "endTrackingWithTouch:withEvent:", &[touch, event]);
    runtime::send(this, "setTracking:", &[0]);
    send_actions(this, if inside { TOUCH_UP_INSIDE } else { TOUCH_UP_OUTSIDE }, event);
    match pressed_since {
        Some(since) if now - since < MIN_PRESS => {
            let s = runtime::sel("performSelector:withObject:afterDelay:");
            if let Some(imp) = runtime::imp_for(this, s) {
                let f: extern "C" fn(Id, Sel, Sel, Id, f64) = unsafe { std::mem::transmute(imp) };
                f(this, s, runtime::sel("_unhighlight"), 0, MIN_PRESS - (now - since));
            }
        }
        _ => set_pressed(this, false),
    }
}

/// A cancelled touch only ends the tracking, through -cancelTrackingWithEvent:.
extern "C" fn control_touches_cancelled(this: Id, _: Sel, _touches: Id, event: Id) {
    if is_tracking(this) {
        runtime::send(this, "cancelTrackingWithEvent:", &[event]);
    }
}

/// UIControl's -cancelTrackingWithEvent:: tracking off, no longer pressed,
/// and the targets hear TouchCancel (a subclass that doesn't call super sends none).
extern "C" fn control_cancel_tracking(this: Id, _: Sel, event: Id) {
    runtime::send(this, "setTracking:", &[0]);
    cancel_unhighlight(this);
    set_pressed(this, false);
    send_actions(this, TOUCH_CANCEL, event);
}

fn cancel_unhighlight(this: Id) {
    if crate::fw::runloop::perform_pending(this, runtime::sel("_unhighlight")) {
        runtime::send(
            runtime::class_named("NSObject"),
            "cancelPreviousPerformRequestsWithTarget:selector:object:",
            &[this, runtime::sel("_unhighlight"), 0],
        );
    }
}

/// UIControl's -hitTest:withEvent:: a disabled control (and all inside it)
/// is skipped, so the touch goes to the view behind it; otherwise UIView's.
extern "C" fn control_hit_test(this: Id, s: Sel, at: Point, event: Id) -> Id {
    if runtime::send(this, "isEnabled", &[]) & 0xFF == 0 {
        return 0;
    }
    let class = runtime::class_named("UIView");
    let found = lock().lookup(class, s);
    match found {
        Some(imp) => {
            let f: extern "C" fn(Id, Sel, Point, Id) -> Id = unsafe { std::mem::transmute(imp) };
            f(this, s, at, event)
        }
        None => crate::fw::touch::view_hit_test(this, at, event),
    }
}

/// The delayed end of a quick press.
extern "C" fn control_unhighlight(this: Id, _: Sel) {
    set_pressed(this, false);
}

extern "C" fn control_set_tracking(this: Id, _: Sel, tracking: u8) {
    set_tracking(this, tracking != 0);
}

/// UIControl's -setEnabled: redraws the control when it changes.
extern "C" fn control_set_enabled(this: Id, _: Sel, enabled: u8) {
    let changed = get_flag(this, "enabled", true) != (enabled != 0);
    property_set(this, "enabled", (enabled != 0) as u64);
    if changed {
        runtime::send(this, "setNeedsDisplay", &[]);
    }
}

extern "C" fn control_begin_tracking(_this: Id, _: Sel, _touch: Id, _event: Id) -> bool {
    true
}

/// The tag of a button's pressed glow view ('prsd', as UIKit's).
const GLOW_TAG: usize = 0x7072_7364;

/// UIButton's state setters: UIControl's, then the button lays itself out
/// again (its title, image and background follow the state).
/// With showsTouchWhenHighlighted, the glow shows at once when pressed and
/// fades out over 0.25 s when let go, as -[UIButton setHighlighted:] does.
extern "C" fn button_set_highlighted(this: Id, _: Sel, value: u8) {
    property_set(this, "highlighted", (value != 0) as u64);
    if get_flag(this, "showsTouchWhenHighlighted", false) {
        let glow = runtime::send(this, "viewWithTag:", &[GLOW_TAG]);
        if glow != 0 {
            center_glow(this, glow);
            if value != 0 {
                send_struct(glow, "setAlpha:", 1.0f64);
            } else {
                let uiview = runtime::class_named("UIView");
                runtime::send(uiview, "beginAnimations:context:", &[0, 0]);
                send_struct(uiview, "setAnimationDuration:", 0.25f64);
                send_struct(glow, "setAlpha:", 0.0f64);
                runtime::send(uiview, "commitAnimations", &[]);
            }
        }
    }
    runtime::send(this, "setNeedsLayout", &[]);
}

/// -setShowsTouchWhenHighlighted: turned on, the button gets its glow: an
/// image view of our own glow picture, not touchable, tagged GLOW_TAG,
/// shown only while pressed; turned off, the glow goes.
extern "C" fn button_set_shows_touch(this: Id, _: Sel, value: u8) {
    if get_flag(this, "showsTouchWhenHighlighted", false) == (value != 0) {
        return;
    }
    property_set(this, "showsTouchWhenHighlighted", (value != 0) as u64);
    let glow = runtime::send(this, "viewWithTag:", &[GLOW_TAG]);
    if value == 0 {
        if glow != 0 {
            runtime::send(glow, "removeFromSuperview", &[]);
        }
        return;
    }
    if glow != 0 {
        return;
    }
    let image_view = runtime::send(runtime::alloc_instance(runtime::class_named("UIImageView")), "initWithImage:", &[crate::fw::graphics::glow_image()]);
    runtime::send(image_view, "setUserInteractionEnabled:", &[0]);
    runtime::send(image_view, "setOpaque:", &[0]);
    runtime::send(image_view, "setTag:", &[GLOW_TAG]);
    let pressed = get_flag(this, "highlighted", false);
    send_struct(image_view, "setAlpha:", if pressed { 1.0f64 } else { 0.0 });
    center_glow(this, image_view);
    // Below the button's picture view, if it has one (the title stays on top).
    match get_id(this, "imageView") {
        0 => runtime::send(this, "addSubview:", &[image_view]),
        picture => runtime::send(this, "insertSubview:belowSubview:", &[image_view, picture]),
    };
    runtime::send(this, "setNeedsLayout", &[]);
}

/// The glow sits in the middle of the button's bounds.
fn center_glow(button: Id, glow: Id) {
    let size = view_state(button).frame.size;
    send_point(glow, "setCenter:", Point { x: size.width / 2.0, y: size.height / 2.0 });
}

extern "C" fn button_set_selected(this: Id, _: Sel, value: u8) {
    property_set(this, "selected", (value != 0) as u64);
    runtime::send(this, "setNeedsLayout", &[]);
}

extern "C" fn button_set_enabled(this: Id, s: Sel, value: u8) {
    control_set_enabled(this, s, value);
    runtime::send(this, "setNeedsLayout", &[]);
}

extern "C" fn control_state_method(this: Id, _: Sel) -> usize {
    control_state(this)
}

extern "C" fn button_with_type(class: Class, _: Sel, kind: isize) -> Id {
    let button = new_view(class, Rect::default());
    property_set(button, "buttonType", kind as u64);
    button
}

// A slider (UISlider): it keeps its value, kept inside its range, and its
// settings; nothing is drawn for it. Its values are C floats, as UIKit's.
fn slider_get(this: Id, key: &'static str, default: f32) -> f32 {
    property_get(this, key).map_or(default, |bits| f32::from_bits(bits as u32))
}

fn slider_put(this: Id, key: &'static str, value: f32) {
    property_set(this, key, value.to_bits() as u64);
}

extern "C" fn slider_value(this: Id, _: Sel) -> f32 {
    slider_get(this, "sliderValue", 0.0)
}

extern "C" fn slider_set_value(this: Id, _: Sel, value: f32) {
    let (low, high) = (slider_get(this, "sliderMinimum", 0.0), slider_get(this, "sliderMaximum", 1.0));
    slider_put(this, "sliderValue", value.min(high).max(low));
}

extern "C" fn slider_set_value_animated(this: Id, s: Sel, value: f32, _animated: u8) {
    slider_set_value(this, s, value);
}

extern "C" fn slider_minimum(this: Id, _: Sel) -> f32 {
    slider_get(this, "sliderMinimum", 0.0)
}

/// A new end of the range moves the value inside it.
extern "C" fn slider_set_minimum(this: Id, s: Sel, value: f32) {
    slider_put(this, "sliderMinimum", value);
    slider_set_value(this, s, slider_value(this, s));
}

extern "C" fn slider_maximum(this: Id, _: Sel) -> f32 {
    slider_get(this, "sliderMaximum", 1.0)
}

extern "C" fn slider_set_maximum(this: Id, s: Sel, value: f32) {
    slider_put(this, "sliderMaximum", value);
    slider_set_value(this, s, slider_value(this, s));
}

property!(slider_continuous, slider_set_continuous, "continuous", bool, true);
property!(slider_minimum_tint, slider_set_minimum_tint, "minimumTrackTintColor", id, 0);
property!(slider_maximum_tint, slider_set_maximum_tint, "maximumTrackTintColor", id, 0);
property!(slider_thumb_tint, slider_set_thumb_tint, "thumbTintColor", id, 0);

property!(button_type, button_set_type_unused, "buttonType", isize, 0);
property!(button_adjusts_highlighted, button_set_adjusts_highlighted, "adjustsImageWhenHighlighted", bool, true);
extern "C" fn button_shows_touch(this: Id, _: Sel) -> bool {
    get_flag(this, "showsTouchWhenHighlighted", false)
}
property!(button_adjusts_disabled, button_set_adjusts_disabled, "adjustsImageWhenDisabled", bool, true);
// Kept for the game to read back; the shadow isn't swapped when highlighted.
property!(button_reverses_shadow, button_set_reverses_shadow, "reversesTitleShadowWhenHighlighted", bool, false);

// ----- a button's parts: its subviews, as UIKit's -----
//
// As -[UIButton layoutSubviews]: the background picture, the picture and
// the title are a UIImageView, a UIImageView and a UIButtonLabel inside the
// button (back to front, the title on top), made when first needed and set
// to the current state's content each time the button is laid out; their
// own drawing (fw/pictures.rs, render.rs) shows them. None of them takes
// touches, so taps land on the button.

/// The button's bounds (its own space).
fn bounds_of(this: Id) -> Rect {
    let state = view_state(this);
    Rect { origin: state.bounds_origin, size: state.frame.size }
}

/// A rect method (-contentRectForBounds: and the like), by message so a
/// game's button can answer.
fn rect_for(this: Id, name: &str, rect: Rect) -> Rect {
    let s = runtime::sel(name);
    match runtime::imp_for(this, s) {
        Some(imp) => {
            let f: extern "C" fn(Id, Sel, Rect) -> Rect = unsafe { std::mem::transmute(imp) };
            f(this, s, rect)
        }
        None => rect,
    }
}

fn content_rect(this: Id) -> Rect {
    rect_for(this, "contentRectForBounds:", bounds_of(this))
}

/// The title label's text alignment for a content alignment: right for
/// right, else left (its frame fits the title).
fn title_alignment(horizontal: isize) -> u64 {
    if horizontal == 2 { 2 } else { 0 }
}

/// -_setupTitleView: made once, a UIButtonLabel over the title's place,
/// see-through and clipped, in the 15-point system font, cut in the
/// middle, with no shadow offset, added on top.
pub(crate) fn button_title_view(this: Id) -> Id {
    if let Some(label) = property_get(this, "titleLabel") {
        return label as Id;
    }
    let frame = rect_for(this, "titleRectForContentRect:", content_rect(this));
    let label = new_view_named("UIButtonLabel", frame);
    runtime::send(label, "setOpaque:", &[0]);
    runtime::send(label, "setClipsToBounds:", &[1]);
    property_set(label, "font", font(SYSTEM_FONT, 15.0) as u64);
    property_set(label, "lineBreakMode", 5);
    set_f(label, "shadowOffset.h", 0.0);
    property_set(label, "textAlignment", title_alignment(property_get(this, "contentHorizontalAlignment").unwrap_or(0) as isize));
    property_set(this, "titleLabel", label as u64);
    runtime::send(this, "addSubview:", &[label]);
    runtime::send(this, "setNeedsLayout", &[]);
    label
}

/// UILabel's own setter, as [super set...:] from a UIButtonLabel.
fn label_super<A>(this: Id, s: Sel, value: A) {
    let class = runtime::class_named("UILabel"); // before taking the lock: it locks too
    let imp = lock().lookup(class, s);
    if let Some(imp) = imp {
        unsafe { std::mem::transmute::<usize, extern "C" fn(Id, Sel, A)>(imp)(this, s, value) }
    }
}

/// -[UIButton _titleAttributesChanged], from the title label: the title's
/// size may have changed, so its button is laid out again.
fn title_attributes_changed(label: Id) {
    let button = view_state(label).superview;
    if class_is(button, "UIButton") {
        runtime::send(button, "setNeedsLayout", &[]);
    }
}

// UIButtonLabel's setters for what sizes the title (font, line break,
// fitting, shadow offset, alignment): UILabel's, then the button hears it.
extern "C" fn button_label_set_font(this: Id, s: Sel, font: Id) {
    let changed = get_id(this, "font") != font;
    label_super(this, s, font);
    if changed {
        title_attributes_changed(this);
    }
}
extern "C" fn button_label_set_int(this: Id, s: Sel, value: isize) {
    label_super(this, s, value);
    title_attributes_changed(this);
}
extern "C" fn button_label_set_bool(this: Id, s: Sel, value: u8) {
    label_super(this, s, value);
    title_attributes_changed(this);
}
extern "C" fn button_label_set_f64(this: Id, s: Sel, value: f64) {
    label_super(this, s, value);
    title_attributes_changed(this);
}
extern "C" fn button_label_set_size(this: Id, s: Sel, value: Size) {
    label_super(this, s, value);
    title_attributes_changed(this);
}

/// A decoded button's content for a state (-initWithCoder: takes its
/// content table as it is: no setter, no layout; the caller marks the
/// button once). `key` is the archive's UIButtonContent name.
pub(crate) fn button_take_content(this: Id, key: &str, state: usize, value: Id) {
    match key {
        "title" => state_set(this, "title", state, copied(value)),
        "titleColor" => state_set(this, "titleColor", state, value),
        "shadowColor" => state_set(this, "titleShadowColor", state, value),
        "image" => state_set_kept(this, "image", state, value),
        "backgroundImage" => state_set_kept(this, "backgroundImage", state, value),
        _ => {}
    }
}

/// A see-through, clipped picture view that takes no touches: a button's
/// background or picture (-_setupBackgroundView, -_setupImageView).
fn part_image_view(frame: Rect) -> Id {
    let view = new_view_named("UIImageView", frame);
    runtime::send(view, "setUserInteractionEnabled:", &[0]);
    runtime::send(view, "setOpaque:", &[0]);
    runtime::send(view, "setClipsToBounds:", &[1]);
    view
}

/// -_setupBackgroundView: made once, at the back.
fn button_background_view(this: Id) -> Id {
    if let Some(view) = property_get(this, "backgroundView") {
        return view as Id;
    }
    let view = part_image_view(rect_for(this, "backgroundRectForBounds:", bounds_of(this)));
    property_set(this, "backgroundView", view as u64);
    runtime::send(this, "insertSubview:atIndex:", &[view, 0]);
    view
}

/// -_setupImageView: made once, above the background and the pressed glow.
fn button_picture_view(this: Id) -> Id {
    if let Some(view) = property_get(this, "imageView") {
        return view as Id;
    }
    let view = part_image_view(rect_for(this, "imageRectForContentRect:", content_rect(this)));
    property_set(this, "imageView", view as u64);
    let glow = runtime::send(this, "viewWithTag:", &[GLOW_TAG]);
    match (glow, get_id(this, "backgroundView")) {
        (0, 0) => runtime::send(this, "insertSubview:atIndex:", &[view, 0]),
        (0, back) => runtime::send(this, "insertSubview:aboveSubview:", &[view, back]),
        (glow, _) => runtime::send(this, "insertSubview:aboveSubview:", &[view, glow]),
    };
    view
}

/// -titleLabel: the title view, the button laid out first.
extern "C" fn button_title_label(this: Id, _: Sel) -> Id {
    let label = button_title_view(this);
    runtime::send(this, "layoutIfNeeded", &[]);
    label
}

/// -imageView: the picture view, the button laid out first.
extern "C" fn button_image_view(this: Id, _: Sel) -> Id {
    let view = button_picture_view(this);
    runtime::send(this, "layoutIfNeeded", &[]);
    view
}

/// After a content setter UIKit lays the button out again, at once when
/// the state set is not the normal one and is the one on show.
fn content_changed(this: Id, state: usize) {
    runtime::send(this, "setNeedsLayout", &[]);
    if state != 0 && state == control_state(this) {
        runtime::send(this, "layoutIfNeeded", &[]);
    }
}

extern "C" fn button_set_title(this: Id, _: Sel, title: Id, state: usize) {
    state_set(this, "title", state, copied(title));
    content_changed(this, state);
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
            content_changed(this, state);
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

/// Whether the button keeps a value of its own under `key` for `state`
/// (else the normal state's shows: UIKit's "uses ... for normal state").
fn has_own(button: Id, key: &'static str, state: usize) -> bool {
    STATE_VALUES.lock().unwrap().as_ref().is_some_and(|values| values.contains_key(&(button, key, state)))
}

/// How a part's image view draws the picture under `key` (-[UIImageView
/// setDrawMode:], as -[UIButton layoutSubviews] sets it): a normal-state
/// picture shown pressed is drawn darker (2) with adjustsImageWhenHighlighted
/// unless the button glows instead, shown disabled dimmed (1) with
/// adjustsImageWhenDisabled; else as it is (0).
fn draw_mode(button: Id, key: &'static str, state: usize) -> usize {
    let uses_normal = !has_own(button, key, state);
    let darkens = get_flag(button, "adjustsImageWhenHighlighted", true) && !get_flag(button, "showsTouchWhenHighlighted", false);
    if uses_normal && darkens && state & 1 != 0 {
        2
    } else if uses_normal && get_flag(button, "adjustsImageWhenDisabled", true) && state & 2 != 0 {
        1
    } else {
        0
    }
}

/// The state whose picture under `key` the button shows in `state`: a
/// selected button still pressed, with no picture of its own for that,
/// shows its selected picture (UIKit falls back to the normal one, drawn
/// darker until the highlight goes; the owner chose not to show that dark
/// step). Else `state` (state_get falls back to normal from there).
fn shown_state(button: Id, key: &'static str, state: usize) -> usize {
    let selected = state & !1;
    if state & 5 == 5 && !has_own(button, key, state) && has_own(button, key, selected) {
        selected
    } else {
        state
    }
}

/// The pictures a press of the button would show that it doesn't show
/// now: for its background and its picture, the pressed state's own, or
/// the darker copy of the one it falls back to (the normal state's, as
/// UIKit's). A tap whose action selects the button also shows it selected
/// and still pressed until the highlight goes (UIControl lets go of it
/// 0.12 s after touch-down): its selected picture (shown_state). A press
/// may last one frame, so fw/pictures.rs starts their files beforehand.
pub fn button_press_pictures(button: Id) -> Vec<Id> {
    const KEYS: [&str; 2] = ["backgroundImage", "image"];
    let state = control_state(button);
    if state & 1 != 0 {
        return Vec::new();
    }
    let mut pressed = vec![state | 1];
    if state & 4 == 0 && KEYS.iter().any(|key| has_own(button, key, 4)) {
        pressed.push(state | 5);
    }
    let mut pictures = Vec::new();
    for &press in &pressed {
        for key in KEYS {
            let press = shown_state(button, key, press);
            let picture = match (has_own(button, key, press), state_get(button, key, press)) {
                (_, 0) => continue,
                (true, own) => own,
                (false, shown) if draw_mode(button, key, press) == 2 => crate::fw::graphics::darker_image(shown),
                (false, _) => continue,
            };
            if picture != 0 && !pictures.contains(&picture) {
                pictures.push(picture);
            }
        }
    }
    pictures
}

fn insets_of(this: Id, key: &'static str) -> Insets {
    property_get(this, key).map(|p| unsafe { *(p as *const Insets) }).unwrap_or_default()
}

fn inset(rect: Rect, by: Insets) -> Rect {
    Rect::new(rect.origin.x + by.left, rect.origin.y + by.top, rect.size.width - by.left - by.right, rect.size.height - by.top - by.bottom)
}

/// The title a button shows in `state`: its attributed title when it has
/// one (with that string), else its plain title (attributed: 0).
fn title_shown(button: Id, state: usize) -> (String, Id) {
    let attributed = state_get(button, "attributedTitle", state);
    if attributed != 0 {
        let text = crate::fw::attributed::text(attributed);
        if !text.is_empty() {
            return (text, attributed);
        }
    }
    (text_of(state_get(button, "title", state)), 0)
}

/// Where the picture and the title go in the content area, as UIButton
/// places them: side by side, placed by the content alignments
/// (UIControlContentHorizontal/VerticalAlignment: centre 0, left/top 1,
/// right/bottom 2, fill 3), each in the content area less its own insets.
/// The picture keeps its size (no bigger than the content) except that
/// Fill stretches it over its area; the title is sized in the title
/// label's font and lines (or the attributed title's own font), cut to the
/// room left.
fn place_parts(button: Id, content: Rect) -> (Rect, Rect) {
    let state = control_state(button);
    let room = |length: f64| length.max(0.0);
    let image = state_get(button, "image", state);
    let mut image_size = match image {
        0 => Size::default(),
        image => {
            let size = crate::fw::uikit::image_points(image);
            Size { width: size.width.min(room(content.size.width)), height: size.height.min(room(content.size.height)) }
        }
    };
    // The title label's settings (before it is made: a button's 15-point system font, cut in the middle).
    let label = get_id(button, "titleLabel");
    let mut font = font_name_and_size(if label != 0 { text_font(label, 0) } else { font(SYSTEM_FONT, 15.0) });
    let line_break = if label != 0 { property_get(label, "lineBreakMode").map_or(5, |v| v as isize) } else { 5 };
    let lines = if label != 0 { property_get(label, "numberOfLines").map_or(1, |v| v as isize) } else { 1 };
    let (title, attributed) = title_shown(button, state);
    if let font_object @ 1.. = if attributed != 0 { crate::fw::attributed::attribute(attributed, "NSFont") } else { 0 } {
        font = font_name_and_size(font_object);
    }
    let title_size = match title.is_empty() {
        true => Size::default(),
        false => {
            let (width, height) = crate::fw::graphics::text_box(&title, &font, room(content.size.width - image_size.width), lines, line_break);
            Size { width, height }
        }
    };
    let horizontal = property_get(button, "contentHorizontalAlignment").unwrap_or(0);
    let vertical = property_get(button, "contentVerticalAlignment").unwrap_or(0);
    let image_area = inset(content, insets_of(button, "imageEdgeInsets"));
    let title_area = inset(content, insets_of(button, "titleEdgeInsets"));
    // Fill: the picture takes the room its area leaves (across: beside the title).
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
    let image_rect = Rect { origin: Point { x: across(image_area, 0.0), y: down(image_area, image_size.height) }, size: image_size };
    let title_rect = Rect { origin: Point { x: across(title_area, image_size.width), y: down(title_area, title_size.height) }, size: title_size };
    (image_rect, title_rect)
}

/// -contentRectForBounds:: the bounds less the content insets.
extern "C" fn button_content_rect(this: Id, _: Sel, bounds: Rect) -> Rect {
    inset(bounds, insets_of(this, "contentEdgeInsets"))
}

/// -backgroundRectForBounds:: the bounds.
extern "C" fn button_background_rect(_this: Id, _: Sel, bounds: Rect) -> Rect {
    bounds
}

extern "C" fn button_image_rect(this: Id, _: Sel, content: Rect) -> Rect {
    place_parts(this, content).0
}

extern "C" fn button_title_rect(this: Id, _: Sel, content: Rect) -> Rect {
    place_parts(this, content).1
}

/// UIButton's normal title colours (+_defaultNormalTitleColor): white for
/// a custom button, the system blue for the others; made once.
fn plain_title_color(button: Id) -> Id {
    static COLORS: [AtomicUsize; 2] = [AtomicUsize::new(0), AtomicUsize::new(0)];
    let custom = get_id(button, "buttonType") == 0;
    let slot = &COLORS[custom as usize];
    match slot.load(Ordering::Relaxed) {
        0 => {
            let color = colored(if custom { [1.0; 4] } else { [0.0, 122.0 / 255.0, 1.0, 1.0] });
            crate::fw::counted::retain(color);
            slot.store(color, Ordering::Relaxed);
            color
        }
        color => color,
    }
}

fn set_hidden(view: Id, hidden: bool) {
    if view != 0 {
        runtime::send(view, "setHidden:", &[hidden as usize]);
    }
}

/// -[UIButton layoutSubviews]: each part set to the current state's
/// content, made when first needed: the background picture over
/// -backgroundRectForBounds:, the picture over -imageRectForContentRect:,
/// the title (attributed, else plain in the state's colour and shadow
/// colour) over -titleRectForContentRect:; a part with nothing to show is
/// hidden. The pressed glow stays in the middle.
extern "C" fn button_layout_subviews(this: Id, _: Sel) {
    let bounds = bounds_of(this);
    let content = rect_for(this, "contentRectForBounds:", bounds);
    let state = control_state(this);
    // With UIButton's own rect methods the title is measured once for both.
    let own = |name: &str, imp: usize| runtime::imp_for(this, runtime::sel(name)) == Some(imp);
    let placed = (own("imageRectForContentRect:", button_image_rect as usize) && own("titleRectForContentRect:", button_title_rect as usize))
        .then(|| place_parts(this, content));
    let image_rect = || placed.map_or_else(|| rect_for(this, "imageRectForContentRect:", content), |p| p.0);
    let title_rect = || placed.map_or_else(|| rect_for(this, "titleRectForContentRect:", content), |p| p.1);
    let shown = |key| shown_state(this, key, state);
    let background = state_get(this, "backgroundImage", shown("backgroundImage"));
    if background != 0 {
        let view = button_background_view(this);
        runtime::send(view, "setImage:", &[background]);
        crate::fw::uikit::send_rect(view, "setFrame:", rect_for(this, "backgroundRectForBounds:", bounds));
        set_hidden(view, false);
        runtime::send(view, "setDrawMode:", &[draw_mode(this, "backgroundImage", shown("backgroundImage"))]);
    } else {
        set_hidden(get_id(this, "backgroundView"), true);
    }
    let image = state_get(this, "image", shown("image"));
    if image != 0 {
        let view = button_picture_view(this);
        runtime::send(view, "setImage:", &[image]);
        crate::fw::uikit::send_rect(view, "setFrame:", image_rect());
        set_hidden(view, false);
        runtime::send(view, "setDrawMode:", &[draw_mode(this, "image", shown("image"))]);
        if get_f(view, "alpha", 1.0) != 1.0 {
            send_struct(view, "setAlpha:", 1.0f64);
        }
    } else {
        set_hidden(get_id(this, "imageView"), true);
    }
    let (title, attributed) = title_shown(this, state);
    if !title.is_empty() {
        let label = button_title_view(this);
        if attributed != 0 {
            if get_id(label, "attributedText") != attributed {
                runtime::send(label, "setAttributedText:", &[attributed]);
            }
            // UILabel's text is the attributed string's.
            if text_of(get_id(label, "text")) != title {
                property_set(label, "text", string::make(&title) as u64);
            }
        } else {
            if get_id(label, "attributedText") != 0 {
                runtime::send(label, "setAttributedText:", &[0]);
            }
            if text_of(get_id(label, "text")) != title {
                runtime::send(label, "setText:", &[state_get(this, "title", state)]);
            }
            let color = match state_get(this, "titleColor", state) {
                0 => plain_title_color(this),
                color => color,
            };
            runtime::send(label, "setTextColor:", &[color]);
            runtime::send(label, "setShadowColor:", &[state_get(this, "titleShadowColor", state)]);
        }
        crate::fw::uikit::send_rect(label, "setFrame:", title_rect());
        set_hidden(label, false);
    } else {
        set_hidden(get_id(this, "titleLabel"), true);
    }
    let glow = runtime::send(this, "viewWithTag:", &[GLOW_TAG]);
    if glow != 0 {
        center_glow(this, glow);
    }
}

/// UIButton's: the title label's alignment follows (right, else left), and
/// the button is laid out again.
extern "C" fn button_set_horizontal(this: Id, s: Sel, value: isize) {
    control_set_horizontal(this, s, value);
    let label = get_id(this, "titleLabel");
    if label != 0 {
        property_set(label, "textAlignment", title_alignment(value));
    }
    runtime::send(this, "setNeedsLayout", &[]);
}

extern "C" fn button_set_vertical(this: Id, s: Sel, value: isize) {
    control_set_vertical(this, s, value);
    runtime::send(this, "setNeedsLayout", &[]);
}

/// The darkening settings lay the button out again (its parts' draw modes follow).
extern "C" fn button_set_adjusts_highlighted_and_lay_out(this: Id, s: Sel, value: u8) {
    button_set_adjusts_highlighted(this, s, value);
    runtime::send(this, "setNeedsLayout", &[]);
}

extern "C" fn button_set_adjusts_disabled_and_lay_out(this: Id, s: Sel, value: u8) {
    button_set_adjusts_disabled(this, s, value);
    runtime::send(this, "setNeedsLayout", &[]);
}

extern "C" fn button_size_that_fits(this: Id, _: Sel, _size: Size) -> Size {
    let title = text_of(state_get(this, "title", control_state(this)));
    let text = text_size(&title, 15.0, f64::INFINITY, 1);
    Size { width: text.width.max(30.0), height: text.height.max(30.0) }
}

// (UIButton has no -sizeToFit of its own: UIView's asks -sizeThatFits:
// and goes through -setFrame:, so the parts and autoresizing follow.)

/// UIControl's (so UIView's) own geometry setter, as [super set...:].
fn control_super_rect(this: Id, s: Sel, rect: Rect) {
    let class = runtime::class_named("UIControl"); // before taking the lock: it locks too
    let imp = lock().lookup(class, s);
    if let Some(imp) = imp {
        unsafe { std::mem::transmute::<usize, extern "C" fn(Id, Sel, Rect)>(imp)(this, s, rect) }
    }
}

/// -[UIButton setFrame:]: a new size marks the button and lays it out at
/// once (-layoutBelowIfNeeded), so inside a size animation its parts move
/// with it.
extern "C" fn button_set_frame(this: Id, s: Sel, frame: Rect) {
    let old = crate::fw::uikit::view_frame(this, s).size;
    control_super_rect(this, s, frame);
    if old != frame.size {
        runtime::send(this, "setNeedsLayout", &[]);
        runtime::send(this, "layoutIfNeeded", &[]);
    }
}

/// -[UIButton setBounds:]: a new size marks the button (laid out in the pass).
extern "C" fn button_set_bounds(this: Id, s: Sel, bounds: Rect) {
    let old = view_state(this).frame.size;
    control_super_rect(this, s, bounds);
    if old != bounds.size {
        runtime::send(this, "setNeedsLayout", &[]);
    }
}

/// -[UIButton dealloc]: the button lets go of its parts and its content
/// table (the pictures it kept), so a freed game button doesn't keep them;
/// then UIView's.
extern "C" fn button_dealloc(this: Id, s: Sel) {
    for key in ["backgroundView", "imageView"] {
        let view = get_id(this, key);
        if view != 0 {
            runtime::send(view, "setImage:", &[0]);
        }
    }
    let kept: Vec<Id> = {
        let mut values = STATE_VALUES.lock().unwrap();
        let mut kept = Vec::new();
        if let Some(values) = values.as_mut() {
            values.retain(|&(button, key, _), &mut value| {
                if button != this {
                    return true;
                }
                if key == "image" || key == "backgroundImage" {
                    kept.push(value);
                }
                false
            });
        }
        kept
    };
    // Outside the lock: letting go may clean a picture up.
    for picture in kept {
        crate::fw::counted::release(picture);
    }
    for key in ["backgroundView", "imageView", "titleLabel"] {
        let view = get_id(this, key);
        if view != 0 {
            property_set(this, key, 0);
            crate::fw::counted::release(view);
        }
    }
    let class = runtime::class_named("UIControl"); // before taking the lock: it locks too
    let imp = lock().lookup(class, s);
    if let Some(imp) = imp {
        unsafe { std::mem::transmute::<usize, extern "C" fn(Id, Sel)>(imp)(this, s) }
    }
}

macro_rules! insets_property {
    ($get:ident, $set:ident, $key:literal) => {
        insets_property!($get, $set, $key, |_this: Id| {});
    };
    // `then` runs after the change (a button lays itself out again).
    ($get:ident, $set:ident, $key:literal, $then:expr) => {
        extern "C" fn $get(this: Id, _: Sel) -> Insets {
            match property_get(this, $key) {
                Some(p) => unsafe { *(p as *const Insets) },
                None => Insets::default(),
            }
        }
        extern "C" fn $set(this: Id, _: Sel, value: Insets) {
            property_set(this, $key, leak(value) as u64);
            ($then)(this);
        }
    };
}
fn lay_out_again(this: Id) {
    runtime::send(this, "setNeedsLayout", &[]);
}
insets_property!(button_content_insets, button_set_content_insets, "contentEdgeInsets", lay_out_again);
insets_property!(button_title_insets, button_set_title_insets, "titleEdgeInsets", lay_out_again);
insets_property!(button_image_insets, button_set_image_insets, "imageEdgeInsets", lay_out_again);
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

extern "C" fn scroll_content_offset(this: Id, _: Sel) -> Point {
    view_state(this).bounds_origin
}

/// A scroll view's content offset is its bounds origin: what shows under the
/// top-left. Setting it (rounded to the screen's pixels, as UIKit does)
/// moves the content through -setBounds: and tells the delegate, so games
/// that watch contentOffset or scrollViewDidScroll: see every change (a
/// subclass's own -setContentOffset: calls this as its super); the same
/// offset again does nothing.
extern "C" fn scroll_set_content_offset(this: Id, _: Sel, value: Point) {
    let scale = crate::fw::uikit::screen_points().2.max(1.0);
    let value = Point { x: (value.x * scale).round() / scale, y: (value.y * scale).round() / scale };
    let state = view_state(this);
    if state.bounds_origin == value {
        return;
    }
    crate::fw::uikit::send_rect(this, "setBounds:", Rect { origin: value, size: state.frame.size });
    // UIKit lays a scroll view out as it scrolls (games add the rows coming
    // into view in -layoutSubviews).
    runtime::send(this, "setNeedsLayout", &[]);
    tell(get_id(this, "delegate"), "scrollViewDidScroll:", &[this]);
}

/// Animated, the content slides there over 0.3 s and the delegate then
/// hears scrollViewDidEndScrollingAnimation:; otherwise -setContentOffset:.
extern "C" fn scroll_set_content_offset_animated(this: Id, _: Sel, offset: Point, animated: u8) {
    if animated == 0 {
        finish_slide(this);
        send_point(this, "setContentOffset:", offset);
        return;
    }
    if content_offset_of(this) != offset {
        start_slide(this, offset, SlideEnd::Animation);
    }
}

property!(scroll_delays_content, scroll_set_delays_content, "delaysContentTouches", bool, true);
property!(scroll_can_cancel, scroll_set_can_cancel, "canCancelContentTouches", bool, true);

/// UIScrollView's -touchesShouldBegin:withEvent:inContentView:: YES.
extern "C" fn scroll_touches_should_begin(_this: Id, _: Sel, _touches: Id, _event: Id, _view: Id) -> bool {
    true
}

/// UIScrollView's -touchesShouldCancelInContentView:: NO for an enabled
/// control (a drag that starts on a pressed button doesn't scroll), else YES.
extern "C" fn scroll_touches_should_cancel(_this: Id, _: Sel, view: Id) -> bool {
    !(class_is(view, "UIControl") && runtime::send(view, "isEnabled", &[]) & 0xFF != 0)
}
extern "C" fn scroll_rect_visible(_this: Id, _: Sel, _rect: Rect, _animated: u8) {}

property!(scroll_delegate, scroll_set_delegate, "delegate", assign, 0);
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

// Set while the finger drags or the content coasts; read by the game.
extern "C" fn scroll_is_dragging(this: Id, _: Sel) -> bool {
    get_flag(this, "dragging", false)
}
extern "C" fn scroll_is_tracking(this: Id, _: Sel) -> bool {
    get_flag(this, "tracking", false)
}
extern "C" fn scroll_is_decelerating(this: Id, _: Sel) -> bool {
    get_flag(this, "decelerating", false)
}

extern "C" fn scroll_init_with_frame(this: Id, _: Sel, frame: Rect) -> Id {
    super_init_frame(this, "UIView", frame);
    mark_not_drawn(this);
    this
}

// ----- scroll view dragging -----

/// Points a finger may move before a scroll view starts dragging (UIKit's pan slop).
const SCROLL_SLOP: f64 = 10.0;
/// The axes a scroll view can move on, as bit flags.
const AXIS_H: u8 = 1;
const AXIS_V: u8 = 2;
/// A finger slower than this (points/s) when it lifts stops dead instead of coasting.
const COAST_MIN_SPEED: f64 = 20.0;
/// A coast slower than this has run out.
const COAST_STOP_SPEED: f64 = 5.0;
/// UIScrollViewDecelerationRateNormal: the speed keeps 0.998 each millisecond.
const COAST_RATE: f64 = 0.998;
/// How often a coast steps (seconds).
const COAST_STEP: f64 = 1.0 / 60.0;

/// One finger's drag on a scroll view.
#[derive(Clone)]
struct ScrollDrag {
    /// The enclosing scroll views, innermost first, each with the axes it can move.
    candidates: Vec<(Id, u8)>,
    /// The scroll view dragging (0 until the finger crosses the slop).
    view: Id,
    /// The axis it moves on (AXIS_H or AXIS_V).
    axis: u8,
    start: Point,
    last: Point,
    last_time: f64,
    /// The finger's speed (points/s), smoothed, for the coast.
    velocity: Point,
    dragging: bool,
}

static SCROLL_DRAGS: Mutex<Option<HashMap<i32, ScrollDrag>>> = Mutex::new(None);

/// A scroll view coasting after the finger let go.
#[derive(Clone, Copy)]
struct Coast {
    /// The content's speed (points/s), opposite the finger's.
    velocity: Point,
    last: f64,
}

static COASTING: Mutex<Option<HashMap<Id, Coast>>> = Mutex::new(None);

fn with_drag<R>(finger: i32, f: impl FnOnce(&mut ScrollDrag) -> R) -> Option<R> {
    SCROLL_DRAGS.lock().unwrap().get_or_insert_with(HashMap::new).get_mut(&finger).map(f)
}

fn remove_drag(finger: i32) -> Option<ScrollDrag> {
    SCROLL_DRAGS.lock().unwrap().get_or_insert_with(HashMap::new).remove(&finger)
}

/// Sends a method taking a point (setContentOffset:) through the game's own
/// override if it has one, so a subclass sees every change.
fn send_point(view: Id, name: &str, point: Point) {
    let s = runtime::sel(name);
    if let Some(imp) = runtime::imp_for(view, s) {
        let f: extern "C" fn(Id, Sel, Point) = unsafe { std::mem::transmute(imp) };
        f(view, s, point);
    }
}

/// Which axes a scroll view can move on: its content is taller or wider
/// than its bounds, or it bounces on that axis anyway.
fn movable_axes(view: Id) -> u8 {
    let size = view_state(view).frame.size;
    let (width, height) = (get_f(view, "contentSize.w", 0.0), get_f(view, "contentSize.h", 0.0));
    let mut axes = 0;
    if width > size.width || get_flag(view, "alwaysBounceHorizontal", false) {
        axes |= AXIS_H;
    }
    if height > size.height || get_flag(view, "alwaysBounceVertical", false) {
        axes |= AXIS_V;
    }
    axes
}

/// The scroll views holding `view` that can scroll, innermost first.
fn enclosing_scroll_views(view: Id) -> Vec<(Id, u8)> {
    let mut list = Vec::new();
    let mut current = view;
    for _ in 0..256 {
        if current == 0 {
            break;
        }
        if class_is(current, "UIScrollView") && get_flag(current, "scrollEnabled", true) {
            let axes = movable_axes(current);
            if axes != 0 {
                list.push((current, axes));
            }
        }
        current = view_state(current).superview;
    }
    list
}

/// A scroll view's content offset: its bounds origin.
fn content_offset_of(view: Id) -> Point {
    view_state(view).bounds_origin
}

/// `offset` kept between the content's start (less the top/left inset) and
/// where its end reaches the bottom/right edge (plus that inset).
fn clamp_content_offset(view: Id, offset: Point) -> Point {
    let size = view_state(view).frame.size;
    let inset: Insets = match property_get(view, "contentInset") {
        Some(p) => unsafe { *(p as *const Insets) },
        None => Insets::default(),
    };
    let (min_x, max_x) = (-inset.left, (get_f(view, "contentSize.w", 0.0) - size.width + inset.right).max(-inset.left));
    let (min_y, max_y) = (-inset.top, (get_f(view, "contentSize.h", 0.0) - size.height + inset.bottom).max(-inset.top));
    Point { x: offset.x.clamp(min_x, max_x), y: offset.y.clamp(min_y, max_y) }
}

/// The finger's speed in points/s, averaged with the drag's earlier speed.
/// How far the finger went from `from` to `to` (window points) in the
/// scroll view's frame: its superview's points, which turn with any turned
/// view on the way but don't move as the scroll view scrolls.
fn finger_motion(scroll: Id, from: Point, to: Point) -> Point {
    let parent = view_state(scroll).superview;
    if parent == 0 {
        return Point { x: to.x - from.x, y: to.y - from.y };
    }
    let (a, b) = (crate::fw::touch::point_in_view(parent, from), crate::fw::touch::point_in_view(parent, to));
    Point { x: b.x - a.x, y: b.y - a.y }
}

/// The finger's smoothed speed after a move of `delta` (scroll view frame points).
fn drag_velocity(drag: &ScrollDrag, delta: Point, now: f64) -> Point {
    let dt = (now - drag.last_time).max(0.0);
    if dt <= 1e-4 {
        return drag.velocity;
    }
    let speed = Point { x: delta.x / dt, y: delta.y / dt };
    Point { x: (drag.velocity.x + speed.x) / 2.0, y: (drag.velocity.y + speed.y) / 2.0 }
}

/// What a scroll view does with one finger change, like a pan recognizer:
/// nothing, or it takes the touch and drags the content.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ScrollVerdict {
    /// The touch flows on to the view as usual.
    Pass,
    /// The scroll view just took the touch: cancel the content view's touch.
    CancelNow,
    /// The scroll view owns the touch (already dragging): the view hears nothing.
    Swallowed,
}

/// From fw/touch.rs, for each finger change, before the view hears it: a
/// scroll view holding the touched view starts dragging past the slop and
/// moves its content with the finger. `taken` says a recognizer already
/// owns the finger, in which case the scroll view steps aside;
/// `content_heard` that the touched view heard the touch begin (so the
/// scroll view must be allowed to cancel it before it drags).
pub(crate) fn scroll_touch(view: Id, _touch: Id, finger: i32, phase: Phase, point: Point, taken: bool, content_heard: bool) -> ScrollVerdict {
    // A recognizer already owns this finger (it took the touch earlier): the
    // scroll view steps aside and leaves the finger to it.
    if taken {
        remove_drag(finger);
        return ScrollVerdict::Pass;
    }
    let now = crate::fw::runloop::media_time();
    match phase {
        Phase::Began => {
            // A new finger stops any coasting, as UIKit's does.
            stop_all_coasting();
            let candidates = enclosing_scroll_views(view);
            if candidates.is_empty() {
                remove_drag(finger);
                return ScrollVerdict::Pass;
            }
            SCROLL_DRAGS.lock().unwrap().get_or_insert_with(HashMap::new).insert(
                finger,
                ScrollDrag { candidates, view: 0, axis: 0, start: point, last: point, last_time: now, velocity: Point::default(), dragging: false },
            );
            ScrollVerdict::Pass
        }
        Phase::Moved => {
            let Some(drag) = with_drag(finger, |d| d.clone()) else { return ScrollVerdict::Pass };
            if drag.dragging {
                let delta = finger_motion(drag.view, drag.last, point);
                let velocity = drag_velocity(&drag, delta, now);
                send_point(drag.view, "setContentOffset:", dragged_offset(drag.view, delta));
                with_drag(finger, |d| {
                    d.last = point;
                    d.last_time = now;
                    d.velocity = velocity;
                });
                return ScrollVerdict::Swallowed;
            }
            with_drag(finger, |d| {
                d.last = point;
                d.last_time = now;
            });
            // The innermost scroll view the finger has moved past the slop
            // on an axis it can move wins (each measures in its own frame).
            let mut past_slop = false;
            let mut chosen = None;
            for &(scroll, axes) in &drag.candidates {
                let moved = finger_motion(scroll, drag.start, point);
                let horizontal = moved.x.abs() > moved.y.abs();
                let (axis, distance) = if horizontal { (AXIS_H, moved.x.abs()) } else { (AXIS_V, moved.y.abs()) };
                if distance <= SCROLL_SLOP {
                    continue;
                }
                past_slop = true;
                if axes & axis != 0 {
                    chosen = Some((scroll, axis));
                    break;
                }
            }
            let Some((scroll, axis)) = chosen else {
                if past_slop {
                    remove_drag(finger);
                }
                return ScrollVerdict::Pass;
            };
            // Content that already heard the touch keeps it unless the scroll
            // view may cancel it (canCancelContentTouches, then
            // -touchesShouldCancelInContentView:, NO for an enabled control).
            if content_heard
                && (!get_flag(scroll, "canCancelContentTouches", true) || runtime::send(scroll, "touchesShouldCancelInContentView:", &[view]) & 0xFF == 0)
            {
                remove_drag(finger);
                return ScrollVerdict::Pass;
            }
            let delta = finger_motion(scroll, drag.last, point);
            property_set(scroll, "dragging", 1);
            property_set(scroll, "tracking", 1);
            tell(get_id(scroll, "delegate"), "scrollViewWillBeginDragging:", &[scroll]);
            send_point(scroll, "setContentOffset:", dragged_offset(scroll, delta));
            with_drag(finger, |d| {
                d.view = scroll;
                d.axis = axis;
                d.dragging = true;
            });
            ScrollVerdict::CancelNow
        }
        Phase::Ended | Phase::Cancelled => {
            let Some(drag) = remove_drag(finger) else { return ScrollVerdict::Pass };
            if !drag.dragging {
                return ScrollVerdict::Pass;
            }
            let scroll = drag.view;
            property_set(scroll, "dragging", 0);
            property_set(scroll, "tracking", 0);
            let paging = get_flag(scroll, "pagingEnabled", false);
            let offset = content_offset_of(scroll);
            // Let go past an end, it slides back; paging slides to its page.
            let content_velocity = Point { x: -drag.velocity.x, y: -drag.velocity.y };
            let mut settle = if paging { page_target(scroll, drag.axis, content_velocity) } else { clamp_content_offset(scroll, offset) };
            let speed = drag.velocity.x.hypot(drag.velocity.y);
            let mut coasting = !paging && settle == offset && speed >= COAST_MIN_SPEED;
            // The delegate hears where it would stop and may move that
            // (-_endPanWithEvent:); moved, the content slides to the new place.
            // A paging scroll view isn't asked (iOS 6 skips it when pagingEnabled).
            let target = if coasting { coast_end(scroll, content_velocity) } else { settle };
            let chosen = if paging { target } else { will_end_dragging(scroll, Point { x: content_velocity.x / 1000.0, y: content_velocity.y / 1000.0 }, target) };
            if chosen != target {
                settle = clamp_content_offset(scroll, chosen);
                coasting = false;
            }
            let sliding = settle != offset;
            tell(get_id(scroll, "delegate"), "scrollViewDidEndDragging:willDecelerate:", &[scroll, (coasting || sliding) as usize]);
            if sliding {
                start_slide(scroll, settle, SlideEnd::Deceleration);
            } else if coasting {
                start_coast(scroll, Point { x: -drag.velocity.x, y: -drag.velocity.y });
            }
            ScrollVerdict::Swallowed
        }
    }
}

/// A scroll view follows a finger or coasts: what shows changes each refresh.
pub(crate) fn scrolling() -> bool {
    SCROLL_DRAGS.lock().unwrap().as_ref().is_some_and(|m| m.values().any(|d| d.dragging))
        || COASTING.lock().unwrap().as_ref().is_some_and(|m| !m.is_empty())
        || SLIDES.lock().unwrap().as_ref().is_some_and(|m| !m.is_empty())
}

/// Whether a scroll view is tracking a scroll: a finger dragging, the coast
/// or bounce after it, or a paging scroll settling (UIKit runs the main run
/// loop in UITrackingRunLoopMode then). -setContentOffset:animated: is not
/// tracking: Core Animation slides that content without the scroll view's
/// own tracking loop.
pub(crate) fn tracking() -> bool {
    #[cfg(test)]
    if let Some(on) = TEST_TRACKING.with(|on| on.get()) {
        return on;
    }
    if SCROLL_DRAGS.lock().unwrap().as_ref().is_some_and(|m| m.values().any(|d| d.dragging)) {
        return true;
    }
    if COASTING.lock().unwrap().as_ref().is_some_and(|m| !m.is_empty()) {
        return true;
    }
    SLIDES.lock().unwrap().as_ref().is_some_and(|m| m.values().any(|s| s.end == SlideEnd::Deceleration))
}

#[cfg(test)]
thread_local! {
    /// For tests: this thread's main-loop turns run as if a scroll tracked
    /// (Some) or did not (None: the real scroll views say).
    static TEST_TRACKING: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// For tests: makes this thread's main-loop turns run in UITrackingRunLoopMode
/// (Some) or lets the real scroll views say (None).
#[cfg(test)]
pub(crate) fn set_test_tracking(on: Option<bool>) {
    TEST_TRACKING.with(|tracking| tracking.set(on));
}

/// Where the content goes for a finger move of `delta`: with the finger
/// inside the content's range; past an end, a bouncing scroll view (on an
/// axis it can move) follows at half the finger's speed, as UIKit's rubber
/// band does; one that doesn't bounce stops at the end.
fn dragged_offset(view: Id, delta: Point) -> Point {
    let offset = content_offset_of(view);
    let edge_low = clamp_content_offset(view, Point { x: f64::NEG_INFINITY, y: f64::NEG_INFINITY });
    let edge_high = clamp_content_offset(view, Point { x: f64::INFINITY, y: f64::INFINITY });
    let bounces = get_flag(view, "bounces", true);
    let axes = movable_axes(view);
    let along = |at: f64, by: f64, low: f64, high: f64, rubber: bool| -> f64 {
        if !rubber {
            return (at - by).clamp(low, high);
        }
        // In finger points (past an end each content point took two), then back.
        let finger = if at > high { high + 2.0 * (at - high) } else if at < low { low - 2.0 * (low - at) } else { at };
        let moved = finger - by;
        if moved > high {
            high + (moved - high) / 2.0
        } else if moved < low {
            low - (low - moved) / 2.0
        } else {
            moved
        }
    };
    Point {
        x: along(offset.x, delta.x, edge_low.x, edge_high.x, bounces && axes & AXIS_H != 0),
        y: along(offset.y, delta.y, edge_low.y, edge_high.y, bounces && axes & AXIS_V != 0),
    }
}

// ----- content sliding to a place (animated offsets, bouncing back, paging) -----

/// How long the content takes to slide to a set offset (UIKit's scrolling animation).
const SLIDE_TIME: f64 = 0.3;

/// What the delegate hears when a slide ends.
#[derive(Clone, Copy, PartialEq, Debug)]
enum SlideEnd {
    /// -setContentOffset:animated:: scrollViewDidEndScrollingAnimation:.
    Animation,
    /// Bouncing back or settling on a page: the scroll view decelerates.
    Deceleration,
}

#[derive(Clone, Copy)]
struct Slide {
    from: Point,
    to: Point,
    start: f64,
    end: SlideEnd,
}

static SLIDES: Mutex<Option<HashMap<Id, Slide>>> = Mutex::new(None);

fn start_slide(view: Id, to: Point, end: SlideEnd) {
    finish_slide(view);
    if end == SlideEnd::Deceleration {
        property_set(view, "decelerating", 1);
        tell(get_id(view, "delegate"), "scrollViewWillBeginDecelerating:", &[view]);
    }
    let slide = Slide { from: content_offset_of(view), to, start: crate::fw::runloop::media_time(), end };
    SLIDES.lock().unwrap().get_or_insert_with(HashMap::new).insert(view, slide);
    crate::fw::runloop::post_after(coast_delay(), Box::new(move || slide_tick(view)));
}

fn slide_tick(view: Id) {
    crate::fw::pictures::draw_soon();
    if slide_step(view, crate::fw::runloop::media_time()) {
        crate::fw::runloop::post_after(coast_delay(), Box::new(move || slide_tick(view)));
    }
}

/// One slide step at `now` (easing out); false once it has arrived (or no longer slides).
fn slide_step(view: Id, now: f64) -> bool {
    let Some(slide) = SLIDES.lock().unwrap().as_ref().and_then(|m| m.get(&view)).copied() else {
        return false;
    };
    let t = ((now - slide.start) / SLIDE_TIME).clamp(0.0, 1.0);
    let eased = 1.0 - (1.0 - t).powi(3);
    let at = Point { x: slide.from.x + (slide.to.x - slide.from.x) * eased, y: slide.from.y + (slide.to.y - slide.from.y) * eased };
    send_point(view, "setContentOffset:", at);
    if t >= 1.0 {
        finish_slide(view);
        return false;
    }
    true
}

/// A slide ends (arrived, replaced, or stopped by a new finger): the delegate hears it.
fn finish_slide(view: Id) {
    let Some(slide) = SLIDES.lock().unwrap().as_mut().and_then(|m| m.remove(&view)) else { return };
    match slide.end {
        SlideEnd::Animation => drop(tell(get_id(view, "delegate"), "scrollViewDidEndScrollingAnimation:", &[view])),
        SlideEnd::Deceleration => {
            property_set(view, "decelerating", 0);
            tell(get_id(view, "delegate"), "scrollViewDidEndDecelerating:", &[view]);
        }
    }
}

/// Where a coast at `velocity` (content points a second) stops: the speed
/// keeps COAST_RATE each millisecond, so it runs velocity / -ln(rate) more.
fn coast_end(view: Id, velocity: Point) -> Point {
    let k = -1000.0 * COAST_RATE.ln();
    let offset = content_offset_of(view);
    clamp_content_offset(view, Point { x: offset.x + velocity.x / k, y: offset.y + velocity.y / k })
}

/// -scrollViewWillEndDragging:withVelocity:targetContentOffset: (velocity in
/// points a millisecond); what the delegate leaves in the target is returned.
/// A 32-bit game's method gets the target as a typed guest temporary, copied
/// back after the call (guest32 convert.rs GuestOut).
fn will_end_dragging(view: Id, velocity: Point, target: Point) -> Point {
    let delegate = get_id(view, "delegate");
    let name = "scrollViewWillEndDragging:withVelocity:targetContentOffset:";
    let mut chosen = target;
    if delegate != 0 && runtime::responds(delegate, name) {
        let s = runtime::sel(name);
        if let Some(imp) = runtime::imp_for(delegate, s) {
            let f: extern "C" fn(Id, Sel, Id, Point, *mut Point) = unsafe { std::mem::transmute(imp) };
            f(delegate, s, view, velocity, &mut chosen);
        }
    }
    chosen
}

/// Starts the content coasting after the finger let go, on the main loop.
fn start_coast(view: Id, velocity: Point) {
    property_set(view, "decelerating", 1);
    tell(get_id(view, "delegate"), "scrollViewWillBeginDecelerating:", &[view]);
    COASTING.lock().unwrap().get_or_insert_with(HashMap::new).insert(view, Coast { velocity, last: crate::fw::runloop::media_time() });
    crate::fw::runloop::post_after(coast_delay(), Box::new(move || coast_tick(view)));
}

/// Until the next coast step: the screen's next refresh, so the content
/// moves once per refresh at the screen's own rate (1/60 s before any is heard).
fn coast_delay() -> f64 {
    let now = crate::fw::runloop::media_time();
    match crate::fw::vsync::grid() {
        (origin, step) if step > 0.0 => crate::fw::vsync::next_on_grid(origin, step, now) - now,
        _ => COAST_STEP,
    }
}

fn coast_tick(view: Id) {
    // Each step lands on a refresh: its list goes now, not a refresh later.
    crate::fw::pictures::draw_soon();
    if coast_step(view, crate::fw::runloop::media_time()) {
        crate::fw::runloop::post_after(coast_delay(), Box::new(move || coast_tick(view)));
    }
}

/// One coast step at `now`: slows the content and moves it; false when it
/// has stopped (or no longer coasts).
fn coast_step(view: Id, now: f64) -> bool {
    let Some(coast) = COASTING.lock().unwrap().as_ref().and_then(|m| m.get(&view)).copied() else {
        return false;
    };
    let dt = (now - coast.last).max(0.0);
    let keep = COAST_RATE.powf(dt * 1000.0);
    let velocity = Point { x: coast.velocity.x * keep, y: coast.velocity.y * keep };
    let offset = content_offset_of(view);
    let next = clamp_content_offset(view, Point {
        x: offset.x + velocity.x * dt,
        y: offset.y + velocity.y * dt,
    });
    send_point(view, "setContentOffset:", next);
    // Stopped: too slow, or held at an edge (the clamp left it where it was).
    if velocity.x.hypot(velocity.y) < COAST_STOP_SPEED || content_offset_of(view) == offset {
        stop_coast(view);
        return false;
    }
    COASTING.lock().unwrap().as_mut().and_then(|m| m.get_mut(&view)).map(|c| {
        c.velocity = velocity;
        c.last = now;
    });
    true
}

fn stop_coast(view: Id) {
    let was = COASTING.lock().unwrap().as_mut().map(|m| m.remove(&view).is_some()).unwrap_or(false);
    if !was {
        return;
    }
    property_set(view, "decelerating", 0);
    tell(get_id(view, "delegate"), "scrollViewDidEndDecelerating:", &[view]);
}

/// Every coast or slide stops the moment a new finger comes down.
fn stop_all_coasting() {
    let views: Vec<Id> = COASTING.lock().unwrap().as_ref().map(|m| m.keys().copied().collect()).unwrap_or_default();
    for view in views {
        stop_coast(view);
    }
    let sliding: Vec<Id> = SLIDES.lock().unwrap().as_ref().map(|m| m.keys().copied().collect()).unwrap_or_default();
    for view in sliding {
        finish_slide(view);
    }
}

/// UIKit drops a paging scroll view's let-go velocity under 0.25 points a
/// millisecond (-[UIScrollView _endPanWithEvent:]: squared under 0.0625).
const PAGE_FLICK_SPEED: f64 = 250.0;

/// With paging on, the drag settles on a whole page: the next one the way
/// the content was flicked (`velocity`, content points a second), else the
/// nearest.
fn page_target(view: Id, axis: u8, velocity: Point) -> Point {
    let size = view_state(view).frame.size;
    let offset = content_offset_of(view);
    let flicked = velocity.x.hypot(velocity.y) >= PAGE_FLICK_SPEED;
    let page = |at: f64, speed: f64, length: f64| {
        let pages = at / length;
        let whole = if flicked && speed > 0.0 {
            pages.ceil()
        } else if flicked && speed < 0.0 {
            pages.floor()
        } else {
            pages.round()
        };
        whole * length
    };
    let mut target = offset;
    if axis & AXIS_H != 0 && size.width > 0.0 {
        target.x = page(offset.x, velocity.x, size.width);
    }
    if axis & AXIS_V != 0 && size.height > 0.0 {
        target.y = page(offset.y, velocity.y, size.height);
    }
    clamp_content_offset(view, target)
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

property!(web_delegate, web_set_delegate, "delegate", assign, 0);
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
property!(bar_delegate, bar_set_delegate, "delegate", assign, 0);
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
property!(item_target, item_set_target, "target", assign, 0);
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
    // The old stack goes: what it held is let go of, autoreleased (a popped
    // controller is handed back alive, as UIKit's).
    old.iter().for_each(|&controller| _ = crate::fw::counted::autorelease(controller));
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
    collections::array_autoreleased(&nav_stack(this))
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
    collections::array_autoreleased(&popped)
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
property!(nav_delegate, nav_set_delegate, "delegate", assign, 0);

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
property!(table_data_source, table_set_data_source, "dataSource", assign, 0);
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

property!(collection_data_source, collection_set_data_source, "dataSource", assign, 0);
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
property!(alert_delegate, alert_set_delegate, "delegate", assign, 0);
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
fn show(this: Id, kind: &str, present_will: &'static str) {
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
    // As UIKit's: "will present" as it is shown, "did present" once it has
    // popped up (unless it was closed or shown again before then).
    let presentation = next_presentation(this);
    tell(get_id(this, "delegate"), present_will, &[this]);
    crate::fw::runloop::post_after(ALERT_ANIMATION, Box::new(move || alert_presented(this, presentation)));
}

/// Each show and dismiss of an alert counts one up, so a "did present"
/// still waiting from an earlier show knows it is out of date.
fn next_presentation(this: Id) -> u64 {
    let next = property_get(this, "presentations").unwrap_or(0) + 1;
    property_set(this, "presentations", next);
    next
}

/// How long an alert takes to pop up or go away.
const ALERT_ANIMATION: f64 = 0.3;

/// The alert has popped up: its delegate hears didPresent.
fn alert_presented(this: Id, presentation: u64) {
    if property_get(this, "presentations") != Some(presentation) {
        return;
    }
    let did = if class_is(this, "UIActionSheet") { "didPresentActionSheet:" } else { "didPresentAlertView:" };
    tell(get_id(this, "delegate"), did, &[this]);
}

/// The alert has gone: its delegate hears didDismissWithButtonIndex:.
fn alert_dismissed(this: Id, index: isize) {
    let did = if class_is(this, "UIActionSheet") { "actionSheet:didDismissWithButtonIndex:" } else { "alertView:didDismissWithButtonIndex:" };
    tell(get_id(this, "delegate"), did, &[this, index as usize]);
}

extern "C" fn alert_show(this: Id, _: Sel) {
    show(this, "alert", "willPresentAlertView:");
}

extern "C" fn alert_dismiss(this: Id, _: Sel, index: isize, _animated: u8) {
    if !get_flag(this, "visible", false) {
        return;
    }
    property_set(this, "visible", 0);
    next_presentation(this);
    for field in collections::array_items(get_id(this, "textFields")) {
        runtime::send(field, "resignFirstResponder", &[]);
    }
    let delegate = get_id(this, "delegate");
    let will = if class_is(this, "UIActionSheet") { "actionSheet:willDismissWithButtonIndex:" } else { "alertView:willDismissWithButtonIndex:" };
    tell(delegate, will, &[this, index as usize]);
    runtime::send(this, "removeFromSuperview", &[]);
    // "did dismiss" once it has gone (UIKit's fade-out).
    crate::fw::runloop::post_after(ALERT_ANIMATION, Box::new(move || alert_dismissed(this, index)));
}

fn sheet_show(this: Id) {
    show(this, "action sheet", "willPresentActionSheet:");
}

// ----- drawing alerts and action sheets (iOS 7's look) -----

#[cfg(test)]
thread_local! {
    /// For tests: draws this thread's alerts in iOS 6's look (Some(true)) or not.
    static IOS6_LOOK: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

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
    // Over the whole screen, upright (turned in an old app's window).
    crate::fw::uikit::place_upright(window, this);
    let area = view_state(this).frame.size;
    runtime::send(this, "setBackgroundColor:", &[colored([0.0, 0.0, 0.0, 0.4])]);
    if ios6_look() {
        ios6_alert_draw(this, sheet, window, area);
        return true;
    }
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

// ----- iOS 6's look, for apps built with the iOS 6 SDK or older -----

/// Apps built for iOS 6 (or older) get iOS 6's alerts, as they would on
/// the iPhone they were made for; newer apps get iOS 7's.
fn ios6_look() -> bool {
    #[cfg(test)]
    if let Some(look) = IOS6_LOOK.with(|l| l.get()) {
        return look;
    }
    crate::fw::uikit::built_for_ios6()
}

const IOS6_ALERT_WIDTH: f64 = 284.0;
const IOS6_ALERT_MARGIN: f64 = 11.0;
const IOS6_BUTTON_HEIGHT: f64 = 43.0;
const IOS6_BUTTON_GAP: f64 = 8.0;
const IOS6_SHEET_BUTTON_WIDTH: f64 = 280.0;
const IOS6_SHEET_BUTTON_HEIGHT: f64 = 44.0;
const WHITE: [f64; 4] = [1.0, 1.0, 1.0, 1.0];

/// The dark blue alert box: a deep blue gradient, a white shine over its
/// top, a light edge.
const IOS6_ALERT_BOX: crate::fw::graphics::Gloss = crate::fw::graphics::Gloss {
    top: [0.16, 0.24, 0.46, 0.92],
    bottom: [0.03, 0.08, 0.26, 0.92],
    shine: 0.22,
    shine_depth: 0.18,
    edge: [0.86, 0.89, 0.96, 0.9],
    edge_width: 2.0,
    radius: 9.0,
};

/// An alert button: see-through blue glass, lighter when pressed; the
/// cancel button darker.
fn ios6_alert_button_look(cancel: bool, pressed: bool) -> crate::fw::graphics::Gloss {
    let (top, bottom) = match (cancel, pressed) {
        (_, true) => ([0.55, 0.66, 0.92, 0.95], [0.27, 0.38, 0.76, 0.95]),
        (true, false) => ([0.30, 0.34, 0.47, 0.85], [0.11, 0.14, 0.27, 0.85]),
        (false, false) => ([0.46, 0.51, 0.63, 0.85], [0.21, 0.25, 0.40, 0.85]),
    };
    crate::fw::graphics::Gloss { top, bottom, shine: 0.3, shine_depth: 0.5, edge: [0.04, 0.07, 0.18, 0.9], edge_width: 1.0, radius: 5.0 }
}

/// An action sheet button: light grey glass with dark text; the destructive
/// one red, the cancel one dark, both with white text.
fn ios6_sheet_button_look(destructive: bool, cancel: bool, pressed: bool) -> (crate::fw::graphics::Gloss, [f64; 4]) {
    let (top, bottom, title) = if pressed {
        ([0.40, 0.55, 0.95, 1.0], [0.15, 0.32, 0.82, 1.0], WHITE)
    } else if destructive {
        ([0.96, 0.38, 0.33, 1.0], [0.74, 0.07, 0.05, 1.0], WHITE)
    } else if cancel {
        ([0.36, 0.36, 0.39, 1.0], [0.14, 0.14, 0.17, 1.0], WHITE)
    } else {
        ([1.0, 1.0, 1.0, 1.0], [0.84, 0.84, 0.86, 1.0], [0.0, 0.0, 0.0, 1.0])
    };
    (crate::fw::graphics::Gloss { top, bottom, shine: 0.25, shine_depth: 0.5, edge: [0.0, 0.0, 0.0, 0.6], edge_width: 1.0, radius: 6.0 }, title)
}

/// A button of an iOS 6 alert or sheet: its pictures for normal and pressed,
/// its title, and the tap the alert hears.
fn ios6_button(this: Id, panel: Id, frame: Rect, title: &str, index: isize, looks: [crate::fw::graphics::Gloss; 2], title_color: [f64; 4], size: f64) -> Id {
    let button = runtime::send(runtime::class_named("UIButton"), "buttonWithType:", &[0]);
    set_frame(button, frame);
    for (look, state) in looks.iter().zip([0usize, 1]) {
        let picture = crate::fw::graphics::glossy_image(frame.size.width, frame.size.height, look);
        runtime::send(button, "setBackgroundImage:forState:", &[picture, state]);
    }
    runtime::send(button, "setAdjustsImageWhenHighlighted:", &[0]);
    runtime::send(button, "setTitle:forState:", &[string::make(title), 0]);
    runtime::send(button, "setTitleColor:forState:", &[colored(title_color), 0]);
    runtime::send(button, "setTitleColor:forState:", &[colored(WHITE), 1]);
    runtime::send(button, "setTitleColor:forState:", &[colored([0.6, 0.6, 0.6, 1.0]), 2]);
    runtime::send(runtime::send(button, "titleLabel", &[]), "setFont:", &[font(BOLD_FONT, size)]);
    runtime::send(button, "setTag:", &[index as usize]);
    runtime::send(button, "addTarget:action:forControlEvents:", &[this, runtime::sel("gamebridgeAlertButton:"), TOUCH_UP_INSIDE]);
    runtime::send(panel, "addSubview:", &[button]);
    button
}

/// Builds an alert or action sheet in iOS 6's look: an alert is a dark blue
/// glossy box in the middle with white text and glass buttons (two side by
/// side, more one above the other); a sheet is a dark see-through panel
/// along the bottom with light buttons. The box is a picture of our own
/// drawing in an image view; the title, message, fields and buttons sit on it.
fn ios6_alert_draw(this: Id, sheet: bool, window: Id, area: Size) {
    let width = if sheet { area.width } else { IOS6_ALERT_WIDTH };
    let inner = width - 2.0 * ALERT_MARGIN;
    let panel = add_view(this, "UIImageView", Rect::default(), None);
    runtime::send(panel, "setUserInteractionEnabled:", &[1]);

    let (title, message) = (text_of(get_id(this, "title")), text_of(get_id(this, "message")));
    let mut y = if title.is_empty() && message.is_empty() { 8.0 } else { 16.0 };
    let (title_font, title_color) = if sheet { (font(SYSTEM_FONT, 13.0), [0.85, 0.85, 0.87, 1.0]) } else { (font(BOLD_FONT, 18.0), WHITE) };
    y += add_alert_label(panel, &title, title_font, title_color, y, inner);
    if !title.is_empty() && !message.is_empty() {
        y += 6.0;
    }
    y += add_alert_label(panel, &message, font(SYSTEM_FONT, 16.0), if sheet { title_color } else { WHITE }, y, inner);
    let fields = collections::array_items(get_id(this, "textFields"));
    for (index, &field) in fields.iter().enumerate() {
        y += if index == 0 { 12.0 } else { 6.0 };
        set_frame(field, Rect::new(IOS6_ALERT_MARGIN + 2.0, y, width - 2.0 * (IOS6_ALERT_MARGIN + 2.0), 29.0));
        runtime::send(field, "setBackgroundColor:", &[colored(WHITE)]);
        runtime::send(field, "setBorderStyle:", &[2]);
        if get_id(field, "font") == 0 {
            runtime::send(field, "setFont:", &[font(SYSTEM_FONT, 16.0)]);
        }
        runtime::send(panel, "addSubview:", &[field]);
        runtime::send(field, "addTarget:action:forControlEvents:", &[this, runtime::sel("gamebridgeAlertTextChanged:"), 1 << 17]);
        runtime::send(field, "addTarget:action:forControlEvents:", &[this, runtime::sel("gamebridgeAlertReturn:"), 1 << 19]);
        y += 29.0;
    }
    y += if y > 16.0 { 14.0 } else { 0.0 };

    let buttons = with_buttons(this, |b| b.clone());
    let side_by_side = !sheet && buttons.titles.len() == 2;
    let mut made = Vec::new();
    for (index, title) in buttons.titles.iter().enumerate() {
        let index_i = index as isize;
        let button = if sheet {
            let x = ((width - IOS6_SHEET_BUTTON_WIDTH) / 2.0).max(IOS6_ALERT_MARGIN);
            let frame = Rect::new(x, y, width - 2.0 * x, IOS6_SHEET_BUTTON_HEIGHT);
            // The cancel button stands a little apart, at the bottom.
            let frame = if index_i == buttons.cancel && index > 0 { Rect { origin: Point { x, y: y + 6.0 }, ..frame } } else { frame };
            y = frame.origin.y + IOS6_SHEET_BUTTON_HEIGHT + IOS6_BUTTON_GAP;
            let destructive = index_i == buttons.destructive;
            let cancel = index_i == buttons.cancel;
            let (look, title_color) = ios6_sheet_button_look(destructive, cancel, false);
            let (pressed, _) = ios6_sheet_button_look(destructive, cancel, true);
            ios6_button(this, panel, frame, title, index_i, [look, pressed], title_color, 19.0)
        } else {
            let frame = if side_by_side {
                let half = (width - 2.0 * IOS6_ALERT_MARGIN - IOS6_BUTTON_GAP) / 2.0;
                Rect::new(IOS6_ALERT_MARGIN + index as f64 * (half + IOS6_BUTTON_GAP), y, half, IOS6_BUTTON_HEIGHT)
            } else {
                Rect::new(IOS6_ALERT_MARGIN, y + index as f64 * (IOS6_BUTTON_HEIGHT + IOS6_BUTTON_GAP), width - 2.0 * IOS6_ALERT_MARGIN, IOS6_BUTTON_HEIGHT)
            };
            let cancel = index_i == buttons.cancel;
            let looks = [ios6_alert_button_look(cancel, false), ios6_alert_button_look(cancel, true)];
            ios6_button(this, panel, frame, title, index_i, looks, WHITE, 18.0)
        };
        made.push(button);
    }
    let height = if sheet {
        y + 12.0
    } else {
        let rows = if side_by_side { 1 } else { made.len() } as f64;
        y + rows * IOS6_BUTTON_HEIGHT + (rows - 1.0).max(0.0) * IOS6_BUTTON_GAP + 15.0
    };
    let look = if sheet {
        // Dark see-through glass along the bottom, a light line along its top.
        crate::fw::graphics::Gloss {
            top: [0.36, 0.38, 0.43, 0.9],
            bottom: [0.07, 0.08, 0.11, 0.9],
            shine: 0.12,
            shine_depth: 0.06,
            edge: [0.0, 0.0, 0.0, 0.0],
            edge_width: 0.0,
            radius: 0.0,
        }
    } else {
        IOS6_ALERT_BOX
    };
    runtime::send(panel, "setImage:", &[crate::fw::graphics::glossy_image(width, height, &look)]);
    let top = if sheet { area.height - height } else { ((area.height - height) / 2.0).max(0.0) };
    set_frame(panel, Rect::new(((area.width - width) / 2.0).round(), top.round(), width, height));
    property_set(this, "shownButtons", collections::array_from(&made) as u64);
    runtime::send(window, "addSubview:", &[this]);
    if !sheet {
        alert_update_first_other(this);
    }
    if let Some(&first) = fields.first() {
        runtime::send(first, "becomeFirstResponder", &[]);
    }
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
property!(popover_delegate, popover_set_delegate, "delegate", assign, 0);
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
            ("baselineAdjustment", label_baseline as usize),
            ("setBaselineAdjustment:", label_set_baseline as usize),
            ("setPreferredMaxLayoutWidth:", ignore_double as usize),
            ("sizeThatFits:", label_size_that_fits as usize),
            ("sizeToFit", label_size_to_fit as usize),
            ("textRectForBounds:limitedToNumberOfLines:", label_text_rect as usize),
            ("drawRect:", label_draw_rect as usize),
            ("drawTextInRect:", label_draw_text_in_rect as usize),
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
            ("setEnabled:", control_set_enabled as usize),
            ("isSelected", control_selected as usize),
            ("setSelected:", control_set_selected as usize),
            ("isHighlighted", text_highlighted as usize),
            ("setHighlighted:", text_set_highlighted as usize),
            ("state", control_state_method as usize),
            ("isTracking", control_tracking as usize),
            ("setTracking:", control_set_tracking as usize),
            ("isTouchInside", control_touch_inside as usize),
            ("pointMostlyInside:withEvent:", control_point_mostly_inside as usize),
            ("touchesBegan:withEvent:", control_touches_began as usize),
            ("touchesMoved:withEvent:", control_touches_moved as usize),
            ("touchesEnded:withEvent:", control_touches_ended as usize),
            ("touchesCancelled:withEvent:", control_touches_cancelled as usize),
            ("beginTrackingWithTouch:withEvent:", control_begin_tracking as usize),
            ("continueTrackingWithTouch:withEvent:", control_begin_tracking as usize),
            ("endTrackingWithTouch:withEvent:", ignore_id_id as usize),
            ("cancelTrackingWithEvent:", control_cancel_tracking as usize),
            ("_unhighlight", control_unhighlight as usize),
            ("hitTest:withEvent:", control_hit_test as usize),
            ("contentHorizontalAlignment", control_horizontal as usize),
            ("setContentHorizontalAlignment:", control_set_horizontal as usize),
            ("contentVerticalAlignment", control_vertical as usize),
            ("setContentVerticalAlignment:", control_set_vertical as usize),
        ],
    );

    let slider = view_class("UISlider", "UIControl");
    methods(
        slider,
        &[
            ("value", slider_value as usize),
            ("setValue:", slider_set_value as usize),
            ("setValue:animated:", slider_set_value_animated as usize),
            ("minimumValue", slider_minimum as usize),
            ("setMinimumValue:", slider_set_minimum as usize),
            ("maximumValue", slider_maximum as usize),
            ("setMaximumValue:", slider_set_maximum as usize),
            ("isContinuous", slider_continuous as usize),
            ("setContinuous:", slider_set_continuous as usize),
            ("minimumTrackTintColor", slider_minimum_tint as usize),
            ("setMinimumTrackTintColor:", slider_set_minimum_tint as usize),
            ("maximumTrackTintColor", slider_maximum_tint as usize),
            ("setMaximumTrackTintColor:", slider_set_maximum_tint as usize),
            ("thumbTintColor", slider_thumb_tint as usize),
            ("setThumbTintColor:", slider_set_thumb_tint as usize),
        ],
    );

    let button = view_class("UIButton", "UIControl");
    // A nib's "Rounded Rect" button is of UIKit's own subclass.
    view_class("UIRoundedRectButton", "UIButton");
    // A button's title view (-_setupTitleView makes one).
    let button_label = view_class("UIButtonLabel", "UILabel");
    methods(
        button_label,
        &[
            ("setFont:", button_label_set_font as usize),
            ("setLineBreakMode:", button_label_set_int as usize),
            ("setTextAlignment:", button_label_set_int as usize),
            ("setAdjustsFontSizeToFitWidth:", button_label_set_bool as usize),
            ("setMinimumFontSize:", button_label_set_f64 as usize),
            ("setMinimumScaleFactor:", button_label_set_f64 as usize),
            ("setShadowOffset:", button_label_set_size as usize),
        ],
    );
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
            ("setAdjustsImageWhenHighlighted:", button_set_adjusts_highlighted_and_lay_out as usize),
            ("showsTouchWhenHighlighted", button_shows_touch as usize),
            ("setShowsTouchWhenHighlighted:", button_set_shows_touch as usize),
            ("adjustsImageWhenDisabled", button_adjusts_disabled as usize),
            ("setAdjustsImageWhenDisabled:", button_set_adjusts_disabled_and_lay_out as usize),
            ("setContentHorizontalAlignment:", button_set_horizontal as usize),
            ("setContentVerticalAlignment:", button_set_vertical as usize),
            ("layoutSubviews", button_layout_subviews as usize),
            ("contentRectForBounds:", button_content_rect as usize),
            ("backgroundRectForBounds:", button_background_rect as usize),
            ("imageRectForContentRect:", button_image_rect as usize),
            ("titleRectForContentRect:", button_title_rect as usize),
            ("reversesTitleShadowWhenHighlighted", button_reverses_shadow as usize),
            ("setReversesTitleShadowWhenHighlighted:", button_set_reverses_shadow as usize),
            ("setHighlighted:", button_set_highlighted as usize),
            ("setSelected:", button_set_selected as usize),
            ("setEnabled:", button_set_enabled as usize),
            ("sizeThatFits:", button_size_that_fits as usize),
            ("setFrame:", button_set_frame as usize),
            ("setBounds:", button_set_bounds as usize),
            ("dealloc", button_dealloc as usize),
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
            ("isDragging", scroll_is_dragging as usize),
            ("isTracking", scroll_is_tracking as usize),
            ("isDecelerating", scroll_is_decelerating as usize),
            ("setScrollsToTop:", ignore_bool as usize),
            ("delaysContentTouches", scroll_delays_content as usize),
            ("setDelaysContentTouches:", scroll_set_delays_content as usize),
            ("canCancelContentTouches", scroll_can_cancel as usize),
            ("setCanCancelContentTouches:", scroll_set_can_cancel as usize),
            ("touchesShouldBegin:withEvent:inContentView:", scroll_touches_should_begin as usize),
            ("touchesShouldCancelInContentView:", scroll_touches_should_cancel as usize),
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

    /// Completion blocks the frameworks copy are let go of when Apple's are:
    /// a one-time answer's copy once it has run, a kept handler's copy when a
    /// new one replaces it, a notification block when its observer is removed.
    #[test]
    fn framework_completion_blocks_are_released_when_done() {
        use crate::libsystem::mach::test_blocks::{counting, Counts};
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let counts: &'static Counts = Box::leak(Box::default());
        let class = |name| runtime::class_named(name) as Id;
        let new = |name| runtime::send(runtime::send(class(name), "alloc", &[]), "init", &[]);
        let url = runtime::send(class("NSURL"), "URLWithString:", &[crate::fw::string::make("http://example.com/")]);
        let request = runtime::send(class("NSURLRequest"), "requestWithURL:", &[url]);
        // One-time answers: run later, then let go.
        let collection = new("UICollectionView");
        runtime::send(collection, "performBatchUpdates:completion:", &[0, counting(counts)]);
        runtime::send(class("NSURLConnection"), "sendAsynchronousRequest:queue:completionHandler:", &[request, 0, counting(counts)]);
        runtime::send(new("ALAssetsLibrary"), "writeImageToSavedPhotosAlbum:orientation:completionBlock:", &[0, 0, counting(counts)]);
        runtime::send(class("AVCaptureDevice"), "requestAccessForMediaType:completionHandler:", &[crate::fw::string::make("vide"), counting(counts)]);
        runtime::send(new("ACAccountStore"), "requestAccessToAccountsWithType:withCompletionHandler:", &[0, counting(counts)]);
        // Kept handlers: the replaced copy goes, the newest stays.
        let tweet = new("TWTweetComposeViewController");
        runtime::send(tweet, "setCompletionHandler:", &[counting(counts)]);
        runtime::send(tweet, "setCompletionHandler:", &[counting(counts)]);
        let matchmaker = new("GKMatchmaker");
        runtime::send(matchmaker, "setInviteHandler:", &[counting(counts)]);
        runtime::send(matchmaker, "setInviteHandler:", &[counting(counts)]);
        let operation = new("NSOperation");
        runtime::send(operation, "setCompletionBlock:", &[counting(counts)]);
        runtime::send(operation, "setCompletionBlock:", &[counting(counts)]);
        // A notification block: until its observer is removed.
        let center = runtime::send(class("NSNotificationCenter"), "defaultCenter", &[]);
        let name = crate::fw::string::make("GBBlockReleaseTest");
        let token = runtime::send(center, "addObserverForName:object:queue:usingBlock:", &[name, 0, 0, counting(counts)]);
        runtime::send(center, "removeObserver:", &[token]);
        assert_eq!(counts.get().0, 12, "each block was copied once");
        let end = std::time::Instant::now() + std::time::Duration::from_millis(500);
        while counts.get().1 < 9 && std::time::Instant::now() < end {
            crate::fw::runloop::run_main_loop_briefly();
        }
        assert_eq!(counts.get(), (12, 9), "all but the three kept handlers were let go of");
    }

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

    /// A tab-like button (its own normal, pressed and selected pictures, none
    /// for selected and pressed) shows its selected picture as soon as a tap
    /// selects it, though the finger is still down. UIKit would show the
    /// normal picture darkened there until the highlight goes (0.12 s after
    /// touch-down); the owner chose not to show that dark step, for every
    /// game. What the game reads back (backgroundImageForState:) stays
    /// UIKit's. Every picture a tap shows is got ready before it.
    #[test]
    fn a_tab_button_shows_its_selected_picture_while_still_pressed() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let solid = |shade: f64| crate::fw::graphics::test_solid_image(8.0, 8.0, [shade, shade, shade, 1.0]);
        let (inactive, highlighted, active) = (solid(0.2), solid(0.5), solid(0.8));
        let button = runtime::send(runtime::class_named("UIButton"), "buttonWithType:", &[0]);
        crate::fw::uikit::send_rect(button, "setFrame:", Rect::new(0.0, 0.0, 40.0, 20.0));
        runtime::send(button, "setBackgroundImage:forState:", &[inactive, 0]);
        runtime::send(button, "setBackgroundImage:forState:", &[highlighted, 1]);
        runtime::send(button, "setBackgroundImage:forState:", &[active, 4]);
        let ready = button_press_pictures(button);
        assert!(ready.contains(&highlighted) && ready.contains(&active), "{ready:x?}");
        runtime::send(button, "setSelected:", &[1]);
        assert_eq!(button_press_pictures(button), vec![active], "selected: its selected picture, not a darker one");
        runtime::send(button, "setHighlighted:", &[1]);
        runtime::send(button, "layoutIfNeeded", &[]);
        let back = get_id(button, "backgroundView");
        assert_eq!(runtime::send(back, "image", &[]), active, "selected and pressed shows the selected picture");
        assert_eq!(property_get(back, "drawMode").unwrap_or(0), 0, "not darkened");
        assert_eq!(runtime::send(button, "backgroundImageForState:", &[5]), inactive, "the game reads UIKit's fallback");
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
        let _turn = crate::fw::typing::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

    /// A table makes cells only for the rows on show, asking every row's
    /// height first; rows that scroll away give their cells back for
    /// dequeueReusableCellWithIdentifier:, and only the rows on show count
    /// as visible.
    #[test]
    fn tables_make_cells_only_for_the_rows_on_show() {
        use crate::fw::object::define_class;
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());
        static MADE: AtomicUsize = AtomicUsize::new(0);
        extern "C" fn rows(_this: Id, _: Sel, _table: Id, _section: isize) -> isize {
            10
        }
        extern "C" fn cell_for(_this: Id, _: Sel, table: Id, path: Id) -> Id {
            LOG.lock().unwrap().push(format!("cell {}", runtime::send(path, "row", &[])));
            let identifier = string::make("Row");
            let cell = runtime::send(table, "dequeueReusableCellWithIdentifier:", &[identifier]);
            if cell != 0 {
                return cell;
            }
            MADE.fetch_add(1, Ordering::SeqCst);
            runtime::send(runtime::send(runtime::class_named("UITableViewCell"), "alloc", &[]), "initWithStyle:reuseIdentifier:", &[0, identifier])
        }
        extern "C" fn height(_this: Id, _: Sel, _table: Id, path: Id) -> f64 {
            LOG.lock().unwrap().push(format!("height {}", runtime::send(path, "row", &[])));
            50.0
        }
        let source_class = define_class("TableReuseSource", "NSObject", 8);
        methods(
            source_class,
            &[
                ("tableView:numberOfRowsInSection:", rows as usize),
                ("tableView:cellForRowAtIndexPath:", cell_for as usize),
                ("tableView:heightForRowAtIndexPath:", height as usize),
            ],
        );
        let source = new_object("TableReuseSource");
        let table = new_view_named("UITableView", Rect::new(0.0, 0.0, 240.0, 174.0));
        runtime::send(table, "setDataSource:", &[source]);
        runtime::send(table, "setDelegate:", &[source]);
        runtime::send(table, "reloadData", &[]);
        let shown_rows = || -> Vec<usize> {
            collections::array_items(runtime::send(table, "indexPathsForVisibleRows", &[])).into_iter().map(|p| runtime::send(p, "row", &[])).collect()
        };
        assert_eq!(collections::array_items(runtime::send(table, "visibleCells", &[])).len(), 4, "cells for the rows on show only");
        assert_eq!(shown_rows(), [0, 1, 2, 3]);
        let log = std::mem::take(&mut *LOG.lock().unwrap());
        let first_cell = log.iter().position(|line| line.starts_with("cell")).unwrap();
        assert_eq!(log[..first_cell].len(), 10, "every height first: {log:?}");
        send_point(table, "setContentOffset:", Point { x: 0.0, y: 300.0 });
        runtime::send(table, "layoutIfNeeded", &[]);
        assert_eq!(shown_rows(), [6, 7, 8, 9]);
        assert!(MADE.load(Ordering::SeqCst) <= 5, "cells that scrolled away were used again: {} made", MADE.load(Ordering::SeqCst));
        let first = runtime::send(runtime::class_named("NSIndexPath"), "indexPathForRow:inSection:", &[0, 0]);
        assert_eq!(runtime::send(table, "cellForRowAtIndexPath:", &[first]), 0, "a row off screen has no cell");
    }

    /// UIAlertView's delegate hears willPresentAlertView: as it is shown and
    /// didPresentAlertView: once it has popped up; willDismiss at the tap,
    /// didDismiss once it has gone.
    #[test]
    fn alerts_tell_their_delegate_around_the_animations() {
        use crate::fw::object::define_class;
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        static HEARD: Mutex<Vec<String>> = Mutex::new(Vec::new());
        extern "C" fn present(_this: Id, s: Sel, _alert: Id) {
            HEARD.lock().unwrap().push(lock().sel_name(s));
        }
        extern "C" fn dismiss(_this: Id, s: Sel, _alert: Id, _index: isize) {
            HEARD.lock().unwrap().push(lock().sel_name(s));
        }
        let class = define_class("AlertTimingDelegate", "NSObject", 8);
        methods(
            class,
            &[
                ("willPresentAlertView:", present as usize),
                ("didPresentAlertView:", present as usize),
                ("alertView:willDismissWithButtonIndex:", dismiss as usize),
                ("alertView:didDismissWithButtonIndex:", dismiss as usize),
            ],
        );
        let heard = || std::mem::take(&mut *HEARD.lock().unwrap());
        let alert = new_view_named("UIAlertView", Rect::default());
        runtime::send(alert, "setDelegate:", &[new_object("AlertTimingDelegate")]);
        runtime::send(alert, "addButtonWithTitle:", &[string::make("OK")]);
        let presentation = || crate::fw::uikit::property_get(alert, "presentations").unwrap_or(0);
        runtime::send(alert, "show", &[]);
        assert_eq!(heard(), ["willPresentAlertView:"], "will at once, did after the pop-up");
        alert_presented(alert, presentation());
        assert_eq!(heard(), ["didPresentAlertView:"]);
        runtime::send(alert, "dismissWithClickedButtonIndex:animated:", &[0, 1]);
        assert_eq!(heard(), ["alertView:willDismissWithButtonIndex:"]);
        alert_dismissed(alert, 0);
        assert_eq!(heard(), ["alertView:didDismissWithButtonIndex:"]);
        // Closed before it finished popping up: no didPresent after willDismiss.
        runtime::send(alert, "show", &[]);
        let shown = presentation();
        runtime::send(alert, "dismissWithClickedButtonIndex:animated:", &[0, 1]);
        alert_presented(alert, shown);
        assert_eq!(heard(), ["willPresentAlertView:", "alertView:willDismissWithButtonIndex:"]);
    }

    /// The keyboard tells the game when it comes up and goes down
    /// (UIKeyboardWill/DidShow, Will/DidHide, with its frame and animation in
    /// userInfo); moving from one field to another keeps it up.
    #[test]
    fn the_keyboard_tells_when_it_shows_and_hides() {
        use crate::fw::object::define_class;
        let _turn = crate::fw::typing::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        static HEARD: Mutex<Vec<String>> = Mutex::new(Vec::new());
        extern "C" fn heard_note(_this: Id, _: Sel, note: Id) {
            let name = string::get(runtime::send(note, "name", &[]));
            let info = runtime::send(note, "userInfo", &[]);
            let frame = if info == 0 { 0 } else { runtime::send(info, "objectForKey:", &[string::make("UIKeyboardFrameEndUserInfoKey")]) };
            HEARD.lock().unwrap().push(format!("{}{}", name.trim_start_matches("UIKeyboard").trim_end_matches("Notification"), if frame != 0 { " +frame" } else { "" }));
        }
        let class = define_class("KeyboardTestObserver", "NSObject", 8);
        methods(class, &[("heard:", heard_note as usize)]);
        let observer = new_object("KeyboardTestObserver");
        let center = runtime::send(runtime::class_named("NSNotificationCenter"), "defaultCenter", &[]);
        for name in ["UIKeyboardWillShowNotification", "UIKeyboardDidShowNotification", "UIKeyboardWillHideNotification", "UIKeyboardDidHideNotification"] {
            runtime::send(center, "addObserver:selector:name:object:", &[observer, runtime::sel("heard:"), string::make(name), 0]);
        }
        let heard = || std::mem::take(&mut *HEARD.lock().unwrap());
        let editing = crate::fw::typing::first_responder();
        if editing != 0 {
            runtime::send(editing, "resignFirstResponder", &[]);
        }
        crate::fw::typing::keyboard_settled();
        heard();
        let (first, second) = (new_view_named("UITextField", Rect::new(0.0, 0.0, 100.0, 30.0)), new_view_named("UITextField", Rect::new(0.0, 40.0, 100.0, 30.0)));
        runtime::send(first, "becomeFirstResponder", &[]);
        assert_eq!(heard(), ["WillShow +frame"]);
        crate::fw::typing::keyboard_settled();
        assert_eq!(heard(), ["DidShow +frame"]);
        runtime::send(second, "becomeFirstResponder", &[]);
        assert!(heard().is_empty(), "still up from field to field");
        runtime::send(second, "resignFirstResponder", &[]);
        assert_eq!(heard(), ["WillHide +frame"]);
        crate::fw::typing::keyboard_settled();
        assert_eq!(heard(), ["DidHide +frame"]);
        runtime::send(center, "removeObserver:", &[observer]);
    }

    /// Animated, a progress bar's fill slides to the new value (the
    /// property is the new value at once).
    #[test]
    fn progress_bars_animate_their_fill() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let bar = new_view_named("UIProgressView", Rect::new(0.0, 0.0, 100.0, 9.0));
        let s = runtime::sel("setProgress:animated:");
        let set: extern "C" fn(Id, Sel, f32, bool) = unsafe { std::mem::transmute(runtime::imp_for(bar, s).unwrap()) };
        set(bar, s, 0.5, true);
        assert_eq!(crate::fw::progress::progress(bar), 0.5);
        let now = crate::fw::runloop::media_time();
        let shown = |at: f64| crate::fw::progress::shown_progress_at(bar, at);
        assert!(shown(now) < 0.25, "starts from where it was: {}", shown(now));
        assert_eq!(shown(now + 1.0), 0.5, "arrives");
        set(bar, s, 0.2, false);
        assert_eq!(shown(now), 0.2f32 as f64, "not animated: at once");
        // A second slide 0.1 s into the first: the first one's end leaves it sliding.
        crate::fw::progress::slide_to(bar, 0.6, now);
        crate::fw::progress::slide_to(bar, 1.0, now + 0.1);
        crate::fw::progress::slide_over(bar, now + 0.25);
        assert!(shown(now + 0.25) < 1.0, "still on its way: {}", shown(now + 0.25));
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

    /// Moving a scroll view's content lays it out again (UIKit's scroll
    /// views run -layoutSubviews as they scroll): SpellCraft's shop row adds
    /// the cards coming into view there. Setting the same offset again doesn't.
    #[test]
    fn scrolling_lays_the_scroll_view_out_again() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        static LAID_OUT: AtomicUsize = AtomicUsize::new(0);
        extern "C" fn layout(_: Id, _: Sel) {
            LAID_OUT.fetch_add(1, Ordering::SeqCst);
        }
        let class = define_class("G32DynamicRow", "UIScrollView", 16);
        methods(class, &[("layoutSubviews", layout as usize)]);
        let row = new_view(class, Rect::new(0.0, 0.0, 100.0, 50.0));
        let offset = |x: f64| {
            let s = runtime::sel("setContentOffset:");
            let f: extern "C" fn(Id, Sel, Point) = unsafe { std::mem::transmute(runtime::imp_for(row, s).unwrap()) };
            f(row, s, Point { x, y: 0.0 });
            runtime::send(row, "layoutIfNeeded", &[]);
            LAID_OUT.load(Ordering::SeqCst)
        };
        let first = offset(0.0);
        assert_eq!(offset(40.0), first + 1, "laid out after moving");
        assert_eq!(offset(40.0), first + 1, "not again for the same offset");
    }

    /// UIScrollView's -setContentOffset: rounds to the screen's pixels and
    /// does nothing when the offset doesn't change; a change goes through
    /// -setBounds: (a game's override sees it), then scrollViewDidScroll:.
    #[test]
    fn a_content_offset_moves_only_when_it_changes() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        static HEARD: Mutex<Vec<String>> = Mutex::new(Vec::new());
        extern "C" fn did_scroll(_this: Id, _: Sel, _scroll: Id) {
            HEARD.lock().unwrap().push("didScroll".to_string());
        }
        extern "C" fn set_bounds(this: Id, s: Sel, bounds: Rect) {
            HEARD.lock().unwrap().push(format!("setBounds {}", bounds.origin.x));
            let class = runtime::class_named("UIScrollView");
            let found = lock().lookup(class, s);
            if let Some(imp) = found {
                unsafe { std::mem::transmute::<usize, extern "C" fn(Id, Sel, Rect)>(imp)(this, s, bounds) };
            }
        }
        let delegate_class = define_class("OffsetTestDelegate", "NSObject", 8);
        methods(delegate_class, &[("scrollViewDidScroll:", did_scroll as usize)]);
        let scroll_class = define_class("OffsetTestScroll", "UIScrollView", 8);
        methods(scroll_class, &[("setBounds:", set_bounds as usize)]);
        let scroll = new_view(scroll_class, Rect::new(0.0, 0.0, 100.0, 100.0));
        runtime::send(scroll, "setDelegate:", &[new_object("OffsetTestDelegate")]);
        let heard = || std::mem::take(&mut *HEARD.lock().unwrap());
        let offset = || {
            let s = runtime::sel("contentOffset");
            let f: extern "C" fn(Id, Sel) -> Point = unsafe { std::mem::transmute(runtime::imp_for(scroll, s).unwrap()) };
            f(scroll, s)
        };
        send_point(scroll, "setContentOffset:", Point::default());
        send_point(scroll, "setContentOffset:", Point::default());
        assert!(heard().is_empty(), "no change, nothing told");
        let scale = crate::fw::uikit::screen_points().2;
        send_point(scroll, "setContentOffset:", Point { x: 10.3, y: 0.0 });
        let x = (10.3 * scale).round() / scale;
        assert_eq!(heard(), [format!("setBounds {x}"), "didScroll".to_string()]);
        assert_eq!(offset().x, x, "rounded to the screen's pixels");
        crate::fw::uikit::send_rect(scroll, "setBounds:", Rect::new(30.0, 0.0, 100.0, 100.0));
        assert_eq!(offset().x, 30.0, "the bounds origin is the content offset");
    }

    /// UIButton's state setters lay the button out again (UIKit calls
    /// -setNeedsLayout), so a game button's -layoutSubviews follows its state.
    #[test]
    fn button_state_changes_lay_the_button_out() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        static LAID_OUT: AtomicUsize = AtomicUsize::new(0);
        extern "C" fn layout(_: Id, _: Sel) {
            LAID_OUT.fetch_add(1, Ordering::SeqCst);
        }
        let class = define_class("LayoutTestButton", "UIButton", 8);
        methods(class, &[("layoutSubviews", layout as usize)]);
        let button = new_view(class, Rect::new(0.0, 0.0, 100.0, 40.0));
        runtime::send(button, "layoutIfNeeded", &[]);
        for (setter, value) in [("setHighlighted:", 1), ("setSelected:", 1), ("setEnabled:", 0)] {
            let before = LAID_OUT.load(Ordering::SeqCst);
            runtime::send(button, setter, &[value]);
            runtime::send(button, "layoutIfNeeded", &[]);
            assert_eq!(LAID_OUT.load(Ordering::SeqCst), before + 1, "{setter}");
        }
    }

    /// A scroll view drags with the finger, the way UIKit's does: the content
    /// hears nothing for its first 0.15 s (delaysContentTouches), so a swipe
    /// that starts sooner never reaches it; past the slop the scroll view
    /// takes the touch (content that heard it gets touchesCancelled), moves
    /// contentOffset by the finger's motion (opposite sign), stops at the
    /// content's end without bouncing, and answers isDragging/isTracking
    /// while the finger is down. A short tap still reaches the content view.
    #[test]
    fn a_drag_scrolls_and_a_short_tap_still_reaches_the_content() {
        let _turn = crate::fw::typing::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        crate::fw::runloop::as_main(drag_scrolls_and_tap_reaches);
    }

    fn drag_scrolls_and_tap_reaches() {
        use crate::fw::geometry::Size;
        use crate::fw::touch::{deliver_in, Phase};
        static HEARD: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let heard = || std::mem::take(&mut *HEARD.lock().unwrap());
        extern "C" fn note(_this: Id, s: Sel, _: Id, _: Id) {
            HEARD.lock().unwrap().push(lock().sel_name(s).trim_end_matches(":withEvent:").to_string());
        }
        let content_class = define_class("ScrollTestContent", "UIView", 8);
        let touches = ["touchesBegan:withEvent:", "touchesMoved:withEvent:", "touchesEnded:withEvent:", "touchesCancelled:withEvent:"];
        methods(content_class, &touches.map(|name| (name, note as usize)));

        let window = new_view_named("UIWindow", Rect::new(0.0, 0.0, 320.0, 480.0));
        let scroll = new_view_named("UIScrollView", Rect::new(0.0, 0.0, 200.0, 200.0));
        runtime::send(window, "addSubview:", &[scroll]);
        let content = new_view_named("ScrollTestContent", Rect::new(0.0, 0.0, 200.0, 600.0));
        runtime::send(scroll, "addSubview:", &[content]);
        let s = runtime::sel("setContentSize:");
        let set_size: extern "C" fn(Id, Sel, Size) = unsafe { std::mem::transmute(runtime::imp_for(scroll, s).unwrap()) };
        set_size(scroll, s, Size { width: 200.0, height: 600.0 }); // taller than its bounds: scrolls vertically

        let offset_y = || content_offset_of(scroll).y;
        let dragging = || runtime::send(scroll, "isDragging", &[]) & 0xFF != 0;
        let tracking = || runtime::send(scroll, "isTracking", &[]) & 0xFF != 0;

        // A quick swipe up past the slop: the content never hears it.
        let finger = 762;
        deliver_in(window, finger, Phase::Began, Point { x: 100.0, y: 150.0 });
        deliver_in(window, finger, Phase::Moved, Point { x: 100.0, y: 145.0 });
        assert!(heard().is_empty(), "held back for its first 0.15 s");
        deliver_in(window, finger, Phase::Moved, Point { x: 100.0, y: 130.0 });
        deliver_in(window, finger, Phase::Ended, Point { x: 100.0, y: 130.0 });
        assert!(heard().is_empty(), "the swipe went to the scroll view only");
        assert_eq!(offset_y(), 15.0);
        send_point(scroll, "setContentOffset:", Point::default());

        // A drag up past the slop, after the content heard the finger.
        let finger = 760;
        runtime::send(scroll, "setBounces:", &[0]);
        deliver_in(window, finger, Phase::Began, Point { x: 100.0, y: 150.0 });
        crate::fw::touch::content_delay_passed(finger, crate::fw::touch::touch_seq(finger));
        deliver_in(window, finger, Phase::Moved, Point { x: 100.0, y: 145.0 });
        assert_eq!(offset_y(), 0.0, "under the slop: not yet scrolling");
        assert_eq!(heard(), ["touchesBegan", "touchesMoved"], "the content has the touch under the slop");
        deliver_in(window, finger, Phase::Moved, Point { x: 100.0, y: 135.0 });
        assert_eq!(offset_y(), 10.0, "the slop-crossing move scrolls by its own motion");
        assert!(dragging() && tracking(), "dragging and tracking while the finger is down");
        assert_eq!(heard(), ["touchesCancelled"], "the scroll view took the touch: the content's is cancelled");
        deliver_in(window, finger, Phase::Moved, Point { x: 100.0, y: 100.0 });
        assert_eq!(offset_y(), 45.0, "each move scrolls by the finger's motion");
        assert!(heard().is_empty(), "no more touches for the content once taken");
        // Without bouncing, it stops at the bottom of the content (600 - 200).
        deliver_in(window, finger, Phase::Moved, Point { x: 100.0, y: -1000.0 });
        assert_eq!(offset_y(), 400.0, "clamped to contentSize - bounds");
        deliver_in(window, finger, Phase::Ended, Point { x: 100.0, y: -1000.0 });
        assert!(!dragging() && !tracking(), "not dragging once the finger is up");
        stop_all_coasting();

        // A plain tap still reaches the content and does not scroll.
        let finger = 761;
        deliver_in(window, finger, Phase::Began, Point { x: 100.0, y: 100.0 });
        deliver_in(window, finger, Phase::Ended, Point { x: 100.0, y: 100.0 });
        assert_eq!(heard(), ["touchesBegan", "touchesEnded"], "a short tap stays a tap for the content");
        assert_eq!(offset_y(), 400.0, "the tap did not scroll");
    }

    /// A button in a scroll view keeps a finger that has pressed it: once
    /// the button heard the touch, a drag asks -touchesShouldCancelInContentView:
    /// (NO for an enabled control), so the content doesn't scroll and the
    /// button follows the finger to TouchUpInside. With delaysContentTouches
    /// off, the button hears the touch at once.
    #[test]
    fn a_pressed_button_keeps_its_finger_in_a_scroll_view() {
        let _turn = crate::fw::typing::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        crate::fw::runloop::as_main(pressed_button_keeps_finger);
    }

    fn pressed_button_keeps_finger() {
        use crate::fw::geometry::Size;
        use crate::fw::touch::{deliver_in, Phase};
        static HEARD: Mutex<Vec<String>> = Mutex::new(Vec::new());
        extern "C" fn note(_this: Id, s: Sel, _: Id, _: Id) {
            HEARD.lock().unwrap().push(lock().sel_name(s));
        }
        let target_class = define_class("ScrollButtonTestTarget", "NSObject", 8);
        methods(target_class, &[("up:event:", note as usize), ("cancel:event:", note as usize)]);
        let target = new_object("ScrollButtonTestTarget");
        let window = new_view_named("UIWindow", Rect::new(0.0, 0.0, 320.0, 480.0));
        let scroll = new_view_named("UIScrollView", Rect::new(0.0, 0.0, 200.0, 200.0));
        runtime::send(window, "addSubview:", &[scroll]);
        let s = runtime::sel("setContentSize:");
        let set_size: extern "C" fn(Id, Sel, Size) = unsafe { std::mem::transmute(runtime::imp_for(scroll, s).unwrap()) };
        set_size(scroll, s, Size { width: 600.0, height: 200.0 });
        let button = runtime::send(runtime::class_named("UIButton"), "buttonWithType:", &[0]);
        crate::fw::uikit::send_rect(button, "setFrame:", Rect::new(50.0, 50.0, 100.0, 50.0));
        runtime::send(scroll, "addSubview:", &[button]);
        runtime::send(button, "addTarget:action:forControlEvents:", &[target, runtime::sel("up:event:"), TOUCH_UP_INSIDE]);
        runtime::send(button, "addTarget:action:forControlEvents:", &[target, runtime::sel("cancel:event:"), TOUCH_CANCEL]);

        let finger = 765;
        deliver_in(window, finger, Phase::Began, Point { x: 100.0, y: 75.0 });
        assert!(runtime::send(button, "isHighlighted", &[]) & 0xFF == 0, "not pressed during the first 0.15 s");
        crate::fw::touch::content_delay_passed(finger, crate::fw::touch::touch_seq(finger));
        assert!(runtime::send(button, "isHighlighted", &[]) & 0xFF != 0, "pressed once the delay passed");
        deliver_in(window, finger, Phase::Moved, Point { x: 60.0, y: 75.0 });
        deliver_in(window, finger, Phase::Ended, Point { x: 60.0, y: 75.0 });
        assert_eq!(content_offset_of(scroll).x, 0.0, "the scroll view left the button its finger");
        assert_eq!(std::mem::take(&mut *HEARD.lock().unwrap()), ["up:event:"]);

        runtime::send(scroll, "setDelaysContentTouches:", &[0]);
        assert!(runtime::send(scroll, "delaysContentTouches", &[]) & 0xFF == 0);
        deliver_in(window, 766, Phase::Began, Point { x: 100.0, y: 75.0 });
        assert!(runtime::send(button, "isHighlighted", &[]) & 0xFF != 0, "no delay: pressed at once");
        deliver_in(window, 766, Phase::Cancelled, Point { x: 100.0, y: 75.0 });
    }

    /// -setContentOffset:animated: slides the content there over UIKit's
    /// 0.3 s, then tells the delegate scrollViewDidEndScrollingAnimation:.
    /// Dragged past an end, a bouncing scroll view follows the finger at
    /// half speed and slides back when let go; paging slides to its page.
    #[test]
    fn scroll_views_slide_bounce_and_page() {
        let _turn = crate::fw::typing::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        crate::fw::runloop::as_main(slide_bounce_and_page);
    }

    fn slide_bounce_and_page() {
        use crate::fw::geometry::Size;
        use crate::fw::touch::{deliver_in, Phase};
        static HEARD: Mutex<Vec<String>> = Mutex::new(Vec::new());
        extern "C" fn note(_this: Id, s: Sel, _scroll: Id) {
            HEARD.lock().unwrap().push(lock().sel_name(s));
        }
        let delegate_class = define_class("SlideTestDelegate", "NSObject", 8);
        methods(delegate_class, &[("scrollViewDidEndScrollingAnimation:", note as usize), ("scrollViewDidEndDecelerating:", note as usize)]);
        let heard = || std::mem::take(&mut *HEARD.lock().unwrap());
        let window = new_view_named("UIWindow", Rect::new(0.0, 0.0, 320.0, 480.0));
        let scroll = new_view_named("UIScrollView", Rect::new(0.0, 0.0, 200.0, 200.0));
        runtime::send(window, "addSubview:", &[scroll]);
        runtime::send(scroll, "setDelegate:", &[new_object("SlideTestDelegate")]);
        let content = new_view_named("UIView", Rect::new(0.0, 0.0, 600.0, 200.0));
        runtime::send(scroll, "addSubview:", &[content]);
        let s = runtime::sel("setContentSize:");
        let set_size: extern "C" fn(Id, Sel, Size) = unsafe { std::mem::transmute(runtime::imp_for(scroll, s).unwrap()) };
        set_size(scroll, s, Size { width: 600.0, height: 200.0 });
        let x = || content_offset_of(scroll).x;

        let s = runtime::sel("setContentOffset:animated:");
        let animated: extern "C" fn(Id, Sel, Point, u8) = unsafe { std::mem::transmute(runtime::imp_for(scroll, s).unwrap()) };
        animated(scroll, s, Point { x: 200.0, y: 0.0 }, 1);
        assert_eq!(x(), 0.0, "not there at once");
        let later = crate::fw::runloop::media_time() + 1.0;
        slide_step(scroll, later - 0.85);
        assert!(x() > 0.0 && x() < 200.0, "on its way: {}", x());
        slide_step(scroll, later);
        assert_eq!(x(), 200.0);
        assert_eq!(heard(), ["scrollViewDidEndScrollingAnimation:"]);

        // Past the right end (400): half the finger's motion, then back.
        send_point(scroll, "setContentOffset:", Point { x: 400.0, y: 0.0 });
        let finger = 767;
        deliver_in(window, finger, Phase::Began, Point { x: 150.0, y: 100.0 });
        deliver_in(window, finger, Phase::Moved, Point { x: 130.0, y: 100.0 });
        deliver_in(window, finger, Phase::Moved, Point { x: 90.0, y: 100.0 });
        assert_eq!(x(), 430.0, "past the end at half the finger's 60 points");
        deliver_in(window, finger, Phase::Ended, Point { x: 90.0, y: 100.0 });
        assert!(runtime::send(scroll, "isDecelerating", &[]) & 0xFF != 0, "sliding back");
        slide_step(scroll, crate::fw::runloop::media_time() + 1.0);
        assert_eq!(x(), 400.0, "back at the end");
        assert_eq!(heard(), ["scrollViewDidEndDecelerating:"]);

        // Paging: let go between pages, it slides to the nearer one.
        runtime::send(scroll, "setPagingEnabled:", &[1]);
        send_point(scroll, "setContentOffset:", Point::default());
        let finger = 768;
        deliver_in(window, finger, Phase::Began, Point { x: 150.0, y: 100.0 });
        deliver_in(window, finger, Phase::Moved, Point { x: 100.0, y: 100.0 });
        deliver_in(window, finger, Phase::Moved, Point { x: 30.0, y: 100.0 });
        deliver_in(window, finger, Phase::Ended, Point { x: 30.0, y: 100.0 });
        assert_eq!(x(), 120.0, "not snapped at once");
        slide_step(scroll, crate::fw::runloop::media_time() + 1.0);
        assert_eq!(x(), 200.0, "on the next page");
        assert_eq!(heard(), ["scrollViewDidEndDecelerating:"]);
    }

    /// Paging: a flick turns the page the way it went even short of halfway
    /// (UIKit keeps a paging scroll view's velocity from 0.25 points a
    /// millisecond); a slow let-go settles on the nearer page.
    #[test]
    fn a_flick_turns_the_page() {
        use crate::fw::geometry::Size;
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let scroll = new_view_named("UIScrollView", Rect::new(0.0, 0.0, 200.0, 200.0));
        let s = runtime::sel("setContentSize:");
        let set_size: extern "C" fn(Id, Sel, Size) = unsafe { std::mem::transmute(runtime::imp_for(scroll, s).unwrap()) };
        set_size(scroll, s, Size { width: 600.0, height: 200.0 });
        let page = |velocity: f64| page_target(scroll, AXIS_H, Point { x: velocity, y: 0.0 }).x;
        send_point(scroll, "setContentOffset:", Point { x: 80.0, y: 0.0 });
        assert_eq!(page(400.0), 200.0, "flicked on to the next page");
        assert_eq!(page(100.0), 0.0, "slow: the nearer page");
        send_point(scroll, "setContentOffset:", Point { x: 120.0, y: 0.0 });
        assert_eq!(page(-400.0), 0.0, "flicked back");
    }

    /// An app built with the iOS 6 SDK (or older) gets iOS 6's alerts: a
    /// dark blue glossy box with white text and glossy buttons; an action
    /// sheet is a dark see-through panel with light buttons, the destructive
    /// one red. Our own drawing, as pictures in image views and button
    /// backgrounds.
    #[test]
    fn old_apps_get_ios_6_alerts() {
        use crate::fw::graphics::{color_rgba, image_cg, straight_pixels};
        let _turn = crate::fw::typing::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let window = new_view_named("UIWindow", Rect::new(0.0, 0.0, 320.0, 480.0));
        runtime::send(window, "makeKeyAndVisible", &[]);
        fn inside(view: Id) -> Vec<Id> {
            view_state(view).subviews.clone().into_iter().flat_map(|child| std::iter::once(child).chain(inside(child))).collect()
        }
        // The middle pixel of a picture, straight RGBA.
        let middle = |image: Id| {
            let pixels = straight_pixels(image_cg(image)).unwrap();
            let info = crate::fw::uikit::image_info(image).unwrap();
            let (w, h) = (info.pixels.0 as usize, info.pixels.1 as usize);
            pixels[((h / 2) * w + w / 2) * 4..][..4].to_vec()
        };
        let white = |color: Id| color_rgba(color).is_some_and(|c| c[..3].iter().all(|&v| v > 0.95));
        IOS6_LOOK.with(|l| l.set(Some(true)));

        let alert = new_view_named("UIAlertView", Rect::default());
        runtime::send(alert, "setTitle:", &[string::make("Saved")]);
        runtime::send(alert, "setMessage:", &[string::make("Your game was saved.")]);
        runtime::send(alert, "addButtonWithTitle:", &[string::make("Cancel")]);
        runtime::send(alert, "addButtonWithTitle:", &[string::make("OK")]);
        runtime::send(alert, "setCancelButtonIndex:", &[0]);
        runtime::send(alert, "show", &[]);
        let panel = view_state(alert).subviews[0];
        assert!(class_is(panel, "UIImageView"), "the box is a picture");
        let backdrop = runtime::send(panel, "image", &[]);
        assert!(backdrop != 0);
        let [r, g, b, a] = middle(backdrop)[..] else { unreachable!() };
        assert!(b > r + 40 && b > g && r < 90 && a > 150, "dark blue: {r} {g} {b} {a}");
        let labels: Vec<Id> = inside(alert).into_iter().filter(|&v| class_is(v, "UILabel") && view_state(v).superview == panel).collect();
        assert_eq!(labels.len(), 2);
        assert!(labels.iter().all(|&l| white(get_id(l, "textColor"))), "white title and message");
        let buttons: Vec<Id> = inside(alert).into_iter().filter(|&v| class_is(v, "UIButton")).collect();
        assert_eq!(buttons.len(), 2);
        for &button in &buttons {
            assert!(runtime::send(button, "backgroundImageForState:", &[0]) != 0, "glossy button picture");
            assert!(white(runtime::send(button, "titleColorForState:", &[0])), "white button title");
        }
        runtime::send(alert, "dismissWithClickedButtonIndex:animated:", &[0, 0]);

        let sheet = new_view_named("UIActionSheet", Rect::default());
        runtime::send(sheet, "addButtonWithTitle:", &[string::make("Delete save")]);
        runtime::send(sheet, "addButtonWithTitle:", &[string::make("Cancel")]);
        runtime::send(sheet, "setDestructiveButtonIndex:", &[0]);
        runtime::send(sheet, "setCancelButtonIndex:", &[1]);
        runtime::send(sheet, "showInView:", &[window]);
        let panel = view_state(sheet).subviews[0];
        assert!(class_is(panel, "UIImageView"));
        let [r, g, b, _] = middle(runtime::send(panel, "image", &[]))[..] else { unreachable!() };
        assert!(r < 110 && g < 110 && b < 110, "a dark panel: {r} {g} {b}");
        let buttons: Vec<Id> = inside(sheet).into_iter().filter(|&v| class_is(v, "UIButton")).collect();
        let [r, g, b, _] = middle(runtime::send(buttons[0], "backgroundImageForState:", &[0]))[..] else { unreachable!() };
        assert!(r > g + 60 && r > b + 60, "the destructive button is red: {r} {g} {b}");
        runtime::send(sheet, "dismissWithClickedButtonIndex:animated:", &[1, 0]);
        IOS6_LOOK.with(|l| l.set(None));
    }

    /// showsTouchWhenHighlighted: as UIKit's -setShowsTouchWhenHighlighted:,
    /// the button gets a see-through image view of a white glow (tag 'prsd'),
    /// centred, not touchable; pressed it shows at once, let go it fades out
    /// over 0.25 s (-[UIButton setHighlighted:]); turned off it goes.
    #[test]
    fn a_pressed_button_can_glow() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let alpha = |view: Id| {
            let s = runtime::sel("alpha");
            let f: extern "C" fn(Id, Sel) -> f64 = unsafe { std::mem::transmute(runtime::imp_for(view, s).unwrap()) };
            f(view, s)
        };
        let button = runtime::send(runtime::class_named("UIButton"), "buttonWithType:", &[0]);
        crate::fw::uikit::send_rect(button, "setFrame:", Rect::new(10.0, 10.0, 100.0, 40.0));
        assert_eq!(runtime::send(button, "viewWithTag:", &[GLOW_TAG]), 0);
        runtime::send(button, "setShowsTouchWhenHighlighted:", &[1]);
        let glow = runtime::send(button, "viewWithTag:", &[GLOW_TAG]);
        assert!(glow != 0 && runtime::send(glow, "isKindOfClass:", &[runtime::class_named("UIImageView")]) & 0xFF != 0);
        assert_eq!(runtime::send(glow, "superview", &[]), button);
        assert_eq!(runtime::send(glow, "isUserInteractionEnabled", &[]) & 0xFF, 0);
        assert_eq!(alpha(glow), 0.0, "hidden until pressed");
        let frame = view_state(glow).frame;
        assert_eq!((frame.origin.x + frame.size.width / 2.0, frame.origin.y + frame.size.height / 2.0), (50.0, 20.0), "centred in the button");
        // Our own drawing: white, solid in the middle, gone at the corners.
        let image = runtime::send(glow, "image", &[]);
        let pixels = crate::fw::graphics::straight_pixels(crate::fw::graphics::image_cg(image)).unwrap();
        let side = (pixels.len() / 4).isqrt();
        let at = |x: usize, y: usize| &pixels[(y * side + x) * 4..][..4];
        assert!(at(side / 2, side / 2)[3] > 200 && at(side / 2, side / 2)[..3] == [255, 255, 255]);
        assert_eq!(at(0, 0)[3], 0);

        runtime::send(button, "setHighlighted:", &[1]);
        assert_eq!(alpha(glow), 1.0, "pressed: shown at once");
        let opacity = string::make("opacity");
        assert_eq!(runtime::send(runtime::send(glow, "layer", &[]), "animationForKey:", &[opacity]), 0);
        runtime::send(button, "setHighlighted:", &[0]);
        assert_eq!(alpha(glow), 0.0);
        let fade = runtime::send(runtime::send(glow, "layer", &[]), "animationForKey:", &[opacity]);
        assert!(fade != 0, "let go: it fades out");
        let s = runtime::sel("duration");
        let duration: extern "C" fn(Id, Sel) -> f64 = unsafe { std::mem::transmute(runtime::imp_for(fade, s).unwrap()) };
        assert_eq!(duration(fade, s), 0.25);

        runtime::send(button, "setShowsTouchWhenHighlighted:", &[0]);
        assert_eq!(runtime::send(button, "viewWithTag:", &[GLOW_TAG]), 0, "turned off: gone");
        assert_eq!(runtime::send(glow, "superview", &[]), 0);
    }

    /// As -[UIButton layoutSubviews]: the background, picture and title are
    /// the button's subviews (UIImageView, UIImageView, UIButtonLabel), made
    /// when it is first laid out (a normal-state setter only marks it), in
    /// that order, placed by its rect methods and not touchable, so a tap
    /// still lands on the button. Pressed, the parts with no pressed picture
    /// of their own are drawn darker (drawMode 2), but not under a glow; the
    /// glow goes below the picture. A part with nothing to show is hidden.
    #[test]
    fn a_buttons_parts_are_its_subviews() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let send_rect_for = |object: Id, name: &str, rect: Rect| {
            let s = runtime::sel(name);
            let f: extern "C" fn(Id, Sel, Rect) -> Rect = unsafe { std::mem::transmute(runtime::imp_for(object, s).expect(name)) };
            f(object, s, rect)
        };
        let is = |view: Id, class: &str| class_is(view, class);
        let window = new_view_named("UIWindow", Rect::new(0.0, 0.0, 320.0, 480.0));
        let button = runtime::send(runtime::class_named("UIButton"), "buttonWithType:", &[0]);
        crate::fw::uikit::send_rect(button, "setFrame:", Rect::new(10.0, 10.0, 100.0, 40.0));
        runtime::send(window, "addSubview:", &[button]);
        let (background, icon) = (crate::fw::graphics::test_solid_image(20.0, 20.0, [1.0, 0.0, 0.0, 1.0]), crate::fw::graphics::test_solid_image(20.0, 10.0, [0.0, 0.0, 1.0, 1.0]));
        runtime::send(button, "setBackgroundImage:forState:", &[background, 0]);
        runtime::send(button, "setImage:forState:", &[icon, 0]);
        runtime::send(button, "setTitle:forState:", &[string::make("Go"), 0]);
        assert!(view_state(button).subviews.is_empty(), "not laid out yet");
        runtime::send(button, "layoutIfNeeded", &[]);
        let parts = view_state(button).subviews.clone();
        assert_eq!(parts.len(), 3, "{parts:x?}");
        let [back, image, title] = [parts[0], parts[1], parts[2]];
        assert!(is(back, "UIImageView") && is(image, "UIImageView") && is(title, "UIButtonLabel") && is(title, "UILabel"));
        assert_eq!((runtime::send(button, "imageView", &[]), runtime::send(button, "titleLabel", &[])), (image, title));
        assert_eq!((runtime::send(back, "image", &[]), runtime::send(image, "image", &[])), (background, icon));
        assert_eq!(text_of(runtime::send(title, "text", &[])), "Go");
        let bounds = Rect::new(0.0, 0.0, 100.0, 40.0);
        let content = send_rect_for(button, "contentRectForBounds:", bounds);
        assert_eq!(frame_of(back), send_rect_for(button, "backgroundRectForBounds:", bounds));
        assert_eq!(frame_of(image), send_rect_for(button, "imageRectForContentRect:", content));
        assert_eq!(frame_of(title), send_rect_for(button, "titleRectForContentRect:", content));
        assert_eq!(frame_of(image).size, Size { width: 20.0, height: 10.0 });
        assert_eq!(frame_of(image).origin.x + 20.0, frame_of(title).origin.x, "side by side");
        for part in parts.iter() {
            assert_eq!(runtime::send(*part, "isUserInteractionEnabled", &[]) & 0xFF, 0);
        }
        assert_eq!(crate::fw::touch::hit_test(window, Point { x: 60.0, y: 30.0 }), button, "a tap on the title lands on the button");
        // The title label's own settings show in what it draws.
        runtime::send(title, "setFont:", &[font(BOLD_FONT, 20.0)]);
        assert_eq!(label_look(title).unwrap().font, font_name_and_size(font(BOLD_FONT, 20.0)));
        // Pressed with no pressed pictures: drawn darker; under a glow, not.
        let draw_mode = |view: Id| property_get(view, "drawMode").unwrap_or(0);
        runtime::send(button, "setHighlighted:", &[1]);
        runtime::send(button, "layoutIfNeeded", &[]);
        assert_eq!((draw_mode(back), draw_mode(image)), (2, 2));
        runtime::send(button, "setShowsTouchWhenHighlighted:", &[1]);
        runtime::send(button, "layoutIfNeeded", &[]);
        assert_eq!((draw_mode(back), draw_mode(image)), (0, 0));
        let glow = runtime::send(button, "viewWithTag:", &[GLOW_TAG]);
        assert_eq!(view_state(button).subviews, [back, glow, image, title], "the glow below the picture");
        // No title: its label is hidden.
        runtime::send(button, "setTitle:forState:", &[0, 0]);
        runtime::send(button, "setHighlighted:", &[0]);
        runtime::send(button, "layoutIfNeeded", &[]);
        assert_eq!(runtime::send(title, "isHidden", &[]) & 0xFF, 1);
        runtime::send(button, "removeFromSuperview", &[]);
    }

    /// A game's change to the title label's font (any -[UIButtonLabel
    /// set...] that changes the title's size) marks the button for layout
    /// (-_titleAttributesChanged), so the title's frame fits the new font.
    #[test]
    fn a_title_label_font_change_lays_the_button_out_again() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let button = runtime::send(runtime::class_named("UIButton"), "buttonWithType:", &[0]);
        crate::fw::uikit::send_rect(button, "setFrame:", Rect::new(0.0, 0.0, 200.0, 60.0));
        runtime::send(button, "setTitle:forState:", &[string::make("Equip"), 0]);
        let title = runtime::send(button, "titleLabel", &[]);
        let small = frame_of(title).size;
        runtime::send(title, "setFont:", &[font(SYSTEM_FONT, 30.0)]);
        runtime::send(button, "layoutIfNeeded", &[]);
        let big = frame_of(title).size;
        assert!(big.width > small.width * 1.5 && big.height > small.height * 1.5, "{small:?} -> {big:?}");
    }

    /// A button's new size lays it out at once (-[UIButton setFrame:]:
    /// -layoutBelowIfNeeded), so its parts follow inside a size animation;
    /// -sizeToFit goes through -setFrame:, so the parts and autoresizing
    /// subviews follow it too.
    #[test]
    fn a_buttons_new_size_lays_it_out_at_once() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let button = runtime::send(runtime::class_named("UIButton"), "buttonWithType:", &[0]);
        crate::fw::uikit::send_rect(button, "setFrame:", Rect::new(0.0, 0.0, 100.0, 40.0));
        runtime::send(button, "setTitle:forState:", &[string::make("Go"), 0]);
        let title = runtime::send(button, "titleLabel", &[]);
        let middle = |view: Id| frame_of(view).origin.x + frame_of(view).size.width / 2.0;
        assert!((middle(title) - 50.0).abs() < 1e-9);
        crate::fw::uikit::send_rect(button, "setFrame:", Rect::new(0.0, 0.0, 200.0, 40.0));
        assert!((middle(title) - 100.0).abs() < 1e-9, "laid out at once: {:?}", frame_of(title));
        let child = new_view_named("UIView", Rect::new(0.0, 0.0, 200.0, 5.0));
        runtime::send(child, "setAutoresizingMask:", &[2]);
        runtime::send(button, "addSubview:", &[child]);
        runtime::send(button, "sizeToFit", &[]);
        let width = frame_of(button).size.width;
        assert!(width < 200.0, "{width}");
        assert_eq!(frame_of(child).size.width, width, "autoresized through -setFrame:");
        assert!((middle(title) - width / 2.0).abs() < 1e-9, "laid out: {:?}", frame_of(title));
    }

    /// Let go, the delegate hears scrollViewWillEndDragging:withVelocity:targetContentOffset:
    /// first (velocity in content points a millisecond, the target where the
    /// slow-down would stop), and the content stops where the delegate moved
    /// the target, as UIKit's -_endPanWithEvent: does.
    #[test]
    fn the_delegate_chooses_where_a_drag_stops() {
        let _turn = crate::fw::typing::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        crate::fw::runloop::as_main(delegate_chooses_stop);
    }

    fn delegate_chooses_stop() {
        use crate::fw::geometry::Size;
        use crate::fw::touch::{deliver_in, Phase};
        static HEARD: Mutex<Vec<String>> = Mutex::new(Vec::new());
        static TOLD: Mutex<Option<(Point, Point)>> = Mutex::new(None);
        extern "C" fn will_end(_this: Id, _: Sel, _scroll: Id, velocity: Point, target: *mut Point) {
            HEARD.lock().unwrap().push("willEndDragging".into());
            *TOLD.lock().unwrap() = Some((velocity, unsafe { *target }));
            unsafe { *target = Point { x: 200.0, y: 0.0 } };
        }
        extern "C" fn did_end(_this: Id, _: Sel, _scroll: Id, decelerate: u8) {
            HEARD.lock().unwrap().push(format!("didEndDragging {decelerate}"));
        }
        extern "C" fn note(_this: Id, s: Sel, _scroll: Id) {
            HEARD.lock().unwrap().push(lock().sel_name(s));
        }
        let delegate_class = define_class("StopTestDelegate", "NSObject", 8);
        methods(delegate_class, &[
            ("scrollViewWillEndDragging:withVelocity:targetContentOffset:", will_end as usize),
            ("scrollViewDidEndDragging:willDecelerate:", did_end as usize),
            ("scrollViewDidEndDecelerating:", note as usize),
        ]);
        let window = new_view_named("UIWindow", Rect::new(0.0, 0.0, 320.0, 480.0));
        let scroll = new_view_named("UIScrollView", Rect::new(0.0, 0.0, 200.0, 200.0));
        runtime::send(window, "addSubview:", &[scroll]);
        runtime::send(scroll, "setDelegate:", &[new_object("StopTestDelegate")]);
        runtime::send(scroll, "addSubview:", &[new_view_named("UIView", Rect::new(0.0, 0.0, 600.0, 200.0))]);
        let s = runtime::sel("setContentSize:");
        let set_size: extern "C" fn(Id, Sel, Size) = unsafe { std::mem::transmute(runtime::imp_for(scroll, s).unwrap()) };
        set_size(scroll, s, Size { width: 600.0, height: 200.0 });

        let finger = 772;
        deliver_in(window, finger, Phase::Began, Point { x: 150.0, y: 100.0 });
        deliver_in(window, finger, Phase::Moved, Point { x: 130.0, y: 100.0 });
        deliver_in(window, finger, Phase::Moved, Point { x: 90.0, y: 100.0 });
        assert_eq!(content_offset_of(scroll).x, 60.0);
        // The finger was going left at 500 points a second as it lifted.
        with_drag(finger, |d| d.velocity = Point { x: -500.0, y: 0.0 });
        deliver_in(window, finger, Phase::Ended, Point { x: 90.0, y: 100.0 });
        let (velocity, target) = TOLD.lock().unwrap().take().expect("told where it would stop");
        assert_eq!((velocity.x, velocity.y), (0.5, 0.0), "content points a millisecond");
        let coast = 500.0 / (-1000.0 * COAST_RATE.ln());
        assert!((target.x - (60.0 + coast)).abs() < 0.01 && target.y == 0.0, "where the slow-down stops: {target:?}");
        assert!(!COASTING.lock().unwrap().as_ref().is_some_and(|m| m.contains_key(&scroll)), "no free coast: it goes where it was told");
        slide_step(scroll, crate::fw::runloop::media_time() + 1.0);
        assert_eq!(content_offset_of(scroll).x, 200.0, "stopped at the delegate's target");
        assert_eq!(std::mem::take(&mut *HEARD.lock().unwrap()), ["willEndDragging", "didEndDragging 1", "scrollViewDidEndDecelerating:"]);

        // A paging scroll view doesn't ask (iOS 6 -_endPanWithEvent: skips it
        // when pagingEnabled): it goes to its page.
        runtime::send(scroll, "setPagingEnabled:", &[1]);
        send_point(scroll, "setContentOffset:", Point::default());
        let finger = 773;
        deliver_in(window, finger, Phase::Began, Point { x: 150.0, y: 100.0 });
        deliver_in(window, finger, Phase::Moved, Point { x: 100.0, y: 100.0 });
        deliver_in(window, finger, Phase::Moved, Point { x: 30.0, y: 100.0 });
        deliver_in(window, finger, Phase::Ended, Point { x: 30.0, y: 100.0 });
        assert!(TOLD.lock().unwrap().is_none(), "not asked while paging");
        slide_step(scroll, crate::fw::runloop::media_time() + 1.0);
        assert_eq!(content_offset_of(scroll).x, 200.0, "on its page");
        HEARD.lock().unwrap().clear();
    }

    /// A recognizer holding a touch back (delaysTouchesBegan) that fails
    /// quickly doesn't cut the scroll view's 0.15 s short: the content hears
    /// the touch once both have let it go.
    #[test]
    fn a_touch_waits_for_every_hold() {
        let _turn = crate::fw::typing::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        crate::fw::runloop::as_main(every_hold);
    }

    fn every_hold() {
        use crate::fw::geometry::Size;
        use crate::fw::touch::{content_delay_passed, deliver_in, touch_seq, Phase};
        static HEARD: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let heard = || std::mem::take(&mut *HEARD.lock().unwrap());
        extern "C" fn note(_this: Id, s: Sel, _: Id, _: Id) {
            HEARD.lock().unwrap().push(lock().sel_name(s).trim_end_matches(":withEvent:").to_string());
        }
        let content_class = define_class("EveryHoldTestContent", "UIView", 8);
        let touches = ["touchesBegan:withEvent:", "touchesMoved:withEvent:", "touchesEnded:withEvent:", "touchesCancelled:withEvent:"];
        methods(content_class, &touches.map(|name| (name, note as usize)));
        let window = new_view_named("UIWindow", Rect::new(0.0, 0.0, 320.0, 480.0));
        let scroll = new_view_named("UIScrollView", Rect::new(0.0, 0.0, 200.0, 200.0));
        runtime::send(window, "addSubview:", &[scroll]);
        let content = new_view_named("EveryHoldTestContent", Rect::new(0.0, 0.0, 200.0, 600.0));
        runtime::send(scroll, "addSubview:", &[content]);
        let s = runtime::sel("setContentSize:");
        let set_size: extern "C" fn(Id, Sel, Size) = unsafe { std::mem::transmute(runtime::imp_for(scroll, s).unwrap()) };
        set_size(scroll, s, Size { width: 200.0, height: 600.0 });
        let tap = runtime::send(runtime::send(runtime::class_named("UITapGestureRecognizer"), "alloc", &[]), "initWithTarget:action:", &[0, 0]);
        runtime::send(tap, "setDelaysTouchesBegan:", &[1]);
        runtime::send(content, "addGestureRecognizer:", &[tap]);

        let finger = 771;
        deliver_in(window, finger, Phase::Began, Point { x: 100.0, y: 100.0 });
        // Sideways past the tap's 10 points: the tap fails (the scroll view only moves up and down).
        deliver_in(window, finger, Phase::Moved, Point { x: 130.0, y: 100.0 });
        assert!(heard().is_empty(), "still within the scroll view's 0.15 s");
        content_delay_passed(finger, touch_seq(finger));
        assert_eq!(heard(), ["touchesBegan", "touchesMoved"]);
        deliver_in(window, finger, Phase::Ended, Point { x: 130.0, y: 100.0 });
        assert_eq!(heard(), ["touchesEnded"]);
    }

    /// What a scroll view held back reaches its content as it happened:
    /// touchesBegan: with the Began phase where the finger went down, even
    /// when heard after a move or at the lift. A finger number used again
    /// doesn't let the old touch's 0.15 s run out early for the new one.
    #[test]
    fn content_hears_a_held_touch_as_it_was() {
        let _turn = crate::fw::typing::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        crate::fw::runloop::as_main(held_touch_as_it_was);
    }

    fn held_touch_as_it_was() {
        use crate::fw::geometry::Size;
        use crate::fw::touch::{content_delay_passed, deliver_in, touch_seq, Phase};
        static HEARD: Mutex<Vec<String>> = Mutex::new(Vec::new());
        // Each message's (touch timestamp, event timestamp, fingers in the event).
        static STAMPS: Mutex<Vec<(f64, f64, usize)>> = Mutex::new(Vec::new());
        let heard = || std::mem::take(&mut *HEARD.lock().unwrap());
        extern "C" fn note(this: Id, s: Sel, touches: Id, event: Id) {
            let touch = runtime::send(touches, "anyObject", &[]);
            let phase = runtime::send(touch, "phase", &[]);
            let s2 = runtime::sel("locationInView:");
            let at: extern "C" fn(Id, Sel, Id) -> Point = unsafe { std::mem::transmute(runtime::imp_for(touch, s2).unwrap()) };
            let y = at(touch, s2, this).y;
            HEARD.lock().unwrap().push(format!("{} {phase} {y}", lock().sel_name(s).trim_end_matches(":withEvent:")));
            let s3 = runtime::sel("timestamp");
            let stamp: extern "C" fn(Id, Sel) -> f64 = unsafe { std::mem::transmute(runtime::imp_for(touch, s3).unwrap()) };
            let event_stamp: extern "C" fn(Id, Sel) -> f64 = unsafe { std::mem::transmute(runtime::imp_for(event, s3).unwrap()) };
            let fingers = runtime::send(runtime::send(event, "allTouches", &[]), "count", &[]);
            STAMPS.lock().unwrap().push((stamp(touch, s3), event_stamp(event, s3), fingers));
        }
        let content_class = define_class("HeldTouchTestContent", "UIView", 8);
        let touches = ["touchesBegan:withEvent:", "touchesMoved:withEvent:", "touchesEnded:withEvent:", "touchesCancelled:withEvent:"];
        methods(content_class, &touches.map(|name| (name, note as usize)));
        let window = new_view_named("UIWindow", Rect::new(0.0, 0.0, 320.0, 480.0));
        let scroll = new_view_named("UIScrollView", Rect::new(0.0, 0.0, 200.0, 200.0));
        runtime::send(window, "addSubview:", &[scroll]);
        runtime::send(scroll, "addSubview:", &[new_view_named("HeldTouchTestContent", Rect::new(0.0, 0.0, 200.0, 600.0))]);
        let s = runtime::sel("setContentSize:");
        let set_size: extern "C" fn(Id, Sel, Size) = unsafe { std::mem::transmute(runtime::imp_for(scroll, s).unwrap()) };
        set_size(scroll, s, Size { width: 200.0, height: 600.0 });
        let finger = 769;

        // A tap, then the same finger number again: the tap's timer is not the new touch's.
        deliver_in(window, finger, Phase::Began, Point { x: 100.0, y: 100.0 });
        let first = touch_seq(finger);
        deliver_in(window, finger, Phase::Ended, Point { x: 100.0, y: 100.0 });
        heard();
        deliver_in(window, finger, Phase::Began, Point { x: 100.0, y: 110.0 });
        content_delay_passed(finger, first);
        assert!(heard().is_empty(), "the old touch's 0.15 s doesn't release the new one");
        content_delay_passed(finger, touch_seq(finger));
        assert_eq!(heard(), ["touchesBegan 0 110"]);
        deliver_in(window, finger, Phase::Cancelled, Point { x: 100.0, y: 110.0 });
        heard();

        // Heard after a move under the slop: Began as it began, then the
        // move, each with its own time and the fingers down then (a second
        // finger went down outside the scroll view in between).
        STAMPS.lock().unwrap().clear();
        deliver_in(window, finger, Phase::Began, Point { x: 100.0, y: 100.0 });
        deliver_in(window, 770, Phase::Began, Point { x: 300.0, y: 300.0 });
        deliver_in(window, finger, Phase::Moved, Point { x: 100.0, y: 102.0 });
        content_delay_passed(finger, touch_seq(finger));
        assert_eq!(heard(), ["touchesBegan 0 100", "touchesMoved 1 102"]);
        let stamps = std::mem::take(&mut *STAMPS.lock().unwrap());
        let (began, moved) = (stamps[0], stamps[1]);
        assert!(began.0 < moved.0, "the touch's time when it began: {stamps:?}");
        assert_eq!(began.1, began.0, "the event's time when the finger went down");
        assert_eq!(moved.1, moved.0, "the event's time at the move");
        assert_eq!(began.2 + 1, moved.2, "the second finger joined the event after the first went down");
        deliver_in(window, 770, Phase::Ended, Point { x: 300.0, y: 300.0 });
        deliver_in(window, finger, Phase::Ended, Point { x: 100.0, y: 102.0 });
        assert_eq!(heard(), ["touchesEnded 3 102"]);

        // Let go within the 0.15 s: Began, then Ended.
        deliver_in(window, finger, Phase::Began, Point { x: 100.0, y: 100.0 });
        deliver_in(window, finger, Phase::Ended, Point { x: 100.0, y: 100.0 });
        assert_eq!(heard(), ["touchesBegan 0 100", "touchesEnded 3 100"]);
    }

    /// Old games turn their root view a quarter turn inside a portrait
    /// window (iOS 6 window model): a sideways swipe on the screen is an up
    /// or down move in window points. The scroll view measures the finger in
    /// its own frame, so SpellCraft's sideways shop row scrolls.
    #[test]
    fn a_scroll_view_in_a_turned_view_follows_the_finger_on_screen() {
        let _turn = crate::fw::typing::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        crate::fw::runloop::as_main(turned_scroll);
    }

    fn turned_scroll() {
        use crate::fw::geometry::{Size, Transform};
        use crate::fw::touch::{deliver_in, view_to_window, Phase};
        let window = new_view_named("UIWindow", Rect::new(0.0, 0.0, 320.0, 568.0));
        let root = new_view_named("UIView", Rect::new(0.0, 0.0, 568.0, 320.0));
        runtime::send(window, "addSubview:", &[root]);
        let s = runtime::sel("setTransform:");
        let turn: extern "C" fn(Id, Sel, Transform) = unsafe { std::mem::transmute(runtime::imp_for(root, s).unwrap()) };
        turn(root, s, Transform { a: 0.0, b: 1.0, c: -1.0, d: 0.0, tx: 0.0, ty: 0.0 });
        send_point(root, "setCenter:", Point { x: 160.0, y: 284.0 });
        let row = new_view_named("UIScrollView", Rect::new(100.0, 100.0, 200.0, 100.0));
        runtime::send(root, "addSubview:", &[row]);
        let cards = new_view_named("UIView", Rect::new(0.0, 0.0, 1000.0, 100.0));
        runtime::send(row, "addSubview:", &[cards]);
        let s = runtime::sel("setContentSize:");
        let set_size: extern "C" fn(Id, Sel, Size) = unsafe { std::mem::transmute(runtime::imp_for(row, s).unwrap()) };
        set_size(row, s, Size { width: 1000.0, height: 100.0 }); // sideways only
        let on_screen = |x: f64, y: f64| {
            let (x, y) = crate::fw::geometry::apply(view_to_window(root), x, y);
            Point { x, y }
        };
        let offset_x = || content_offset_of(row).x;

        // The finger moves left 50 points across the row, as the player sees it.
        let finger = 790;
        deliver_in(window, finger, Phase::Began, on_screen(250.0, 150.0));
        deliver_in(window, finger, Phase::Moved, on_screen(230.0, 150.0));
        deliver_in(window, finger, Phase::Moved, on_screen(200.0, 150.0));
        assert_eq!(offset_x(), 50.0, "the slop-crossing move, then each move, scroll by the finger's motion");
        deliver_in(window, finger, Phase::Ended, on_screen(200.0, 150.0));
    }

    /// Nested scroll views: the innermost one that can move on the finger's
    /// axis scrolls; an outer one takes over on the axis the inner one can't.
    #[test]
    fn nested_scroll_views_scroll_the_innermost_that_moves_on_the_axis() {
        let _turn = crate::fw::typing::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        crate::fw::runloop::as_main(nested_scrolls);
    }

    fn nested_scrolls() {
        use crate::fw::geometry::Size;
        use crate::fw::touch::{deliver_in, Phase};
        let window = new_view_named("UIWindow", Rect::new(0.0, 0.0, 320.0, 480.0));
        let outer = new_view_named("UIScrollView", Rect::new(0.0, 0.0, 300.0, 300.0));
        runtime::send(window, "addSubview:", &[outer]);
        let inner = new_view_named("UIScrollView", Rect::new(10.0, 10.0, 200.0, 200.0));
        runtime::send(outer, "addSubview:", &[inner]);
        let content = new_view_named("UIView", Rect::new(0.0, 0.0, 500.0, 200.0));
        runtime::send(inner, "addSubview:", &[content]);
        let set_size = |view: Id, w: f64, h: f64| {
            let s = runtime::sel("setContentSize:");
            let f: extern "C" fn(Id, Sel, Size) = unsafe { std::mem::transmute(runtime::imp_for(view, s).unwrap()) };
            f(view, s, Size { width: w, height: h });
        };
        set_size(outer, 300.0, 600.0); // taller than its bounds: vertical only
        set_size(inner, 500.0, 200.0); // wider than its bounds: horizontal only
        let offset_x = |view: Id| content_offset_of(view).x;
        let offset_y = |view: Id| content_offset_of(view).y;

        // A horizontal drag scrolls the inner view, not the outer one.
        let finger = 780;
        deliver_in(window, finger, Phase::Began, Point { x: 100.0, y: 100.0 });
        deliver_in(window, finger, Phase::Moved, Point { x: 70.0, y: 100.0 });
        deliver_in(window, finger, Phase::Ended, Point { x: 70.0, y: 100.0 });
        assert_eq!(offset_x(inner), 30.0, "the inner view scrolled horizontally");
        assert_eq!(offset_y(outer), 0.0, "the outer view did not move");

        // A vertical drag scrolls the outer view (the inner one can't move that way).
        let finger = 781;
        deliver_in(window, finger, Phase::Began, Point { x: 100.0, y: 100.0 });
        deliver_in(window, finger, Phase::Moved, Point { x: 100.0, y: 70.0 });
        deliver_in(window, finger, Phase::Ended, Point { x: 100.0, y: 70.0 });
        assert_eq!(offset_y(outer), 30.0, "the outer view scrolled vertically");
        assert_eq!(offset_x(inner), 30.0, "the inner view kept its horizontal offset");
    }

    /// A scroll view let go fast coasts (isDecelerating) and slows to a stop.
    #[test]
    fn a_flick_coasts_and_stops() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        crate::fw::runloop::as_main(flick_coasts_and_stops);
    }

    fn flick_coasts_and_stops() {
        use crate::fw::geometry::Size;
        let window = new_view_named("UIWindow", Rect::new(0.0, 0.0, 320.0, 480.0));
        let scroll = new_view_named("UIScrollView", Rect::new(0.0, 0.0, 200.0, 200.0));
        runtime::send(window, "addSubview:", &[scroll]);
        let s = runtime::sel("setContentSize:");
        let set_size: extern "C" fn(Id, Sel, Size) = unsafe { std::mem::transmute(runtime::imp_for(scroll, s).unwrap()) };
        set_size(scroll, s, Size { width: 200.0, height: 2000.0 });
        let offset_y = || content_offset_of(scroll).y;
        let decelerating = || runtime::send(scroll, "isDecelerating", &[]) & 0xFF != 0;

        // A fast upward flick: the content coasts up (the offset grows).
        start_coast(scroll, Point { x: 0.0, y: 500.0 });
        assert!(decelerating(), "coasting once the finger flicks");
        let mut t = crate::fw::runloop::media_time();
        let mut last = offset_y();
        let mut moved = false;
        for _ in 0..200 {
            t += 1.0 / 60.0;
            if !coast_step(scroll, t) {
                break;
            }
            if offset_y() > last {
                moved = true;
            }
            last = offset_y();
        }
        assert!(moved, "the content coasted along");
        assert!(!decelerating(), "the coast stopped");
        let stopped = offset_y();
        coast_step(scroll, t + 1.0);
        assert_eq!(offset_y(), stopped, "nothing moves after the coast stopped");
    }
}
