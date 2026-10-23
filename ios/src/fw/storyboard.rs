//! Storyboards: what UIKit's classes do with a nib's coder (-initWithCoder:),
//! and the storyboard flow: scenes and their views, segues, and the main
//! storyboard UIApplicationMain opens.
//!
//! Coders are read only through NSCoder messages (-decodeObjectForKey:,
//! -decodeBoolForKey:, -decodeIntegerForKey:, -decodeDoubleForKey:,
//! -decodeCGPointForKey: and the like, -containsValueForKey:), and nibs are
//! loaded only through +[UINib nibWithNibName:bundle:] and
//! -[UINib instantiateWithOwner:options:] (with UINibExternalObjects), so
//! the nib reader (fw/nib.rs) is free to work however it likes behind them.
//!
//! What the real Dungeon Plunder nibs keep (checked with a NIBArchive dump):
//! views: UIBounds (CGRect), UICenter (CGPoint), UISubviews, UIBackgroundColor,
//! UIAlpha, UIHidden, UIOpaque, UIClipsToBounds, UIAutoresizingMask,
//! UIAutoresizeSubviews, UIContentMode, UIUserInteractionDisabled (YES on
//! labels and image views); labels: UIText, UIFont, UITextColor,
//! UIShadowColor, UIShadowOffset (CGSize), UITextAlignment, UINumberOfLines,
//! UIMinimumFontSize, UIMinimumScaleFactor, UIAdjustsFontSizeToFit,
//! UIPreferredMaxLayoutWidth; image views: UIImage (a UIImageNibPlaceholder
//! with UIResourceName); buttons: UIButtonStatefulContent (state number ->
//! UIButtonContent with UITitle, UITitleColor, UIShadowColor, UIImage),
//! UIFont, UITitleShadowOffset, UIAdjustsImageWhenHighlighted/Disabled;
//! colours: UIRed/UIGreen/UIBlue/UIAlpha or UIWhite (floats) next to NSRGB /
//! NSWhite text, UISystemColorName; fonts: UIFontName, UIFontPointSize,
//! UIFontTraits (2 = bold), UISystemFont; controllers (the game's classes
//! through UIClassSwapper): UINibName, UIStoryboardSegueTemplates,
//! UIExternalObjectsTableForViewLoading, UIWantsFullScreenLayout,
//! UIModalTransitionStyle; segue templates (UIStoryboardModalSegueTemplate):
//! UIIdentifier, UIDestinationViewControllerIdentifier. A scene's nib
//! connects the controller's "storyboard" outlet to the external object
//! UIStoryboardPlaceholder, the owner's "sceneViewController" outlet to the
//! controller and each template's "viewController" outlet to it; its view
//! nib's owner is the controller ("view" outlet), its external objects are
//! the controller's table (UpstreamPlaceholder-N: the controller and its
//! segue templates), and buttons send -perform: to the templates.

use crate::fw::geometry::{Point, Rect, Size};
use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::plist::Value;
use crate::fw::uikit::{property_get, property_set, property_set_object, view_state};
use crate::fw::{collections, string};
use crate::objc::runtime::{self, lock, Class, Id, Sel};
use crate::symbols::Registry;

// ----- reading a coder: NSCoder messages only -----

/// Sends a coder message taking a key; None when the coder has no such method.
fn coder_call<R>(coder: Id, name: &str, key: &str) -> Option<R> {
    let sel = runtime::sel(name);
    let imp = runtime::imp_for(coder, sel)?;
    let function: extern "C" fn(Id, Sel, Id) -> R = unsafe { std::mem::transmute(imp) };
    Some(function(coder, sel, string::make(key)))
}

fn contains(coder: Id, key: &str) -> bool {
    coder_call::<bool>(coder, "containsValueForKey:", key).unwrap_or(false)
}

fn decode_object(coder: Id, key: &str) -> Id {
    coder_call::<Id>(coder, "decodeObjectForKey:", key).unwrap_or(0)
}

fn decode_bool(coder: Id, key: &str) -> bool {
    coder_call::<bool>(coder, "decodeBoolForKey:", key).unwrap_or(false)
}

fn decode_integer(coder: Id, key: &str) -> isize {
    coder_call::<isize>(coder, "decodeIntegerForKey:", key).unwrap_or(0)
}

fn decode_double(coder: Id, key: &str) -> f64 {
    coder_call::<f64>(coder, "decodeDoubleForKey:", key).unwrap_or(0.0)
}

fn decode_point(coder: Id, key: &str) -> Point {
    coder_call::<Point>(coder, "decodeCGPointForKey:", key).unwrap_or_default()
}

fn decode_size(coder: Id, key: &str) -> Size {
    coder_call::<Size>(coder, "decodeCGSizeForKey:", key).unwrap_or_default()
}

fn decode_rect(coder: Id, key: &str) -> Rect {
    coder_call::<Rect>(coder, "decodeCGRectForKey:", key).unwrap_or_default()
}

/// The object for a key, when the coder has one.
fn object_for(coder: Id, key: &str) -> Option<Id> {
    contains(coder, key).then(|| decode_object(coder, key))
}

fn text_for(coder: Id, key: &str) -> Option<String> {
    object_for(coder, key).filter(|&text| text != 0 && string::is_string(text)).map(string::get)
}

fn bool_for(coder: Id, key: &str) -> Option<bool> {
    contains(coder, key).then(|| decode_bool(coder, key))
}

fn integer_for(coder: Id, key: &str) -> Option<isize> {
    contains(coder, key).then(|| decode_integer(coder, key))
}

fn double_for(coder: Id, key: &str) -> Option<f64> {
    contains(coder, key).then(|| decode_double(coder, key))
}

// ----- sending to our own (framework) methods -----

/// Whether a class is the game's own (a 64-bit game's, or the host mirror
/// of a 32-bit game's class).
fn is_game_class(class: Class) -> bool {
    let from_game = lock().classes.get(&class).is_some_and(|record| record.from_game);
    from_game || crate::guest32::objc::mirror::mirror_guest_class(class).is_some()
}

