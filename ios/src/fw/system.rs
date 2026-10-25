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
// Apple's keyed archive format: a property list whose "$objects" list holds
// every object once (by identity, so loops come back as loops), linked by
// object numbers, with "$top" holding the root's keys. Property-list objects
// are written in Apple's own layout; any other object saves itself with
// -encodeWithCoder: and is made again with -initWithCoder:, its class chain
// kept under "$classes". The coders keep their state behind their second
// word; the state is never locked while an object's own coding code runs.

use crate::fw::plist::Value;
use std::sync::{MutexGuard, RwLock};

/// How a 32-bit game's memory holds values, for -encodeValueOfObjCType:at:
/// and -decodeValueOfObjCType:at: (4-byte pointers and longs, 8-byte values
/// 4-aligned, and its own object words).
#[derive(Clone, Copy)]
pub struct GuestMemory {
    pub owns: fn(usize) -> bool,
    pub object_to_host: fn(u32) -> Id,
    pub object_to_guest: fn(Id) -> u32,
}

static GUEST_MEMORY: RwLock<Option<GuestMemory>> = RwLock::new(None);

pub fn set_guest_memory(memory: GuestMemory) {
    *GUEST_MEMORY.write().unwrap_or_else(|e| e.into_inner()) = Some(memory);
}

/// The 32-bit game's memory rules, when a 32-bit game runs.
pub fn guest_memory() -> Option<GuestMemory> {
    *GUEST_MEMORY.read().unwrap_or_else(|e| e.into_inner())
}

fn guest_memory_at(address: usize) -> Option<GuestMemory> {
    GUEST_MEMORY.read().unwrap_or_else(|e| e.into_inner()).filter(|m| (m.owns)(address))
}

/// Objects nested deeper than this (containers or objects coding
/// themselves) are taken as runaway.
const MAX_DEPTH: usize = 256;

#[derive(Default)]
struct Frame {
    entries: Vec<(String, Value)>,
    /// The next "$n" key for values written without a key.
    unkeyed: usize,
}

struct Writer {
    objects: Vec<Value>,
    classes: HashMap<String, u64>,
    /// Object -> its object number.
    numbers: HashMap<Id, u64>,
    /// The keys of the objects being written, innermost last; the first is "$top".
    frames: Vec<Frame>,
    data: Id,
    failed: bool,
    /// How many objects are being written inside one another.
    depth: usize,
}

struct ReadFrame {
    /// The object whose keys are read; None for "$top".
    object: Option<u64>,
    unkeyed: usize,
}

struct Reader {
    objects: Vec<Value>,
    top: Value,
    made: HashMap<u64, Id>,
    frames: Vec<ReadFrame>,
    /// How many objects are being read inside one another.
    depth: usize,
    /// Plain containers being filled, and whether a member pointed back at one.
    open: HashMap<u64, bool>,
    failed: bool,
}

fn guard<T>(state: &Mutex<T>) -> MutexGuard<'_, T> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

fn state_of<T>(coder: Id) -> Option<&'static Mutex<T>> {
    let state = unsafe { *((coder + 8) as *const *const Mutex<T>) };
    (!state.is_null()).then(|| unsafe { &*state })
}

fn set_state<T>(coder: Id, state: T) {
    unsafe { *((coder + 8) as *mut *mut Mutex<T>) = Box::into_raw(Box::new(Mutex::new(state))) };
}

fn writer_of(coder: Id) -> Option<&'static Mutex<Writer>> {
    let state = state_of(coder);
    if state.is_none() {
        crate::log!("NSKeyedArchiver: written to before -initForWritingWithMutableData:; ignored");
    }
    state
}

fn reader_of(coder: Id) -> Option<&'static Mutex<Reader>> {
    let state = state_of(coder);
    if state.is_none() {
        crate::log!("NSKeyedUnarchiver: read before -initForReadingWithData:; nothing");
    }
    state
}

impl Writer {
    fn new(data: Id) -> Writer {
        Writer {
            objects: vec![Value::String("$null".into())],
            classes: HashMap::new(),
            numbers: HashMap::new(),
            frames: vec![Frame::default()],
            data,
            failed: false,
            depth: 0,
        }
    }

    fn add(&mut self, value: Value) -> u64 {
        self.objects.push(value);
        (self.objects.len() - 1) as u64
    }

    /// The class entry for a class chain (the class first, NSObject last).
    fn class(&mut self, chain: &[String]) -> u64 {
        if let Some(&number) = self.classes.get(&chain[0]) {
            return number;
        }
        let number = self.add(Value::Dict(vec![
            ("$classes".into(), Value::Array(chain.iter().map(|c| Value::String(c.clone())).collect())),
            ("$classname".into(), Value::String(chain[0].clone())),
        ]));
        self.classes.insert(chain[0].clone(), number);
        number
    }

    fn put(&mut self, key: Option<String>, value: Value) {
        let frame = self.frames.last_mut().expect("the top frame stays");
        let key = key.unwrap_or_else(|| {
            frame.unkeyed += 1;
            format!("${}", frame.unkeyed - 1)
        });
        frame.entries.retain(|(k, _)| *k != key);
        frame.entries.push((key, value));
    }

    fn archive(&self) -> Value {
        Value::Dict(vec![
            ("$archiver".into(), Value::String("NSKeyedArchiver".into())),
            ("$objects".into(), Value::Array(self.objects.clone())),
            ("$top".into(), Value::Dict(self.frames[0].entries.clone())),
            ("$version".into(), Value::Integer(100000)),
        ])
    }
}

fn chain(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| n.to_string()).collect()
}

/// `object`'s class and its superclasses by name.
fn class_chain(object: Id) -> Vec<String> {
    let mut out = Vec::new();
    let mut class = runtime::lock().class_of(object);
    while class != 0 && out.len() < 64 {
        out.push(runtime::lock().class_name(class));
        class = runtime::send(class, "superclass", &[]);
    }
    out
}

