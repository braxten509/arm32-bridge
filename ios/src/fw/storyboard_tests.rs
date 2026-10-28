//! Tests for storyboards and -initWithCoder:. A small coder answers NSCoder
//! messages from a map, and a stand-in UINib (the class named by
//! TEST_NIB_CLASS) hands out objects the test made, connecting outlets the
//! way a real nib does; so only Apple's API is used between the two.

use super::*;
use crate::fw::collections;
use crate::fw::controls::Insets;
use crate::fw::geometry::{Point, Rect, Size};
use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::string;
use crate::fw::uikit::{property_get, property_set, view_state};
use crate::objc::runtime::{self, lock, Class, Sel};
use std::collections::HashMap;
use std::sync::Mutex;

/// The image tests share the app folder and its once-made listing, so they
/// run one at a time and forget the listing between them.
static IMAGE_TESTS: Mutex<()> = Mutex::new(());

// ----- a coder that answers from a map -----

#[derive(Clone, Copy)]
enum Held {
    Object(Id),
    Bool(bool),
    Integer(i64),
    Double(f64),
    Point(Point),
    Size(Size),
    Rect(Rect),
    Insets(Insets),
}

static HELD: Mutex<Option<HashMap<Id, HashMap<String, Held>>>> = Mutex::new(None);

fn held(coder: Id, key: Id) -> Option<Held> {
    HELD.lock().unwrap().as_ref()?.get(&coder)?.get(&string::get(key)).copied()
}

extern "C" fn coder_object(this: Id, _: Sel, key: Id) -> Id {
    match held(this, key) {
        Some(Held::Object(object)) => object,
        _ => 0,
    }
}
extern "C" fn coder_contains(this: Id, _: Sel, key: Id) -> bool {
    held(this, key).is_some()
}
extern "C" fn coder_bool(this: Id, _: Sel, key: Id) -> bool {
    match held(this, key) {
        Some(Held::Bool(value)) => value,
        Some(Held::Integer(value)) => value != 0,
        _ => false,
    }
}
fn integer_of(this: Id, key: Id) -> i64 {
    match held(this, key) {
        Some(Held::Integer(value)) => value,
        Some(Held::Bool(value)) => value as i64,
        Some(Held::Double(value)) => value as i64,
        _ => 0,
    }
}
extern "C" fn coder_integer(this: Id, _: Sel, key: Id) -> i64 {
    integer_of(this, key)
}
extern "C" fn coder_int32(this: Id, _: Sel, key: Id) -> i32 {
    integer_of(this, key) as i32
}
fn double_value(this: Id, key: Id) -> f64 {
    match held(this, key) {
        Some(Held::Double(value)) => value,
        Some(Held::Integer(value)) => value as f64,
        _ => 0.0,
    }
}
extern "C" fn coder_double(this: Id, _: Sel, key: Id) -> f64 {
    double_value(this, key)
}
extern "C" fn coder_float(this: Id, _: Sel, key: Id) -> f32 {
    double_value(this, key) as f32
}
extern "C" fn coder_point(this: Id, _: Sel, key: Id) -> Point {
    match held(this, key) {
        Some(Held::Point(value)) => value,
        _ => Point::default(),
    }
}
extern "C" fn coder_size(this: Id, _: Sel, key: Id) -> Size {
    match held(this, key) {
        Some(Held::Size(value)) => value,
        _ => Size::default(),
    }
}
extern "C" fn coder_rect(this: Id, _: Sel, key: Id) -> Rect {
    match held(this, key) {
        Some(Held::Rect(value)) => value,
        _ => Rect::default(),
    }
}
extern "C" fn coder_insets(this: Id, _: Sel, key: Id) -> Insets {
    match held(this, key) {
        Some(Held::Insets(value)) => value,
        _ => Insets::default(),
    }
}

fn coder(values: Vec<(&str, Held)>) -> Id {
    let coder = runtime::alloc_instance(runtime::class_named("StoryboardTestCoder"));
    let map = values.into_iter().map(|(key, value)| (key.to_string(), value)).collect();
    HELD.lock().unwrap().get_or_insert_with(HashMap::new).insert(coder, map);
    coder
}

fn text(value: &str) -> Held {
    Held::Object(string::make(value))
}

fn array(list: &[Id]) -> Held {
    Held::Object(collections::array_from(list))
}

/// [[Class alloc] initWithCoder:coder]
fn decoded(class: &str, values: Vec<(&str, Held)>) -> Id {
    let object = runtime::send(runtime::class_named(class), "alloc", &[]);
    decoded_into(object, values)
}

fn decoded_into(object: Id, values: Vec<(&str, Held)>) -> Id {
    assert!(runtime::responds(object, "initWithCoder:"), "{} has no initWithCoder:", string::describe_class_of(object));
    runtime::send(object, "initWithCoder:", &[coder(values)])
}

// ----- a stand-in UINib: +nibWithNibName:bundle:, -instantiateWithOwner:options: -----

type NibContents = Box<dyn Fn(Id, Id) -> Vec<Id> + Send>;

static NIBS: Mutex<Vec<(String, NibContents)>> = Mutex::new(Vec::new());

#[derive(Clone, Debug)]
struct NibLoad {
    name: String,
    bundle: Id,
    owner: Id,
    external: Id,
}

static NIB_LOADS: Mutex<Vec<NibLoad>> = Mutex::new(Vec::new());

/// What the nib named `name` holds: `contents(owner, external objects)`
/// makes its objects and connects their outlets.
fn fake_nib(name: &str, contents: impl Fn(Id, Id) -> Vec<Id> + Send + 'static) {
    NIBS.lock().unwrap().push((name.to_string(), Box::new(contents)));
}

fn nib_loads(name: &str) -> Vec<NibLoad> {
    NIB_LOADS.lock().unwrap().iter().filter(|load| load.name == name).cloned().collect()
}

extern "C" fn test_nib_with_name(_: Class, _: Sel, name: Id, bundle: Id) -> Id {
    let text = string::get(name);
    if !NIBS.lock().unwrap().iter().any(|(known, _)| *known == text) {
        // Not one of these tests' nibs: the real reader's (other tests use it).
        return runtime::send(runtime::class_named("UINib"), "nibWithNibName:bundle:", &[name, bundle]);
    }
    let name = text;
    let nib = runtime::alloc_instance(runtime::class_named(TEST_NIB_CLASS));
    property_set(nib, "testNibName", string::make(&name) as u64);
    property_set(nib, "testNibBundle", bundle as u64);
    nib
}

extern "C" fn test_nib_instantiate(this: Id, _: Sel, owner: Id, options: Id) -> Id {
    let name = string::get(property_get(this, "testNibName").unwrap() as Id);
    let external = if options == 0 { 0 } else { collections::dictionary_get(options, string::make("UINibExternalObjects")) };
    let bundle = property_get(this, "testNibBundle").unwrap_or(0) as Id;
    NIB_LOADS.lock().unwrap().push(NibLoad { name: name.clone(), bundle, owner, external });
    let nibs = NIBS.lock().unwrap();
    let (_, contents) = nibs.iter().find(|(known, _)| *known == name).unwrap();
    collections::array_from(&contents(owner, external))
}

// ----- a view controller of the "game's" own class that notes what it is told -----

static EVENTS: Mutex<Vec<(Id, String)>> = Mutex::new(Vec::new());

fn note(this: Id, what: &str) {
    EVENTS.lock().unwrap().push((this, what.to_string()));
}

/// What these controllers were told since the last call, in order.
fn events_of(controllers: &[Id]) -> Vec<(Id, String)> {
    let mut events = EVENTS.lock().unwrap();
    let (mine, others): (Vec<_>, Vec<_>) = events.drain(..).partition(|(id, _)| controllers.contains(id));
    *events = others;
    mine
}

/// The animated flag as the notes write it.
fn yes_no(flag: bool) -> &'static str {
    if flag { "YES" } else { "NO" }
}

extern "C" fn noted_will_appear(this: Id, _: Sel, animated: bool) {
    note(this, &format!("viewWillAppear: {}", yes_no(animated)));
}
extern "C" fn noted_did_appear(this: Id, _: Sel, animated: bool) {
    note(this, &format!("viewDidAppear: {}", yes_no(animated)));
}
extern "C" fn noted_will_disappear(this: Id, _: Sel, animated: bool) {
    note(this, &format!("viewWillDisappear: {}", yes_no(animated)));
}
extern "C" fn noted_did_disappear(this: Id, _: Sel, animated: bool) {
    note(this, &format!("viewDidDisappear: {}", yes_no(animated)));
}
extern "C" fn noted_did_layout(this: Id, _: Sel) {
    note(this, "viewDidLayoutSubviews");
}
extern "C" fn noted_did_load(this: Id, _: Sel) {
    note(this, "viewDidLoad");
}
extern "C" fn noted_prepare(this: Id, _: Sel, segue: Id, sender: Id) {
    note(this, "prepareForSegue:sender:");
    property_set(this, "testPreparedSegue", segue as u64);
    property_set(this, "testPreparedSender", sender as u64);
}
extern "C" fn noted_should_perform(this: Id, _: Sel, _identifier: Id, _sender: Id) -> bool {
    note(this, "shouldPerformSegueWithIdentifier:sender:");
    property_get(this, "testRefuseSegues") != Some(1)
}

extern "C" fn no_forwarding(_this: Id, _: Sel) -> bool {
    false
}

/// A delegate that keeps the window it is given, like a real app delegate.
extern "C" fn delegate_set_window(this: Id, _: Sel, window: Id) {
    property_set(this, "testWindow", window as u64);
}
extern "C" fn delegate_window(this: Id, _: Sel) -> Id {
    property_get(this, "testWindow").unwrap_or(0) as Id
}

static GAME_INIT_WITH_FRAME: Mutex<Vec<Id>> = Mutex::new(Vec::new());

extern "C" fn game_view_init_with_frame(this: Id, _: Sel, _frame: Rect) -> Id {
    GAME_INIT_WITH_FRAME.lock().unwrap().push(this);
    this
}

