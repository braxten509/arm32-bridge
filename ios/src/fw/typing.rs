//! Typing into text fields and text views: the first responder (what is
//! being edited), letters typed into it through its delegate as UIKit's
//! keyboard does, and how a text field looks. Letters come from the
//! phone's keyboard (android.rs) or, on the PC, IOS_TYPE="seconds:text;...".

use crate::fw::geometry::Rect;
use crate::fw::graphics::TextLook;
use crate::fw::object::methods;
use crate::fw::string;
use crate::fw::uikit::{property_get, property_set, view_state};
use crate::objc::runtime::{self, lock, Id, Sel};
use crate::symbols::Registry;
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(test)]
use std::sync::Mutex;

/// Placeholder text is light grey (UITextField's own colour).
pub const PLACEHOLDER: [f64; 4] = [0.7, 0.7, 0.7, 1.0];

/// Tests that edit text take turns (there is one first responder).
#[cfg(test)]
pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());

static FIRST_RESPONDER: AtomicUsize = AtomicUsize::new(0);

/// The phone app's keyboard: shown (true) while something is edited.
static KEYBOARD: std::sync::OnceLock<fn(bool)> = std::sync::OnceLock::new();

pub fn set_keyboard_screen(show: fn(bool)) {
    let _ = KEYBOARD.set(show);
}

/// Shows or hides the phone's keyboard once the change of first responder
/// has settled (moving from one field to another keeps it up).
fn keyboard_follows() {
    if let Some(&show) = KEYBOARD.get() {
        crate::fw::controls::later(move || show(first_responder() != 0));
    }
}

/// What is being edited (0: nothing); the phone shows its keyboard while
/// something is.
pub fn first_responder() -> Id {
    FIRST_RESPONDER.load(Ordering::Relaxed) as Id
}

// UIControlEvents of editing.
const EDITING_DID_BEGIN: usize = 1 << 16;
const EDITING_CHANGED: usize = 1 << 17;
const EDITING_DID_END: usize = 1 << 18;
const EDITING_DID_END_ON_EXIT: usize = 1 << 19;

/// The delegate messages and notifications of one kind of text input.
struct Kind {
    field: bool,
    should_begin: &'static str,
    did_begin: &'static str,
    should_change: &'static str,
    did_change: Option<&'static str>,
    should_end: &'static str,
    did_end: &'static str,
    began: &'static str,
    changed: &'static str,
    ended: &'static str,
}

const FIELD: Kind = Kind {
    field: true,
    should_begin: "textFieldShouldBeginEditing:",
    did_begin: "textFieldDidBeginEditing:",
    should_change: "textField:shouldChangeCharactersInRange:replacementString:",
    did_change: None,
    should_end: "textFieldShouldEndEditing:",
    did_end: "textFieldDidEndEditing:",
    began: "UITextFieldTextDidBeginEditingNotification",
    changed: "UITextFieldTextDidChangeNotification",
    ended: "UITextFieldTextDidEndEditingNotification",
};

const VIEW: Kind = Kind {
    field: false,
    should_begin: "textViewShouldBeginEditing:",
    did_begin: "textViewDidBeginEditing:",
    should_change: "textView:shouldChangeTextInRange:replacementText:",
    did_change: Some("textViewDidChange:"),
    should_end: "textViewShouldEndEditing:",
    did_end: "textViewDidEndEditing:",
    began: "UITextViewTextDidBeginEditingNotification",
    changed: "UITextViewTextDidChangeNotification",
    ended: "UITextViewTextDidEndEditingNotification",
};

/// The notification names games may listen for.
pub const NOTIFICATIONS: [&str; 6] = [FIELD.began, FIELD.changed, FIELD.ended, VIEW.began, VIEW.changed, VIEW.ended];

fn is(object: Id, name: &str) -> bool {
    let class = runtime::class_named(name);
    object != 0 && class != 0 && {
        let rt = lock();
        rt.is_subclass(rt.class_of(object), class)
    }
}

fn kind_of(object: Id) -> Option<&'static Kind> {
    if is(object, "UITextField") {
        Some(&FIELD)
    } else if is(object, "UITextView") {
        Some(&VIEW)
    } else {
        None
    }
}

fn flag(object: Id, key: &'static str, default: bool) -> bool {
    property_get(object, key).map_or(default, |v| v != 0)
}

fn text_of(object: Id) -> String {
    match property_get(object, "text").unwrap_or(0) as Id {
        0 => String::new(),
        text => string::get(text),
    }
}