/// Writes `object` (and everything in it) into `coder`'s archive: its object
/// number, 0 (nil) for something that can't be written (and the archive is
/// marked failed).
fn encode_object(coder: Id, object: Id) -> u64 {
    let Some(state) = writer_of(coder) else { return 0 };
    if object == 0 {
        return 0;
    }
    {
        let w = guard(state);
        if let Some(&number) = w.numbers.get(&object) {
            return number;
        }
    }
    let plain = if string::is_string(object) && !is_kind(object, "NSMutableString") {
        Some(Value::String(string::get(object)))
    } else if collections::is_number(object) {
        Some(if collections::number_is_bool(object) {
            Value::Bool(collections::number_as_f64(object) != 0.0)
        } else {
            let number = collections::number_as_f64(object);
            if number.fract() == 0.0 && number.abs() < 9.0e18 { Value::Integer(collections::number_as_i64(object)) } else { Value::Real(number) }
        })
    } else {
        None
    };
    if let Some(value) = plain {
        let mut w = guard(state);
        let number = w.add(value);
        w.numbers.insert(object, number);
        return number;
    }
    // Numbered before its parts, so a part pointing back finds it.
    let at = {
        let mut w = guard(state);
        if w.depth >= MAX_DEPTH {
            w.failed = true;
            drop(w);
            crate::log!("NSKeyedArchiver: objects nested too deep; not written");
            return 0;
        }
        w.depth += 1;
        let at = w.add(Value::String(String::new()));
        w.numbers.insert(object, at);
        at
    };
    let refs = |items: Vec<Id>| Value::Array(items.into_iter().map(|item| Value::Uid(encode_object(coder, item))).collect());
    let written: Option<(Vec<(String, Value)>, Vec<String>)> = if string::is_string(object) {
        Some((vec![("NS.string".into(), Value::String(string::get(object)))], chain(&["NSMutableString", "NSString", "NSObject"])))
    } else if crate::fw::data::is_data(object) {
        let names: &[&str] = if is_kind(object, "NSMutableData") { &["NSMutableData", "NSData", "NSObject"] } else { &["NSData", "NSObject"] };
        Some((vec![("NS.data".into(), Value::Data(crate::fw::data::bytes(object)))], chain(names)))
    } else if is_kind(object, "NSDate") {
        Some((vec![("NS.time".into(), Value::Real(crate::fw::misc::date_value(object)))], chain(&["NSDate", "NSObject"])))
    } else if is_kind(object, "NSNull") {
        Some((Vec::new(), chain(&["NSNull", "NSObject"])))
    } else if collections::is_dictionary(object) {
        let (keys, values): (Vec<Id>, Vec<Id>) = collections::dictionary_pairs(object).into_iter().unzip();
        let names: &[&str] = if is_kind(object, "NSMutableDictionary") { &["NSMutableDictionary", "NSDictionary", "NSObject"] } else { &["NSDictionary", "NSObject"] };
        Some((vec![("NS.keys".into(), refs(keys)), ("NS.objects".into(), refs(values))], chain(names)))
    } else if collections::is_array(object) {
        let names: &[&str] = if is_kind(object, "NSMutableArray") { &["NSMutableArray", "NSArray", "NSObject"] } else { &["NSArray", "NSObject"] };
        Some((vec![("NS.objects".into(), refs(collections::array_items(object)))], chain(names)))
    } else if is_set(object) {
        let names: &[&str] = if is_kind(object, "NSMutableSet") { &["NSMutableSet", "NSSet", "NSObject"] } else { &["NSSet", "NSObject"] };
        Some((vec![("NS.objects".into(), refs(collections::array_items(runtime::send(object, "allObjects", &[]))))], chain(names)))
    } else if runtime::responds(object, "encodeWithCoder:") {
        guard(state).frames.push(Frame::default());
        runtime::send(object, "encodeWithCoder:", &[coder]);
        let entries = guard(state).frames.pop().map(|f| f.entries).unwrap_or_default();
        Some((entries, class_chain(object)))
    } else {
        // Apple raises here (the object doesn't adopt NSCoding).
        crate::log!("NSKeyedArchiver: a {} can't be saved (no -encodeWithCoder:)", string::describe_class_of(object));
        None
    };
    let mut w = guard(state);
    w.depth -= 1;
    match written {
        Some((mut entries, names)) => {
            let class = w.class(&names);
            entries.push(("$class".into(), Value::Uid(class)));
            w.objects[at as usize] = Value::Dict(entries);
            at
        }
        None => {
            w.failed = true;
            w.numbers.insert(object, 0);
            0
        }
    }
}

fn is_kind(object: Id, class: &str) -> bool {
    let class = runtime::class_named(class);
    class != 0 && runtime::send(object, "isKindOfClass:", &[class]) & 0xFF != 0
}

fn is_set(object: Id) -> bool {
    is_kind(object, "NSSet")
}

fn new_coder(class: &str) -> Id {
    runtime::alloc_instance(runtime::class_named(class))
}

/// The keyed archive for `object`, or None if it holds something that
/// can't be written.
fn archive(object: Id) -> Option<Value> {
    let coder = new_coder("NSKeyedArchiver");
    set_state(coder, Writer::new(0));
    let root = encode_object(coder, object);
    let state = writer_of(coder)?;
    let mut w = guard(state);
    w.put(Some("root".into()), Value::Uid(root));
    (!w.failed).then(|| w.archive())
}

/// A reader for a keyed archive, or None if `value` isn't one.
fn reader_for(value: Value) -> Option<Reader> {
    let Some(Value::Array(objects)) = value.get("$objects") else {
        crate::log!("NSKeyedUnarchiver: not a keyed archive");
        return None;
    };
    let top = value.get("$top")?.clone();
    Some(Reader {
        objects: objects.clone(),
        top,
        made: HashMap::new(),
        frames: vec![ReadFrame { object: None, unkeyed: 0 }],
        depth: 0,
        open: HashMap::new(),
        failed: false,
    })
}

/// The root object of a keyed archive, or None if it isn't one or holds a
/// class this layer can't make.
fn unarchive(value: &Value) -> Option<Id> {
    let reader = reader_for(value.clone())?;
    let root = reader.top.get("root").or_else(|| match &reader.top {
        Value::Dict(entries) => entries.first().map(|(_, v)| v),
        _ => None,
    })?;
    let root = root.as_uid()?;
    let coder = new_coder("NSKeyedUnarchiver");
    set_state(coder, reader);
    let object = decode_object(coder, root);
    let failed = reader_of(coder).is_some_and(|state| guard(state).failed);
    (!failed && (object != 0 || root == 0)).then_some(object)
}

