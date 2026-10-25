//! NSObject: the root of every class.

use crate::objc::runtime::{self, lock, Class, Id, Sel};
use crate::symbols::Registry;

extern "C" fn alloc(class: Class, _: Sel) -> Id {
    runtime::alloc_instance(class)
}

extern "C" fn alloc_with_zone(class: Class, _: Sel, _zone: usize) -> Id {
    runtime::alloc_instance(class)
}

extern "C" fn new(class: Class, _: Sel) -> Id {
    let object = runtime::send(class, "alloc", &[]);
    runtime::send(object, "init", &[])
}

extern "C" fn return_self(this: Id, _: Sel) -> Id {
    this
}

extern "C" fn nothing(_this: Id, _: Sel) {}

extern "C" fn retain_count(_this: Id, _: Sel) -> usize {
    1
}

extern "C" fn class_of_instance(this: Id, _: Sel) -> Class {
    lock().class_of(this)
}

extern "C" fn superclass_of_instance(this: Id, _: Sel) -> Class {
    let rt = lock();
    let class = rt.class_of(this);
    rt.classes.get(&class).map(|r| r.superclass).unwrap_or(0)
}

extern "C" fn superclass_of_class(this: Class, _: Sel) -> Class {
    lock().classes.get(&this).map(|r| r.superclass).unwrap_or(0)
}

extern "C" fn is_kind_of(this: Id, _: Sel, class: Class) -> bool {
    let rt = lock();
    rt.is_subclass(rt.class_of(this), class)
}

extern "C" fn is_member_of(this: Id, _: Sel, class: Class) -> bool {
    lock().class_of(this) == class
}

extern "C" fn is_subclass_of(this: Class, _: Sel, class: Class) -> bool {
    lock().is_subclass(this, class)
}

extern "C" fn responds_to(this: Id, _: Sel, selector: Sel) -> bool {
    let class = lock().class_of(this);
    lock().lookup(class, selector).is_some()
}

extern "C" fn instances_respond_to(this: Class, _: Sel, selector: Sel) -> bool {
    lock().lookup(this, selector).is_some()
}

extern "C" fn conforms_to(this: Id, _: Sel, protocol: usize) -> bool {
    let class = lock().class_of(this);
    let class = if lock().classes.get(&class).map(|r| r.meta).unwrap_or(false) { this } else { class };
    runtime::conforms(class, protocol)
}

extern "C" fn class_conforms_to(this: Class, _: Sel, protocol: usize) -> bool {
    runtime::conforms(this, protocol)
}

extern "C" fn hash(this: Id, _: Sel) -> usize {
    this
}

extern "C" fn is_equal(this: Id, _: Sel, other: Id) -> bool {
    this == other
}

extern "C" fn description(this: Id, _: Sel) -> Id {
    let name = {
        let rt = lock();
        let class = rt.class_of(this);
        rt.class_name(class)
    };
    crate::fw::string::make(&format!("<{name}: {this:#x}>"))
}

extern "C" fn class_description(this: Class, _: Sel) -> Id {
    let name = lock().class_name(this);
    crate::fw::string::make(&name)
}

/// Sends setValue:forKey: for each entry; NSNull stands for nil, as on Apple.
extern "C" fn set_values_for_keys(this: Id, _: Sel, values: Id) {
    let null = runtime::send(runtime::class_named("NSNull"), "null", &[]);
    for (key, value) in crate::fw::collections::dictionary_pairs(values) {
        let value = if value == null { 0 } else { value };
        runtime::send(this, "setValue:forKey:", &[value, key]);
    }
}

extern "C" fn copy(this: Id, _: Sel) -> Id {
    runtime::send(this, "copyWithZone:", &[0])
}

extern "C" fn mutable_copy(this: Id, _: Sel) -> Id {
    runtime::send(this, "mutableCopyWithZone:", &[0])
}

extern "C" fn perform(this: Id, _: Sel, selector: Sel) -> Id {
    let class = lock().class_of(this);
    let found = lock().lookup(class, selector);
    match found {
        Some(imp) => unsafe { std::mem::transmute::<usize, extern "C" fn(Id, Sel) -> Id>(imp)(this, selector) },
        None => {
            let name = lock().sel_name(selector);
            runtime::send(this, &name, &[])
        }
    }
}

extern "C" fn perform_with(this: Id, _: Sel, selector: Sel, a: Id) -> Id {
    let name = lock().sel_name(selector);
    runtime::send(this, &name, &[a])
}

extern "C" fn perform_with_with(this: Id, _: Sel, selector: Sel, a: Id, b: Id) -> Id {
    let name = lock().sel_name(selector);
    runtime::send(this, &name, &[a, b])
}

extern "C" fn dealloc(this: Id, _: Sel) {
    // Memory is not given back yet (reference counting comes later).
    let _ = this;
}

// ----- key-value observing: observers are kept; changes are sent by kvo_notify -----

#[derive(Clone)]
struct KeyObserver {
    object: Id,
    key: String,
    observer: Id,
    context: usize,
}

static KEY_OBSERVERS: std::sync::Mutex<Vec<KeyObserver>> = std::sync::Mutex::new(Vec::new());

extern "C" fn add_key_observer(this: Id, _: Sel, observer: Id, key: Id, _options: usize, context: usize) {
    let key = crate::fw::string::get(key);
    KEY_OBSERVERS.lock().unwrap().push(KeyObserver { object: this, key, observer, context });
}

extern "C" fn remove_key_observer(this: Id, _: Sel, observer: Id, key: Id) {
    let key = crate::fw::string::get(key);
    KEY_OBSERVERS.lock().unwrap().retain(|o| !(o.object == this && o.observer == observer && o.key == key));
}