/// The nearest class of ours an object is: the class itself, or the first
/// one above the game's own classes.
fn framework_class(object: Id) -> Class {
    let mut class = lock().class_of(object);
    for _ in 0..256 {
        if class == 0 || !is_game_class(class) {
            break;
        }
        class = lock().classes.get(&class).map(|record| record.superclass).unwrap_or(0);
    }
    class
}

/// Our own method for a message to an object, skipping the game's
/// overrides: what UIKit's -initWithCoder: does when it sets its own state
/// rather than calling methods a subclass may have changed.
fn framework_imp(object: Id, name: &str) -> Option<(usize, Sel)> {
    let class = framework_class(object);
    let sel = runtime::sel(name);
    let imp = lock().lookup(class, sel)?;
    Some((imp, sel))
}

fn own_send(object: Id, name: &str, arguments: &[usize]) {
    let Some((imp, sel)) = framework_imp(object, name) else { return };
    match arguments {
        [] => unsafe { std::mem::transmute::<usize, extern "C" fn(Id, Sel)>(imp)(object, sel) },
        [a] => unsafe { std::mem::transmute::<usize, extern "C" fn(Id, Sel, usize)>(imp)(object, sel, *a) },
        [a, b] => unsafe { std::mem::transmute::<usize, extern "C" fn(Id, Sel, usize, usize)>(imp)(object, sel, *a, *b) },
        _ => unreachable!("own_send takes up to two arguments"),
    }
}

fn own_send_double(object: Id, name: &str, value: f64) {
    if let Some((imp, sel)) = framework_imp(object, name) {
        unsafe { std::mem::transmute::<usize, extern "C" fn(Id, Sel, f64)>(imp)(object, sel, value) }
    }
}

fn own_send_size(object: Id, name: &str, value: Size) {
    if let Some((imp, sel)) = framework_imp(object, name) {
        unsafe { std::mem::transmute::<usize, extern "C" fn(Id, Sel, Size)>(imp)(object, sel, value) }
    }
}

/// [super initWithCoder:] from one of our classes: `class`'s parent's method.
fn super_init_with_coder(this: Id, class: &str, coder: Id) -> Id {
    let sel = runtime::sel("initWithCoder:");
    let class = runtime::class_named(class); // before taking the lock: it locks too
    let imp = {
        let mut runtime_lock = lock();
        let parent = runtime_lock.classes.get(&class).map(|record| record.superclass).unwrap_or(0);
        runtime_lock.lookup(parent, sel)
    };
    match imp {
        Some(imp) => unsafe { std::mem::transmute::<usize, extern "C" fn(Id, Sel, Id) -> Id>(imp)(this, sel, coder) },
        None => this,
    }
}

fn get_id(object: Id, key: &'static str) -> Id {
    property_get(object, key).unwrap_or(0) as Id
}

fn get_text(object: Id, key: &'static str) -> String {
    match get_id(object, key) {
        0 => String::new(),
        text => string::get(text),
    }
}

fn is_kind_of(object: Id, class_name: &str) -> bool {
    let class = runtime::class_named(class_name);
    object != 0 && class != 0 && {
        let runtime_lock = lock();
        runtime_lock.is_subclass(runtime_lock.class_of(object), class)
    }
}

// ----- views -----

/// The frame a nib gives a view: its size from UIBounds, placed by its
/// centre (UICenter), as UIKit stores views.
fn decoded_frame(coder: Id) -> Rect {
    let size = if contains(coder, "UIBounds") { decode_rect(coder, "UIBounds").size } else { Size::default() };
    match contains(coder, "UICenter").then(|| decode_point(coder, "UICenter")) {
        Some(center) => Rect::new(center.x - size.width / 2.0, center.y - size.height / 2.0, size.width, size.height),
        None => Rect { origin: Point::default(), size },
    }
}

/// -[UIView initWithCoder:]. Our classes set themselves up in
/// -initWithFrame:, so the nearest one of ours does that (a game's own
/// -initWithFrame: is not run: iOS doesn't call it while decoding); then
/// what the nib stores is set straight into the view.
extern "C" fn view_init_with_coder(this: Id, _: Sel, coder: Id) -> Id {
    let frame = decoded_frame(coder);
    let this = match framework_imp(this, "initWithFrame:") {
        Some((imp, sel)) => unsafe { std::mem::transmute::<usize, extern "C" fn(Id, Sel, Rect) -> Id>(imp)(this, sel, frame) },
        None => this,
    };
    if this == 0 {
        return 0;
    }
    if contains(coder, "UIBounds") {
        view_state(this).bounds_origin = decode_rect(coder, "UIBounds").origin;
    }
    if let Some(color) = object_for(coder, "UIBackgroundColor") {
        property_set_object(this, "background", color);
    }
    if let Some(alpha) = double_for(coder, "UIAlpha") {
        property_set(this, "alpha", alpha.to_bits());
    }
    for (key, stored) in [
        ("UIHidden", "hidden"),
        ("UIOpaque", "opaque"),
        ("UIClipsToBounds", "clips"),
        ("UIAutoresizeSubviews", "autoresizes"),
        ("UIMultipleTouchEnabled", "multipleTouch"),
    ] {
        if let Some(value) = bool_for(coder, key) {
            property_set(this, stored, value as u64);
        }
    }
    if let Some(disabled) = bool_for(coder, "UIUserInteractionDisabled") {
        property_set(this, "userInteraction", !disabled as u64);
    }
    for (key, stored) in [("UITag", "tag"), ("UIAutoresizingMask", "autoresizing"), ("UIContentMode", "contentMode")] {
        if let Some(value) = integer_for(coder, key) {
            property_set(this, stored, value as u64);
        }
    }
    if let Some(subviews) = object_for(coder, "UISubviews") {
        for child in collections::array_items(subviews) {
            own_send(this, "addSubview:", &[child]);
        }
    }
    this
}