/// A yes-or-no question to the delegate; yes when it doesn't answer.
fn ask(object: Id, name: &str, args: &[usize]) -> bool {
    let delegate = property_get(object, "delegate").unwrap_or(0) as Id;
    delegate == 0 || !runtime::responds(delegate, name) || runtime::send(delegate, name, args) & 0xFF != 0
}

fn tell(object: Id, name: &str, args: &[usize]) {
    let delegate = property_get(object, "delegate").unwrap_or(0) as Id;
    if delegate != 0 && runtime::responds(delegate, name) {
        runtime::send(delegate, name, args);
    }
}

fn notify(name: &str, object: Id) {
    crate::fw::foundation::post(name, object, 0);
}

fn editable(object: Id, kind: &Kind) -> bool {
    if kind.field { flag(object, "enabled", true) } else { flag(object, "editable", true) }
}

extern "C" fn become_first_responder(this: Id, _: Sel) -> bool {
    let Some(kind) = kind_of(this) else { return false };
    if first_responder() == this {
        return true;
    }
    if !editable(this, kind) || !ask(this, kind.should_begin, &[this]) {
        return false;
    }
    let old = first_responder();
    if old != 0 && runtime::send(old, "resignFirstResponder", &[]) & 0xFF == 0 {
        return false;
    }
    FIRST_RESPONDER.store(this, Ordering::Relaxed);
    if kind.field && flag(this, "clearsOnBeginEditing", false) {
        property_set(this, "text", string::make("") as u64);
    }
    crate::log!("{} {this:#x} is being edited (keyboard up)", string::describe_class_of(this));
    keyboard_follows();
    tell(this, kind.did_begin, &[this]);
    if kind.field {
        crate::fw::controls::send_actions(this, EDITING_DID_BEGIN, 0);
    }
    notify(kind.began, this);
    true
}

extern "C" fn resign_first_responder(this: Id, _: Sel) -> bool {
    let Some(kind) = kind_of(this) else { return true };
    if first_responder() != this {
        return true;
    }
    if !ask(this, kind.should_end, &[this]) {
        return false;
    }
    FIRST_RESPONDER.store(0, Ordering::Relaxed);
    keyboard_follows();
    tell(this, kind.did_end, &[this]);
    if kind.field {
        crate::fw::controls::send_actions(this, EDITING_DID_END, 0);
    }
    notify(kind.ended, this);
    true
}

extern "C" fn is_first_responder(this: Id, _: Sel) -> bool {
    first_responder() == this
}

extern "C" fn can_become_first_responder(this: Id, _: Sel) -> bool {
    kind_of(this).is_some_and(|kind| editable(this, kind))
}

/// A tap on a text field (let go on it) starts editing it.
extern "C" fn field_touches_ended(this: Id, s: Sel, touches: Id, event: Id) {
    let passing = flag(this, "passingTouches", false);
    crate::fw::controls::control_touches_ended(this, s, touches, event);
    if !passing && flag(this, "touchInside", false) {
        runtime::send(this, "becomeFirstResponder", &[]);
    }
}

/// Letters typed on the keyboard into what is being edited, one at a
/// time as a keyboard sends them: each through the delegate, which may
/// refuse it; "\u{8}" deletes the last letter and "\n" in a text field is
/// the return key.
pub fn type_text(text: &str) {
    for letter in text.chars() {
        let object = first_responder();
        let Some(kind) = kind_of(object) else { return };
        if letter == '\n' && kind.field {
            press_return(object);
            continue;
        }
        let old = text_of(object);
        let end = old.encode_utf16().count();
        let (location, length, insert) = match letter {
            '\u{8}' => match old.chars().last() {
                Some(last) => (end - last.len_utf16(), last.len_utf16(), String::new()),
                None => continue,
            },
            _ => (end, 0, letter.to_string()),
        };
        if !ask(object, kind.should_change, &[object, location, length, string::make(&insert)]) {
            continue;
        }
        let kept: Vec<u16> = old.encode_utf16().take(location).collect();
        let new = String::from_utf16_lossy(&kept) + &insert;
        property_set(object, "text", string::make(&new) as u64);
        if let Some(did_change) = kind.did_change {
            tell(object, did_change, &[object]);
        }
        if kind.field {
            crate::fw::controls::send_actions(object, EDITING_CHANGED, 0);
        }
        notify(kind.changed, object);
    }
}

/// Text from the phone's keyboard as Java holds it (UTF-16 units).
pub fn type_java_text(units: &[u16]) {
    type_text(&String::from_utf16_lossy(units));
}

/// The return key: textFieldShouldReturn:, then EditingDidEndOnExit,
/// which ends editing when someone listens for it (UIKit's way).
fn press_return(field: Id) {
    if !ask(field, "textFieldShouldReturn:", &[field]) {
        return;
    }
    if crate::fw::controls::listens_for(field, EDITING_DID_END_ON_EXIT) {
        crate::fw::controls::send_actions(field, EDITING_DID_END_ON_EXIT, 0);
        runtime::send(field, "resignFirstResponder", &[]);
    }
}