extern "C" fn remove_key_observer_context(this: Id, s: Sel, observer: Id, key: Id, _context: usize) {
    remove_key_observer(this, s, observer, key);
}

/// Tells observers of `key` on `object` that it now holds `value`.
pub fn kvo_notify(object: Id, key: &str, value: Id) {
    let list: Vec<KeyObserver> = KEY_OBSERVERS.lock().unwrap().iter().filter(|o| o.object == object && o.key == key).cloned().collect();
    for o in list {
        let change = crate::fw::collections::dictionary_from(&[
            (crate::fw::string::make("new"), value),
            (crate::fw::string::make("kind"), crate::fw::collections::number_int(1)),
        ]);
        runtime::send(o.observer, "observeValueForKeyPath:ofObject:change:context:", &[crate::fw::string::make(key), object, change, o.context]);
    }
}

pub fn define(_: &mut Registry) -> Class {
    let class = lock().define_class("NSObject", 0, 8);
    let m = |name: &str, imp: usize| {
        let mut rt = lock();
        let s = rt.sel(name);
        rt.add_method(class, s, imp);
    };
    m("init", return_self as usize);
    m("self", return_self as usize);
    m("retain", return_self as usize);
    m("autorelease", return_self as usize);
    m("release", nothing as usize);
    m("dealloc", dealloc as usize);
    m("retainCount", retain_count as usize);
    m("class", class_of_instance as usize);
    m("superclass", superclass_of_instance as usize);
    m("isKindOfClass:", is_kind_of as usize);
    m("isMemberOfClass:", is_member_of as usize);
    m("respondsToSelector:", responds_to as usize);
    m("conformsToProtocol:", conforms_to as usize);
    m("hash", hash as usize);
    m("isEqual:", is_equal as usize);
    m("description", description as usize);
    m("debugDescription", description as usize);
    m("copy", copy as usize);
    m("setValuesForKeysWithDictionary:", set_values_for_keys as usize);
    m("mutableCopy", mutable_copy as usize);
    m("performSelector:", perform as usize);
    m("performSelector:withObject:", perform_with as usize);
    m("performSelector:withObject:withObject:", perform_with_with as usize);
    m("zone", hash_zero as usize);
    m("addObserver:forKeyPath:options:context:", add_key_observer as usize);
    m("removeObserver:forKeyPath:", remove_key_observer as usize);
    m("removeObserver:forKeyPath:context:", remove_key_observer_context as usize);
    m("willChangeValueForKey:", ignore_one as usize);
    m("didChangeValueForKey:", ignore_one as usize);
    m("valueForKey:", value_for_key as usize);
    m("setValue:forKey:", set_value_for_key as usize);
    m("valueForKeyPath:", value_for_key_path as usize);
    m("setValue:forKeyPath:", set_value_for_key_path as usize);
    m("dictionaryWithValuesForKeys:", dictionary_with_values_for_keys as usize);
    m("valueForUndefinedKey:", value_for_undefined_key as usize);
    m("setValue:forUndefinedKey:", set_value_for_undefined_key as usize);
    m("setNilValueForKey:", set_nil_value_for_key as usize);
    m("doesNotRecognizeSelector:", does_not_recognize as usize);
    let meta = lock().classes[&class].partner;
    let c = |name: &str, imp: usize| {
        let mut rt = lock();
        let s = rt.sel(name);
        rt.add_method(meta, s, imp);
    };
    c("alloc", alloc as usize);
    c("allocWithZone:", alloc_with_zone as usize);
    c("new", new as usize);
    c("class", return_self as usize);
    c("superclass", superclass_of_class as usize);
    c("initialize", nothing as usize);
    c("load", nothing as usize);
    c("isSubclassOfClass:", is_subclass_of as usize);
    c("instancesRespondToSelector:", instances_respond_to as usize);
    c("conformsToProtocol:", class_conforms_to as usize);
    c("description", class_description as usize);
    c("copyWithZone:", return_self_with as usize);
    class
}

extern "C" fn ignore_one(_this: Id, _: Sel, _value: usize) {}

extern "C" fn hash_zero(_this: Id, _: Sel) -> usize {
    0
}

extern "C" fn return_self_with(this: Id, _: Sel, _a: usize) -> Id {
    this
}

/// Adds instance methods to one of our classes.
pub fn methods(class: Class, list: &[(&str, usize)]) {
    let mut rt = lock();
    for (name, imp) in list {
        let s = rt.sel(name);
        rt.add_method(class, s, *imp);
    }
}

/// Adds class methods to one of our classes.
pub fn class_methods(class: Class, list: &[(&str, usize)]) {
    let mut rt = lock();
    let meta = rt.classes[&class].partner;
    for (name, imp) in list {
        let s = rt.sel(name);
        rt.add_method(meta, s, *imp);
    }
}

/// Method types the host's own tables don't have (a 32-bit game's methods,
/// known to its bridge): `class` (a metaclass for class methods) and the
/// selector to an encoding as the host receives the values.
type TypesHook = fn(Class, Sel) -> Option<String>;

static EXTRA_TYPES: std::sync::RwLock<Option<TypesHook>> = std::sync::RwLock::new(None);

pub fn set_extra_method_types(hook: TypesHook) {
    *EXTRA_TYPES.write().unwrap() = Some(hook);
}