/// -[UILabel initWithCoder:]
extern "C" fn label_init_with_coder(this: Id, _: Sel, coder: Id) -> Id {
    let this = super_init_with_coder(this, "UILabel", coder);
    if this == 0 {
        return 0;
    }
    for (key, setter) in [
        ("UIText", "setText:"),
        ("UIFont", "setFont:"),
        ("UITextColor", "setTextColor:"),
        ("UIShadowColor", "setShadowColor:"),
        ("UIHighlightedColor", "setHighlightedTextColor:"),
    ] {
        if let Some(value) = object_for(coder, key) {
            own_send(this, setter, &[value]);
        }
    }
    if contains(coder, "UIShadowOffset") {
        own_send_size(this, "setShadowOffset:", decode_size(coder, "UIShadowOffset"));
    }
    for (key, setter) in [
        ("UITextAlignment", "setTextAlignment:"),
        ("UINumberOfLines", "setNumberOfLines:"),
        ("UILineBreakMode", "setLineBreakMode:"),
        ("UIBaselineAdjustment", "setBaselineAdjustment:"),
    ] {
        if let Some(value) = integer_for(coder, key) {
            own_send(this, setter, &[value as usize]);
        }
    }
    // Older nibs keep a smallest font size, newer ones a scale; ours keep
    // one value for both, and the size is what this UIKit works with.
    for (key, setter) in [("UIMinimumFontSize", "setMinimumFontSize:"), ("UIPreferredMaxLayoutWidth", "setPreferredMaxLayoutWidth:")] {
        if let Some(value) = double_for(coder, key) {
            own_send_double(this, setter, value);
        }
    }
    if let Some(adjusts) = bool_for(coder, "UIAdjustsFontSizeToFit") {
        own_send(this, "setAdjustsFontSizeToFitWidth:", &[adjusts as usize]);
    }
    for (key, setter) in [("UIEnabled", "setEnabled:"), ("UIHighlighted", "setHighlighted:")] {
        if let Some(value) = bool_for(coder, key) {
            own_send(this, setter, &[value as usize]);
        }
    }
    this
}

/// -[UIImageView initWithCoder:]
extern "C" fn image_view_init_with_coder(this: Id, _: Sel, coder: Id) -> Id {
    let this = super_init_with_coder(this, "UIImageView", coder);
    if this == 0 {
        return 0;
    }
    if let Some(image) = object_for(coder, "UIImage") {
        own_send(this, "setImage:", &[image]);
    }
    if let Some(image) = object_for(coder, "UIHighlightedImage") {
        property_set_object(this, "highlightedImage", image);
    }
    this
}

extern "C" fn image_view_highlighted_image(this: Id, _: Sel) -> Id {
    get_id(this, "highlightedImage")
}

extern "C" fn image_view_set_highlighted_image(this: Id, _: Sel, image: Id) {
    property_set_object(this, "highlightedImage", image);
}

/// -[UIControl initWithCoder:]
extern "C" fn control_init_with_coder(this: Id, _: Sel, coder: Id) -> Id {
    let this = super_init_with_coder(this, "UIControl", coder);
    if this == 0 {
        return 0;
    }
    if let Some(disabled) = bool_for(coder, "UIDisabled") {
        own_send(this, "setEnabled:", &[!disabled as usize]);
    }
    if let Some(selected) = bool_for(coder, "UISelected") {
        own_send(this, "setSelected:", &[selected as usize]);
    }
    for (key, setter) in [
        ("UIContentHorizontalAlignment", "setContentHorizontalAlignment:"),
        ("UIContentVerticalAlignment", "setContentVerticalAlignment:"),
    ] {
        if let Some(value) = integer_for(coder, key) {
            own_send(this, setter, &[value as usize]);
        }
    }
    this
}

/// -[UIButton initWithCoder:]: the title label's font and shadow, and per
/// state (UIButtonStatefulContent: state number -> UIButtonContent) the
/// title, its colours and the pictures.
extern "C" fn button_init_with_coder(this: Id, _: Sel, coder: Id) -> Id {
    let this = super_init_with_coder(this, "UIButton", coder);
    if this == 0 {
        return 0;
    }
    let label = runtime::send(this, "titleLabel", &[]);
    if label != 0 {
        if let Some(font) = object_for(coder, "UIFont") {
            own_send(label, "setFont:", &[font]);
        }
        if contains(coder, "UITitleShadowOffset") {
            own_send_size(label, "setShadowOffset:", decode_size(coder, "UITitleShadowOffset"));
        }
    }
    for (key, setter) in [
        ("UIAdjustsImageWhenHighlighted", "setAdjustsImageWhenHighlighted:"),
        ("UIAdjustsImageWhenDisabled", "setAdjustsImageWhenDisabled:"),
        ("UIShowsTouchWhenHighlighted", "setShowsTouchWhenHighlighted:"),
    ] {
        if let Some(value) = bool_for(coder, key) {
            own_send(this, setter, &[value as usize]);
        }
    }
    if let Some(content) = object_for(coder, "UIButtonStatefulContent") {
        for (state, content) in collections::dictionary_pairs(content) {
            let state = collections::number_as_i64(state) as usize;
            for (stored, setter) in [
                ("title", "setTitle:forState:"),
                ("titleColor", "setTitleColor:forState:"),
                ("shadowColor", "setTitleShadowColor:forState:"),
                ("image", "setImage:forState:"),
                ("backgroundImage", "setBackgroundImage:forState:"),
            ] {
                let value = get_id(content, stored);
                if value != 0 {
                    own_send(this, setter, &[value, state]);
                }
            }
        }
    }
    this
}