/// Classes made from their own archive layout rather than -initWithCoder:.
const PLAIN_CLASSES: [&str; 13] = [
    "NSDictionary", "NSMutableDictionary", "NSArray", "NSMutableArray", "NSSet", "NSMutableSet", "NSString", "NSMutableString", "NSDate",
    "NSData", "NSMutableData", "NSNull", "NSObject",
];

/// The object numbered `number` in `coder`'s archive (made once), 0 for
/// nil or something that can't be made.
fn decode_object(coder: Id, number: u64) -> Id {
    let Some(state) = reader_of(coder) else { return 0 };
    let (value, class) = {
        let mut r = guard(state);
        if let Some(&made) = r.made.get(&number) {
            if let Some(looped) = r.open.get_mut(&number) {
                *looped = true;
            }
            return made;
        }
        let Some(value) = r.objects.get(number as usize).cloned() else { return 0 };
        // As in encode_object: plain strings and numbers don't count.
        if matches!(value, Value::Dict(_)) && r.depth >= MAX_DEPTH {
            r.failed = true;
            drop(r);
            crate::log!("NSKeyedUnarchiver: objects nested too deep; nil");
            return 0;
        }
        let class = value.get("$class").and_then(Value::as_uid).and_then(|c| r.objects.get(c as usize).cloned());
        (value, class)
    };
    let remember = |object: Id| {
        guard(state).made.insert(number, object);
        object
    };
    let made = match &value {
        Value::String(text) if number == 0 && text == "$null" => 0,
        Value::String(text) => string::make(text),
        Value::Integer(n) => collections::number_int(*n),
        Value::Real(f) => collections::number_float(*f),
        Value::Bool(b) => collections::number_bool(*b),
        Value::Data(bytes) => crate::fw::data::make(bytes),
        Value::Dict(_) => {
            let Some(class) = class else { return 0 };
            let Some(Value::String(name)) = class.get("$classname") else { return 0 };
            let mutable = name.starts_with("NSMutable");
            let made = match name.trim_start_matches("NSMutable").trim_start_matches("NS") {
                kind @ ("Dictionary" | "Array" | "Set") => return decode_container(coder, state, number, &value, kind, mutable),
                "String" => match value.get("NS.string") {
                    Some(Value::String(text)) => string::make(text),
                    _ => string::make(""),
                },
                "Date" => match value.get("NS.time") {
                    Some(Value::Real(seconds)) => crate::fw::misc::date(*seconds),
                    Some(Value::Integer(seconds)) => crate::fw::misc::date(*seconds as f64),
                    _ => crate::fw::misc::date(0.0),
                },
                "Data" => match value.get("NS.data") {
                    Some(Value::Data(bytes)) => crate::fw::data::make(bytes),
                    _ => crate::fw::data::make(&[]),
                },
                "Null" => runtime::send(runtime::class_named("NSNull"), "null", &[]),
                _ => return decode_custom(coder, state, number, &class, name),
            };
            if mutable { runtime::send(made, "mutableCopy", &[]) } else { made }
        }
        _ => return 0,
    };
    remember(made)
}

/// A dictionary, array or set: made (empty) and remembered before its
/// members are read, so a member holding the container gets the container.
fn decode_container(coder: Id, state: &Mutex<Reader>, number: u64, value: &Value, kind: &str, mutable: bool) -> Id {
    let class = match kind {
        "Dictionary" => "NSMutableDictionary",
        "Array" => "NSMutableArray",
        _ => "NSMutableSet",
    };
    let made = runtime::send(runtime::send(runtime::class_named(class), "alloc", &[]), "init", &[]);
    {
        let mut r = guard(state);
        r.made.insert(number, made);
        r.open.insert(number, false);
        r.depth += 1;
    }
    let list = |key: &str| -> Vec<Id> {
        match value.get(key) {
            Some(Value::Array(items)) => items.iter().map(|item| item.as_uid().map_or(0, |n| decode_object(coder, n))).collect(),
            _ => Vec::new(),
        }
    };
    if kind == "Dictionary" {
        for (key, item) in list("NS.keys").into_iter().zip(list("NS.objects")) {
            if key != 0 && item != 0 {
                runtime::send(made, "setObject:forKey:", &[item, key]);
            }
        }
    } else {
        for item in list("NS.objects").into_iter().filter(|&item| item != 0) {
            runtime::send(made, "addObject:", &[item]);
        }
    }
    let looped = {
        let mut r = guard(state);
        r.depth -= 1;
        r.open.remove(&number).unwrap_or(false)
    };
    // An immutable one comes back immutable, unless a member holds it (the
    // member has this very object).
    if mutable || looped {
        return made;
    }
    let copy = runtime::send(made, "copy", &[]);
    guard(state).made.insert(number, copy);
    copy
}

/// An object that saved itself: made with -initWithCoder: from the first
/// class of its chain that exists here.
fn decode_custom(coder: Id, state: &Mutex<Reader>, number: u64, class: &Value, name: &str) -> Id {
    let names: Vec<String> = match class.get("$classes") {
        Some(Value::Array(list)) => list.iter().filter_map(|v| if let Value::String(s) = v { Some(s.clone()) } else { None }).collect(),
        _ => vec![name.to_string()],
    };
    let Some(found) = names.iter().filter(|n| !PLAIN_CLASSES.contains(&n.as_str())).map(|n| runtime::class_named(n)).find(|&c| c != 0) else {
        // Apple raises NSInvalidUnarchiveOperationException.
        crate::log!("NSKeyedUnarchiver: no class {name} here; nil");
        return 0;
    };
    let object = runtime::send(found, "alloc", &[]);
    if !runtime::responds(object, "initWithCoder:") {
        // Apple raises: the class doesn't adopt NSCoding.
        crate::log!("NSKeyedUnarchiver: a {name} can't be read (no -initWithCoder:); nil");
        return 0;
    }
    {
        let mut r = guard(state);
        r.made.insert(number, object);
        r.frames.push(ReadFrame { object: Some(number), unkeyed: 0 });
        r.depth += 1;
    }
    let mut made = runtime::send(object, "initWithCoder:", &[coder]);
    {
        let mut r = guard(state);
        r.frames.pop();
        r.depth -= 1;
    }
    if made != 0 && runtime::responds(made, "awakeAfterUsingCoder:") {
        made = runtime::send(made, "awakeAfterUsingCoder:", &[coder]);
    }
    guard(state).made.insert(number, made);
    made
}