/// -doesNotRecognizeSelector: raises NSInvalidArgumentException, as Apple's does.
extern "C" fn does_not_recognize(this: Id, _: Sel, selector: Sel) {
    let name = lock().sel_name(selector);
    crate::fw::exception::raise(unrecognized_selector_exception(this, &name));
}

/// The NSInvalidArgumentException Apple raises for `selector` sent to `this`.
pub fn unrecognized_selector_exception(this: Id, selector: &str) -> Id {
    let (class_name, meta) = {
        let rt = lock();
        let class = rt.class_of(this);
        (rt.class_name(class), rt.classes.get(&class).is_some_and(|r| r.meta))
    };
    let reason = format!("{}[{class_name} {selector}]: unrecognized selector sent to {} {this:#x}", if meta { "+" } else { "-" }, if meta { "class" } else { "instance" });
    runtime::send(
        runtime::class_named("NSException"),
        "exceptionWithName:reason:userInfo:",
        &[crate::fw::string::make("NSInvalidArgumentException"), crate::fw::string::make(&reason), 0],
    )
}

// ----- key-value coding -----

/// The encoding of the method `receiver` answers `sel` with: the bridge's
/// (a 32-bit game's method) first, else the host's own tables.
fn types_of(receiver: Id, sel: Sel) -> Option<String> {
    let class = lock().class_of(receiver);
    let owner = lock().owner_of(class, sel)?;
    let hook = *EXTRA_TYPES.read().unwrap();
    hook.and_then(|h| h(owner, sel)).or_else(|| crate::fw::invocation::method_types(class, sel))
}

/// The type codes of an encoding, one per value (return type first),
/// without qualifiers and offsets: "v24@0:8q16" -> ["v", "@", ":", "q"].
pub fn type_codes(types: &str) -> Vec<String> {
    fn skip(b: &[u8], i: usize) -> usize {
        match b.get(i) {
            None => i,
            Some(b'^') => skip(b, i + 1),
            Some(b'@') => match b.get(i + 1) {
                Some(b'?') => i + 2,
                Some(b'"') => b[i + 2..].iter().position(|&c| c == b'"').map_or(b.len(), |end| i + 3 + end),
                _ => i + 1,
            },
            Some(b'{' | b'(' | b'[') => {
                let mut depth = 0;
                let mut j = i;
                while j < b.len() {
                    match b[j] {
                        b'{' | b'(' | b'[' => depth += 1,
                        b'}' | b')' | b']' => {
                            depth -= 1;
                            if depth == 0 {
                                return j + 1;
                            }
                        }
                        b'"' => j += b[j + 1..].iter().position(|&c| c == b'"').map_or(b.len(), |end| end + 1),
                        _ => {}
                    }
                    j += 1;
                }
                b.len()
            }
            Some(b'b') => i + 1 + b[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count(),
            Some(_) => i + 1,
        }
    }
    let b = types.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && b"rnNoORV".contains(&b[i]) {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        let start = i;
        i = skip(b, i).min(b.len());
        out.push(types[start..i].to_string());
        while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'-') {
            i += 1;
        }
    }
    out
}

/// An encoding with every float code made a double, struct and field names
/// left alone: "{CGAffineTransform=ffffff}" -> "{CGAffineTransform=dddddd}".
pub fn float_as_double(types: &str) -> String {
    let mut out = String::with_capacity(types.len());
    let mut chars = types.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(match c {
            'f' => 'd',
            _ => c,
        });
        let end = match c {
            '{' | '(' => '=',
            '"' => '"',
            _ => continue,
        };
        // A name: copied as written, up to its end mark.
        for n in chars.by_ref() {
            out.push(n);
            if n == end || (end == '=' && matches!(n, '}' | ')')) {
                break;
            }
        }
    }
    out
}