/// -[UIButtonContent initWithCoder:]: one state's content of a button.
extern "C" fn button_content_init_with_coder(this: Id, _: Sel, coder: Id) -> Id {
    for (key, stored) in [
        ("UITitle", "title"),
        ("UITitleColor", "titleColor"),
        ("UIShadowColor", "shadowColor"),
        ("UIImage", "image"),
        ("UIBackgroundImage", "backgroundImage"),
    ] {
        if let Some(value) = object_for(coder, key) {
            property_set_object(this, stored, value);
        }
    }
    this
}

// ----- colours, fonts and pictures -----

/// The numbers in the text older nibs keep a colour in ("0.5 0.5 0.5").
fn numbers_in(data: Id) -> Vec<f64> {
    let bytes = crate::fw::data::bytes(data);
    String::from_utf8_lossy(&bytes)
        .split(|c: char| c.is_whitespace() || c == ',' || c == '\0')
        .filter_map(|part| part.parse::<f64>().ok())
        .collect()
}

/// -[UIColor initWithCoder:]: a named system colour is that colour; else
/// the parts (UIRed...UIAlpha, or UIWhite), else the text older readers use
/// (NSRGB "r g b [a]", NSWhite "w [a]").
extern "C" fn color_init_with_coder(this: Id, _: Sel, coder: Id) -> Id {
    if let Some(name) = text_for(coder, "UISystemColorName") {
        let class = runtime::class_named("UIColor");
        let meta_responds = runtime::responds(class, &name);
        if meta_responds {
            return runtime::send(class, &name, &[]);
        }
    }
    let alpha = double_for(coder, "UIAlpha");
    let parts = if contains(coder, "UIRed") {
        [decode_double(coder, "UIRed"), decode_double(coder, "UIGreen"), decode_double(coder, "UIBlue"), alpha.unwrap_or(1.0)]
    } else if let Some(white) = double_for(coder, "UIWhite") {
        [white, white, white, alpha.unwrap_or(1.0)]
    } else if let Some(rgb) = object_for(coder, "NSRGB") {
        match numbers_in(rgb)[..] {
            [red, green, blue, alpha, ..] => [red, green, blue, alpha],
            [red, green, blue] => [red, green, blue, 1.0],
            _ => [0.0, 0.0, 0.0, 1.0],
        }
    } else if let Some(white) = object_for(coder, "NSWhite") {
        match numbers_in(white)[..] {
            [white, alpha, ..] => [white, white, white, alpha],
            [white] => [white, white, white, 1.0],
            _ => [0.0, 0.0, 0.0, 1.0],
        }
    } else {
        [0.0, 0.0, 0.0, 1.0]
    };
    // UIColor keeps its parts after the isa (fw/uikit.rs).
    unsafe { *((this + 8) as *mut [f64; 4]) = parts };
    this
}

/// -[UIFont initWithCoder:]: the font of that name and size (fonts are
/// shared, fw/controls.rs); a nib's "system font" is ours, bold or italic
/// as its traits say.
extern "C" fn font_init_with_coder(_this: Id, _: Sel, coder: Id) -> Id {
    let size = double_for(coder, "UIFontPointSize").filter(|&size| size > 0.0).unwrap_or(12.0);
    let name = text_for(coder, "UIFontName").unwrap_or_default();
    if name.is_empty() || bool_for(coder, "UISystemFont").unwrap_or(false) {
        let traits = integer_for(coder, "UIFontTraits").unwrap_or(0);
        return crate::fw::controls::system_font(traits, size);
    }
    crate::fw::controls::font(&name, size)
}

/// -[NSObject awakeFromNib]: nothing by default.
extern "C" fn awake_from_nib(_this: Id, _: Sel) {}

// ----- view controllers -----

/// -[UIViewController initWithCoder:]: what a storyboard scene keeps
/// about its controller.
extern "C" fn controller_init_with_coder(this: Id, _: Sel, coder: Id) -> Id {
    for (key, stored) in [
        ("UINibName", "nibName"),
        ("UIStoryboardSegueTemplates", "segueTemplates"),
        ("UIExternalObjectsTableForViewLoading", "externalObjectsForViewLoading"),
        ("UIStoryboardIdentifier", "storyboardIdentifier"),
    ] {
        if let Some(value) = object_for(coder, key) {
            property_set_object(this, stored, value);
        }
    }
    if let Some(style) = integer_for(coder, "UIModalTransitionStyle") {
        property_set(this, "modalTransitionStyle", style as u64);
    }
    this
}

/// -[UIViewController initWithNibName:bundle:]: the view comes from that
/// nib when first asked for.
pub extern "C" fn controller_init_with_nib(this: Id, _: Sel, name: Id, bundle: Id) -> Id {
    if name != 0 {
        property_set_object(this, "nibName", name);
    }
    if bundle != 0 {
        property_set(this, "nibBundle", bundle as u64);
    }
    this
}

extern "C" fn controller_nib_name(this: Id, _: Sel) -> Id {
    get_id(this, "nibName")
}

extern "C" fn controller_nib_bundle(this: Id, _: Sel) -> Id {
    get_id(this, "nibBundle")
}

extern "C" fn controller_storyboard(this: Id, _: Sel) -> Id {
    get_id(this, "storyboard")
}

extern "C" fn controller_set_storyboard(this: Id, _: Sel, storyboard: Id) {
    property_set(this, "storyboard", storyboard as u64);
}

pub extern "C" fn controller_modal_transition_style(this: Id, _: Sel) -> isize {
    property_get(this, "modalTransitionStyle").unwrap_or(0) as isize
}

pub extern "C" fn controller_set_modal_transition_style(this: Id, _: Sel, style: isize) {
    property_set(this, "modalTransitionStyle", style as u64);
}