/// The value under `key` (the next "$n" for None) in the object being read.
fn read_entry(coder: Id, key: Option<String>) -> Option<Value> {
    let state = reader_of(coder)?;
    let mut r = guard(state);
    let (key, object) = {
        let frame = r.frames.last_mut()?;
        let key = key.unwrap_or_else(|| {
            frame.unkeyed += 1;
            format!("${}", frame.unkeyed - 1)
        });
        (key, frame.object)
    };
    let holder = match object {
        Some(number) => r.objects.get(number as usize)?,
        None => &r.top,
    };
    holder.get(&key).cloned()
}

fn entry_object(coder: Id, value: Option<Value>) -> Id {
    match value.as_ref().and_then(Value::as_uid) {
        Some(number) => decode_object(coder, number),
        None => 0,
    }
}

fn entry_integer(value: Option<Value>) -> i64 {
    match value {
        Some(Value::Integer(n)) => n,
        Some(Value::Real(f)) => f as i64,
        Some(Value::Bool(b)) => b as i64,
        _ => 0,
    }
}

fn entry_real(value: Option<Value>) -> f64 {
    match value {
        Some(Value::Integer(n)) => n as f64,
        Some(Value::Real(f)) => f,
        Some(Value::Bool(b)) => b as i64 as f64,
        _ => 0.0,
    }
}

// --- values by their type encoding (-encodeValueOfObjCType:at:) ---

/// Size and alignment of one encoded type, in host or 32-bit game layout.
pub(crate) fn type_size(code: &str, guest: bool) -> (usize, usize) {
    let word = if guest { 4 } else { 8 };
    let wide_align = if guest { 4 } else { 8 };
    match code.as_bytes().first() {
        Some(b'c' | b'C' | b'B') => (1, 1),
        Some(b's' | b'S') => (2, 2),
        Some(b'i' | b'I' | b'f') => (4, 4),
        Some(b'l' | b'L') => (word, word),
        Some(b'q' | b'Q' | b'd') => (8, wide_align),
        Some(b'{') => {
            let (mut size, mut align) = (0usize, 1usize);
            for field in struct_fields(code) {
                let (s, a) = type_size(&field, guest);
                size = size.next_multiple_of(a) + s;
                align = align.max(a);
            }
            (size.next_multiple_of(align), align)
        }
        Some(b'[') => {
            let (count, element) = array_parts(code);
            let (s, a) = type_size(&element, guest);
            (s * count, a)
        }
        _ => (word, word),
    }
}

/// The field types of "{Name=fields}", without field names.
fn struct_fields(code: &str) -> Vec<String> {
    let inner = code.strip_prefix('{').and_then(|c| c.strip_suffix('}')).unwrap_or("");
    let Some((_, fields)) = inner.split_once('=') else { return Vec::new() };
    let mut plain = String::new();
    let mut quoted = false;
    for c in fields.chars() {
        if c == '"' && (quoted || !plain.ends_with('@')) {
            quoted = !quoted;
        } else if !quoted {
            plain.push(c);
        }
    }
    crate::fw::object::type_codes(&plain)
}

/// "[4f]" -> (4, "f").
fn array_parts(code: &str) -> (usize, String) {
    let inner = code.trim_start_matches('[').trim_end_matches(']');
    let digits = inner.chars().take_while(char::is_ascii_digit).count();
    (inner[..digits].parse().unwrap_or(0), inner[digits..].to_string())
}

fn read_value(coder: Id, code: &str, at: usize, guest: Option<GuestMemory>) -> Value {
    unsafe {
        let read = |p: usize| p as *const u8;
        match code.as_bytes().first() {
            Some(b'c') => Value::Integer(*read(at) as i8 as i64),
            Some(b'C') => Value::Integer(*read(at) as i64),
            Some(b'B') => Value::Bool(*read(at) != 0),
            Some(b's') => Value::Integer(std::ptr::read_unaligned(at as *const i16) as i64),
            Some(b'S') => Value::Integer(std::ptr::read_unaligned(at as *const u16) as i64),
            Some(b'i') => Value::Integer(std::ptr::read_unaligned(at as *const i32) as i64),
            Some(b'I') => Value::Integer(std::ptr::read_unaligned(at as *const u32) as i64),
            Some(b'l') if guest.is_some() => Value::Integer(std::ptr::read_unaligned(at as *const i32) as i64),
            Some(b'L') if guest.is_some() => Value::Integer(std::ptr::read_unaligned(at as *const u32) as i64),
            Some(b'q' | b'Q' | b'l' | b'L') => Value::Integer(std::ptr::read_unaligned(at as *const i64)),
            Some(b'f') => Value::Real(std::ptr::read_unaligned(at as *const f32) as f64),
            Some(b'd') => Value::Real(std::ptr::read_unaligned(at as *const f64)),
            Some(b'@') => {
                let object = match guest {
                    Some(g) => (g.object_to_host)(std::ptr::read_unaligned(at as *const u32)),
                    None => std::ptr::read_unaligned(at as *const usize),
                };
                Value::Uid(encode_object(coder, object))
            }
            Some(b'{') => {
                let mut offset = 0usize;
                let mut fields = Vec::new();
                for field in struct_fields(code) {
                    let (s, a) = type_size(&field, guest.is_some());
                    offset = offset.next_multiple_of(a);
                    fields.push(read_value(coder, &field, at + offset, guest));
                    offset += s;
                }
                Value::Array(fields)
            }
            Some(b'[') => {
                let (count, element) = array_parts(code);
                let (s, _) = type_size(&element, guest.is_some());
                Value::Array((0..count).map(|i| read_value(coder, &element, at + i * s, guest)).collect())
            }
            _ => {
                // A pointer, C string, selector or class means nothing in another run.
                crate::log!("NSKeyedArchiver: a {code} value can't be saved; nil kept");
                Value::Integer(0)
            }
        }
    }
}