/// What a UITextField shows (its text, dots for a password, or the
/// placeholder in grey) and where in the field: inside a 7-point margin
/// when it has a border, as UIKit draws it.
pub fn text_field_look(field: Id) -> Option<(TextLook, Rect)> {
    let text = text_of(field);
    let (shown, color) = if text.is_empty() {
        let placeholder = property_get(field, "placeholder").unwrap_or(0) as Id;
        (if placeholder == 0 { String::new() } else { string::get(placeholder) }, PLACEHOLDER)
    } else if flag(field, "secureTextEntry", false) {
        ("•".repeat(text.chars().count()), [0.0, 0.0, 0.0, 1.0])
    } else {
        let color = crate::fw::graphics::color_rgba(property_get(field, "textColor").unwrap_or(0) as Id);
        (text, color.unwrap_or([0.0, 0.0, 0.0, 1.0]))
    };
    if shown.is_empty() {
        return None;
    }
    let size = view_state(field).frame.size;
    let margin = if property_get(field, "borderStyle").unwrap_or(0) != 0 { 7.0 } else { 0.0 };
    let rect = Rect::new(margin, 0.0, (size.width - 2.0 * margin).max(0.0), size.height);
    let font = crate::fw::controls::font_name_and_size(runtime::send(field, "font", &[]));
    let minimum = property_get(field, "minimumFontSize").map_or(0.0, f64::from_bits);
    let look = TextLook {
        text: shown,
        font,
        color,
        shadow: None,
        alignment: property_get(field, "textAlignment").map_or(0, |v| v as isize),
        // NSLineBreakByClipping: a field's text is never cut with dots.
        line_break: 2,
        lines: 1,
        size: (rect.size.width, rect.size.height),
        minimum_font_size: flag(field, "adjustsFontSizeToFitWidth", false).then_some(minimum),
    };
    Some((look, rect))
}

/// IOS_TYPE="seconds:text;seconds:text" (PC tests): typed into what is
/// being edited at those times; "\n" in the text is the return key and
/// "\b" deletes.
pub fn schedule_test_typing() {
    let Ok(list) = std::env::var("IOS_TYPE") else { return };
    let typed: Vec<(f64, String)> = list
        .split(';')
        .filter_map(|part| {
            let (time, text) = part.split_once(':')?;
            Some((time.trim().parse().ok()?, text.replace("\\n", "\n").replace("\\b", "\u{8}")))
        })
        .collect();
    std::thread::spawn(move || {
        let start = std::time::Instant::now();
        for (time, text) in typed {
            let wait = time - start.elapsed().as_secs_f64();
            if wait > 0.0 {
                std::thread::sleep(std::time::Duration::from_secs_f64(wait));
            }
            crate::log!("test typing {text:?}");
            crate::fw::runloop::post(crate::fw::runloop::Work::Native(Box::new(move || type_text(&text))));
        }
    });
}