/// -[UIViewController loadView]: a controller with a nib (a storyboard
/// scene's "-view-" nib, or initWithNibName:bundle:'s) gets its view from
/// it, as the nib's owner; others get a plain view (fw/uikit.rs).
pub extern "C" fn controller_load_view(this: Id, _: Sel) {
    let nib_name = get_text(this, "nibName");
    if nib_name.is_empty() {
        return;
    }
    let storyboard = get_id(this, "storyboard");
    let (name, bundle) = if storyboard != 0 {
        (format!("{}/{nib_name}", get_text(storyboard, "folder")), get_id(storyboard, "bundle"))
    } else {
        let bundle = match get_id(this, "nibBundle") {
            0 => main_bundle(),
            bundle => bundle,
        };
        (nib_name, bundle)
    };
    let objects = instantiate_nib(&name, bundle, this, get_id(this, "externalObjectsForViewLoading"));
    if property_get(this, "view").is_none() {
        // The nib should have set the "view" outlet; else its first view.
        let view = objects.iter().copied().find(|&object| is_kind_of(object, "UIView")).unwrap_or(0);
        if view != 0 {
            runtime::send(this, "setView:", &[view]);
        } else {
            crate::log!("{} {this:#x}: nib {name} gave no view", string::describe_class_of(this));
        }
    }
}

// ----- nibs (fw/nib.rs reads them; only UINib's messages are used here) -----

fn nib_class() -> Class {
    #[cfg(test)]
    if let Some(class) = runtime::existing_class(TEST_NIB_CLASS, false) {
        return class;
    }
    runtime::existing_class("UINib", false).unwrap_or(0)
}

/// The top-level objects of a nib: +[UINib nibWithNibName:bundle:] then
/// -instantiateWithOwner:options: with the external objects.
fn instantiate_nib(name: &str, bundle: Id, owner: Id, external_objects: Id) -> Vec<Id> {
    let class = nib_class();
    if class == 0 || !runtime::responds(class, "nibWithNibName:bundle:") {
        crate::log!("nib {name}: there is no nib reader (UINib)");
        return Vec::new();
    }
    let nib = runtime::send(class, "nibWithNibName:bundle:", &[string::make(name), bundle]);
    if nib == 0 {
        crate::log!("nib {name}: not found");
        return Vec::new();
    }
    let options = if external_objects == 0 {
        0
    } else {
        collections::dictionary_from(&[(string::make("UINibExternalObjects"), external_objects)])
    };
    collections::array_items(runtime::send(nib, "instantiateWithOwner:options:", &[owner, options]))
}

fn main_bundle() -> Id {
    runtime::send(runtime::class_named("NSBundle"), "mainBundle", &[])
}

// ----- storyboards -----

/// A compiled storyboard's folder inside a bundle ("Name.storyboardc" at
/// the top or in a localisation folder), relative to the bundle.
fn storyboard_folder(bundle_path: &str, name: &str) -> Option<String> {
    let folder = format!("{}.storyboardc", name.trim_end_matches(".storyboardc"));
    let mut places = vec![String::new(), "Base.lproj".to_string(), "en.lproj".to_string(), "English.lproj".to_string()];
    let mut others: Vec<String> = std::fs::read_dir(bundle_path)
        .map(|list| list.filter_map(|entry| entry.ok()?.file_name().into_string().ok()).filter(|n| n.ends_with(".lproj")).collect())
        .unwrap_or_default();
    others.sort();
    places.extend(others);
    places
        .into_iter()
        .map(|place| if place.is_empty() { folder.clone() } else { format!("{place}/{folder}") })
        .find(|relative| std::path::Path::new(&format!("{bundle_path}/{relative}/Info.plist")).is_file())
}

/// +[UIStoryboard storyboardWithName:bundle:]
extern "C" fn storyboard_with_name(_: Class, _: Sel, name: Id, bundle: Id) -> Id {
    if name == 0 {
        return 0;
    }
    let name = string::get(name);
    let bundle = if bundle == 0 { main_bundle() } else { bundle };
    let path = if bundle == 0 { String::new() } else { string::get(runtime::send(bundle, "bundlePath", &[])) };
    let Some(folder) = storyboard_folder(&path, &name) else {
        crate::log!("storyboard {name}: not in {path}");
        return 0;
    };
    let Some(info) = std::fs::read(format!("{path}/{folder}/Info.plist")).ok().and_then(|bytes| crate::fw::plist::parse(&bytes)) else {
        crate::log!("storyboard {name}: {folder}/Info.plist can't be read");
        return 0;
    };
    let storyboard = runtime::send(runtime::send(runtime::class_named("UIStoryboard"), "alloc", &[]), "init", &[]);
    property_set(storyboard, "folder", string::make(&folder) as u64);
    property_set(storyboard, "bundle", bundle as u64);
    if let Some(Value::String(entry)) = info.get("UIStoryboardDesignatedEntryPointIdentifier") {
        property_set(storyboard, "entryPoint", string::make(entry) as u64);
    }
    let scenes = match info.get("UIViewControllerIdentifiersToNibNames") {
        Some(scenes @ Value::Dict(_)) => crate::fw::plist::to_object(scenes),
        _ => collections::dictionary_from(&[]),
    };
    property_set(storyboard, "scenes", scenes as u64);
    crate::log!("storyboard {name}: {folder}, starts at {}", get_text(storyboard, "entryPoint"));
    storyboard
}

/// -[UIStoryboard instantiateInitialViewController]
extern "C" fn storyboard_initial_controller(this: Id, _: Sel) -> Id {
    let entry = get_id(this, "entryPoint");
    if entry == 0 {
        crate::log!("storyboard {}: no starting scene", get_text(this, "folder"));
        return 0;
    }
    runtime::send(this, "instantiateViewControllerWithIdentifier:", &[entry])
}