/// What a game view's own setters were sent while it was decoded, with how
/// many subviews it had then.
static GAME_SETTERS: Mutex<Vec<(Id, &'static str, usize)>> = Mutex::new(Vec::new());

/// [super name:value] from the game view: UIView's own setter.
fn view_super<A>(this: Id, name: &str, value: A) {
    let sel = runtime::sel(name);
    let class = runtime::class_named("UIView");
    let imp = lock().lookup(class, sel).unwrap();
    unsafe { std::mem::transmute::<usize, extern "C" fn(Id, Sel, A)>(imp)(this, sel, value) }
}

extern "C" fn game_view_set_background(this: Id, _: Sel, color: Id) {
    GAME_SETTERS.lock().unwrap().push((this, "setBackgroundColor:", view_state(this).subviews.len()));
    view_super(this, "setBackgroundColor:", color);
}
extern "C" fn game_button_set_shows_touch(this: Id, _: Sel, _shows: u8) {
    GAME_SETTERS.lock().unwrap().push((this, "setShowsTouchWhenHighlighted:", 0));
}
extern "C" fn game_field_set_text(this: Id, _: Sel, _text: Id) {
    GAME_SETTERS.lock().unwrap().push((this, "setText:", 0));
}
extern "C" fn game_view_set_hidden(this: Id, _: Sel, hidden: u8) {
    GAME_SETTERS.lock().unwrap().push((this, "setHidden:", view_state(this).subviews.len()));
    view_super(this, "setHidden:", hidden);
}

fn setup() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        crate::run::setup();
        let coder_class = define_class("StoryboardTestCoder", "NSObject", 8);
        methods(
            coder_class,
            &[
                ("decodeObjectForKey:", coder_object as usize),
                ("containsValueForKey:", coder_contains as usize),
                ("decodeBoolForKey:", coder_bool as usize),
                ("decodeIntegerForKey:", coder_integer as usize),
                ("decodeIntForKey:", coder_int32 as usize),
                ("decodeInt32ForKey:", coder_int32 as usize),
                ("decodeInt64ForKey:", coder_integer as usize),
                ("decodeFloatForKey:", coder_float as usize),
                ("decodeDoubleForKey:", coder_double as usize),
                ("decodeCGPointForKey:", coder_point as usize),
                ("decodeCGSizeForKey:", coder_size as usize),
                ("decodeCGRectForKey:", coder_rect as usize),
                ("decodeUIEdgeInsetsForKey:", coder_insets as usize),
            ],
        );
        let nib_class = define_class(TEST_NIB_CLASS, "NSObject", 8);
        class_methods(nib_class, &[("nibWithNibName:bundle:", test_nib_with_name as usize)]);
        methods(nib_class, &[("instantiateWithOwner:options:", test_nib_instantiate as usize)]);
        let controller = define_class("StoryboardTestController", "UIViewController", 8);
        methods(
            controller,
            &[
                ("viewWillAppear:", noted_will_appear as usize),
                ("viewDidAppear:", noted_did_appear as usize),
                ("viewWillDisappear:", noted_will_disappear as usize),
                ("viewDidDisappear:", noted_did_disappear as usize),
                ("prepareForSegue:sender:", noted_prepare as usize),
                ("shouldPerformSegueWithIdentifier:sender:", noted_should_perform as usize),
            ],
        );
        // The same, also noting when its view is loaded.
        let loading = define_class("StoryboardTestLoadingController", "StoryboardTestController", 8);
        methods(loading, &[("viewDidLoad", noted_did_load as usize)]);
        // The same, also noting its layouts.
        let laying = define_class("StoryboardTestLayoutController", "StoryboardTestController", 8);
        methods(laying, &[("viewDidLayoutSubviews", noted_did_layout as usize)]);
        // A container that tells its children itself (iOS 5's automatic forwarding off).
        let quiet = define_class("StoryboardTestQuietController", "StoryboardTestController", 8);
        methods(quiet, &[("automaticallyForwardAppearanceAndRotationMethodsToChildViewControllers", no_forwarding as usize)]);
        let delegate = define_class("StoryboardTestDelegate", "NSObject", 8);
        methods(delegate, &[("setWindow:", delegate_set_window as usize), ("window", delegate_window as usize)]);
        // A view class of the game's own (as if loaded from its program).
        let game_view = define_class("StoryboardTestGameView", "UIView", 16);
        methods(game_view, &[("initWithFrame:", game_view_init_with_frame as usize)]);
        lock().classes.get_mut(&game_view).unwrap().from_game = true;
        // A game view with its own setters (as games override them).
        let setter_view = define_class("StoryboardTestSetterView", "UIView", 16);
        methods(setter_view, &[("setBackgroundColor:", game_view_set_background as usize), ("setHidden:", game_view_set_hidden as usize)]);
        lock().classes.get_mut(&setter_view).unwrap().from_game = true;
        // A game button and text field with their own setters.
        let game_button = define_class("StoryboardTestGameButton", "UIButton", 16);
        methods(game_button, &[("setShowsTouchWhenHighlighted:", game_button_set_shows_touch as usize)]);
        let game_field = define_class("StoryboardTestGameField", "UITextField", 16);
        methods(game_field, &[("setText:", game_field_set_text as usize)]);
        for class in [game_button, game_field] {
            lock().classes.get_mut(&class).unwrap().from_game = true;
        }
    });
}

// ----- typed calls -----

fn call<R>(object: Id, name: &str) -> R {
    let sel = runtime::sel(name);
    let imp = runtime::imp_for(object, sel).unwrap_or_else(|| panic!("{} has no {name}", string::describe_class_of(object)));
    let function: extern "C" fn(Id, Sel) -> R = unsafe { std::mem::transmute(imp) };
    function(object, sel)
}

fn call_with<A, R>(object: Id, name: &str, argument: A) -> R {
    let sel = runtime::sel(name);
    let imp = runtime::imp_for(object, sel).unwrap_or_else(|| panic!("{} has no {name}", string::describe_class_of(object)));
    let function: extern "C" fn(Id, Sel, A) -> R = unsafe { std::mem::transmute(imp) };
    function(object, sel, argument)
}

fn flag(object: Id, name: &str) -> bool {
    call::<bool>(object, name)
}

fn text_of(object: Id, name: &str) -> String {
    let value = runtime::send(object, name, &[]);
    if value == 0 { String::new() } else { string::get(value) }
}

/// A colour's parts, as UIColor keeps them (fw/uikit.rs: { isa, r, g, b, a }).
fn parts_of(colour: Id) -> [f64; 4] {
    assert_ne!(colour, 0, "no colour");
    unsafe { *((colour + 8) as *const [f64; 4]) }
}

fn new_window() -> Id {
    let (width, height, _) = crate::fw::uikit::screen_points();
    let window = runtime::send(runtime::class_named("UIWindow"), "alloc", &[]);
    call_with::<Rect, Id>(window, "initWithFrame:", Rect::new(0.0, 0.0, width, height))
}

// ----- test apps: a folder with storyboards in it -----