fn write_value(coder: Id, code: &str, at: usize, value: Option<&Value>, guest: Option<GuestMemory>) {
    let integer = || entry_integer(value.cloned());
    let real = || entry_real(value.cloned());
    unsafe {
        match code.as_bytes().first() {
            Some(b'c' | b'C' | b'B') => *(at as *mut u8) = integer() as u8,
            Some(b's' | b'S') => std::ptr::write_unaligned(at as *mut u16, integer() as u16),
            Some(b'i' | b'I') => std::ptr::write_unaligned(at as *mut u32, integer() as u32),
            Some(b'l' | b'L') if guest.is_some() => std::ptr::write_unaligned(at as *mut u32, integer() as u32),
            Some(b'q' | b'Q' | b'l' | b'L') => std::ptr::write_unaligned(at as *mut i64, integer()),
            Some(b'f') => std::ptr::write_unaligned(at as *mut f32, real() as f32),
            Some(b'd') => std::ptr::write_unaligned(at as *mut f64, real()),
            Some(b'@') => {
                let object = entry_object(coder, value.cloned());
                match guest {
                    Some(g) => std::ptr::write_unaligned(at as *mut u32, (g.object_to_guest)(object)),
                    None => std::ptr::write_unaligned(at as *mut usize, object),
                }
            }
            Some(b'{') => {
                let parts = match value {
                    Some(Value::Array(parts)) => parts.as_slice(),
                    _ => &[],
                };
                let mut offset = 0usize;
                for (i, field) in struct_fields(code).iter().enumerate() {
                    let (s, a) = type_size(field, guest.is_some());
                    offset = offset.next_multiple_of(a);
                    write_value(coder, field, at + offset, parts.get(i), guest);
                    offset += s;
                }
            }
            Some(b'[') => {
                let parts = match value {
                    Some(Value::Array(parts)) => parts.as_slice(),
                    _ => &[],
                };
                let (count, element) = array_parts(code);
                let (s, _) = type_size(&element, guest.is_some());
                for i in 0..count {
                    write_value(coder, &element, at + i * s, parts.get(i), guest);
                }
            }
            _ => {
                let (size, _) = type_size(code, guest.is_some());
                std::ptr::write_bytes(at as *mut u8, 0, size);
            }
        }
    }
}

// --- NSKeyedArchiver ---

fn put(coder: Id, key: Id, value: Value) {
    if let Some(state) = writer_of(coder) {
        guard(state).put(Some(string::get(key)), value);
    }
}

extern "C" fn writer_init(this: Id, _: Sel, data: Id) -> Id {
    set_state(this, Writer::new(data));
    this
}

extern "C" fn encode_object_for_key(this: Id, _: Sel, object: Id, key: Id) {
    let number = encode_object(this, object);
    put(this, key, Value::Uid(number));
}

extern "C" fn encode_object_unkeyed(this: Id, _: Sel, object: Id) {
    let number = encode_object(this, object);
    if let Some(state) = writer_of(this) {
        guard(state).put(None, Value::Uid(number));
    }
}

extern "C" fn encode_bool(this: Id, _: Sel, value: u8, key: Id) {
    put(this, key, Value::Bool(value != 0));
}

extern "C" fn encode_int32(this: Id, _: Sel, value: i64, key: Id) {
    put(this, key, Value::Integer(value as i32 as i64));
}

extern "C" fn encode_int64(this: Id, _: Sel, value: i64, key: Id) {
    put(this, key, Value::Integer(value));
}

extern "C" fn encode_float(this: Id, _: Sel, value: f32, key: Id) {
    put(this, key, Value::Real(value as f64));
}

extern "C" fn encode_double(this: Id, _: Sel, value: f64, key: Id) {
    put(this, key, Value::Real(value));
}

extern "C" fn encode_value_of_type(this: Id, _: Sel, kind: *const libc::c_char, at: usize) {
    if kind.is_null() || at == 0 {
        return;
    }
    let code = unsafe { std::ffi::CStr::from_ptr(kind) }.to_string_lossy().into_owned();
    let value = read_value(this, &code, at, guest_memory_at(at));
    if let Some(state) = writer_of(this) {
        guard(state).put(None, value);
    }
}

extern "C" fn finish_encoding(this: Id, _: Sel) {
    let Some(state) = writer_of(this) else { return };
    let (data, bytes) = {
        let w = guard(state);
        (w.data, plist::to_binary(&w.archive()))
    };
    if data != 0 {
        runtime::send(data, "appendData:", &[crate::fw::data::make(&bytes)]);
    }
}

/// NSPropertyListBinaryFormat_v1_0, as the archiver writes.
extern "C" fn output_format(_this: Id, _: Sel) -> usize {
    200
}

extern "C" fn set_output_format(_this: Id, _: Sel, _format: usize) {}

extern "C" fn yes(_this: Id, _: Sel) -> bool {
    true
}

extern "C" fn archive_root_object(_: Class, _: Sel, object: Id, path: Id) -> bool {
    let path = string::get(path);
    match archive(object) {
        Some(value) => std::fs::write(&path, plist::to_binary(&value)).is_ok(),
        None => {
            crate::log!("NSKeyedArchiver: {path} not written");
            false
        }
    }
}

extern "C" fn archived_data_with_root_object(_: Class, _: Sel, object: Id) -> Id {
    match archive(object) {
        Some(value) => crate::fw::counted::autorelease(crate::fw::data::make(&plist::to_binary(&value))),
        None => 0,
    }
}

// --- NSKeyedUnarchiver ---

extern "C" fn reader_init(this: Id, _: Sel, data: Id) -> Id {
    match plist::parse(&crate::fw::data::bytes(data)).and_then(reader_for) {
        Some(reader) => {
            set_state(this, reader);
            this
        }
        // Apple raises NSInvalidArgumentException for data that isn't an archive.
        None => 0,
    }
}

fn key_entry(this: Id, key: Id) -> Option<Value> {
    read_entry(this, Some(string::get(key)))
}

extern "C" fn decode_object_for_key(this: Id, _: Sel, key: Id) -> Id {
    entry_object(this, key_entry(this, key))
}

extern "C" fn decode_object_of_class_for_key(this: Id, _: Sel, _class: Id, key: Id) -> Id {
    entry_object(this, key_entry(this, key))
}

extern "C" fn decode_object_unkeyed(this: Id, _: Sel) -> Id {
    entry_object(this, read_entry(this, None))
}

extern "C" fn contains_value_for_key(this: Id, _: Sel, key: Id) -> bool {
    key_entry(this, key).is_some()
}

extern "C" fn decode_bool(this: Id, _: Sel, key: Id) -> bool {
    entry_integer(key_entry(this, key)) != 0
}

extern "C" fn decode_int32(this: Id, _: Sel, key: Id) -> i32 {
    entry_integer(key_entry(this, key)) as i32
}