/// -[UIStoryboard instantiateViewControllerWithIdentifier:]: the scene's
/// nib, owned by a scene object (its "sceneViewController" outlet names
/// the controller) with the storyboard as an external object.
extern "C" fn storyboard_controller_with_identifier(this: Id, _: Sel, identifier: Id) -> Id {
    if identifier == 0 {
        return 0;
    }
    let nib_name = collections::dictionary_get(get_id(this, "scenes"), identifier);
    let identifier_text = string::get(identifier);
    if nib_name == 0 {
        crate::log!("storyboard {}: no scene {identifier_text}", get_text(this, "folder"));
        return 0;
    }
    let scene = runtime::send(runtime::send(runtime::class_named("UIStoryboardScene"), "alloc", &[]), "init", &[]);
    let external = collections::dictionary_from(&[(string::make("UIStoryboardPlaceholder"), this)]);
    let name = format!("{}/{}", get_text(this, "folder"), string::get(nib_name));
    let objects = instantiate_nib(&name, get_id(this, "bundle"), scene, external);
    let controller = match get_id(scene, "sceneViewController") {
        0 => objects.iter().copied().find(|&object| is_kind_of(object, "UIViewController")).unwrap_or(0),
        controller => controller,
    };
    if controller == 0 {
        crate::log!("storyboard {}: scene {identifier_text} made no view controller", get_text(this, "folder"));
        return 0;
    }
    if get_id(controller, "storyboard") == 0 {
        property_set(controller, "storyboard", this as u64);
    }
    if get_id(controller, "storyboardIdentifier") == 0 {
        property_set_object(controller, "storyboardIdentifier", identifier);
    }
    crate::log!("storyboard scene {identifier_text}: {} {controller:#x}", string::describe_class_of(controller));
    controller
}

extern "C" fn scene_controller(this: Id, _: Sel) -> Id {
    get_id(this, "sceneViewController")
}

extern "C" fn scene_set_controller(this: Id, _: Sel, controller: Id) {
    property_set(this, "sceneViewController", controller as u64);
}

// ----- segues -----

/// -[UIStoryboardSegue initWithIdentifier:source:destination:]
extern "C" fn segue_init(this: Id, _: Sel, identifier: Id, source: Id, destination: Id) -> Id {
    property_set_object(this, "identifier", identifier);
    property_set(this, "source", source as u64);
    property_set(this, "destination", destination as u64);
    this
}

/// +[UIStoryboardSegue segueWithIdentifier:source:destination:performHandler:]
extern "C" fn segue_with_handler(class: Class, _: Sel, identifier: Id, source: Id, destination: Id, handler: Id) -> Id {
    let segue = runtime::send(class, "alloc", &[]);
    let segue = runtime::send(segue, "initWithIdentifier:source:destination:", &[identifier, source, destination]);
    if handler != 0 {
        property_set(segue, "performHandler", crate::libsystem::mach::block_copy(handler as *mut _) as u64);
    }
    segue
}

extern "C" fn segue_identifier(this: Id, _: Sel) -> Id {
    get_id(this, "identifier")
}

extern "C" fn segue_source(this: Id, _: Sel) -> Id {
    get_id(this, "source")
}

extern "C" fn segue_destination(this: Id, _: Sel) -> Id {
    get_id(this, "destination")
}

/// -[UIStoryboardSegue perform]: its handler, if it was made with one.
extern "C" fn segue_perform(this: Id, _: Sel) {
    let handler = get_id(this, "performHandler");
    if handler != 0 {
        unsafe { crate::libsystem::mach::call_block(handler) };
    }
}

/// A modal segue presents its destination over its source.
extern "C" fn modal_segue_perform(this: Id, _: Sel) {
    let (source, destination) = (get_id(this, "source"), get_id(this, "destination"));
    if source != 0 && destination != 0 {
        runtime::send(source, "presentViewController:animated:completion:", &[destination, 1, 0]);
    }
}

/// A push segue pushes its destination on the source's navigation controller.
extern "C" fn push_segue_perform(this: Id, _: Sel) {
    let (source, destination) = (get_id(this, "source"), get_id(this, "destination"));
    let navigation = if source == 0 { 0 } else { runtime::send(source, "navigationController", &[]) };
    if navigation != 0 && destination != 0 {
        runtime::send(navigation, "pushViewController:animated:", &[destination, 1]);
    } else {
        crate::log!("push segue {}: its source is in no navigation controller", get_text(this, "identifier"));
    }
}

/// A show segue pushes when the source is in a navigation controller and
/// presents otherwise.
extern "C" fn show_segue_perform(this: Id, s: Sel) {
    let source = get_id(this, "source");
    if source != 0 && runtime::send(source, "navigationController", &[]) != 0 {
        push_segue_perform(this, s);
    } else {
        modal_segue_perform(this, s);
    }
}

/// -[UIStoryboardSegueTemplate initWithCoder:]
extern "C" fn template_init_with_coder(this: Id, _: Sel, coder: Id) -> Id {
    for (key, stored) in [
        ("UIIdentifier", "identifier"),
        ("UIDestinationViewControllerIdentifier", "destinationIdentifier"),
        ("UISegueClassName", "segueClassName"),
    ] {
        if let Some(value) = object_for(coder, key) {
            property_set_object(this, stored, value);
        }
    }
    if let Some(style) = integer_for(coder, "UIModalTransitionStyle") {
        property_set(this, "modalTransitionStyle", style as u64 + 1);
    }
    this
}

extern "C" fn template_identifier(this: Id, _: Sel) -> Id {
    get_id(this, "identifier")
}

extern "C" fn template_destination_identifier(this: Id, _: Sel) -> Id {
    get_id(this, "destinationIdentifier")
}

extern "C" fn template_view_controller(this: Id, _: Sel) -> Id {
    get_id(this, "viewController")
}

extern "C" fn template_set_view_controller(this: Id, _: Sel, controller: Id) {
    property_set(this, "viewController", controller as u64);
}