fn test_app(test: &str) -> String {
    let folder = std::env::temp_dir().join(format!("gamebridge-storyboard-{}/{test}/Test.app", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    folder.to_string_lossy().into_owned()
}

fn bundle_at(folder: &str) -> Id {
    let bundle = runtime::send(runtime::class_named("NSBundle"), "bundleWithPath:", &[string::make(folder)]);
    assert_ne!(bundle, 0, "{folder} is a bundle");
    bundle
}

/// A compiled storyboard's folder and its Info.plist (`place`: "" or a
/// localisation folder such as "en.lproj").
fn write_storyboard(app: &str, place: &str, name: &str, entry_point: &str, scenes: &[(&str, &str)]) {
    let folder = if place.is_empty() { format!("{app}/{name}.storyboardc") } else { format!("{app}/{place}/{name}.storyboardc") };
    std::fs::create_dir_all(&folder).unwrap();
    let scenes: String = scenes.iter().map(|(identifier, nib)| format!("<key>{identifier}</key><string>{nib}</string>")).collect();
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>UIStoryboardDesignatedEntryPointIdentifier</key><string>{entry_point}</string>
<key>UIStoryboardVersion</key><integer>1</integer>
<key>UIViewControllerIdentifiersToNibNames</key><dict>{scenes}</dict>
</dict></plist>"#
    );
    std::fs::write(format!("{folder}/Info.plist"), plist).unwrap();
}

/// The folder +[UIImage imageNamed:] reads (the app folder, set for the
/// test process if no game set it).
fn image_folder() -> String {
    if crate::loader::try_app_dir().is_none() {
        let folder = test_app("images");
        crate::loader::set_guest32_app("", &folder);
    }
    crate::loader::try_app_dir().unwrap()
}

/// What a scene's controller nib does: the controller's outlets to the
/// storyboard (an external object) and from the scene (the nib's owner).
fn scene_nib(name: &str, controller: Id, templates: Vec<Id>) {
    fake_nib(name, move |owner, external| {
        let storyboard = collections::dictionary_get(external, string::make("UIStoryboardPlaceholder"));
        runtime::send(controller, "setStoryboard:", &[storyboard]);
        runtime::send(owner, "setSceneViewController:", &[controller]);
        for &template in &templates {
            runtime::send(template, "setViewController:", &[controller]);
        }
        vec![controller]
    });
}

/// What a scene's "-view-" nib does: its view goes to the owner's "view" outlet.
fn view_nib(name: &str, view: Id) {
    fake_nib(name, move |owner, _external| {
        runtime::send(owner, "setView:", &[view]);
        vec![view]
    });
}

fn plain_view(width: f64, height: f64) -> Id {
    decoded("UIView", vec![("UIBounds", Held::Rect(Rect::new(0.0, 0.0, width, height))), ("UICenter", Held::Point(Point { x: width / 2.0, y: height / 2.0 }))])
}

fn test_controller(nib_name: &str, templates: &[Id]) -> Id {
    let controller = runtime::send(runtime::class_named("StoryboardTestController"), "alloc", &[]);
    decoded_into(
        controller,
        vec![
            ("UINibName", text(nib_name)),
            ("UIStoryboardSegueTemplates", array(templates)),
            ("UIExternalObjectsTableForViewLoading", Held::Object(collections::dictionary_from(&[(string::make("UpstreamPlaceholder-1"), controller)]))),
        ],
    )
}

// ----- the tests -----

/// A view's frame comes from its centre (UICenter) and size (UIBounds), its
/// subviews are added in the nib's order, and the rest of what a nib keeps
/// about a view is set; a view with nothing stored keeps UIKit's defaults.
#[test]
fn a_view_reads_its_place_look_and_subviews_from_a_coder() {
    setup();
    let first = plain_view(10.0, 10.0);
    let second = plain_view(20.0, 20.0);
    let red = runtime::send(runtime::class_named("UIColor"), "redColor", &[]);
    let view = decoded(
        "UIView",
        vec![
            ("UIBounds", Held::Rect(Rect::new(5.0, 6.0, 100.0, 50.0))),
            ("UICenter", Held::Point(Point { x: 160.0, y: 100.0 })),
            ("UISubviews", array(&[first, second])),
            ("UIBackgroundColor", Held::Object(red)),
            ("UIAlpha", Held::Double(0.5)),
            ("UIHidden", Held::Bool(true)),
            ("UIOpaque", Held::Bool(false)),
            ("UIClipsToBounds", Held::Bool(true)),
            ("UITag", Held::Integer(7)),
            ("UIAutoresizingMask", Held::Integer(18)),
            ("UIAutoresizeSubviews", Held::Bool(false)),
            ("UIContentMode", Held::Integer(1)),
            ("UIUserInteractionDisabled", Held::Bool(true)),
            ("UIMultipleTouchEnabled", Held::Bool(true)),
        ],
    );
    assert_eq!(call::<Rect>(view, "frame"), Rect::new(110.0, 75.0, 100.0, 50.0), "frame from centre and size");
    assert_eq!(call::<Rect>(view, "bounds"), Rect::new(5.0, 6.0, 100.0, 50.0));
    assert_eq!(view_state(view).subviews, vec![first, second], "subviews in the nib's order");
    assert_eq!(runtime::send(first, "superview", &[]), view);
    assert_eq!(runtime::send(view, "backgroundColor", &[]), red);
    assert_eq!(call::<f64>(view, "alpha"), 0.5);
    assert!(flag(view, "isHidden"));
    assert!(!flag(view, "isOpaque"));
    assert!(flag(view, "clipsToBounds"));
    assert_eq!(call::<isize>(view, "tag"), 7);
    assert_eq!(call::<isize>(view, "autoresizingMask"), 18);
    assert!(!flag(view, "autoresizesSubviews"));
    assert_eq!(call::<isize>(view, "contentMode"), 1);
    assert!(!flag(view, "isUserInteractionEnabled"));
    assert!(flag(view, "isMultipleTouchEnabled"));
    assert_ne!(runtime::send(view, "layer", &[]), 0, "the view has its layer");

    // Nothing stored: what -[UIView initWithCoder:] reads from absent keys
    // (opaque NO, autoresizing subviews off), not -initWithFrame:'s defaults.
    let plain = decoded("UIView", vec![]);
    assert_eq!(call::<Rect>(plain, "frame"), Rect::default());
    assert_eq!(call::<f64>(plain, "alpha"), 1.0);
    assert!(!flag(plain, "isOpaque"));
    assert!(!flag(plain, "autoresizesSubviews"));
    assert!(flag(plain, "isUserInteractionEnabled"));
    assert!(!flag(plain, "isHidden"));
}

/// A nib view takes touches unless the nib says not (UIUserInteractionDisabled),
/// image views and labels too: their -initWithFrame: default doesn't apply.
/// The look is set through the view's own setters (a game's overrides run),
/// after its subviews are in.
#[test]
fn nib_views_take_touches_unless_told_and_their_setters_are_sent() {
    setup();
    let image_view = decoded("UIImageView", vec![("UIBounds", Held::Rect(Rect::new(0.0, 0.0, 40.0, 40.0)))]);
    assert!(flag(image_view, "isUserInteractionEnabled"), "an image view from a nib takes touches");
    assert!(!flag(image_view, "isOpaque"));
    let label = decoded("UILabel", vec![("UIText", text("x"))]);
    assert!(flag(label, "isUserInteractionEnabled"), "a label from a nib takes touches unless the nib says not");

    let red = runtime::send(runtime::class_named("UIColor"), "redColor", &[]);
    let child = plain_view(5.0, 5.0);
    let view = decoded("StoryboardTestSetterView", vec![("UISubviews", array(&[child])), ("UIBackgroundColor", Held::Object(red))]);
    let sent: Vec<(&str, usize)> = GAME_SETTERS.lock().unwrap().iter().filter(|(id, ..)| *id == view).map(|&(_, name, count)| (name, count)).collect();
    assert_eq!(sent, [("setHidden:", 1), ("setBackgroundColor:", 1)], "the game's setters, after the subviews");
    assert_eq!(runtime::send(view, "backgroundColor", &[]), red);
}

/// A scroll view's paging, bouncing, indicators, size, zoom and delegate
/// from a nib; what isn't stored keeps UIScrollView's defaults.
#[test]
fn a_scroll_view_reads_its_scrolling_from_a_coder() {
    setup();
    let delegate = runtime::send(runtime::send(runtime::class_named("NSObject"), "alloc", &[]), "init", &[]);
    let scroll = decoded(
        "UIScrollView",
        vec![
            ("UIBounds", Held::Rect(Rect::new(0.0, 0.0, 320.0, 100.0))),
            ("UIPagingEnabled", Held::Bool(true)),
            ("UIAlwaysBounceHorizontal", Held::Bool(true)),
            ("UIShowsHorizontalScrollIndicator", Held::Bool(false)),
            ("UIBounceEnabled", Held::Bool(false)),
            ("UIContentSize", Held::Size(Size { width: 960.0, height: 100.0 })),
            ("UIMaximumZoomScale", Held::Double(2.0)),
            ("UIDelegate", Held::Object(delegate)),
        ],
    );
    assert!(flag(scroll, "isPagingEnabled"));
    assert!(flag(scroll, "alwaysBounceHorizontal"));
    assert!(!flag(scroll, "showsHorizontalScrollIndicator"));
    assert!(!flag(scroll, "bounces"));
    assert_eq!(call::<Size>(scroll, "contentSize"), Size { width: 960.0, height: 100.0 });
    assert_eq!(call::<f64>(scroll, "maximumZoomScale"), 2.0);
    assert_eq!(runtime::send(scroll, "delegate", &[]), delegate);
    let plain = decoded("UIScrollView", vec![("UIScrollDisabled", Held::Bool(true))]);
    assert!(!flag(plain, "isScrollEnabled"));
    assert!(!flag(plain, "isPagingEnabled") && flag(plain, "bounces") && flag(plain, "showsVerticalScrollIndicator"));
}

/// A text field's text, look and keyboard from a nib, and a text view's.
#[test]
fn text_fields_and_text_views_read_their_text_from_a_coder() {
    setup();
    let white = decoded("UIColor", vec![("UIWhite", Held::Double(1.0)), ("UIAlpha", Held::Double(1.0))]);
    let font = decoded("UIFont", vec![("UIFontName", text("Philosopher")), ("UIFontPointSize", Held::Double(20.0))]);
    let field = decoded(
        "UITextField",
        vec![
            ("UIBounds", Held::Rect(Rect::new(0.0, 0.0, 200.0, 30.0))),
            ("UIText", text("Merlin")),
            ("UITextColor", Held::Object(white)),
            ("UIFont", Held::Object(font)),
            ("UITextAlignment", Held::Integer(1)),
            ("UIPlaceholder", text("Name")),
            ("UIBorderStyle", Held::Integer(3)),
            ("UIClearButtonMode", Held::Integer(3)),
            ("UIClearsOnBeginEditing", Held::Bool(true)),
            ("UIMinimumFontSize", Held::Double(14.0)),
            ("UIAdjustsFontSizeToFit", Held::Bool(true)),
            ("UIAutocapitalizationType", Held::Integer(1)),
            ("UIAutocorrectionType", Held::Integer(1)),
            ("UIKeyboardType", Held::Integer(7)),
            ("UIReturnKeyType", Held::Integer(9)),
            ("UIEnablesReturnKeyAutomatically", Held::Bool(true)),
            ("UISecureTextEntry", Held::Bool(true)),
        ],
    );
    assert_eq!(text_of(field, "text"), "Merlin");
    assert_eq!(runtime::send(field, "textColor", &[]), white);
    assert_eq!(runtime::send(field, "font", &[]), font);
    assert_eq!(text_of(field, "placeholder"), "Name");
    for (getter, value) in [
        ("textAlignment", 1),
        ("borderStyle", 3),
        ("clearButtonMode", 3),
        ("autocapitalizationType", 1),
        ("autocorrectionType", 1),
        ("keyboardType", 7),
        ("returnKeyType", 9),
    ] {
        assert_eq!(call::<isize>(field, getter), value, "{getter}");
    }
    assert!(flag(field, "clearsOnBeginEditing") && flag(field, "adjustsFontSizeToFitWidth"));
    assert!(flag(field, "enablesReturnKeyAutomatically") && flag(field, "isSecureTextEntry"));
    assert_eq!(call::<f64>(field, "minimumFontSize"), 14.0);

    let view = decoded(
        "UITextView",
        vec![
            ("UIBounds", Held::Rect(Rect::new(0.0, 0.0, 200.0, 80.0))),
            ("UIText", text("An offer")),
            ("UIFont", Held::Object(font)),
            ("UITextColor", Held::Object(white)),
            ("UITextAlignment", Held::Integer(2)),
            ("UIEditable", Held::Bool(false)),
        ],
    );
    assert_eq!(text_of(view, "text"), "An offer");
    assert_eq!(runtime::send(view, "font", &[]), font);
    assert_eq!(runtime::send(view, "textColor", &[]), white);
    assert_eq!(call::<isize>(view, "textAlignment"), 2);
    assert!(!flag(view, "isEditable"));
}

/// A button's type, edge insets and title line break from a nib; a rounded
/// rect button (UIRoundedRectButton) is a button.
#[test]
fn a_button_reads_its_type_insets_and_line_break_from_a_coder() {
    setup();
    let insets = |top, left, bottom, right| Insets { top, left, bottom, right };
    let button = decoded(
        "UIButton",
        vec![
            ("UIBounds", Held::Rect(Rect::new(0.0, 0.0, 100.0, 40.0))),
            ("UIButtonType", Held::Integer(1)),
            ("UIContentEdgeInsets", Held::Insets(insets(0.0, 10.0, 0.0, 0.0))),
            ("UITitleEdgeInsets", Held::Insets(insets(2.0, 0.0, 0.0, 0.0))),
            ("UIImageEdgeInsets", Held::Insets(insets(0.0, 0.0, 0.0, 4.0))),
            ("UILineBreakMode", Held::Integer(0)),
        ],
    );
    assert_eq!(call::<isize>(button, "buttonType"), 1);
    assert_eq!(call::<Insets>(button, "contentEdgeInsets"), insets(0.0, 10.0, 0.0, 0.0));
    assert_eq!(call::<Insets>(button, "titleEdgeInsets"), insets(2.0, 0.0, 0.0, 0.0));
    assert_eq!(call::<Insets>(button, "imageEdgeInsets"), insets(0.0, 0.0, 0.0, 4.0));
    assert_eq!(call::<isize>(runtime::send(button, "titleLabel", &[]), "lineBreakMode"), 0);
    assert!(runtime::existing_class("UIRoundedRectButton", false).is_some(), "UIRoundedRectButton exists");
    let rounded = decoded("UIRoundedRectButton", vec![("UIButtonType", Held::Integer(1))]);
    assert!(is_kind_of(rounded, "UIButton"));
    assert_eq!(call::<isize>(rounded, "buttonType"), 1);
}

/// Older nibs keep the button's "reverses title shadow" setting under
/// UIReversesTitleShadowOnHighlight; UIKit reads either name.
#[test]
fn a_button_reads_either_name_of_its_shadow_reversing() {
    setup();
    let newer = decoded("UIButton", vec![("UIReversesTitleShadowWhenHighlighted", Held::Bool(true))]);
    let older = decoded("UIButton", vec![("UIReversesTitleShadowOnHighlight", Held::Bool(true))]);
    let plain = decoded("UIButton", vec![]);
    assert!(flag(newer, "reversesTitleShadowWhenHighlighted"));
    assert!(flag(older, "reversesTitleShadowWhenHighlighted"), "the older name is read too");
    assert!(!flag(plain, "reversesTitleShadowWhenHighlighted"));
}

/// The decoders send the setters UIKit's own decoders send by message, so
/// a game's subclass hears them (a button's -setShowsTouchWhenHighlighted:,
/// a text field's -setText:).
#[test]
fn decoders_send_the_setters_a_game_can_override() {
    setup();
    let button = decoded("StoryboardTestGameButton", vec![("UIShowsTouchWhenHighlighted", Held::Bool(true))]);
    let field = decoded("StoryboardTestGameField", vec![("UIText", text("Merlin"))]);
    let heard = |object: Id| -> Vec<&str> { GAME_SETTERS.lock().unwrap().iter().filter(|(id, ..)| *id == object).map(|&(_, name, _)| name).collect() };
    assert_eq!(heard(button), ["setShowsTouchWhenHighlighted:"]);
    assert_eq!(heard(field), ["setText:"]);
}

/// A spinner's style and state, a slider's values and a web view's settings
/// from a nib.
#[test]
fn spinners_sliders_and_web_views_read_their_settings_from_a_coder() {
    setup();
    let red = runtime::send(runtime::class_named("UIColor"), "redColor", &[]);
    let spinner = decoded(
        "UIActivityIndicatorView",
        vec![
            ("UIActivityIndicatorViewStyle", Held::Integer(2)),
            ("UIHidesWhenStopped", Held::Bool(false)),
            ("UIAnimating", Held::Bool(true)),
            ("UITintColor", Held::Object(red)),
        ],
    );
    assert_eq!(call::<isize>(spinner, "activityIndicatorViewStyle"), 2);
    assert!(!flag(spinner, "hidesWhenStopped"));
    assert!(flag(spinner, "isAnimating"));
    assert_eq!(runtime::send(spinner, "color", &[]), red);

    let slider = decoded("UISlider", vec![("UIValue", Held::Double(5.0)), ("UIMinValue", Held::Double(1.0)), ("UIMaxValue", Held::Double(20.0))]);
    assert_eq!((call::<f32>(slider, "value"), call::<f32>(slider, "minimumValue"), call::<f32>(slider, "maximumValue")), (5.0, 1.0, 20.0));
    assert!(flag(slider, "isContinuous"), "continuous unless the nib says not");
    assert!(is_kind_of(slider, "UIControl"));

    let delegate = runtime::send(runtime::send(runtime::class_named("NSObject"), "alloc", &[]), "init", &[]);
    let web = decoded("UIWebView", vec![("UIScalesPageToFit", Held::Bool(true)), ("UIDelegate", Held::Object(delegate))]);
    assert!(flag(web, "scalesPageToFit"));
    assert_eq!(runtime::send(web, "delegate", &[]), delegate);
}

/// A nib window starts hidden (until it is made visible) and gets its root
/// view controller from the nib.
#[test]
fn a_nib_window_gets_its_root_view_controller() {
    setup();
    let controller = runtime::send(runtime::send(runtime::class_named("UIViewController"), "alloc", &[]), "init", &[]);
    let window = decoded("UIWindow", vec![("UIBounds", Held::Rect(Rect::new(0.0, 0.0, 320.0, 480.0))), ("UIRootViewController", Held::Object(controller))]);
    assert_eq!(runtime::send(window, "rootViewController", &[]), controller);
    assert!(flag(window, "isHidden"), "a nib window waits to be made visible");
}

/// A nib's window marked "Full Screen at Launch" (UIResizesToFullScreen)
/// takes the screen's size (nibs made for a 3.5-inch iPhone are 320x480);
/// without the mark it keeps the nib's size.
#[test]
fn a_full_screen_nib_window_takes_the_screens_size() {
    setup();
    let nib_size = vec![("UIBounds", Held::Rect(Rect::new(0.0, 0.0, 320.0, 480.0))), ("UICenter", Held::Point(Point { x: 160.0, y: 240.0 }))];
    let mut values = nib_size.clone();
    values.push(("UIResizesToFullScreen", Held::Bool(true)));
    let full = decoded("UIWindow", values);
    let (width, height, _) = crate::fw::uikit::screen_points();
    assert_eq!(call::<Rect>(full, "frame"), Rect::new(0.0, 0.0, width, height));
    let kept = decoded("UIWindow", nib_size);
    assert_eq!(call::<Rect>(kept, "frame"), Rect::new(0.0, 0.0, 320.0, 480.0));
}

/// A progress bar's progress, style and colours from a nib.
#[test]
fn a_progress_bar_reads_its_progress_from_a_coder() {
    setup();
    let red = runtime::send(runtime::class_named("UIColor"), "redColor", &[]);
    let bar = decoded(
        "UIProgressView",
        vec![
            ("UIBounds", Held::Rect(Rect::new(0.0, 0.0, 127.0, 9.0))),
            ("UIProgress", Held::Double(0.5)),
            ("UIProgressViewStyle", Held::Integer(1)),
            ("UIProgressTintColor", Held::Object(red)),
        ],
    );
    assert_eq!(call::<Rect>(bar, "frame").size, Size { width: 127.0, height: 9.0 });
    assert_eq!((call::<f32>(bar, "progress"), call::<isize>(bar, "progressViewStyle")), (0.5, 1));
    assert_eq!(runtime::send(bar, "progressTintColor", &[]), red);
}

/// -[UIView initWithCoder:] doesn't run -initWithFrame: overrides of the
/// game's classes (iOS never calls it while decoding), but our own classes'
/// setup still happens (a text view isn't drawn as a plain view).
#[test]
fn decoding_skips_the_games_init_with_frame_but_keeps_ours() {
    setup();
    let view = decoded("StoryboardTestGameView", vec![("UIBounds", Held::Rect(Rect::new(0.0, 0.0, 40.0, 20.0)))]);
    assert!(!GAME_INIT_WITH_FRAME.lock().unwrap().contains(&view), "the game's initWithFrame: was called");
    assert_eq!(call::<Rect>(view, "frame").size, Size { width: 40.0, height: 20.0 });
    let text = decoded("UITextView", vec![("UIBounds", Held::Rect(Rect::new(0.0, 0.0, 40.0, 20.0)))]);
    assert_eq!(property_get(text, "notDrawn"), Some(1), "UITextView's own setup ran");
}

/// A label's text, font, colours and the rest of what a nib stores.
#[test]
fn a_label_reads_its_text_font_and_colours_from_a_coder() {
    setup();
    let font = decoded(
        "UIFont",
        vec![
            ("UIFontName", text("Philosopher-Bold")),
            ("UIFontPointSize", Held::Double(24.0)),
            ("UIFontTraits", Held::Integer(0)),
            ("UISystemFont", Held::Bool(false)),
        ],
    );
    assert_eq!(text_of(font, "fontName"), "Philosopher-Bold");
    assert_eq!(call::<f64>(font, "pointSize"), 24.0);
    let white = decoded("UIColor", vec![("UIWhite", Held::Double(1.0)), ("UIAlpha", Held::Double(1.0))]);
    let shadow = decoded("UIColor", vec![("UIWhite", Held::Double(0.0)), ("UIAlpha", Held::Double(1.0))]);
    let label = decoded(
        "UILabel",
        vec![
            ("UIBounds", Held::Rect(Rect::new(0.0, 0.0, 200.0, 30.0))),
            ("UICenter", Held::Point(Point { x: 100.0, y: 15.0 })),
            ("UIText", text("Select your character:")),
            ("UIFont", Held::Object(font)),
            ("UITextColor", Held::Object(white)),
            ("UIShadowColor", Held::Object(shadow)),
            ("UIShadowOffset", Held::Size(Size { width: 1.0, height: 1.0 })),
            ("UITextAlignment", Held::Integer(1)),
            ("UINumberOfLines", Held::Integer(0)),
            ("UIMinimumFontSize", Held::Double(9.0)),
            ("UIAdjustsFontSizeToFit", Held::Bool(true)),
            ("UIUserInteractionDisabled", Held::Bool(true)),
        ],
    );
    assert_eq!(call::<Rect>(label, "frame"), Rect::new(0.0, 0.0, 200.0, 30.0));
    assert_eq!(text_of(label, "text"), "Select your character:");
    assert_eq!(runtime::send(label, "font", &[]), font);
    assert_eq!(runtime::send(label, "textColor", &[]), white);
    assert_eq!(runtime::send(label, "shadowColor", &[]), shadow);
    assert_eq!(call::<Size>(label, "shadowOffset"), Size { width: 1.0, height: 1.0 });
    assert_eq!(call::<isize>(label, "textAlignment"), 1);
    assert_eq!(call::<isize>(label, "numberOfLines"), 0);
    assert_eq!(call::<f64>(label, "minimumFontSize"), 9.0);
    assert!(flag(label, "adjustsFontSizeToFitWidth"));
    // Nothing stored: one line, as UILabel starts.
    let plain = decoded("UILabel", vec![("UIText", text("x"))]);
    assert_eq!(call::<isize>(plain, "numberOfLines"), 1);

    // A nib's "system font" is ours, bold when its traits say so (2).
    let system = decoded(
        "UIFont",
        vec![
            ("UIFontName", text("Helvetica-Bold")),
            ("UIFontPointSize", Held::Double(15.0)),
            ("UIFontTraits", Held::Integer(2)),
            ("UISystemFont", Held::Bool(true)),
        ],
    );
    let bold = call_with::<f64, Id>(runtime::class_named("UIFont"), "boldSystemFontOfSize:", 15.0);
    assert_eq!(text_of(system, "fontName"), text_of(bold, "fontName"));
    assert_eq!(call::<f64>(system, "pointSize"), 15.0);
}

/// +[UIImage imageNamed:] answers a name it has seen from its cache (an
/// app's pictures don't change), so a game asking every frame doesn't reach
/// the phone's slow shared storage each time.
#[test]
fn image_named_answers_a_name_it_has_seen_from_its_cache() {
    setup();
    let _image_tests = IMAGE_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    crate::fw::uikit::clear_app_files();
    let named = |name: &str| runtime::send(runtime::class_named("UIImage"), "imageNamed:", &[string::make(name)]);
    let pid = std::process::id();
    let (found, missing) = (format!("cache_test_found_{pid}.png"), format!("cache_test_missing_{pid}.png"));
    for _ in 0..3 {
        let folder = image_folder();
        let path = |name: &str| format!("{folder}/{name}");
        std::fs::write(path(&found), crate::fw::png::encode(2, 2, &[0x80u8; 16], false)).unwrap();
        let image = named(&found);
        assert_eq!(named(&missing), 0);
        std::fs::remove_file(path(&found)).unwrap();
        std::fs::write(path(&missing), crate::fw::png::encode(2, 2, &[0x80u8; 16], false)).unwrap();
        let (again, still_missing) = (named(&found), named(&missing));
        std::fs::remove_file(path(&missing)).unwrap();
        if image_folder() != folder {
            continue;
        }
        assert_ne!(image, 0);
        assert_eq!(again, image, "a found picture comes from the cache");
        assert_eq!(still_missing, 0, "a missing one is remembered too");
        return;
    }
    panic!("the app folder kept changing");
}

/// A picture made from a file keeps its bytes, so its CGImage still decodes
/// (at the right size) after the file is gone: the file is read once, not
/// again when the CGImage is first asked for.
#[test]
fn a_picture_keeps_its_bytes_until_it_is_decoded() {
    setup();
    let _image_tests = IMAGE_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    crate::fw::uikit::clear_app_files();
    let named = |name: &str| runtime::send(runtime::class_named("UIImage"), "imageNamed:", &[string::make(name)]);
    let name = format!("keeps_bytes_{}.png", std::process::id());
    let (width, height) = (6usize, 4usize);
    for _ in 0..3 {
        let folder = image_folder();
        let path = format!("{folder}/{name}");
        std::fs::write(&path, crate::fw::png::encode(width as u32, height as u32, &vec![0x80u8; width * height * 4], false)).unwrap();
        let image = named(&name);
        std::fs::remove_file(&path).unwrap();
        if image_folder() != folder {
            continue;
        }
        assert_ne!(image, 0, "the picture is found");
        assert_eq!(call::<Size>(image, "size"), Size { width: width as f64, height: height as f64 });
        // The file is gone: the CGImage still decodes from the kept bytes.
        let cg = crate::fw::graphics::image_cg(image);
        assert_ne!(cg, 0, "the CGImage decodes without the file");
        assert_eq!(crate::fw::graphics::straight_pixels(cg).map(|pixels| pixels.len()), Some(width * height * 4), "the decoded size");
        return;
    }
    panic!("the app folder kept changing");
}

/// A picture is kept in a nib as its name (UIImageNibPlaceholder), which
/// becomes the app's picture of that name; image views show it.
#[test]
fn an_image_view_reads_its_picture_from_a_coder() {
    setup();
    let _image_tests = IMAGE_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    crate::fw::uikit::clear_app_files();
    // The app folder is the process's (another test may load a game and
    // change it), so the picture is there only while it is read.
    let pixels = vec![0x80u8; 8 * 4 * 4];
    let mut image = 0;
    for _ in 0..3 {
        let folder = image_folder();
        let path = format!("{folder}/storyboard_test_arrow_{}.png", std::process::id());
        std::fs::write(&path, crate::fw::png::encode(8, 4, &pixels, false)).unwrap();
        image = decoded(
            "UIImageNibPlaceholder",
            vec![
                ("UIResourceName", text(&format!("storyboard_test_arrow_{}.png", std::process::id()))),
                ("UIImageWidth", Held::Double(1.0)),
                ("UIImageHeight", Held::Double(1.0)),
            ],
        );
        std::fs::remove_file(&path).unwrap();
        if image != 0 || image_folder() == folder {
            break;
        }
    }
    assert_ne!(image, 0, "the placeholder becomes the picture");
    assert_eq!(string::describe_class_of(image), "UIImage");
    assert_eq!(call::<Size>(image, "size"), Size { width: 8.0, height: 4.0 });
    let missing = decoded("UIImageNibPlaceholder", vec![("UIResourceName", text("storyboard_test_nothing.png"))]);
    assert_eq!(missing, 0, "a picture the app doesn't have is nil");

    let view = decoded(
        "UIImageView",
        vec![
            ("UIBounds", Held::Rect(Rect::new(0.0, 0.0, 16.0, 8.0))),
            ("UIImage", Held::Object(image)),
            ("UIHighlightedImage", Held::Object(image)),
            ("UIUserInteractionDisabled", Held::Bool(true)),
        ],
    );
    assert_eq!(runtime::send(view, "image", &[]), image);
    assert_eq!(runtime::send(view, "highlightedImage", &[]), image);
    assert!(!flag(view, "isUserInteractionEnabled"));
    let touchable = decoded("UIImageView", vec![("UIUserInteractionDisabled", Held::Bool(false))]);
    assert!(flag(touchable, "isUserInteractionEnabled"), "the nib can turn touches on");
}

/// A button's titles, colours and pictures per state (UIButtonStatefulContent:
/// state number -> UIButtonContent), and its title label's font and shadow.
#[test]
fn a_button_reads_its_content_per_state_from_a_coder() {
    setup();
    let white = decoded("UIColor", vec![("UIWhite", Held::Double(1.0)), ("UIAlpha", Held::Double(1.0))]);
    let black = decoded("UIColor", vec![("UIWhite", Held::Double(0.0)), ("UIAlpha", Held::Double(1.0))]);
    let picture = runtime::send(runtime::class_named("UIImage"), "alloc", &[]);
    let normal = decoded("UIButtonContent", vec![("UITitle", text("Play")), ("UITitleColor", Held::Object(white)), ("UIShadowColor", Held::Object(black))]);
    let highlighted = decoded("UIButtonContent", vec![("UIImage", Held::Object(picture)), ("UIBackgroundImage", Held::Object(picture)), ("UITitleColor", Held::Object(black))]);
    let content = collections::dictionary_from(&[(collections::number_int(0), normal), (collections::number_int(1), highlighted)]);
    let font = decoded("UIFont", vec![("UIFontName", text("Philosopher")), ("UIFontPointSize", Held::Double(15.0))]);
    let button = decoded(
        "UIButton",
        vec![
            ("UIBounds", Held::Rect(Rect::new(0.0, 0.0, 50.0, 50.0))),
            ("UICenter", Held::Point(Point { x: 59.0, y: 156.0 })),
            ("UIButtonStatefulContent", Held::Object(content)),
            ("UIAdjustsImageWhenHighlighted", Held::Bool(false)),
            ("UIFont", Held::Object(font)),
            ("UITitleShadowOffset", Held::Size(Size { width: 1.0, height: 1.0 })),
        ],
    );
    assert_eq!(call::<Rect>(button, "frame"), Rect::new(34.0, 131.0, 50.0, 50.0));
    assert_eq!(string::get(runtime::send(button, "titleForState:", &[0])), "Play");
    assert_eq!(runtime::send(button, "titleColorForState:", &[0]), white);
    assert_eq!(runtime::send(button, "titleColorForState:", &[1]), black);
    assert_eq!(runtime::send(button, "titleShadowColorForState:", &[0]), black);
    assert_eq!(runtime::send(button, "imageForState:", &[1]), picture);
    assert_eq!(runtime::send(button, "backgroundImageForState:", &[1]), picture);
    assert!(!flag(button, "adjustsImageWhenHighlighted"));
    let label = runtime::send(button, "titleLabel", &[]);
    assert_eq!(runtime::send(label, "font", &[]), font);
    assert_eq!(call::<Size>(label, "shadowOffset"), Size { width: 1.0, height: 1.0 });
    assert!(flag(button, "isUserInteractionEnabled"), "buttons take touches");
}

static CODER_LAYOUTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

extern "C" fn coder_button_layout(this: Id, s: Sel) {
    CODER_LAYOUTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let class = runtime::class_named("UIButton"); // before taking the lock: it locks too
    let imp = lock().lookup(class, s).unwrap();
    unsafe { std::mem::transmute::<usize, extern "C" fn(Id, Sel)>(imp)(this, s) }
}

/// A button saved selected, with content for that state, is filled in
/// without being laid out (Apple's -initWithCoder: takes its content table
/// as it is): the game's -layoutSubviews doesn't run inside its init; it is
/// marked, and laid out later shows the selected title.
#[test]
fn a_decoded_selected_button_is_not_laid_out_during_its_init() {
    setup();
    let class = define_class("StoryboardTestLayoutButton", "UIButton", 16);
    methods(class, &[("layoutSubviews", coder_button_layout as usize)]);
    let selected = decoded("UIButtonContent", vec![("UITitle", text("On"))]);
    let content = collections::dictionary_from(&[(collections::number_int(4), selected)]);
    let before = CODER_LAYOUTS.load(std::sync::atomic::Ordering::SeqCst);
    let button = decoded(
        "StoryboardTestLayoutButton",
        vec![("UIBounds", Held::Rect(Rect::new(0.0, 0.0, 80.0, 30.0))), ("UISelected", Held::Bool(true)), ("UIButtonStatefulContent", Held::Object(content))],
    );
    assert_eq!(CODER_LAYOUTS.load(std::sync::atomic::Ordering::SeqCst), before, "no layout inside initWithCoder:");
    runtime::send(button, "layoutIfNeeded", &[]);
    assert_eq!(CODER_LAYOUTS.load(std::sync::atomic::Ordering::SeqCst), before + 1, "marked for layout");
    assert_eq!(string::get(runtime::send(runtime::send(button, "titleLabel", &[]), "text", &[])), "On");
}

/// Colours: separate parts (UIRed...), the "r g b" text older readers use,
/// white, and a system colour by name.
#[test]
fn colours_read_their_parts_from_a_coder() {
    setup();
    let parts = decoded(
        "UIColor",
        vec![
            ("UIColorComponentCount", Held::Integer(4)),
            ("UIRed", Held::Double(0.5)),
            ("UIGreen", Held::Double(0.25)),
            ("UIBlue", Held::Double(0.75)),
            ("UIAlpha", Held::Double(0.5)),
            ("NSRGB", Held::Object(crate::fw::data::make(b"0.5 0.25 0.75"))),
        ],
    );
    assert_eq!(parts_of(parts), [0.5, 0.25, 0.75, 0.5]);
    let rgb_only = decoded("UIColor", vec![("NSRGB", Held::Object(crate::fw::data::make(b"0.1 0.2 0.3\0")))]);
    assert_eq!(parts_of(rgb_only), [0.1, 0.2, 0.3, 1.0]);
    let white = decoded("UIColor", vec![("UIWhite", Held::Double(0.75)), ("UIAlpha", Held::Double(1.0))]);
    assert_eq!(parts_of(white), [0.75, 0.75, 0.75, 1.0]);
    let white_text = decoded("UIColor", vec![("NSWhite", Held::Object(crate::fw::data::make(b"0.5")))]);
    assert_eq!(parts_of(white_text), [0.5, 0.5, 0.5, 1.0]);
    let clear = decoded(
        "UIColor",
        vec![("UISystemColorName", text("clearColor")), ("UIWhite", Held::Double(0.0)), ("UIAlpha", Held::Double(0.0))],
    );
    assert_eq!(parts_of(clear), [0.0, 0.0, 0.0, 0.0]);
    let named_only = decoded("UIColor", vec![("UISystemColorName", text("whiteColor"))]);
    assert_eq!(parts_of(named_only), [1.0, 1.0, 1.0, 1.0]);
}

/// A storyboard scene's controller: its view's nib, its segues, the objects
/// its view's nib refers to; and every object answers -awakeFromNib.
#[test]
fn a_view_controller_reads_its_scene_from_a_coder() {
    setup();
    let template = decoded(
        "UIStoryboardModalSegueTemplate",
        vec![("UIIdentifier", text("ShowMapView")), ("UIDestinationViewControllerIdentifier", text("UIViewController-map"))],
    );
    assert_eq!(text_of(template, "identifier"), "ShowMapView");
    assert_eq!(text_of(template, "destinationViewControllerIdentifier"), "UIViewController-map");
    let controller = test_controller("2-view-3", &[template]);
    assert_eq!(text_of(controller, "nibName"), "2-view-3");
    assert!(!flag(controller, "isViewLoaded"), "the view waits until it is asked for");
    assert_eq!(runtime::send(controller, "storyboard", &[]), 0, "not from a storyboard yet");
    for object in [controller, template, runtime::send(runtime::class_named("NSObject"), "new", &[])] {
        assert!(runtime::responds(object, "awakeFromNib"));
        runtime::send(object, "awakeFromNib", &[]);
    }
}

/// A storyboard finds its folder (in a localisation folder or not), makes
/// its first scene's controller from the scene's nib (the storyboard is an
/// external object, the scene the owner), and the controller's view comes
/// from its "-view-" nib when first asked for, owned by the controller.
#[test]
fn a_storyboard_makes_its_scenes_and_their_views() {
    setup();
    let app = test_app("scenes");
    write_storyboard(&app, "en.lproj", "SceneBoard", "UIViewController-2", &[("UIViewController-2", "UIViewController-2"), ("Lost", "UIViewController-lost")]);
    write_storyboard(&app, "", "RootBoard", "First", &[("First", "UIViewController-first")]);
    let bundle = bundle_at(&app);
    let controller = test_controller("2-view-3", &[]);
    scene_nib("en.lproj/SceneBoard.storyboardc/UIViewController-2", controller, vec![]);
    let view = plain_view(568.0, 320.0);
    view_nib("en.lproj/SceneBoard.storyboardc/2-view-3", view);

    let storyboard_class = runtime::class_named("UIStoryboard");
    assert_ne!(storyboard_class, 0, "UIStoryboard exists");
    let storyboard = runtime::send(storyboard_class, "storyboardWithName:bundle:", &[string::make("SceneBoard"), bundle]);
    assert_ne!(storyboard, 0, "found in en.lproj");
    assert_ne!(runtime::send(storyboard_class, "storyboardWithName:bundle:", &[string::make("RootBoard"), bundle]), 0, "found at the top");
    assert_eq!(runtime::send(storyboard_class, "storyboardWithName:bundle:", &[string::make("Missing"), bundle]), 0);

    let first = runtime::send(storyboard, "instantiateInitialViewController", &[]);
    assert_eq!(first, controller, "the entry point's controller");
    assert_eq!(runtime::send(first, "storyboard", &[]), storyboard);
    let loads = nib_loads("en.lproj/SceneBoard.storyboardc/UIViewController-2");
    assert_eq!(loads.len(), 1);
    assert_eq!(loads[0].bundle, bundle);
    assert!(loads[0].owner != 0 && loads[0].owner != controller, "a scene object owns the controller's nib");
    assert_eq!(collections::dictionary_get(loads[0].external, string::make("UIStoryboardPlaceholder")), storyboard);
    assert!(!flag(first, "isViewLoaded"));

    assert_eq!(runtime::send(first, "view", &[]), view);
    let loads = nib_loads("en.lproj/SceneBoard.storyboardc/2-view-3");
    assert_eq!(loads.len(), 1);
    assert_eq!(loads[0].owner, controller, "the controller owns its view's nib");
    assert_eq!(collections::dictionary_get(loads[0].external, string::make("UpstreamPlaceholder-1")), controller, "the controller's table of objects");

    // A scene the storyboard doesn't list, or whose nib can't be read, is nil.
    assert_eq!(runtime::send(storyboard, "instantiateViewControllerWithIdentifier:", &[string::make("Nope")]), 0);
    assert_eq!(runtime::send(storyboard, "instantiateViewControllerWithIdentifier:", &[string::make("Lost")]), 0);
}

/// performSegueWithIdentifier:sender: makes the scene its segue leads to,
/// lets the source prepare the segue, and presents the destination over
/// the source: its view takes the source's place in the window, with the
/// appearance messages iOS sends. Dismissing brings the source back.
#[test]
fn a_segue_prepares_and_presents_its_destination_in_the_window() {
    setup();
    let app = test_app("segues");
    write_storyboard(&app, "en.lproj", "SegueBoard", "Start", &[("Start", "UIViewController-start"), ("Map", "UIViewController-map")]);
    let bundle = bundle_at(&app);
    let template = decoded(
        "UIStoryboardModalSegueTemplate",
        vec![("UIIdentifier", text("ShowMapView")), ("UIDestinationViewControllerIdentifier", text("Map"))],
    );
    let source = test_controller("start-view", &[template]);
    let destination = test_controller("map-view", &[]);
    scene_nib("en.lproj/SegueBoard.storyboardc/UIViewController-start", source, vec![template]);
    scene_nib("en.lproj/SegueBoard.storyboardc/UIViewController-map", destination, vec![]);
    let source_view = plain_view(568.0, 320.0);
    let destination_view = plain_view(100.0, 100.0);
    view_nib("en.lproj/SegueBoard.storyboardc/start-view", source_view);
    view_nib("en.lproj/SegueBoard.storyboardc/map-view", destination_view);
    let storyboard = runtime::send(runtime::class_named("UIStoryboard"), "storyboardWithName:bundle:", &[string::make("SegueBoard"), bundle]);
    assert_eq!(runtime::send(storyboard, "instantiateInitialViewController", &[]), source);

    let window = new_window();
    runtime::send(window, "setRootViewController:", &[source]);
    runtime::send(window, "makeKeyAndVisible", &[]);
    assert_eq!(view_state(source_view).superview, window);
    let appeared = events_of(&[source]);
    assert_eq!(appeared, [(source, "viewWillAppear: NO".to_string()), (source, "viewDidAppear: NO".to_string())], "the root controller appears with its window");

    runtime::send(source, "performSegueWithIdentifier:sender:", &[string::make("ShowMapView"), source]);
    let segue = property_get(source, "testPreparedSegue").unwrap_or(0) as Id;
    assert_ne!(segue, 0, "the source prepared the segue");
    assert_eq!(string::describe_class_of(segue), "UIStoryboardModalSegue");
    assert_eq!(text_of(segue, "identifier"), "ShowMapView");
    assert_eq!(runtime::send(segue, "sourceViewController", &[]), source);
    assert_eq!(runtime::send(segue, "destinationViewController", &[]), destination);
    assert_eq!(property_get(source, "testPreparedSender"), Some(source as u64));
    assert_eq!(runtime::send(source, "presentedViewController", &[]), destination);
    assert_eq!(runtime::send(destination, "presentingViewController", &[]), source);
    assert_eq!(view_state(destination_view).superview, window, "the presented view is in the window");
    assert_eq!(call::<Rect>(destination_view, "frame"), call::<Rect>(window, "bounds"), "full screen");
    // Animated (a modal segue animates): the new view slides up over the
    // source's for 0.4 s; then the source's view leaves the window, as on iOS.
    let mut told = events_of(&[source, destination]);
    let end = crate::fw::runloop::media_time() + 0.5;
    while view_state(source_view).superview != 0 && crate::fw::runloop::media_time() < end {
        crate::fw::runloop::run_main_loop_briefly();
    }
    told.extend(events_of(&[source, destination]));
    assert_eq!(view_state(source_view).superview, 0, "the source's view leaves the window once the slide is done");
    assert_eq!(
        told,
        [
            (source, "prepareForSegue:sender:".to_string()),
            (destination, "viewWillAppear: YES".to_string()),
            (source, "viewWillDisappear: YES".to_string()),
            (source, "viewDidDisappear: YES".to_string()),
            (destination, "viewDidAppear: YES".to_string()),
        ],
        "performSegueWithIdentifier: doesn't ask shouldPerformSegueWithIdentifier:sender:"
    );

    runtime::send(destination, "dismissViewControllerAnimated:completion:", &[0, 0]);
    assert_eq!(runtime::send(source, "presentedViewController", &[]), 0);
    assert_eq!(view_state(destination_view).superview, 0);
    assert_eq!(view_state(source_view).superview, window, "the source's view comes back");
    assert_eq!(
        events_of(&[source, destination]),
        [
            (destination, "viewWillDisappear: NO".to_string()),
            (source, "viewWillAppear: NO".to_string()),
            (destination, "viewDidDisappear: NO".to_string()),
            (source, "viewDidAppear: NO".to_string()),
        ]
    );

    // A button wired to the template (-perform:) asks the source first.
    property_set(source, "testRefuseSegues", 1);
    runtime::send(template, "perform:", &[source_view]);
    assert_eq!(runtime::send(source, "presentedViewController", &[]), 0, "refused");
    assert_eq!(events_of(&[source, destination]), [(source, "shouldPerformSegueWithIdentifier:sender:".to_string())]);
    property_set(source, "testRefuseSegues", 0);
    runtime::send(template, "perform:", &[source_view]);
    assert_eq!(runtime::send(source, "presentedViewController", &[]), destination);
    assert_eq!(property_get(source, "testPreparedSender"), Some(source_view as u64), "the button is the sender");
    // A dismiss while the slide runs is refused (as UIKit's): wait for it.
    let end = crate::fw::runloop::media_time() + 0.5;
    while view_state(source_view).superview != 0 && crate::fw::runloop::media_time() < end {
        crate::fw::runloop::run_main_loop_briefly();
    }
    runtime::send(source, "dismissViewControllerAnimated:completion:", &[0, 0]);
    assert_eq!(view_state(source_view).superview, window);

    // An identifier no template has does nothing.
    let _ = events_of(&[source, destination]);
    runtime::send(source, "performSegueWithIdentifier:sender:", &[string::make("Nothing"), 0]);
    assert_eq!(runtime::send(source, "presentedViewController", &[]), 0);
    assert!(events_of(&[source, destination]).is_empty());
}

/// +[UIStoryboardSegue segueWithIdentifier:source:destination:performHandler:]
/// runs its handler when performed.
#[test]
fn a_segue_made_in_code_runs_its_handler() {
    setup();
    static RAN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let handler = crate::fw::media::make_callback(7, |value| RAN.store(value, std::sync::atomic::Ordering::SeqCst));
    let source = runtime::send(runtime::send(runtime::class_named("UIViewController"), "alloc", &[]), "init", &[]);
    let destination = runtime::send(runtime::send(runtime::class_named("UIViewController"), "alloc", &[]), "init", &[]);
    let segue = runtime::send(
        runtime::class_named("UIStoryboardSegue"),
        "segueWithIdentifier:source:destination:performHandler:",
        &[string::make("Custom"), source, destination, handler],
    );
    assert_ne!(segue, 0);
    assert_eq!(text_of(segue, "identifier"), "Custom");
    assert_eq!(runtime::send(segue, "sourceViewController", &[]), source);
    assert_eq!(runtime::send(segue, "destinationViewController", &[]), destination);
    runtime::send(segue, "perform", &[]);
    assert_eq!(RAN.load(std::sync::atomic::Ordering::SeqCst), 7);
}

/// The main storyboard: the iPhone variant of its name, then what iOS does
/// before the app's launch code (a window the size of the screen, given to
/// the delegate, with the storyboard's first controller as its root) and
/// after it (the window shown, unless the app showed another).
#[test]
fn the_main_storyboard_makes_and_shows_the_window() {
    setup();
    let both = Value::Dict(vec![
        ("UIMainStoryboardFile".into(), Value::String("Main_iPhone".into())),
        ("UIMainStoryboardFile~ipad".into(), Value::String("Main_iPad".into())),
    ]);
    assert_eq!(main_storyboard_name(&both, false).as_deref(), Some("Main_iPhone"));
    assert_eq!(main_storyboard_name(&both, true).as_deref(), Some("Main_iPad"));
    let variant = Value::Dict(vec![
        ("UIMainStoryboardFile".into(), Value::String("Any".into())),
        ("UIMainStoryboardFile~iphone".into(), Value::String("Phone".into())),
    ]);
    assert_eq!(main_storyboard_name(&variant, false).as_deref(), Some("Phone"));
    assert_eq!(main_storyboard_name(&variant, true).as_deref(), Some("Any"));
    assert_eq!(main_storyboard_name(&Value::Dict(vec![]), false), None);

    let app = test_app("main");
    write_storyboard(&app, "Base.lproj", "MainLaunch", "Root", &[("Root", "UIViewController-root")]);
    let bundle = bundle_at(&app);
    let info = Value::Dict(vec![("UIMainStoryboardFile".into(), Value::String("MainLaunch".into()))]);
    let controller = test_controller("root-view", &[]);
    scene_nib("Base.lproj/MainLaunch.storyboardc/UIViewController-root", controller, vec![]);
    let view = plain_view(10.0, 10.0);
    view_nib("Base.lproj/MainLaunch.storyboardc/root-view", view);
    let delegate = runtime::send(runtime::send(runtime::class_named("StoryboardTestDelegate"), "alloc", &[]), "init", &[]);

    let window = load_main_interface_from(&info, bundle, delegate);
    assert_ne!(window, 0, "a window for the storyboard");
    assert_eq!(string::describe_class_of(window), "UIWindow");
    let (width, height, _) = crate::fw::uikit::screen_points();
    assert_eq!(call::<Rect>(window, "frame"), Rect::new(0.0, 0.0, width, height), "the screen's size");
    assert_eq!(runtime::send(delegate, "window", &[]), window, "the delegate is given the window");
    assert_eq!(runtime::send(window, "rootViewController", &[]), controller);
    // As UIKit's setRootViewController: (0x3c95e): the root's view joins a
    // window only once it shows.
    assert_eq!(view_state(view).superview, 0, "the window is still hidden");
    assert!(!crate::fw::uikit::windows().contains(&window), "not shown before the app's launch code");
    assert!(events_of(&[controller]).is_empty());

    show_main_window(delegate, window);
    assert!(crate::fw::uikit::windows().contains(&window), "shown after didFinishLaunching");
    assert_eq!(view_state(view).superview, window);
    assert_eq!(events_of(&[controller]), [(controller, "viewWillAppear: NO".to_string()), (controller, "viewDidAppear: NO".to_string())]);
    show_main_window(delegate, window);
    assert!(events_of(&[controller]).is_empty(), "shown once");

    // A delegate with no window property still gets its storyboard shown.
    let plain_delegate = runtime::send(runtime::class_named("NSObject"), "new", &[]);
    let window = load_main_interface_from(&info, bundle, plain_delegate);
    assert_ne!(window, 0);
    show_main_window(plain_delegate, window);
    assert!(crate::fw::uikit::windows().contains(&window));

    // An app that put up its own window keeps it: the storyboard's isn't shown.
    let replacing = runtime::send(runtime::send(runtime::class_named("StoryboardTestDelegate"), "alloc", &[]), "init", &[]);
    let window = load_main_interface_from(&info, bundle, replacing);
    let own = new_window();
    runtime::send(replacing, "setWindow:", &[own]);
    runtime::send(own, "makeKeyAndVisible", &[]);
    show_main_window(replacing, window);
    assert!(!crate::fw::uikit::windows().contains(&window));

    // No main storyboard: nothing is made.
    assert_eq!(load_main_interface_from(&Value::Dict(vec![]), bundle, delegate), 0);
}

/// As on UIKit, a controller's view is laid out between viewWillAppear: and
/// viewDidAppear:: SpellCraft opens a tutorial box in -viewDidAppear: and
/// adds its drawer in -viewDidLayoutSubviews, so the drawer covered the box
/// when the layout came after.
#[test]
fn a_controller_is_laid_out_before_it_appears() {
    setup();
    let make = |class: &str| runtime::send(runtime::send(runtime::class_named(class), "alloc", &[]), "init", &[]);
    let window = new_window();
    let parent = make("StoryboardTestController");
    runtime::send(window, "setRootViewController:", &[parent]);
    runtime::send(window, "makeKeyAndVisible", &[]);
    let child = make("StoryboardTestLayoutController");
    runtime::send(parent, "addChildViewController:", &[child]);
    let child_view = runtime::send(child, "view", &[]);
    let _ = events_of(&[child]);
    runtime::send(runtime::send(parent, "view", &[]), "addSubview:", &[child_view]);
    let told: Vec<String> = events_of(&[child]).into_iter().map(|e| e.1).collect();
    assert_eq!(told, ["viewWillAppear: NO", "viewDidLayoutSubviews", "viewDidAppear: NO"]);
}

/// transitionFromViewController: as UIKit's _transitionFromView: (0x57c47):
/// nothing lays the arriving view out in the call, so its controller hears
/// viewWillAppear: first; the run loop's layout pass lays it out before
/// viewDidAppear:. (An earlier fix laid it out before viewWillAppear: for
/// SpellCraft's tutorial box and drawer; Apple's order is kept here.)
#[test]
fn a_transition_lays_out_the_arriving_view_after_will_appear() {
    setup();
    let make = |class: &str| runtime::send(runtime::send(runtime::class_named(class), "alloc", &[]), "init", &[]);
    let window = new_window();
    let parent = make("StoryboardTestController");
    runtime::send(window, "setRootViewController:", &[parent]);
    runtime::send(window, "makeKeyAndVisible", &[]);
    let parent_view = runtime::send(parent, "view", &[]);
    let (from, to) = (make("StoryboardTestController"), make("StoryboardTestLayoutController"));
    runtime::send(parent, "addChildViewController:", &[from]);
    runtime::send(parent_view, "addSubview:", &[runtime::send(from, "view", &[])]);
    runtime::send(parent, "addChildViewController:", &[to]);
    let _ = runtime::send(to, "view", &[]);
    // As main-loop work, so other tests' run loop turns (which lay out shown
    // windows at the end of each turn) can't come in between.
    let mut told: Vec<String> = crate::fw::runloop::as_main(|| {
        let _ = events_of(&[parent, from, to]);
        crate::fw::uikit::controller_transition(parent, 0, from, to, 0.0, 0, 0, 0);
        events_of(&[to]).into_iter().map(|e| e.1).collect()
    });
    assert_eq!(told.first().map(String::as_str), Some("viewWillAppear: YES"), "nothing laid out before it");
    let end = crate::fw::runloop::media_time() + 0.3;
    while !told.iter().any(|e| e == "viewDidAppear: YES") && crate::fw::runloop::media_time() < end {
        crate::fw::runloop::run_main_loop_briefly();
        told.extend(events_of(&[to]).into_iter().map(|e| e.1));
    }
    assert_eq!(told, ["viewWillAppear: YES", "viewDidLayoutSubviews", "viewDidAppear: YES"]);
}

/// UIKit's rule for every controller, not just the window's root: a
/// controller hears viewWillAppear: / viewDidAppear: (once each) when its
/// view comes into a shown window, and viewWillDisappear: /
/// viewDidDisappear: when it leaves — a child added with
/// addChildViewController: whose view the parent puts in its own, views
/// that come along inside a view put on screen, a window being shown. A
/// container whose automaticallyForwardAppearanceAndRotationMethodsToChildViewControllers
/// says NO tells its children itself.
#[test]
fn controllers_appear_and_disappear_with_their_views() {
    setup();
    let make = |class: &str| runtime::send(runtime::send(runtime::class_named(class), "alloc", &[]), "init", &[]);
    let event = |id: Id, what: &str| (id, what.to_string());
    let window = new_window();
    let (parent, child, grandchild) = (make("StoryboardTestController"), make("StoryboardTestController"), make("StoryboardTestController"));
    runtime::send(window, "setRootViewController:", &[parent]);
    let parent_view = runtime::send(parent, "view", &[]);
    // A child already inside the root's view before the window is shown comes on screen with it.
    runtime::send(parent, "addChildViewController:", &[grandchild]);
    runtime::send(parent_view, "addSubview:", &[runtime::send(grandchild, "view", &[])]);
    assert!(events_of(&[parent, grandchild]).is_empty(), "nothing is on screen yet");
    runtime::send(window, "makeKeyAndVisible", &[]);
    let shown = events_of(&[parent, grandchild]);
    assert_eq!(shown.iter().filter(|e| e.0 == parent).cloned().collect::<Vec<_>>(), [event(parent, "viewWillAppear: NO"), event(parent, "viewDidAppear: NO")], "the root, once");
    assert_eq!(
        shown.iter().filter(|e| e.0 == grandchild).cloned().collect::<Vec<_>>(),
        [event(grandchild, "viewWillAppear: NO"), event(grandchild, "viewDidAppear: NO")]
    );

    // A child's view put in the shown parent's view.
    runtime::send(parent, "addChildViewController:", &[child]);
    let child_view = runtime::send(child, "view", &[]);
    runtime::send(parent_view, "addSubview:", &[child_view]);
    assert_eq!(events_of(&[parent, child]), [event(child, "viewWillAppear: NO"), event(child, "viewDidAppear: NO")]);
    runtime::send(parent_view, "insertSubview:atIndex:", &[child_view, 0]);
    assert!(events_of(&[parent, child]).is_empty(), "moving inside the window changes nothing");
    runtime::send(child_view, "removeFromSuperview", &[]);
    assert_eq!(events_of(&[parent, child]), [event(child, "viewWillDisappear: NO"), event(child, "viewDidDisappear: NO")]);
    runtime::send(child_view, "removeFromSuperview", &[]);
    assert!(events_of(&[child]).is_empty(), "gone once");

    // A view holding a controller's view, put on screen, brings it along.
    let holder = plain_view(50.0, 50.0);
    runtime::send(holder, "addSubview:", &[child_view]);
    assert!(events_of(&[child]).is_empty(), "the holder is off screen");
    runtime::send(parent_view, "addSubview:", &[holder]);
    assert_eq!(events_of(&[child]), [event(child, "viewWillAppear: NO"), event(child, "viewDidAppear: NO")]);
    runtime::send(holder, "removeFromSuperview", &[]);
    assert_eq!(events_of(&[child]), [event(child, "viewWillDisappear: NO"), event(child, "viewDidDisappear: NO")]);

    // A container that doesn't forward: it hears its own, its children nothing.
    let (quiet, inner) = (make("StoryboardTestQuietController"), make("StoryboardTestController"));
    runtime::send(quiet, "addChildViewController:", &[inner]);
    let quiet_view = runtime::send(quiet, "view", &[]);
    runtime::send(quiet_view, "addSubview:", &[runtime::send(inner, "view", &[])]);
    runtime::send(parent, "addChildViewController:", &[quiet]);
    runtime::send(parent_view, "addSubview:", &[quiet_view]);
    assert_eq!(events_of(&[quiet, inner]), [event(quiet, "viewWillAppear: NO"), event(quiet, "viewDidAppear: NO")]);
    // Its child's view put in while it is on screen: still nothing.
    let late = make("StoryboardTestController");
    runtime::send(quiet, "addChildViewController:", &[late]);
    runtime::send(quiet_view, "addSubview:", &[runtime::send(late, "view", &[])]);
    assert!(events_of(&[late]).is_empty(), "the container tells its children itself");
    runtime::send(quiet_view, "removeFromSuperview", &[]);
    assert_eq!(events_of(&[quiet, inner, late]), [event(quiet, "viewWillDisappear: NO"), event(quiet, "viewDidDisappear: NO")]);
    assert!(events_of(&[parent]).is_empty(), "the root heard nothing more");
}

/// transitionFromViewController: the arriving child and the controllers
/// inside its view hear viewWillAppear:YES at the start and viewDidAppear:YES
/// when it is over (the leaving one alike). Dismissing A→B→C from A: only A
/// comes back; B, never uncovered, hears nothing.
#[test]
fn transitions_and_stacked_dismissals_tell_the_right_controllers() {
    setup();
    let make = |class: &str| runtime::send(runtime::send(runtime::class_named(class), "alloc", &[]), "init", &[]);
    let event = |id: Id, what: &str| (id, what.to_string());
    let window = new_window();
    let parent = make("StoryboardTestController");
    runtime::send(window, "setRootViewController:", &[parent]);
    runtime::send(window, "makeKeyAndVisible", &[]);
    let parent_view = runtime::send(parent, "view", &[]);
    let (from, to, inside) = (make("StoryboardTestController"), make("StoryboardTestController"), make("StoryboardTestController"));
    runtime::send(parent, "addChildViewController:", &[from]);
    runtime::send(parent_view, "addSubview:", &[runtime::send(from, "view", &[])]);
    runtime::send(parent, "addChildViewController:", &[to]);
    runtime::send(to, "addChildViewController:", &[inside]);
    runtime::send(runtime::send(to, "view", &[]), "addSubview:", &[runtime::send(inside, "view", &[])]);
    let _ = events_of(&[parent, from, to, inside]);
    crate::fw::uikit::controller_transition(parent, 0, from, to, 0.0, 0, 0, 0);
    // The starts come at once; the ends after the (zero-length) animation,
    // at a later main-loop turn. Another test may turn the shared main loop
    // meanwhile, so the ends can already be here: the order is what counts.
    let mut heard = events_of(&[from, to, inside]);
    let end = crate::fw::runloop::media_time() + 0.3;
    while !heard.contains(&event(inside, "viewDidAppear: YES")) && crate::fw::runloop::media_time() < end {
        crate::fw::runloop::run_main_loop_briefly();
        heard.extend(events_of(&[from, to, inside]));
    }
    assert_eq!(
        heard,
        [
            event(from, "viewWillDisappear: YES"),
            event(to, "viewWillAppear: YES"),
            event(inside, "viewWillAppear: YES"),
            event(from, "viewDidDisappear: YES"),
            event(to, "viewDidAppear: YES"),
            event(inside, "viewDidAppear: YES"),
        ],
        "the starts, then the ends"
    );

    // A shows B shows C; dismissing from A.
    let (a, b, c) = (make("StoryboardTestController"), make("StoryboardTestController"), make("StoryboardTestController"));
    let other = new_window();
    runtime::send(other, "setRootViewController:", &[a]);
    runtime::send(other, "makeKeyAndVisible", &[]);
    runtime::send(a, "presentViewController:animated:completion:", &[b, 0, 0]);
    runtime::send(b, "presentViewController:animated:completion:", &[c, 0, 0]);
    let _ = events_of(&[a, b, c]);
    runtime::send(a, "dismissViewControllerAnimated:completion:", &[0, 0]);
    let heard = events_of(&[a, b, c]);
    assert!(heard.iter().all(|e| e.0 != b), "B is never uncovered: {heard:?}");
    assert_eq!(heard.iter().filter(|e| e.0 == a).cloned().collect::<Vec<_>>(), [event(a, "viewWillAppear: NO"), event(a, "viewDidAppear: NO")]);
    assert_eq!(heard.iter().filter(|e| e.0 == c).cloned().collect::<Vec<_>>(), [event(c, "viewWillDisappear: NO"), event(c, "viewDidDisappear: NO")]);
}

/// A shown window given a new root controller: the controller's view is
/// loaded (viewDidLoad) before it hears viewWillAppear:, as in UIKit.
#[test]
fn a_new_root_of_a_shown_window_loads_its_view_before_it_appears() {
    setup();
    let window = new_window();
    runtime::send(window, "makeKeyAndVisible", &[]);
    let fresh = runtime::send(runtime::send(runtime::class_named("StoryboardTestLoadingController"), "alloc", &[]), "init", &[]);
    runtime::send(window, "setRootViewController:", &[fresh]);
    assert_eq!(
        events_of(&[fresh]),
        [(fresh, "viewDidLoad".to_string()), (fresh, "viewWillAppear: NO".to_string()), (fresh, "viewDidAppear: NO".to_string())]
    );
}

/// A presents B, then the window's root becomes C: what A presented is
/// closed with it (B's view leaves, the links go), so dismissing B later
/// can't put A's view back over C.
#[test]
fn replacing_a_presenting_root_closes_what_it_presented() {
    setup();
    let new_controller = || runtime::send(runtime::send(runtime::class_named("StoryboardTestController"), "alloc", &[]), "init", &[]);
    let (first, presented, replacement) = (new_controller(), new_controller(), new_controller());
    let window = new_window();
    runtime::send(window, "setRootViewController:", &[first]);
    runtime::send(window, "makeKeyAndVisible", &[]);
    runtime::send(first, "presentViewController:animated:completion:", &[presented, 0, 0]);
    let (first_view, presented_view) = (runtime::send(first, "view", &[]), runtime::send(presented, "view", &[]));
    assert_eq!((view_state(first_view).superview, view_state(presented_view).superview), (0, window));
    let _ = events_of(&[first, presented, replacement]);

    runtime::send(window, "setRootViewController:", &[replacement]);
    let replacement_view = runtime::send(replacement, "view", &[]);
    assert_eq!(view_state(presented_view).superview, 0, "the presented view leaves with its presenter");
    assert_eq!(view_state(first_view).superview, 0);
    assert_eq!(view_state(replacement_view).superview, window);
    assert_eq!(runtime::send(first, "presentedViewController", &[]), 0);
    assert_eq!(runtime::send(presented, "presentingViewController", &[]), 0);
    assert_eq!(
        events_of(&[first, presented]),
        [(presented, "viewWillDisappear: NO".to_string()), (presented, "viewDidDisappear: NO".to_string())]
    );

    runtime::send(presented, "dismissViewControllerAnimated:completion:", &[0, 0]);
    assert_eq!(view_state(first_view).superview, 0, "the old root stays gone");
    assert_eq!(view_state(replacement_view).superview, window);
}

/// 32-bit games send these messages through the bridge, which needs their
/// types (framework_selectors.tsv) when the game doesn't implement them.
#[test]
fn games_on_the_32_bit_bridge_reach_the_storyboard_messages() {
    use crate::guest32::objc::bridge::{send_from_guest, Bridge, SendKind};
    use crate::guest32::objc::testing::Fake;
    setup();
    let app = test_app("bridge");
    write_storyboard(&app, "en.lproj", "BridgeBoard", "Start", &[("Start", "UIViewController-start"), ("Map", "UIViewController-map")]);
    let bundle = bundle_at(&app);
    let source = test_controller("bridge-start-view", &[]);
    let destination = test_controller("bridge-map-view", &[]);
    scene_nib("en.lproj/BridgeBoard.storyboardc/UIViewController-start", source, vec![]);
    scene_nib("en.lproj/BridgeBoard.storyboardc/UIViewController-map", destination, vec![]);

    let fake = Fake::new();
    let bridge = Mutex::new(Bridge::new(&guest32_image::ObjcMetadata32::default()).unwrap());
    let guest = |host: Id| bridge.lock().unwrap().guest_object(fake, host).unwrap();
    let host = |guest: u32| bridge.lock().unwrap().host_object(guest).unwrap_or(0);
    let send = |receiver: u32, name: &str, arguments: &[u32]| -> u32 {
        let selector = bridge.lock().unwrap().guest_selector(fake, name);
        let words: Vec<u32> = [receiver, selector].iter().chain(arguments).copied().collect();
        let word = |index: u32| words.get(index as usize).copied().unwrap_or(0);
        send_from_guest(&bridge, fake, SendKind::Normal, &word).unwrap_or_else(|error| panic!("{name}: {error}")).r0
    };
    let class = |name: &str| guest(runtime::class_named(name));
    let text_object = |value: &str| guest(string::make(value));

    let storyboard = send(class("UIStoryboard"), "storyboardWithName:bundle:", &[text_object("BridgeBoard"), guest(bundle)]);
    assert_eq!(string::describe_class_of(host(storyboard)), "UIStoryboard");
    let first = send(storyboard, "instantiateInitialViewController", &[]);
    assert_eq!(host(first), source);
    assert_eq!(host(send(storyboard, "instantiateViewControllerWithIdentifier:", &[text_object("Map")])), destination);
    assert_eq!(send(first, "storyboard", &[]), storyboard);
    assert_eq!(string::get(host(send(first, "nibName", &[]))), "bridge-start-view");
    send(first, "nibBundle", &[]);
    send(first, "setModalTransitionStyle:", &[2]);
    assert_eq!(send(first, "modalTransitionStyle", &[]), 2);
    send(first, "shouldPerformSegueWithIdentifier:sender:", &[text_object("Map"), 0]);

    let segue = send(
        class("UIStoryboardSegue"),
        "segueWithIdentifier:source:destination:performHandler:",
        &[text_object("Custom"), first, guest(destination), 0],
    );
    assert_eq!(send(segue, "sourceViewController", &[]), first);
    assert_eq!(host(send(segue, "destinationViewController", &[])), destination);
    assert_eq!(string::get(host(send(segue, "identifier", &[]))), "Custom");
    send(first, "prepareForSegue:sender:", &[segue, 0]);
    send(segue, "perform", &[]);
    let made = send(send(class("UIStoryboardSegue"), "alloc", &[]), "initWithIdentifier:source:destination:", &[text_object("Made"), first, 0]);
    assert_eq!(send(made, "sourceViewController", &[]), first);

    let plain = send(send(class("UIViewController"), "alloc", &[]), "init", &[]);
    send(plain, "loadView", &[]);
    for name in ["viewWillAppear:", "viewDidAppear:", "viewWillDisappear:", "viewDidDisappear:"] {
        send(plain, name, &[0]);
    }
    send(send(class("NSObject"), "new", &[]), "awakeFromNib", &[]);
    let view = send(class("UIView"), "alloc", &[]);
    let coded = coder(vec![("UIBounds", Held::Rect(Rect::new(0.0, 0.0, 30.0, 20.0)))]);
    assert_eq!(send(view, "initWithCoder:", &[guest(coded)]), view);
    assert_eq!(call::<Rect>(host(view), "frame").size, Size { width: 30.0, height: 20.0 });
    let image_view = send(send(class("UIImageView"), "alloc", &[]), "init", &[]);
    send(image_view, "setHighlightedImage:", &[0]);
    assert_eq!(send(image_view, "highlightedImage", &[]), 0);
}

/// A table from a storyboard keeps its prototype cells
/// (UITableViewCellPrototypeNibs): dequeueReusableCellWithIdentifier: makes
/// a new cell from that cell's nib, given its external objects; a cell reads
/// its reuse identifier, style, content view and labels from its coder.
#[test]
fn a_table_makes_cells_from_its_prototypes() {
    setup();
    fake_nib("TestCharacterCell", |_owner, _external| {
        let label = |text_shown: &str, y: f64| {
            decoded(
                "UILabel",
                vec![
                    ("UIBounds", Held::Rect(Rect::new(0.0, 0.0, 40.0, 20.0))),
                    ("UICenter", Held::Point(Point { x: 30.0, y })),
                    ("UIText", text(text_shown)),
                ],
            )
        };
        let content = plain_view(240.0, 44.0);
        let cell = decoded(
            "UITableViewCell",
            vec![
                ("UIBounds", Held::Rect(Rect::new(0.0, 0.0, 240.0, 44.0))),
                ("UICenter", Held::Point(Point { x: 120.0, y: 22.0 })),
                ("UISubviews", array(&[content])),
                ("UIReuseIdentifier", text("characterCell")),
                ("UITableViewCellStyle", Held::Integer(3)),
                ("UIContentView", Held::Object(content)),
                ("UITextLabel", Held::Object(label("Title", 14.0))),
                ("UIDetailTextLabel", Held::Object(label("Subtitle", 33.0))),
            ],
        );
        vec![cell]
    });
    let nib = runtime::send(runtime::class_named(TEST_NIB_CLASS), "nibWithNibName:bundle:", &[string::make("TestCharacterCell"), 0]);
    let externals = collections::dictionary_from(&[]);
    let table = decoded(
        "UITableView",
        vec![
            ("UIBounds", Held::Rect(Rect::new(0.0, 0.0, 240.0, 174.0))),
            ("UICenter", Held::Point(Point { x: 120.0, y: 87.0 })),
            ("UITableViewCellPrototypeNibs", Held::Object(collections::dictionary_from(&[(string::make("characterCell"), nib)]))),
            ("UITableViewCellPrototypeNibExternalObjects", Held::Object(collections::dictionary_from(&[(string::make("characterCell"), externals)]))),
        ],
    );
    let cell = runtime::send(table, "dequeueReusableCellWithIdentifier:", &[string::make("characterCell")]);
    assert!(cell != 0, "a cell from the prototype");
    assert_eq!(text_of(runtime::send(cell, "textLabel", &[]), "text"), "Title");
    assert_eq!(text_of(runtime::send(cell, "detailTextLabel", &[]), "text"), "Subtitle");
    assert_eq!(text_of(cell, "reuseIdentifier"), "characterCell");
    let content = runtime::send(cell, "contentView", &[]);
    assert!(view_state(cell).subviews.contains(&content), "its content view is inside it");
    assert_eq!(view_state(runtime::send(cell, "textLabel", &[])).superview, content, "its labels are in the content view");
    let loads = nib_loads("TestCharacterCell");
    assert_eq!(loads.len(), 1);
    assert_eq!(loads[0].external, externals, "the prototype's external objects");
    let other = runtime::send(table, "dequeueReusableCellWithIdentifier:forIndexPath:", &[string::make("characterCell"), 0]);
    assert!(other != 0 && other != cell, "each dequeue makes a new cell");
    assert_eq!(runtime::send(table, "dequeueReusableCellWithIdentifier:", &[string::make("nothing")]), 0);
}

/// A cell nib may name a label the reader couldn't make (nil): it is left
/// out. UIKit's own label and content view classes in cell nibs exist here.
#[test]
fn cell_nibs_name_uikits_own_cell_classes() {
    setup();
    let without = decoded("UITableViewCell", vec![("UIBounds", Held::Rect(Rect::new(0.0, 0.0, 240.0, 44.0))), ("UIDetailTextLabel", Held::Object(0))]);
    assert!(without != 0);
    for name in ["UITableViewLabel", "UITableViewCellContentView"] {
        assert!(runtime::existing_class(name, false).is_some(), "{name}");
    }
    let label = decoded("UITableViewLabel", vec![("UIText", text("Subtitle"))]);
    assert_eq!(text_of(label, "text"), "Subtitle");
}