extern "C" fn decode_int64(this: Id, _: Sel, key: Id) -> i64 {
    entry_integer(key_entry(this, key))
}

extern "C" fn decode_float(this: Id, _: Sel, key: Id) -> f32 {
    entry_real(key_entry(this, key)) as f32
}

extern "C" fn decode_double(this: Id, _: Sel, key: Id) -> f64 {
    entry_real(key_entry(this, key))
}

extern "C" fn decode_value_of_type(this: Id, _: Sel, kind: *const libc::c_char, at: usize) {
    if kind.is_null() || at == 0 {
        return;
    }
    let code = unsafe { std::ffi::CStr::from_ptr(kind) }.to_string_lossy().into_owned();
    let value = read_entry(this, None);
    write_value(this, &code, at, value.as_ref(), guest_memory_at(at));
}

extern "C" fn finish_decoding(_this: Id, _: Sel) {}

extern "C" fn unarchive_object_with_file(_: Class, _: Sel, path: Id) -> Id {
    let path = string::get(path);
    std::fs::read(&path).ok().and_then(|bytes| plist::parse(&bytes)).and_then(|value| unarchive(&value)).unwrap_or(0)
}

extern "C" fn unarchive_object_with_data(_: Class, _: Sel, data: Id) -> Id {
    plist::parse(&crate::fw::data::bytes(data)).and_then(|value| unarchive(&value)).unwrap_or(0)
}