/// The segue class each kind of template makes (a custom one names its own).
fn segue_class_for(template: Id) -> Class {
    let custom = get_text(template, "segueClassName");
    if !custom.is_empty() {
        let class = runtime::class_named(&custom);
        if class != 0 {
            return class;
        }
        crate::log!("segue {}: no class {custom}", get_text(template, "identifier"));
    }
    let kind = [
        ("UIStoryboardModalSegueTemplate", "UIStoryboardModalSegue"),
        ("UIStoryboardPushSegueTemplate", "UIStoryboardPushSegue"),
        ("UIStoryboardShowSegueTemplate", "UIStoryboardShowSegue"),
    ]
    .into_iter()
    .find(|(template_class, _)| is_kind_of(template, template_class))
    .map(|(_, segue_class)| segue_class)
    .unwrap_or("UIStoryboardShowSegue");
    runtime::class_named(kind)
}

/// What a template does once it is decided to go: makes the destination
/// scene, the segue, lets the source prepare it, and performs it.
fn perform_template(template: Id, sender: Id) {
    let source = get_id(template, "viewController");
    let identifier = get_text(template, "identifier");
    let storyboard = if source == 0 { 0 } else { runtime::send(source, "storyboard", &[]) };
    if storyboard == 0 {
        crate::log!("segue {identifier}: its view controller is in no storyboard");
        return;
    }
    let destination = runtime::send(storyboard, "instantiateViewControllerWithIdentifier:", &[get_id(template, "destinationIdentifier")]);
    if destination == 0 {
        crate::log!("segue {identifier}: no destination");
        return;
    }
    if let Some(style) = property_get(template, "modalTransitionStyle") {
        runtime::send(destination, "setModalTransitionStyle:", &[(style - 1) as usize]);
    }
    let segue = runtime::send(segue_class_for(template), "alloc", &[]);
    let segue = runtime::send(segue, "initWithIdentifier:source:destination:", &[get_id(template, "identifier"), source, destination]);
    crate::log!("segue {identifier}: {} -> {}", string::describe_class_of(source), string::describe_class_of(destination));
    runtime::send(source, "prepareForSegue:sender:", &[segue, sender]);
    runtime::send(segue, "perform", &[]);
}

/// -[UIStoryboardSegueTemplate perform:]: the action of a control wired to
/// a segue; the source controller may refuse it.
extern "C" fn template_perform(this: Id, _: Sel, sender: Id) {
    let source = get_id(this, "viewController");
    if source == 0 {
        return;
    }
    let should = framework_should_perform(source, get_id(this, "identifier"), sender);
    if should {
        perform_template(this, sender);
    }
}

/// -shouldPerformSegueWithIdentifier:sender:, answered as BOOL.
fn framework_should_perform(source: Id, identifier: Id, sender: Id) -> bool {
    let sel = runtime::sel("shouldPerformSegueWithIdentifier:sender:");
    match runtime::imp_for(source, sel) {
        Some(imp) => unsafe { std::mem::transmute::<usize, extern "C" fn(Id, Sel, Id, Id) -> bool>(imp)(source, sel, identifier, sender) },
        None => true,
    }
}

/// -[UIViewController performSegueWithIdentifier:sender:]: the scene's
/// template of that name goes (without asking shouldPerformSegue..., as iOS).
extern "C" fn controller_perform_segue(this: Id, _: Sel, identifier: Id, sender: Id) {
    let name = if identifier == 0 { String::new() } else { string::get(identifier) };
    let template = collections::array_items(get_id(this, "segueTemplates")).into_iter().find(|&template| get_text(template, "identifier") == name);
    match template {
        Some(template) => {
            if get_id(template, "viewController") == 0 {
                property_set(template, "viewController", this as u64);
            }
            perform_template(template, sender);
        }
        None => crate::log!("{} {this:#x}: no segue {name}", string::describe_class_of(this)),
    }
}

extern "C" fn should_perform_segue(_this: Id, _: Sel, _identifier: Id, _sender: Id) -> bool {
    true
}

extern "C" fn prepare_for_segue(_this: Id, _: Sel, _segue: Id, _sender: Id) {}

// ----- the main storyboard (UIApplicationMain) -----

/// The name of the storyboard an app starts from: Info.plist
/// UIMainStoryboardFile, its variant for this kind of device first.
pub fn main_storyboard_name(info: &Value, tablet: bool) -> Option<String> {
    let variant = if tablet { "UIMainStoryboardFile~ipad" } else { "UIMainStoryboardFile~iphone" };
    [variant, "UIMainStoryboardFile"].into_iter().find_map(|key| match info.get(key) {
        Some(Value::String(name)) if !name.is_empty() => Some(name.clone()),
        _ => None,
    })
}

/// What iOS does with the main storyboard before the app's launch code
/// (Apple's launch sequence: main storyboard, then
/// willFinishLaunchingWithOptions:, then didFinishLaunchingWithOptions:):
/// a window the size of the screen, handed to the delegate (-setWindow:),
/// with the storyboard's first controller as its root. The window, or 0
/// when the app has no main storyboard.
pub fn load_main_interface_from(info: &Value, bundle: Id, delegate: Id) -> Id {
    let tablet = crate::libsystem::io::device_model().starts_with("iPad");
    let Some(name) = main_storyboard_name(info, tablet) else { return 0 };
    let storyboard = runtime::send(runtime::class_named("UIStoryboard"), "storyboardWithName:bundle:", &[string::make(&name), bundle]);
    if storyboard == 0 {
        crate::log!("main storyboard {name}: can't be opened");
        return 0;
    }
    let (width, height, _) = crate::fw::uikit::screen_points();
    let window = runtime::send(runtime::class_named("UIWindow"), "alloc", &[]);
    let window = match framework_imp(window, "initWithFrame:") {
        Some((imp, sel)) => unsafe {
            std::mem::transmute::<usize, extern "C" fn(Id, Sel, Rect) -> Id>(imp)(window, sel, Rect::new(0.0, 0.0, width, height))
        },
        None => window,
    };
    if delegate != 0 && runtime::responds(delegate, "setWindow:") {
        runtime::send(delegate, "setWindow:", &[window]);
    } else {
        crate::log!("main storyboard {name}: the app delegate has no window property; the window is kept here");
    }
    let controller = runtime::send(storyboard, "instantiateInitialViewController", &[]);
    if controller != 0 {
        runtime::send(window, "setRootViewController:", &[controller]);
    }
    crate::log!("main storyboard {name}: window {window:#x}, root {}", string::describe_class_of(controller));
    window
}