fn capitalized(key: &str) -> String {
    let mut chars = key.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Calls getter `sel` and boxes its result by its return type (numbers in
/// NSNumber, geometry in NSValue; an unknown type is taken as an object).
fn boxed_result(receiver: Id, sel: Sel) -> Id {
    use crate::fw::collections::{number_bool, number_float, number_int, value_with, POINT_TYPE, RANGE_TYPE, RECT_TYPE, SIZE_TYPE, TRANSFORM_TYPE};
    use crate::fw::geometry::{Point, Rect, Size, Transform};
    let Some(imp) = runtime::imp_for(receiver, sel) else { return 0 };
    let code = types_of(receiver, sel).and_then(|t| type_codes(&t).into_iter().next()).unwrap_or_else(|| "@".into());
    let word = || unsafe { std::mem::transmute::<usize, extern "C" fn(Id, Sel) -> u64>(imp)(receiver, sel) };
    match code.as_bytes()[0] {
        b'c' => number_int(word() as i8 as i64),
        b'C' => number_int(word() as u8 as i64),
        b's' => number_int(word() as i16 as i64),
        b'S' => number_int(word() as u16 as i64),
        b'i' | b'l' => number_int(word() as i32 as i64),
        b'I' | b'L' => number_int(word() as u32 as i64),
        b'q' | b'Q' => number_int(word() as i64),
        b'B' => number_bool(word() & 0xFF != 0),
        b'f' => number_float(unsafe { std::mem::transmute::<usize, extern "C" fn(Id, Sel) -> f32>(imp)(receiver, sel) } as f64),
        b'd' => number_float(unsafe { std::mem::transmute::<usize, extern "C" fn(Id, Sel) -> f64>(imp)(receiver, sel) }),
        b'v' => 0,
        b'{' => {
            let call = |kind: &str| -> Id {
                unsafe {
                    match kind {
                        POINT_TYPE => value_with(kind, &std::mem::transmute::<usize, extern "C" fn(Id, Sel) -> Point>(imp)(receiver, sel)),
                        SIZE_TYPE => value_with(kind, &std::mem::transmute::<usize, extern "C" fn(Id, Sel) -> Size>(imp)(receiver, sel)),
                        RECT_TYPE => value_with(kind, &std::mem::transmute::<usize, extern "C" fn(Id, Sel) -> Rect>(imp)(receiver, sel)),
                        TRANSFORM_TYPE => value_with(kind, &std::mem::transmute::<usize, extern "C" fn(Id, Sel) -> Transform>(imp)(receiver, sel)),
                        _ => value_with(kind, &std::mem::transmute::<usize, extern "C" fn(Id, Sel) -> crate::fw::string::Range>(imp)(receiver, sel)),
                    }
                }
            };
            // The same struct in either side's spelling (CGFloat float or double).
            let host = float_as_double(&code).replace("{_NSRange=II}", RANGE_TYPE).replace("{_NSRange=QQ}", RANGE_TYPE);
            match host.as_str() {
                POINT_TYPE | SIZE_TYPE | RECT_TYPE | TRANSFORM_TYPE | RANGE_TYPE => call(&host),
                _ => {
                    crate::log!("valueForKey: a {code} result can't be boxed; nil");
                    0
                }
            }
        }
        _ => word() as Id,
    }
}

/// Apple's order: get<Key>, <key>, is<Key>, _<key>.
fn getter(receiver: Id, key: &str) -> Option<Sel> {
    let cap = capitalized(key);
    [format!("get{cap}"), key.to_string(), format!("is{cap}"), format!("_{key}")]
        .into_iter()
        .find(|name| !name.is_empty() && runtime::responds(receiver, name))
        .map(|name| runtime::sel(&name))
}

/// The instance variable key-value coding falls back to without accessor
/// methods, as Apple's does: _key, _isKey, key, isKey. Its address and type.
/// A field of an object: where it is, its type, and whether it is in a
/// 32-bit game's memory (4-byte long and pointers, 4-byte alignment).
pub struct Field {
    pub at: usize,
    pub kind: String,
    pub guest: bool,
}

/// Fields the host's class tables don't describe (a 32-bit game's objects,
/// known to its bridge): the named field of an object, if it has one.
type FieldsHook = fn(Id, &str) -> Option<Field>;

static EXTRA_FIELDS: std::sync::RwLock<Option<FieldsHook>> = std::sync::RwLock::new(None);

pub fn set_extra_fields(hook: FieldsHook) {
    *EXTRA_FIELDS.write().unwrap() = Some(hook);
}

fn field_for(receiver: Id, key: &str) -> Option<Field> {
    let class = lock().class_of(receiver);
    let cap = capitalized(key);
    let hook = *EXTRA_FIELDS.read().unwrap();
    for name in [format!("_{key}"), format!("_is{cap}"), key.to_string(), format!("is{cap}")] {
        if let Some(field) = hook.and_then(|h| h(receiver, &name)) {
            return Some(field);
        }
        let Ok(name) = std::ffi::CString::new(name) else { continue };
        let ivar = crate::objc::reflect::get_instance_variable(class, name.as_ptr());
        if ivar == 0 {
            continue;
        }
        // ivar_t { int32_t *offset; const char *name; const char *type; ... }
        unsafe {
            let offset_cell = *(ivar as *const usize);
            let offset = if offset_cell == 0 { 0 } else { *(offset_cell as *const i32) as isize };
            let types = *((ivar + 16) as *const *const libc::c_char);
            let kind = if types.is_null() { "@".to_string() } else { std::ffi::CStr::from_ptr(types).to_string_lossy().into_owned() };
            return Some(Field { at: (receiver as isize + offset) as usize, kind, guest: false });
        }
    }
    None
}

fn read_field(field: &Field) -> Option<Id> {
    use crate::fw::collections::{number_bool, number_float, number_int};
    use std::ptr::read_unaligned as read;
    let at = field.at;
    let objects = field.guest.then(crate::fw::system::guest_memory).flatten();
    unsafe {
        Some(match field.kind.as_bytes().first()? {
            b'@' | b'#' => match objects {
                Some(memory) => (memory.object_to_host)(read(at as *const u32)),
                None if field.guest => return None,
                None => read(at as *const Id),
            },
            b'c' => number_int(read(at as *const i8) as i64),
            b'C' => number_int(read(at as *const u8) as i64),
            b'B' => number_bool(read(at as *const u8) != 0),
            b's' => number_int(read(at as *const i16) as i64),
            b'S' => number_int(read(at as *const u16) as i64),
            b'i' => number_int(read(at as *const i32) as i64),
            b'I' => number_int(read(at as *const u32) as i64),
            // A 32-bit game's long is 4 bytes.
            b'l' if field.guest => number_int(read(at as *const i32) as i64),
            b'L' if field.guest => number_int(read(at as *const u32) as i64),
            b'l' | b'q' | b'L' | b'Q' => number_int(read(at as *const i64)),
            b'f' => number_float(read(at as *const f32) as f64),
            b'd' => number_float(read(at as *const f64)),
            _ => return None,
        })
    }
}

/// Stores `value` in a field: objects retained (the old one released),
/// numbers unboxed. False for a type key-value coding can't fill.
fn write_field(field: &Field, value: Id) -> bool {
    use crate::fw::collections::{is_number, number_as_f64, number_as_i64};
    use std::ptr::{read_unaligned as read, write_unaligned as write};
    let at = field.at;
    let integer = || if is_number(value) { number_as_i64(value) } else { crate::fw::string::get(value).trim().parse().unwrap_or(0) };
    let real = || if is_number(value) { number_as_f64(value) } else { crate::fw::string::get(value).trim().parse().unwrap_or(0.0) };
    let objects = field.guest.then(crate::fw::system::guest_memory).flatten();
    unsafe {
        match field.kind.as_bytes().first() {
            Some(b'@') => {
                let new = if value == 0 { 0 } else { runtime::send(value, "retain", &[]) };
                let old = match objects {
                    Some(memory) => {
                        let old = (memory.object_to_host)(read(at as *const u32));
                        write(at as *mut u32, (memory.object_to_guest)(new));
                        old
                    }
                    None if field.guest => return false,
                    None => {
                        let old = read(at as *const Id);
                        write(at as *mut Id, new);
                        old
                    }
                };
                if old != 0 {
                    runtime::send(old, "release", &[]);
                }
            }
            Some(b'c' | b'C' | b'B') => write(at as *mut u8, integer() as u8),
            Some(b's' | b'S') => write(at as *mut u16, integer() as u16),
            Some(b'i' | b'I') => write(at as *mut u32, integer() as u32),
            Some(b'l' | b'L') if field.guest => write(at as *mut u32, integer() as u32),
            Some(b'l' | b'q' | b'L' | b'Q') => write(at as *mut i64, integer()),
            Some(b'f') => write(at as *mut f32, real() as f32),
            Some(b'd') => write(at as *mut f64, real()),
            _ => return false,
        }
    }
    true
}

extern "C" fn value_for_key(this: Id, _: Sel, key: Id) -> Id {
    let name = crate::fw::string::get(key);
    if let Some(sel) = getter(this, &name) {
        return boxed_result(this, sel);
    }
    match field_for(this, &name).and_then(|field| read_field(&field)) {
        Some(value) => value,
        None => runtime::send(this, "valueForUndefinedKey:", &[key]),
    }
}

/// Apple's NSUnknownKeyException for `key` of `this`, raised from `method`.
fn raise_unknown_key(this: Id, method: &str, key: Id) {
    let class = crate::fw::string::describe_class_of(this);
    let key_text = crate::fw::string::get(key);
    crate::log!("{method} {class} has no key {key_text}; raising {}", crate::fw::exception::UNDEFINED_KEY);
    let reason = format!("[<{class} {this:#x}> {method}]: this class is not key value coding-compliant for the key {key_text}.");
    let info = crate::fw::collections::dictionary_from(&[
        (crate::fw::string::make("NSTargetObjectUserInfoKey"), this),
        (crate::fw::string::make("NSUnknownUserInfoKey"), key),
    ]);
    let exception = runtime::send(
        runtime::class_named("NSException"),
        "exceptionWithName:reason:userInfo:",
        &[crate::fw::string::make(crate::fw::exception::UNDEFINED_KEY), crate::fw::string::make(&reason), info],
    );
    crate::fw::exception::raise(exception);
}

extern "C" fn value_for_undefined_key(this: Id, _: Sel, key: Id) -> Id {
    raise_unknown_key(this, "valueForUndefinedKey:", key);
    0
}

extern "C" fn set_value_for_undefined_key(this: Id, _: Sel, _value: Id, key: Id) {
    raise_unknown_key(this, "setValue:forUndefinedKey:", key);
}

extern "C" fn set_nil_value_for_key(this: Id, _: Sel, key: Id) {
    crate::log!("setValue:forKey: nil for {}'s number key {}; ignored", crate::fw::string::describe_class_of(this), crate::fw::string::get(key));
}

/// Calls setter `sel` with `value` unboxed for its argument type.
fn call_setter(receiver: Id, sel: Sel, value: Id, key: Id) {
    use crate::fw::collections::{is_number, number_as_f64, number_as_i64, value_struct};
    use crate::fw::geometry::{Point, Rect, Size};
    let Some(imp) = runtime::imp_for(receiver, sel) else { return };
    let code = types_of(receiver, sel).and_then(|t| type_codes(&t).get(3).cloned()).unwrap_or_else(|| "@".into());
    let first = code.as_bytes()[0];
    if value == 0 && !matches!(first, b'@' | b'#') {
        runtime::send(receiver, "setNilValueForKey:", &[key]);
        return;
    }
    let number = |v: Id| if is_number(v) { number_as_f64(v) } else { crate::fw::string::get(v).trim().parse().unwrap_or(0.0) };
    let integer = |v: Id| if is_number(v) { number_as_i64(v) } else { number(v) as i64 };
    unsafe {
        match first {
            b'f' => std::mem::transmute::<usize, extern "C" fn(Id, Sel, f32)>(imp)(receiver, sel, number(value) as f32),
            b'd' => std::mem::transmute::<usize, extern "C" fn(Id, Sel, f64)>(imp)(receiver, sel, number(value)),
            b'c' | b'C' | b's' | b'S' | b'i' | b'I' | b'l' | b'L' | b'q' | b'Q' | b'B' => {
                std::mem::transmute::<usize, extern "C" fn(Id, Sel, i64)>(imp)(receiver, sel, integer(value))
            }
            b'{' => match float_as_double(&code).as_str() {
                crate::fw::collections::POINT_TYPE => std::mem::transmute::<usize, extern "C" fn(Id, Sel, Point)>(imp)(receiver, sel, value_struct(value)),
                crate::fw::collections::SIZE_TYPE => std::mem::transmute::<usize, extern "C" fn(Id, Sel, Size)>(imp)(receiver, sel, value_struct(value)),
                crate::fw::collections::RECT_TYPE => std::mem::transmute::<usize, extern "C" fn(Id, Sel, Rect)>(imp)(receiver, sel, value_struct(value)),
                _ => crate::log!("setValue:forKey: a {code} argument can't be unboxed; ignored"),
            },
            _ => std::mem::transmute::<usize, extern "C" fn(Id, Sel, Id)>(imp)(receiver, sel, value),
        }
    }
}

extern "C" fn set_value_for_key(this: Id, _: Sel, value: Id, key: Id) {
    let name = crate::fw::string::get(key);
    let cap = capitalized(&name);
    match [format!("set{cap}:"), format!("_set{cap}:")].into_iter().find(|n| runtime::responds(this, n)) {
        Some(setter) => call_setter(this, runtime::sel(&setter), value, key),
        None => match field_for(this, &name) {
            Some(field) if value == 0 && !field.kind.starts_with('@') => {
                runtime::send(this, "setNilValueForKey:", &[key]);
            }
            Some(field) if write_field(&field, value) => {}
            _ => {
                runtime::send(this, "setValue:forUndefinedKey:", &[value, key]);
            }
        },
    }
}

/// The members of a collection (arrays, sets), None for anything else.
fn members(object: Id) -> Option<Vec<Id>> {
    if crate::fw::collections::is_array(object) {
        return Some(crate::fw::collections::array_items(object));
    }
    let set = runtime::class_named("NSSet");
    (set != 0 && is_kind_of(object, 0, set)).then(|| crate::fw::collections::array_items(runtime::send(object, "allObjects", &[])))
}

/// @count, @sum, @avg, @max, @min, @unionOfObjects and
/// @distinctUnionOfObjects over a collection's members' `rest` key path.
fn collection_operator(list: Vec<Id>, operator: &str, rest: Option<&str>) -> Id {
    use crate::fw::collections::{is_number, number_as_f64, number_float, number_int};
    let null = runtime::send(runtime::class_named("NSNull"), "null", &[]);
    let values = || -> Vec<Id> {
        list.iter()
            .map(|&item| match rest {
                Some(path) => runtime::send(item, "valueForKeyPath:", &[crate::fw::string::make(path)]),
                None => item,
            })
            .filter(|&v| v != 0 && v != null)
            .collect()
    };
    let numbers = || -> Vec<f64> { values().into_iter().map(|v| if is_number(v) { number_as_f64(v) } else { crate::fw::string::get(v).trim().parse().unwrap_or(0.0) }).collect() };
    let whole = |total: f64| if total.fract() == 0.0 && total.abs() < 9.0e15 { number_int(total as i64) } else { number_float(total) };
    match operator {
        // Apple ignores a key path after @count.
        "@count" => number_int(list.len() as i64),
        "@sum" => whole(numbers().iter().sum()),
        "@avg" => {
            let all = numbers();
            if all.is_empty() { 0 } else { number_float(all.iter().sum::<f64>() / all.len() as f64) }
        }
        "@max" | "@min" => {
            let want = if operator == "@max" { std::cmp::Ordering::Greater } else { std::cmp::Ordering::Less };
            values().into_iter().fold(0, |best, v| {
                if best == 0 || crate::fw::predicate::compare_values(v, best, 0) == Some(want) { v } else { best }
            })
        }
        "@unionOfObjects" => crate::fw::collections::array_from(&values()),
        "@distinctUnionOfObjects" => {
            let mut kept: Vec<Id> = Vec::new();
            for v in values() {
                if !kept.iter().any(|&k| k == v || runtime::send(k, "isEqual:", &[v]) & 0xFF != 0) {
                    kept.push(v);
                }
            }
            crate::fw::collections::array_from(&kept)
        }
        other => {
            crate::log!("valueForKeyPath: collection operator {other} not known; nil");
            0
        }
    }
}

extern "C" fn value_for_key_path(this: Id, _: Sel, path: Id) -> Id {
    let text = crate::fw::string::get(path);
    if text.starts_with('@') {
        if let Some(list) = members(this) {
            let (name, rest) = match text.split_once('.') {
                Some((name, rest)) => (name, Some(rest)),
                None => (text.as_str(), None),
            };
            return collection_operator(list, name, rest);
        }
    }
    match text.split_once('.') {
        None => runtime::send(this, "valueForKey:", &[path]),
        Some((first, rest)) => {
            let value = runtime::send(this, "valueForKey:", &[crate::fw::string::make(first)]);
            if value == 0 { 0 } else { runtime::send(value, "valueForKeyPath:", &[crate::fw::string::make(rest)]) }
        }
    }
}

extern "C" fn set_value_for_key_path(this: Id, _: Sel, value: Id, path: Id) {
    let text = crate::fw::string::get(path);
    match text.rsplit_once('.') {
        None => {
            runtime::send(this, "setValue:forKey:", &[value, path]);
        }
        Some((owner, key)) => {
            let target = runtime::send(this, "valueForKeyPath:", &[crate::fw::string::make(owner)]);
            if target != 0 {
                runtime::send(target, "setValue:forKey:", &[value, crate::fw::string::make(key)]);
            }
        }
    }
}

/// Each key's value; nil ones as NSNull.
extern "C" fn dictionary_with_values_for_keys(this: Id, _: Sel, keys: Id) -> Id {
    let null = runtime::send(runtime::class_named("NSNull"), "null", &[]);
    let pairs: Vec<(Id, Id)> = crate::fw::collections::array_items(keys)
        .into_iter()
        .map(|key| {
            let value = runtime::send(this, "valueForKey:", &[key]);
            (key, if value == 0 { null } else { value })
        })
        .collect();
    crate::fw::collections::dictionary_from(&pairs)
}

/// An array's valueForKey: is each member's value (nil as NSNull).
extern "C" fn array_value_for_key(this: Id, _: Sel, key: Id) -> Id {
    let null = runtime::send(runtime::class_named("NSNull"), "null", &[]);
    let values: Vec<Id> = crate::fw::collections::array_items(this)
        .into_iter()
        .map(|item| match runtime::send(item, "valueForKey:", &[key]) {
            0 => null,
            v => v,
        })
        .collect();
    crate::fw::collections::array_from(&values)
}

extern "C" fn array_set_value_for_key(this: Id, _: Sel, value: Id, key: Id) {
    for item in crate::fw::collections::array_items(this) {
        runtime::send(item, "setValue:forKey:", &[value, key]);
    }
}

/// A set's valueForKey: is the set of its members' values (nils left out).
extern "C" fn set_members_value_for_key(this: Id, _: Sel, key: Id) -> Id {
    let values: Vec<Id> = crate::fw::collections::array_items(runtime::send(this, "allObjects", &[]))
        .into_iter()
        .map(|item| runtime::send(item, "valueForKey:", &[key]))
        .filter(|&v| v != 0)
        .collect();
    crate::fw::misc::set_from(&values)
}

/// Key-value coding on collections (after NSArray and NSSet exist).
pub fn register_collection_coding() {
    methods(
        runtime::class_named("NSArray"),
        &[("valueForKey:", array_value_for_key as usize), ("setValue:forKey:", array_set_value_for_key as usize)],
    );
    methods(runtime::class_named("NSSet"), &[("valueForKey:", set_members_value_for_key as usize)]);
}

pub fn define_class(name: &str, parent: &str, instance_size: usize) -> Class {
    let mut rt = lock();
    let parent = rt.by_name.get(parent).copied().unwrap_or_else(|| panic!("parent class {parent} not defined yet"));
    rt.define_class(name, parent, instance_size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fw::{collections, string};

    extern "C" fn thing_title(this: Id, _: Sel) -> Id {
        crate::fw::uikit::property_get(this, "title").unwrap_or(0) as Id
    }
    extern "C" fn thing_set_title(this: Id, _: Sel, title: Id) {
        crate::fw::uikit::property_set(this, "title", title as u64);
    }
    extern "C" fn thing_count(this: Id, _: Sel) -> i64 {
        crate::fw::uikit::property_get(this, "count").unwrap_or(0) as i64
    }
    extern "C" fn thing_set_count(this: Id, _: Sel, count: i64) {
        crate::fw::uikit::property_set(this, "count", count as u64);
    }
    extern "C" fn thing_ratio(this: Id, _: Sel) -> f64 {
        crate::fw::uikit::property_get(this, "ratio").map(f64::from_bits).unwrap_or(0.0)
    }
    extern "C" fn thing_set_ratio(this: Id, _: Sel, ratio: f64) {
        crate::fw::uikit::property_set(this, "ratio", ratio.to_bits());
    }
    extern "C" fn thing_child(this: Id, _: Sel) -> Id {
        crate::fw::uikit::property_get(this, "child").unwrap_or(0) as Id
    }
    extern "C" fn thing_nothing(_this: Id, _: Sel) -> Id {
        0
    }
    extern "C" fn thing_label(_this: Id, _: Sel) -> Id {
        string::make("plain")
    }
    extern "C" fn thing_get_label(_this: Id, _: Sel) -> Id {
        string::make("get")
    }

    /// Types for the test class's methods, as a 32-bit game's would come.
    fn test_types(class: Class, sel: Sel) -> Option<String> {
        let (class_name, name) = { let rt = lock(); (rt.class_name(class), rt.sel_name(sel)) };
        if class_name != "KvcTestThing" {
            return None;
        }
        Some(match name.as_str() {
            "count" => "q@:",
            "setCount:" => "v@:q",
            "ratio" => "d@:",
            "setRatio:" => "v@:d",
            _ => "@@:",
        }.to_string())
    }

    fn thing(title: &str, count: i64, child: Id) -> Id {
        static CLASS: std::sync::OnceLock<Class> = std::sync::OnceLock::new();
        let class = *CLASS.get_or_init(|| {
            let class = define_class("KvcTestThing", "NSObject", 8);
            methods(
                class,
                &[
                    ("title", thing_title as usize),
                    ("setTitle:", thing_set_title as usize),
                    ("count", thing_count as usize),
                    ("setCount:", thing_set_count as usize),
                    ("ratio", thing_ratio as usize),
                    ("setRatio:", thing_set_ratio as usize),
                    ("child", thing_child as usize),
                    ("nothing", thing_nothing as usize),
                    ("label", thing_label as usize),
                    ("getLabel", thing_get_label as usize),
                ],
            );
            set_extra_method_types(test_types);
            class
        });
        let object = runtime::alloc_instance(class);
        crate::fw::uikit::property_set(object, "title", string::make(title) as u64);
        crate::fw::uikit::property_set(object, "count", count as u64);
        crate::fw::uikit::property_set(object, "child", child as u64);
        object
    }

    /// Key-value coding: getters found by name and their results boxed by
    /// the method's type, key paths through objects and dictionaries,
    /// setters fed unboxed values, and the collection operators.
    #[test]
    fn key_paths_reach_through_objects_and_collections() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let s = string::make;
        let inner = thing("inner", 2, 0);
        let outer = thing("outer", 12, inner);
        crate::fw::uikit::property_set(outer, "ratio", 0.5f64.to_bits());
        assert_eq!(string::get(runtime::send(outer, "valueForKey:", &[s("title")])), "outer");
        let count = runtime::send(outer, "valueForKey:", &[s("count")]);
        assert!(collections::is_number(count) && collections::number_as_i64(count) == 12, "count boxed as a number");
        assert_eq!(collections::number_as_f64(runtime::send(outer, "valueForKey:", &[s("ratio")])), 0.5);
        assert_eq!(string::get(runtime::send(outer, "valueForKeyPath:", &[s("child.title")])), "inner");
        runtime::send(outer, "setValue:forKey:", &[collections::number_int(-3), s("count")]);
        assert_eq!(thing_count(outer, 0), -3);
        runtime::send(outer, "setValue:forKey:", &[collections::number_float(0.25), s("ratio")]);
        assert_eq!(thing_ratio(outer, 0), 0.25);
        runtime::send(outer, "setValue:forKeyPath:", &[s("renamed"), s("child.title")]);
        assert_eq!(string::get(thing_title(inner, 0)), "renamed");
        let values = runtime::send(outer, "dictionaryWithValuesForKeys:", &[collections::array_from(&[s("title"), s("nothing")])]);
        let null = runtime::send(runtime::class_named("NSNull"), "null", &[]);
        assert_eq!(collections::dictionary_get(values, s("nothing")), null, "nil comes back as NSNull");

        let list = collections::array_from(&[thing("a", 1, 0), thing("b", 5, 0), thing("c", 3, 0)]);
        let number = |path: &str| collections::number_as_f64(runtime::send(list, "valueForKeyPath:", &[s(path)]));
        assert_eq!(number("@sum.count"), 9.0);
        assert_eq!(number("@max.count"), 5.0);
        assert_eq!(number("@min.count"), 1.0);
        assert_eq!(number("@avg.count"), 3.0);
        assert_eq!(number("@count"), 3.0);
        let titles: Vec<String> = collections::array_items(runtime::send(list, "valueForKey:", &[s("title")])).into_iter().map(string::get).collect();
        assert_eq!(titles, ["a", "b", "c"]);
        let nested = collections::dictionary_from(&[(s("a"), collections::dictionary_from(&[(s("b"), s("deep"))]))]);
        assert_eq!(string::get(runtime::send(nested, "valueForKeyPath:", &[s("a.b")])), "deep");
    }

    /// Apple's search order (get<Key> first), @count ignoring the path after
    /// it, and an unknown key raising NSUnknownKeyException for the game.
    #[test]
    fn key_value_coding_follows_apples_rules() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let s = string::make;
        let one = thing("one", 1, 0);
        assert_eq!(string::get(runtime::send(one, "valueForKey:", &[s("label")])), "get");
        let list = collections::array_from(&[thing("a", 1, 0), thing("b", 2, 0)]);
        assert_eq!(collections::number_as_i64(runtime::send(list, "valueForKeyPath:", &[s("@count.child")])), 2);
        for (raised, what) in [
            (crate::fw::exception::catching(|| runtime::send(one, "valueForKey:", &[s("nope")])).1, "get"),
            (crate::fw::exception::catching(|| runtime::send(one, "setValue:forKey:", &[s("x"), s("nope")])).1, "set"),
        ] {
            let raised = raised.unwrap_or_else(|| panic!("{what} of an unknown key raised nothing"));
            // Apple: the constant NSUndefinedKeyException is @"NSUnknownKeyException".
            let (constant, _) = crate::symbols::lookup("NSUndefinedKeyException").expect("NSUndefinedKeyException exported");
            let constant = string::get(unsafe { *(constant as *const Id) });
            assert_eq!(constant, "NSUnknownKeyException");
            assert_eq!(string::get(runtime::send(raised, "name", &[])), constant);
            assert!(string::get(runtime::send(raised, "reason", &[])).contains("not key value coding-compliant for the key nope"));
        }
        // A raise ends the host method: members after the one that raised are left alone.
        let plain = runtime::send(runtime::class_named("NSObject"), "new", &[]);
        let after = thing("after", 1, 0);
        let mixed = collections::array_from(&[plain, after]);
        let (_, raised) = crate::fw::exception::catching(|| runtime::send(mixed, "setValue:forKey:", &[s("changed"), s("title")]));
        assert!(raised.is_some(), "the plain object has no title");
        assert_eq!(string::get(thing_title(after, 0)), "after");
    }

    /// Fields of a 32-bit game's object: long is 4 bytes, 8-byte values may
    /// sit on 4-byte boundaries, and neighbours are left alone.
    #[test]
    fn fields_read_and_write_in_their_classes_layout() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let mut words = [0u32, 0xAAAA_AAAA, 0, 0, 0xBBBB_BBBB];
        let at = |i: usize, words: &mut [u32; 5]| words.as_mut_ptr() as usize + 4 * i;
        let long = Field { at: at(0, &mut words), kind: "l".into(), guest: true };
        assert!(write_field(&long, collections::number_int(-7)));
        assert_eq!(words[..2], [(-7i32) as u32, 0xAAAA_AAAA]);
        assert_eq!(collections::number_as_i64(read_field(&long).unwrap()), -7);
        let double = Field { at: at(2, &mut words), kind: "d".into(), guest: true };
        assert!(write_field(&double, collections::number_float(2.5)));
        assert_eq!(collections::number_as_f64(read_field(&double).unwrap()), 2.5);
        assert_eq!(words[4], 0xBBBB_BBBB);
    }
}