fn register_coders() {
    if runtime::class_named("NSCoder") == 0 {
        define_class("NSCoder", "NSObject", 8);
    }
    let archiver = define_class("NSKeyedArchiver", "NSCoder", 16);
    class_methods(
        archiver,
        &[
            ("archiveRootObject:toFile:", archive_root_object as usize),
            ("archivedDataWithRootObject:", archived_data_with_root_object as usize),
        ],
    );
    methods(
        archiver,
        &[
            ("initForWritingWithMutableData:", writer_init as usize),
            ("encodeObject:forKey:", encode_object_for_key as usize),
            ("encodeConditionalObject:forKey:", encode_object_for_key as usize),
            ("encodeObject:", encode_object_unkeyed as usize),
            ("encodeRootObject:", encode_object_unkeyed as usize),
            ("encodeBool:forKey:", encode_bool as usize),
            ("encodeInt:forKey:", encode_int32 as usize),
            ("encodeInt32:forKey:", encode_int32 as usize),
            ("encodeInteger:forKey:", encode_int64 as usize),
            ("encodeInt64:forKey:", encode_int64 as usize),
            ("encodeFloat:forKey:", encode_float as usize),
            ("encodeDouble:forKey:", encode_double as usize),
            ("encodeValueOfObjCType:at:", encode_value_of_type as usize),
            ("finishEncoding", finish_encoding as usize),
            ("outputFormat", output_format as usize),
            ("setOutputFormat:", set_output_format as usize),
            ("allowsKeyedCoding", yes as usize),
        ],
    );
    let unarchiver = define_class("NSKeyedUnarchiver", "NSCoder", 16);
    class_methods(
        unarchiver,
        &[
            ("unarchiveObjectWithFile:", unarchive_object_with_file as usize),
            ("unarchiveObjectWithData:", unarchive_object_with_data as usize),
        ],
    );
    methods(
        unarchiver,
        &[
            ("initForReadingWithData:", reader_init as usize),
            ("decodeObjectForKey:", decode_object_for_key as usize),
            ("decodeObjectOfClass:forKey:", decode_object_of_class_for_key as usize),
            ("decodeObjectOfClasses:forKey:", decode_object_of_class_for_key as usize),
            ("decodeObject", decode_object_unkeyed as usize),
            ("containsValueForKey:", contains_value_for_key as usize),
            ("decodeBoolForKey:", decode_bool as usize),
            ("decodeIntForKey:", decode_int32 as usize),
            ("decodeInt32ForKey:", decode_int32 as usize),
            ("decodeIntegerForKey:", decode_int64 as usize),
            ("decodeInt64ForKey:", decode_int64 as usize),
            ("decodeFloatForKey:", decode_float as usize),
            ("decodeDoubleForKey:", decode_double as usize),
            ("decodeValueOfObjCType:at:", decode_value_of_type as usize),
            ("finishDecoding", finish_decoding as usize),
            ("allowsKeyedCoding", yes as usize),
        ],
    );
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
    register_coders();

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

#[cfg(test)]
mod tests {
    use super::*;

    fn ready() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
    }

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect()
    }

    /// A binary keyed archive as Apple writes it: {"score": 42, "names": ["Siris"]}.
    const APPLE_ARCHIVE: &str = "62706c6973743030d4010203040506070a59246172636869766572582476657273696f6e5424746f7058246f626a656374735f100f4e534b65796564417263686976657212000186a0d1080954726f6f748001a90b0c1718191a1e1f2555246e756c6cd30d0e0f101316574e532e6b6579735a4e532e6f626a656374735624636c617373a2111280028003a214158004800580075573636f7265556e616d6573102ad20e0f1b1da11c80068008555369726973d2202122235a24636c6173736e616d655824636c61737365735c4e5344696374696f6e617279a22224584e534f626a656374d220212627574e534172726179a2262408111b24293244494c51535d636a727d8487898b8e9092949aa0a2a7a9abadb3b8c3ccd9dce5eaf200000000000001010000000000000028000000000000000000000000000000f5";

    #[test]
    fn apple_keyed_archive_reads_as_its_objects() {
        ready();
        let root = unarchive(&plist::parse(&hex(APPLE_ARCHIVE)).unwrap()).unwrap();
        assert!(collections::is_dictionary(root));
        assert_eq!(collections::number_as_i64(collections::dictionary_get(root, string::make("score"))), 42);
        let names = collections::array_items(collections::dictionary_get(root, string::make("names")));
        assert_eq!(names.len(), 1);
        assert_eq!(string::get(names[0]), "Siris");
    }

    #[test]
    fn keyed_archive_round_trip() {
        ready();
        let list = collections::array_from(&[string::make("a"), collections::number_int(7), collections::number_bool(true), collections::number_float(1.5)]);
        let object = collections::dictionary_from(&[
            (string::make("list"), list),
            (string::make("blob"), crate::fw::data::make(b"xyz")),
            (string::make("when"), crate::fw::misc::date(12345.5)),
        ]);
        let text = plist::to_xml(&archive(object).unwrap());
        assert!(text.contains("NSKeyedArchiver") && text.contains("CF$UID"));
        let back = unarchive(&plist::parse(text.as_bytes()).unwrap()).unwrap();
        let items = collections::array_items(collections::dictionary_get(back, string::make("list")));
        assert_eq!(string::get(items[0]), "a");
        assert_eq!(collections::number_as_i64(items[1]), 7);
        assert!(collections::number_is_bool(items[2]));
        assert_eq!(collections::number_as_f64(items[3]), 1.5);
        assert_eq!(crate::fw::data::bytes(collections::dictionary_get(back, string::make("blob"))), b"xyz");
        assert_eq!(crate::fw::misc::date_value(collections::dictionary_get(back, string::make("when"))), 12345.5);
    }

    /// Boxed geometry saves the way Foundation writes it ("NS.special" and
    /// the text form) and comes back as the same value; other structs come
    /// back too.
    #[test]
    fn keyed_archives_keep_boxed_values() {
        use crate::fw::geometry::{Point, Rect, Transform};
        ready();
        let point = collections::value_with(collections::POINT_TYPE, &Point { x: 1.5, y: -2.0 });
        let rect = collections::value_with(collections::RECT_TYPE, &Rect::new(0.1, 2.0, 30.0, 40.0));
        let range = collections::value_with(collections::RANGE_TYPE, &string::Range { location: 3, length: 9 });
        let turn = Transform { a: 0.0, b: 1.0, c: -1.0, d: 0.0, tx: 7.0, ty: 8.0 };
        let transform = collections::value_with(collections::TRANSFORM_TYPE, &turn);
        let object = collections::array_from(&[point, rect, range, transform]);
        let text = plist::to_xml(&archive(object).expect("boxed values save"));
        assert!(text.contains("<key>NS.pointval</key>") && text.contains("{1.5, -2}"), "{text}");
        let back = collections::array_items(unarchive(&plist::parse(text.as_bytes()).unwrap()).unwrap());
        assert_eq!(back.len(), 4);
        for (before, after) in [point, rect, range, transform].into_iter().zip(back) {
            assert_eq!(runtime::send(before, "isEqualToValue:", &[after]) & 0xFF, 1, "{} came back as {}", string::describe(before), string::describe(after));
        }
    }

    #[test]
    fn plain_property_lists_are_not_archives() {
        ready();
        let value = plist::Value::Dict(vec![("score".into(), plist::Value::Integer(1))]);
        assert!(unarchive(&value).is_none());
    }

    /// A container holding itself comes back holding itself; plain
    /// containers nested past the limit fail cleanly both ways.
    #[test]
    fn keyed_archives_with_container_loops_and_deep_nesting() {
        ready();
        let list = runtime::send(runtime::send(runtime::class_named("NSMutableArray"), "alloc", &[]), "init", &[]);
        runtime::send(list, "addObject:", &[string::make("x")]);
        runtime::send(list, "addObject:", &[list]);
        let back = unarchive(&archive(list).expect("a looped array is written")).expect("and read");
        let items = collections::array_items(back);
        assert_eq!(string::get(items[0]), "x");
        assert_eq!(items[1], back, "the array holds itself again");

        let mut deep = string::make("bottom");
        for _ in 0..MAX_DEPTH {
            deep = collections::array_from(&[deep]);
        }
        let back = unarchive(&archive(deep).expect("nesting up to the limit is written")).expect("and read back");
        let bottom = (0..MAX_DEPTH).fold(back, |list, _| collections::array_items(list)[0]);
        assert_eq!(string::get(bottom), "bottom");
        for _ in 0..10 {
            deep = collections::array_from(&[deep]);
        }
        assert!(archive(deep).is_none(), "nesting past the limit isn't written");
        // The same depth written by hand: read as nothing, not a crash.
        let class = Value::Dict(vec![
            ("$classes".into(), Value::Array(vec![Value::String("NSArray".into()), Value::String("NSObject".into())])),
            ("$classname".into(), Value::String("NSArray".into())),
        ]);
        let mut objects = vec![Value::String("$null".into()), class, Value::String("bottom".into())];
        for _ in 0..MAX_DEPTH + 10 {
            let inner = objects.len() as u64 - 1;
            objects.push(Value::Dict(vec![("NS.objects".into(), Value::Array(vec![Value::Uid(inner)])), ("$class".into(), Value::Uid(1))]));
        }
        let root = objects.len() as u64 - 1;
        let archive = Value::Dict(vec![
            ("$archiver".into(), Value::String("NSKeyedArchiver".into())),
            ("$objects".into(), Value::Array(objects)),
            ("$top".into(), Value::Dict(vec![("root".into(), Value::Uid(root))])),
            ("$version".into(), Value::Integer(100000)),
        ]);
        assert!(unarchive(&archive).is_none());
    }

    // A game class that saves itself (NSCoding), as games write them.
    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq, Default)]
    struct Spot {
        x: f32,
        y: f32,
    }

    fn imp_of(object: Id, name: &str) -> usize {
        runtime::imp_for(object, runtime::sel(name)).unwrap_or_else(|| panic!("{name} is missing"))
    }

    extern "C" fn hero_encode(this: Id, _: Sel, coder: Id) {
        let key = string::make;
        let field = |name: &'static str| property_get(this, name).unwrap_or(0);
        runtime::send(coder, "encodeObject:forKey:", &[field("name") as Id, key("name")]);
        runtime::send(coder, "encodeObject:forKey:", &[field("pet") as Id, key("pet")]);
        runtime::send(coder, "encodeInt:forKey:", &[field("level") as usize, key("level")]);
        runtime::send(coder, "encodeInteger:forKey:", &[field("gold") as usize, key("gold")]);
        runtime::send(coder, "encodeInt64:forKey:", &[field("xp") as usize, key("xp")]);
        runtime::send(coder, "encodeBool:forKey:", &[field("alive") as usize, key("alive")]);
        let double: extern "C" fn(Id, Sel, f64, Id) = unsafe { std::mem::transmute(imp_of(coder, "encodeDouble:forKey:")) };
        double(coder, 0, f64::from_bits(field("speed")), key("speed"));
        let float: extern "C" fn(Id, Sel, f32, Id) = unsafe { std::mem::transmute(imp_of(coder, "encodeFloat:forKey:")) };
        float(coder, 0, f64::from_bits(field("luck")) as f32, key("luck"));
        let spot = Spot { x: 1.5, y: -2.0 };
        let kind = c"{Spot=ff}";
        runtime::send(coder, "encodeValueOfObjCType:at:", &[kind.as_ptr() as usize, &spot as *const Spot as usize]);
    }

    extern "C" fn hero_init_with_coder(this: Id, _: Sel, coder: Id) -> Id {
        let key = string::make;
        let object = |name: &str| runtime::send(coder, "decodeObjectForKey:", &[key(name)]);
        property_set(this, "name", object("name") as u64);
        property_set(this, "pet", object("pet") as u64);
        property_set(this, "level", runtime::send(coder, "decodeIntForKey:", &[key("level")]) as u32 as i32 as i64 as u64);
        property_set(this, "gold", runtime::send(coder, "decodeIntegerForKey:", &[key("gold")]) as u64);
        property_set(this, "xp", runtime::send(coder, "decodeInt64ForKey:", &[key("xp")]) as u64);
        property_set(this, "alive", (runtime::send(coder, "decodeBoolForKey:", &[key("alive")]) & 0xFF) as u64);
        let double: extern "C" fn(Id, Sel, Id) -> f64 = unsafe { std::mem::transmute(imp_of(coder, "decodeDoubleForKey:")) };
        property_set(this, "speed", double(coder, 0, key("speed")).to_bits());
        let float: extern "C" fn(Id, Sel, Id) -> f32 = unsafe { std::mem::transmute(imp_of(coder, "decodeFloatForKey:")) };
        property_set(this, "luck", (float(coder, 0, key("luck")) as f64).to_bits());
        let mut spot = Spot::default();
        let kind = c"{Spot=ff}";
        runtime::send(coder, "decodeValueOfObjCType:at:", &[kind.as_ptr() as usize, &mut spot as *mut Spot as usize]);
        property_set(this, "spotX", (spot.x as f64).to_bits());
        property_set(this, "spotY", (spot.y as f64).to_bits());
        this
    }

    fn hero_class() -> Class {
        static CLASS: std::sync::OnceLock<Class> = std::sync::OnceLock::new();
        *CLASS.get_or_init(|| {
            let class = define_class("CoderTestHero", "NSObject", 8);
            methods(class, &[("encodeWithCoder:", hero_encode as usize), ("initWithCoder:", hero_init_with_coder as usize)]);
            class
        })
    }

    fn hero(name: &str, level: i32) -> Id {
        let object = runtime::alloc_instance(hero_class());
        property_set(object, "name", string::make(name) as u64);
        property_set(object, "level", level as i64 as u64);
        property_set(object, "gold", (-5i64) as u64);
        property_set(object, "xp", (1u64 << 40) + 3);
        property_set(object, "alive", 1);
        property_set(object, "speed", 2.25f64.to_bits());
        property_set(object, "luck", 0.75f64.to_bits());
        object
    }

    /// Game objects save through encodeWithCoder: and come back through
    /// initWithCoder:, every value kind kept, an object met twice (here a
    /// loop: the hero's pet's pet is the hero) coming back as one object.
    #[test]
    fn keyed_coders_save_and_read_game_objects() {
        ready();
        let (siris, drake) = (hero("Siris", -7), hero("Drake", 3));
        property_set(siris, "pet", drake as u64);
        property_set(drake, "pet", siris as u64);
        let data = runtime::send(runtime::class_named("NSKeyedArchiver"), "archivedDataWithRootObject:", &[siris]);
        assert_ne!(data, 0, "the archiver refused a game object");
        let text = format!("{:?}", plist::parse(&crate::fw::data::bytes(data)).unwrap());
        assert!(text.contains("CoderTestHero") && text.contains("NSKeyedArchiver"), "{text}");
        let back = runtime::send(runtime::class_named("NSKeyedUnarchiver"), "unarchiveObjectWithData:", &[data]);
        assert_ne!(back, 0, "the unarchiver read nothing");
        let field = |object: Id, name: &'static str| property_get(object, name).unwrap_or(0);
        assert_eq!(string::get(field(back, "name") as Id), "Siris");
        assert_eq!(field(back, "level") as i64, -7);
        assert_eq!(field(back, "gold") as i64, -5);
        assert_eq!(field(back, "xp"), (1u64 << 40) + 3);
        assert_eq!(field(back, "alive"), 1);
        assert_eq!(f64::from_bits(field(back, "speed")), 2.25);
        assert_eq!(f64::from_bits(field(back, "luck")), 0.75);
        assert_eq!((f64::from_bits(field(back, "spotX")), f64::from_bits(field(back, "spotY"))), (1.5, -2.0));
        let pet = field(back, "pet") as Id;
        assert_eq!(string::get(field(pet, "name") as Id), "Drake");
        assert_eq!(field(pet, "pet") as Id, back, "the loop comes back as the same object");

        // The long way: a coder writing into a mutable data, keys at the top level.
        let bytes = runtime::send(runtime::class_named("NSMutableData"), "data", &[]);
        let writer = runtime::send(runtime::alloc_instance(runtime::class_named("NSKeyedArchiver")), "initForWritingWithMutableData:", &[bytes]);
        runtime::send(writer, "encodeObject:forKey:", &[collections::array_from(&[drake]), string::make("party")]);
        runtime::send(writer, "encodeInt:forKey:", &[42, string::make("turn")]);
        runtime::send(writer, "finishEncoding", &[]);
        assert!(!crate::fw::data::bytes(bytes).is_empty(), "finishEncoding wrote nothing");
        let reader = runtime::send(runtime::alloc_instance(runtime::class_named("NSKeyedUnarchiver")), "initForReadingWithData:", &[bytes]);
        assert_eq!(runtime::send(reader, "decodeIntForKey:", &[string::make("turn")]) as i32, 42);
        assert_eq!(runtime::send(reader, "containsValueForKey:", &[string::make("nope")]) & 0xFF, 0);
        let party = collections::array_items(runtime::send(reader, "decodeObjectForKey:", &[string::make("party")]));
        assert_eq!(string::get(field(party[0], "name") as Id), "Drake");
        runtime::send(reader, "finishDecoding", &[]);
    }
}