/// The main storyboard of the running app (its Info.plist).
pub fn load_main_interface(delegate: Id) -> Id {
    let Some(app_dir) = crate::loader::try_app_dir() else { return 0 };
    let Some(info) = std::fs::read(format!("{app_dir}/Info.plist")).ok().and_then(|bytes| crate::fw::plist::parse(&bytes)) else {
        return 0;
    };
    load_main_interface_from(&info, main_bundle(), delegate)
}

/// After -application:didFinishLaunchingWithOptions:, iOS shows the app
/// delegate's window (the storyboard's, unless the app put up another);
/// a window already shown stays as it is.
pub fn show_main_window(delegate: Id, window: Id) {
    if window == 0 {
        return;
    }
    let shown = match delegate != 0 && runtime::responds(delegate, "window") {
        true => match runtime::send(delegate, "window", &[]) {
            0 => window,
            other => other,
        },
        false => window,
    };
    if !crate::fw::uikit::windows().contains(&shown) {
        runtime::send(shown, "makeKeyAndVisible", &[]);
    }
}

// ----- what the classes are given -----

/// The tests' stand-in for UINib (fw/storyboard_tests.rs).
#[cfg(test)]
const TEST_NIB_CLASS: &str = "StoryboardTestNib";

pub fn register(_r: &mut Registry) {
    methods(runtime::class_named("NSObject"), &[("awakeFromNib", awake_from_nib as usize)]);
    methods(runtime::class_named("UIView"), &[("initWithCoder:", view_init_with_coder as usize)]);
    methods(runtime::class_named("UILabel"), &[("initWithCoder:", label_init_with_coder as usize)]);
    methods(
        runtime::class_named("UIImageView"),
        &[
            ("initWithCoder:", image_view_init_with_coder as usize),
            ("highlightedImage", image_view_highlighted_image as usize),
            ("setHighlightedImage:", image_view_set_highlighted_image as usize),
        ],
    );
    methods(runtime::class_named("UIControl"), &[("initWithCoder:", control_init_with_coder as usize)]);
    methods(runtime::class_named("UIButton"), &[("initWithCoder:", button_init_with_coder as usize)]);
    let button_content = define_class("UIButtonContent", "NSObject", 8);
    methods(button_content, &[("initWithCoder:", button_content_init_with_coder as usize)]);
    methods(runtime::class_named("UIColor"), &[("initWithCoder:", color_init_with_coder as usize)]);
    methods(runtime::class_named("UIFont"), &[("initWithCoder:", font_init_with_coder as usize)]);
    // UIImageNibPlaceholder (a picture named in a nib) is in fw/nib.rs.

    // initWithNibName:bundle:, loadView and the modal transition style are
    // in fw/uikit.rs's own list (registering them twice would let a second
    // registration briefly undo the first).
    methods(
        runtime::class_named("UIViewController"),
        &[
            ("initWithCoder:", controller_init_with_coder as usize),
            ("nibName", controller_nib_name as usize),
            ("nibBundle", controller_nib_bundle as usize),
            ("storyboard", controller_storyboard as usize),
            ("setStoryboard:", controller_set_storyboard as usize),
            ("performSegueWithIdentifier:sender:", controller_perform_segue as usize),
            ("shouldPerformSegueWithIdentifier:sender:", should_perform_segue as usize),
            ("prepareForSegue:sender:", prepare_for_segue as usize),
        ],
    );

    let storyboard = define_class("UIStoryboard", "NSObject", 8);
    class_methods(storyboard, &[("storyboardWithName:bundle:", storyboard_with_name as usize)]);
    methods(
        storyboard,
        &[
            ("instantiateInitialViewController", storyboard_initial_controller as usize),
            ("instantiateViewControllerWithIdentifier:", storyboard_controller_with_identifier as usize),
        ],
    );
    let scene = define_class("UIStoryboardScene", "NSObject", 8);
    methods(scene, &[("sceneViewController", scene_controller as usize), ("setSceneViewController:", scene_set_controller as usize)]);

    let segue = define_class("UIStoryboardSegue", "NSObject", 8);
    class_methods(segue, &[("segueWithIdentifier:source:destination:performHandler:", segue_with_handler as usize)]);
    methods(
        segue,
        &[
            ("initWithIdentifier:source:destination:", segue_init as usize),
            ("identifier", segue_identifier as usize),
            ("sourceViewController", segue_source as usize),
            ("destinationViewController", segue_destination as usize),
            ("perform", segue_perform as usize),
        ],
    );
    for (name, perform) in [
        ("UIStoryboardModalSegue", modal_segue_perform as usize),
        ("UIStoryboardPushSegue", push_segue_perform as usize),
        ("UIStoryboardShowSegue", show_segue_perform as usize),
    ] {
        let class = define_class(name, "UIStoryboardSegue", 8);
        methods(class, &[("perform", perform)]);
    }
    let template = define_class("UIStoryboardSegueTemplate", "NSObject", 8);
    methods(
        template,
        &[
            ("initWithCoder:", template_init_with_coder as usize),
            ("identifier", template_identifier as usize),
            ("destinationViewControllerIdentifier", template_destination_identifier as usize),
            ("viewController", template_view_controller as usize),
            ("setViewController:", template_set_view_controller as usize),
            ("perform:", template_perform as usize),
        ],
    );
    for name in ["UIStoryboardModalSegueTemplate", "UIStoryboardPushSegueTemplate", "UIStoryboardShowSegueTemplate"] {
        define_class(name, "UIStoryboardSegueTemplate", 8);
    }
}

#[cfg(test)]
#[path = "storyboard_tests.rs"]
mod tests;