pub fn register(_: &mut Registry) {
    let editing: &[(&str, usize)] = &[
        ("becomeFirstResponder", become_first_responder as usize),
        ("resignFirstResponder", resign_first_responder as usize),
        ("isFirstResponder", is_first_responder as usize),
        ("canBecomeFirstResponder", can_become_first_responder as usize),
        ("isEditing", is_first_responder as usize),
    ];
    for name in ["UITextField", "UITextView"] {
        methods(runtime::class_named(name), editing);
    }
    methods(runtime::class_named("UITextField"), &[("touchesEnded:withEvent:", field_touches_ended as usize)]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fw::geometry::Point;
    use crate::fw::object::{define_class, methods};
    use crate::fw::string;
    use crate::fw::touch::{deliver_in, Phase};
    use crate::objc::runtime::{self, lock, Sel};

    static HEARD: Mutex<Vec<String>> = Mutex::new(Vec::new());

    fn heard() -> Vec<String> {
        std::mem::take(&mut *HEARD.lock().unwrap())
    }

    fn new_view(class: &str, frame: Rect) -> Id {
        let object = runtime::send(runtime::class_named(class), "alloc", &[]);
        let s = runtime::sel("initWithFrame:");
        let init: extern "C" fn(Id, Sel, Rect) -> Id = unsafe { std::mem::transmute(runtime::imp_for(object, s).unwrap()) };
        init(object, s, frame)
    }

    /// Text fields are edited as on iOS: tapped (or asked) they become the
    /// first responder if their delegate agrees; each typed letter goes
    /// through textField:shouldChangeCharactersInRange:replacementString:
    /// (which may refuse it) and targets hear EditingChanged; delete takes
    /// the last letter, return asks textFieldShouldReturn:. They show their
    /// text, dots for a password, or the placeholder in grey.
    #[test]
    fn text_fields_are_edited_through_their_delegate() {
        let _turn = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        extern "C" fn note(_this: Id, s: Sel, _field: Id) -> bool {
            HEARD.lock().unwrap().push(lock().sel_name(s));
            true
        }
        extern "C" fn change(_this: Id, _: Sel, _field: Id, location: usize, length: usize, text: Id) -> bool {
            let text = string::get(text);
            HEARD.lock().unwrap().push(format!("change {location},{length} \"{text}\""));
            text != "x"
        }
        extern "C" fn changed(_this: Id, _: Sel, _sender: Id, _event: Id) {
            HEARD.lock().unwrap().push("changed".into());
        }
        let delegate_class = define_class("TypingTestDelegate", "NSObject", 8);
        methods(
            delegate_class,
            &[
                ("textFieldShouldBeginEditing:", note as usize),
                ("textFieldDidBeginEditing:", note as usize),
                ("textField:shouldChangeCharactersInRange:replacementString:", change as usize),
                ("textFieldShouldReturn:", note as usize),
                ("textFieldDidEndEditing:", note as usize),
                ("changed:event:", changed as usize),
            ],
        );
        let delegate = runtime::send(runtime::send(delegate_class, "alloc", &[]), "init", &[]);
        let window = new_view("UIWindow", Rect::new(0.0, 0.0, 568.0, 320.0));
        let field = new_view("UITextField", Rect::new(10.0, 10.0, 200.0, 30.0));
        runtime::send(window, "addSubview:", &[field]);
        runtime::send(field, "setDelegate:", &[delegate]);
        runtime::send(field, "setPlaceholder:", &[string::make("Name")]);
        runtime::send(field, "addTarget:action:forControlEvents:", &[delegate, runtime::sel("changed:event:"), 1 << 17]);
        let shown = || text_field_look(field).map(|(look, _)| (look.text, look.color));
        let text = || string::get(runtime::send(field, "text", &[]));
        assert_eq!(shown(), Some(("Name".to_string(), PLACEHOLDER)), "the placeholder, in grey");

        // A tap starts editing.
        deliver_in(window, 701, Phase::Began, Point { x: 20.0, y: 20.0 });
        deliver_in(window, 701, Phase::Ended, Point { x: 20.0, y: 20.0 });
        assert_eq!(first_responder(), field);
        assert!(runtime::send(field, "isFirstResponder", &[]) & 0xFF != 0);
        assert_eq!(heard(), ["textFieldShouldBeginEditing:", "textFieldDidBeginEditing:"]);
        type_text("abx");
        assert_eq!(text(), "ab", "x refused by the delegate");
        assert_eq!(heard(), ["change 0,0 \"a\"", "changed", "change 1,0 \"b\"", "changed", "change 2,0 \"x\""]);
        type_text("\u{8}");
        assert_eq!(text(), "a");
        assert_eq!(heard(), ["change 1,1 \"\"", "changed"]);
        type_text("\n");
        assert_eq!(heard(), ["textFieldShouldReturn:"]);
        assert_eq!(text(), "a");
        assert_eq!(shown(), Some(("a".to_string(), [0.0, 0.0, 0.0, 1.0])));
        // From the phone's keyboard (Java's UTF-16): an emoji is one letter
        // (two UTF-16 units), typed and deleted whole.
        type_java_text(&"😀".encode_utf16().collect::<Vec<u16>>());
        assert_eq!(text(), "a😀");
        assert_eq!(heard(), ["change 1,0 \"😀\"", "changed"]);
        type_java_text(&[8]);
        assert_eq!(text(), "a");
        assert_eq!(heard(), ["change 1,2 \"\"", "changed"]);
        runtime::send(field, "setSecureTextEntry:", &[1]);
        assert_eq!(shown().unwrap().0, "•", "a password shows dots");

        assert!(runtime::send(field, "resignFirstResponder", &[]) & 0xFF != 0);
        assert_eq!(heard(), ["textFieldDidEndEditing:"]);
        assert_eq!(first_responder(), 0);
        type_text("z");
        assert_eq!(text(), "a", "nothing is being edited");
        // A disabled field can't be edited.
        runtime::send(field, "setEnabled:", &[0]);
        assert!(runtime::send(field, "becomeFirstResponder", &[]) & 0xFF == 0);
        assert!(heard().is_empty());
        assert_eq!(first_responder(), 0);
    }
}
