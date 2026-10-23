//! UINib: Apple's compiled .nib files read into live objects, the coder
//! (UINibDecoder) that hands each object its stored values, the outlet and
//! action connections, and -[NSBundle loadNibNamed:owner:options:].
//!
//! The file format ("NIBArchive"), as Xcode writes it (checked on Dungeon
//! Plunder's storyboard nibs):
//! - Header: the 10 bytes "NIBArchive", then ten little-endian u32: format
//!   version (1), coder version (9), then the count and file offset of the
//!   objects, the keys, the values and the class names.
//! - Numbers inside the tables are varints: 7 bits per byte, lowest bits
//!   first, and the LAST byte has its top bit set (the opposite of LEB128).
//! - Object: varint class index, varint index of its first value, varint
//!   value count (its values are consecutive).
//! - Key: varint length, then the UTF-8 name (no terminator).
//! - Value: varint key index, one type byte, then the payload: 0 int8,
//!   1 int16, 2 int32, 3 int64 (signed, little-endian), 4 false, 5 true (no
//!   payload), 6 float32, 7 float64, 8 data (varint length + bytes), 9 nil
//!   (no payload), 10 object reference (u32 object index).
//! - Class name: varint length (with the closing zero byte), varint count of
//!   fallback classes, that many u32 class indexes (classes to make when this
//!   one does not exist, e.g. UITableViewCellContentView -> UIView), then the
//!   name and its zero byte.
//! - Object 0 is the root, an NSObject with UINibObjectsKey (every object),
//!   UINibTopLevelObjectsKey, UINibConnectionsKey, UINibVisibleWindowsKey...
//! - Foundation objects are stored by content: NSString and NSMutableString
//!   as "NS.bytes" (UTF-8 data); NSNumber as one number value ("NS.intval");
//!   NSData as "NS.bytes"; NSArray, NSSet and NSDictionary as
//!   NSInlinedValue = true plus one UINibEncoderEmptyKey reference per item
//!   (key, value, key, value... for a dictionary).
//! - Structs (UICenter: CGPoint; UIBounds: CGRect; UIShadowOffset,
//!   UIContentSize: CGSize) are data: one type byte, 6 for 32-bit floats or
//!   7 for 64-bit floats, then the fields in order (x, y, width, height).
//!   Every struct in Dungeon Plunder's nibs uses 6; decodeCGPointForKey: and
//!   friends read either.
//! - Placeholders are UIProxyObject with UIProxiedObjectIdentifier:
//!   IBFilesOwner is the owner, IBFirstResponder is nil, any other name is
//!   looked up in options[UINibExternalObjects] (storyboards pass
//!   UIStoryboardPlaceholder and UpstreamPlaceholder-N that way).
//! - UIClassSwapper stands for an object of a custom class: UIClassName (the
//!   game's class) or, if missing, UIOriginalClassName; the made object reads
//!   the swapper's own values.

use crate::fw::geometry::{Point, Rect, Size};
use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::{collections, data, string};
use crate::objc::runtime::{self, Class, Id, Sel};
use crate::symbols::Registry;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

// ----- the reader -----

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Integer(i64),
    Bool(bool),
    Float(f32),
    Double(f64),
    Data(Vec<u8>),
    Nil,
    Object(usize),
}

pub struct ArchiveClass {
    pub name: String,
    /// Indexes of the classes to use when this one does not exist.
    pub fallbacks: Vec<usize>,
}

pub struct ArchiveObject {
    pub class: usize,
    pub values: std::ops::Range<usize>,
}

pub struct Archive {
    pub objects: Vec<ArchiveObject>,
    pub keys: Vec<String>,
    /// (key index, value)
    pub values: Vec<(usize, Value)>,
    pub classes: Vec<ArchiveClass>,
}

impl Archive {
    pub fn class_name(&self, object: usize) -> &str {
        self.objects.get(object).map(|o| self.classes[o.class].name.as_str()).unwrap_or("")
    }

    /// The object's (key, value) pairs in stored order.
    pub fn entries(&self, object: usize) -> impl Iterator<Item = (&str, &Value)> {
        let range = self.objects.get(object).map(|o| o.values.clone()).unwrap_or(0..0);
        self.values[range].iter().map(|(key, value)| (self.keys[*key].as_str(), value))
    }

    pub fn value(&self, object: usize, key: &str) -> Option<&Value> {
        self.entries(object).find(|(k, _)| *k == key).map(|(_, v)| v)
    }

    /// The text of a string object (NSString, NSMutableString...).
    fn text(&self, object: usize) -> Option<String> {
        match self.value(object, "NS.bytes") {
            Some(Value::Data(bytes)) => Some(String::from_utf8_lossy(bytes).into_owned()),
            _ => None,
        }
    }

    /// The text of the string object a key of `object` refers to.
    fn text_at(&self, object: usize, key: &str) -> Option<String> {
        match self.value(object, key) {
            Some(&Value::Object(index)) => self.text(index),
            _ => None,
        }
    }

    /// The objects an inlined collection lists (nil entries left out).
    fn items(&self, object: usize) -> Vec<usize> {
        self.entries(object)
            .filter(|(key, _)| *key == "UINibEncoderEmptyKey")
            .filter_map(|(_, value)| if let Value::Object(index) = value { Some(*index) } else { None })
            .collect()
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(count).filter(|&end| end <= self.bytes.len()).ok_or("the file ends too early")?;
        let part = &self.bytes[self.at..end];
        self.at = end;
        Ok(part)
    }

    fn byte(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn varint(&mut self) -> Result<usize, String> {
        let mut value = 0usize;
        for shift in (0..64).step_by(7) {
            let byte = self.byte()?;
            value |= ((byte & 0x7f) as usize) << shift;
            if byte & 0x80 != 0 {
                return Ok(value);
            }
        }
        Err("a number is too long".into())
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], String> {
        Ok(self.take(N)?.try_into().unwrap())
    }
}

pub fn parse(bytes: &[u8]) -> Result<Archive, String> {
    if bytes.len() < 50 || &bytes[..10] != b"NIBArchive" {
        return Err("not a NIBArchive file".into());
    }
    let word = |index: usize| u32::from_le_bytes(bytes[10 + index * 4..14 + index * 4].try_into().unwrap()) as usize;
    let (object_count, object_offset, key_count, key_offset) = (word(2), word(3), word(4), word(5));
    let (value_count, value_offset, class_count, class_offset) = (word(6), word(7), word(8), word(9));
    let reader_at = |at: usize| Reader { bytes, at };
    // Counts come from the file: a damaged one must not make huge lists up front.
    let room = |count: usize| count.min(bytes.len());

    let mut r = reader_at(class_offset);
    let mut classes = Vec::with_capacity(room(class_count));
    for _ in 0..class_count {
        let length = r.varint()?;
        let fallback_count = r.varint()?;
        let mut fallbacks = Vec::with_capacity(room(fallback_count));
        for _ in 0..fallback_count {
            fallbacks.push(u32::from_le_bytes(r.array()?) as usize);
        }
        let name = r.take(length)?;
        let name = name.strip_suffix(&[0]).unwrap_or(name);
        classes.push(ArchiveClass { name: String::from_utf8_lossy(name).into_owned(), fallbacks });
    }
    if classes.iter().any(|c| c.fallbacks.iter().any(|&f| f >= class_count)) {
        return Err("a fallback class is not in the class list".into());
    }

    let mut r = reader_at(key_offset);
    let mut keys = Vec::with_capacity(room(key_count));
    for _ in 0..key_count {
        let length = r.varint()?;
        keys.push(String::from_utf8_lossy(r.take(length)?).into_owned());
    }

    let mut r = reader_at(value_offset);
    let mut values = Vec::with_capacity(room(value_count));
    for _ in 0..value_count {
        let key = r.varint()?;
        if key >= key_count {
            return Err(format!("value {} names key {key} of {key_count}", values.len()));
        }
        let value = match r.byte()? {
            0 => Value::Integer(i8::from_le_bytes(r.array()?) as i64),
            1 => Value::Integer(i16::from_le_bytes(r.array()?) as i64),
            2 => Value::Integer(i32::from_le_bytes(r.array()?) as i64),
            3 => Value::Integer(i64::from_le_bytes(r.array()?)),
            4 => Value::Bool(false),
            5 => Value::Bool(true),
            6 => Value::Float(f32::from_le_bytes(r.array()?)),
            7 => Value::Double(f64::from_le_bytes(r.array()?)),
            8 => {
                let length = r.varint()?;
                Value::Data(r.take(length)?.to_vec())
            }
            9 => Value::Nil,
            10 => {
                let index = u32::from_le_bytes(r.array()?) as usize;
                if index >= object_count {
                    return Err(format!("a value refers to object {index} of {object_count}"));
                }
                Value::Object(index)
            }
            other => return Err(format!("value kind {other} is not known")),
        };
        values.push((key, value));
    }

    let mut r = reader_at(object_offset);
    let mut objects = Vec::with_capacity(room(object_count));
    for _ in 0..object_count {
        let class = r.varint()?;
        let first = r.varint()?;
        let count = r.varint()?;
        if class >= class_count {
            return Err(format!("object {} names class {class} of {class_count}", objects.len()));
        }
        let end = first.checked_add(count).filter(|&end| end <= value_count).ok_or("an object's values are past the end")?;
        objects.push(ArchiveObject { class, values: first..end });
    }
    if objects.is_empty() {
        return Err("the archive has no root object".into());
    }
    Ok(Archive { objects, keys, values, classes })
}

/// The fields of a struct stored as data (type byte 6: 32-bit floats, 7:
/// 64-bit floats).
pub fn struct_fields(data: &[u8]) -> Option<Vec<f64>> {
    let (&kind, rest) = data.split_first()?;
    match kind {
        6 if rest.len() % 4 == 0 => Some(rest.chunks(4).map(|c| f32::from_le_bytes(c.try_into().unwrap()) as f64).collect()),
        7 if rest.len() % 8 == 0 => Some(rest.chunks(8).map(|c| f64::from_le_bytes(c.try_into().unwrap())).collect()),
        _ => None,
    }
}

fn log_once(text: String) {
    static SEEN: Mutex<Option<HashSet<String>>> = Mutex::new(None);
    if SEEN.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashSet::new).insert(text.clone()) {
        crate::log!("{text}");
    }
}

// ----- the coder: UINibDecoder { isa, state } -----

/// One nib being turned into objects.
struct Decoding {
    archive: Arc<Archive>,
    bundle: Id,
    owner: Id,
    externals: Id,
    /// The archive object whose values the decode messages read.
    current: usize,
    /// The live object of each archive object, once made.
    made: Vec<Option<Id>>,
    /// Objects given for placeholders (left out of the top-level list and
    /// not woken).
    placeholders: Vec<Id>,
    /// Bytes handed out by decodeBytesForKey:returnedLength: (they live as
    /// long as the coder).
    kept: Vec<Vec<u8>>,
}

fn decoding(coder: Id) -> Option<&'static Mutex<Decoding>> {
    if coder == 0 {
        return None;
    }
    let decoder_class = runtime::class_named("UINibDecoder");
    let is_ours = {
        let rt = runtime::lock();
        rt.is_subclass(rt.class_of(coder), decoder_class)
    };
    let state = unsafe { *((coder + 8) as *const *const Mutex<Decoding>) };
    if !is_ours || state.is_null() {
        return None;
    }
    Some(unsafe { &*state })
}

fn with_state<R>(coder: Id, f: impl FnOnce(&mut Decoding) -> R) -> Option<R> {
    decoding(coder).map(|state| f(&mut state.lock().unwrap_or_else(|e| e.into_inner())))
}

fn new_coder(archive: Arc<Archive>, bundle: Id, owner: Id, externals: Id) -> Id {
    let count = archive.objects.len();
    let state = Decoding { archive, bundle, owner, externals, current: 0, made: vec![None; count], placeholders: Vec::new(), kept: Vec::new() };
    let coder = runtime::alloc_instance(runtime::class_named("UINibDecoder"));
    unsafe { *((coder + 8) as *mut *const Mutex<Decoding>) = Box::into_raw(Box::new(Mutex::new(state))) };
    coder
}

fn remember(coder: Id, index: usize, object: Id) {
    with_state(coder, |s| s.made[index] = Some(object));
}

/// The live object for archive object `index`, made the first time it is
/// asked for (later references get the same one).
fn object_at(coder: Id, index: usize) -> Id {
    let Some((archive, known)) = with_state(coder, |s| (s.archive.clone(), s.made.get(index).copied().flatten())) else {
        return 0;
    };
    if let Some(object) = known {
        return object;
    }
    let class = archive.class_name(index).to_string();
    let made = match class.as_str() {
        "NSString" | "NSMutableString" | "NSLocalizableString" => {
            let text = string::make(&archive.text(index).unwrap_or_default());
            if class == "NSMutableString" { runtime::send(text, "mutableCopy", &[]) } else { text }
        }
        "NSNumber" => match archive.entries(index).next().map(|(_, value)| value) {
            Some(Value::Integer(value)) => collections::number_int(*value),
            Some(Value::Bool(value)) => collections::number_bool(*value),
            Some(Value::Float(value)) => collections::number_float(*value as f64),
            Some(Value::Double(value)) => collections::number_float(*value),
            _ => collections::number_int(0),
        },
        "NSData" | "NSMutableData" => {
            let bytes = match archive.value(index, "NS.bytes") {
                Some(Value::Data(bytes)) => data::make(bytes),
                _ => data::make(&[]),
            };
            if class == "NSMutableData" { runtime::send(bytes, "mutableCopy", &[]) } else { bytes }
        }
        "NSNull" => runtime::send(runtime::class_named("NSNull"), "null", &[]),
        "NSArray" | "NSMutableArray" | "NSSet" | "NSMutableSet" | "NSDictionary" | "NSMutableDictionary" | "NSOrderedSet"
        | "NSMutableOrderedSet" => return collection(coder, &archive, index, &class),
        "UIProxyObject" => placeholder(coder, &archive, index),
        _ => return make_instance(coder, &archive, index, &class),
    };
    remember(coder, index, made);
    made
}

/// A list, set or dictionary: made and remembered empty first, then filled,
/// so an item that refers back to the collection gets the collection itself.
fn collection(coder: Id, archive: &Archive, index: usize, class_name: &str) -> Id {
    // NSOrderedSet is not one of ours: an array keeps the order.
    let made_as = match class_name {
        "NSOrderedSet" => "NSArray",
        "NSMutableOrderedSet" => "NSMutableArray",
        other => other,
    };
    let object = runtime::alloc_instance(runtime::class_named(made_as));
    remember(coder, index, object);
    let items: Vec<Id> = archive.items(index).into_iter().map(|item| object_at(coder, item)).collect();
    let filled = if made_as.ends_with("Dictionary") {
        let pairs: Vec<(Id, Id)> = items.chunks_exact(2).map(|pair| (pair[0], pair[1])).collect();
        runtime::send(object, "initWithDictionary:", &[collections::dictionary_from(&pairs)])
    } else {
        runtime::send(object, "initWithArray:", &[collections::array_from(&items)])
    };
    remember(coder, index, filled);
    filled
}

/// The object a UIProxyObject stands for.
fn placeholder(coder: Id, archive: &Archive, index: usize) -> Id {
    let identifier = archive.text_at(index, "UIProxiedObjectIdentifier").unwrap_or_default();
    let Some((owner, externals)) = with_state(coder, |s| (s.owner, s.externals)) else { return 0 };
    let object = match identifier.as_str() {
        "IBFilesOwner" => owner,
        "IBFirstResponder" => 0,
        _ => {
            let found = if externals == 0 { 0 } else { runtime::send(externals, "objectForKey:", &[string::make(&identifier)]) };
            if found == 0 {
                log_once(format!("UINib: no object given for the placeholder {identifier} (UINibExternalObjects)"));
            }
            found
        }
    };
    with_state(coder, |s| s.placeholders.push(object));
    object
}

/// +alloc of the object's class, then -initWithCoder: with this coder
/// reading the object's own values.
fn make_instance(coder: Id, archive: &Archive, index: usize, class_name: &str) -> Id {
    let names: Vec<String> = if class_name == "UIClassSwapper" {
        ["UIClassName", "UIOriginalClassName"].iter().filter_map(|key| archive.text_at(index, key)).collect()
    } else {
        let fallbacks = archive.classes[archive.objects[index].class].fallbacks.iter();
        std::iter::once(class_name.to_string()).chain(fallbacks.map(|&f| archive.classes[f].name.clone())).collect()
    };
    let Some((name, class)) = names.iter().find_map(|name| runtime::existing_class(name, false).map(|class| (name, class))) else {
        log_once(format!("UINib: no class {} (nor a fallback for it); left out", names.first().map(String::as_str).unwrap_or(class_name)));
        remember(coder, index, 0);
        return 0;
    };
    if Some(name) != names.first() {
        log_once(format!("UINib: no class {}; made a {name} instead", names[0]));
    }
    let object = runtime::send(class, "alloc", &[]);
    // A reference back to this object while it reads its values gets this one.
    remember(coder, index, object);
    let Some(outer) = with_state(coder, |s| std::mem::replace(&mut s.current, index)) else { return 0 };
    let result = if runtime::responds(object, "initWithCoder:") {
        runtime::send(object, "initWithCoder:", &[coder])
    } else {
        log_once(format!("UINib: {name} can't be read from a nib (no -initWithCoder:); made with -init"));
        runtime::send(object, "init", &[])
    };
    with_state(coder, |s| s.current = outer);
    remember(coder, index, result);
    result
}

fn value_for(coder: Id, key: Id) -> Option<Value> {
    let key = string::get(key);
    with_state(coder, |s| s.archive.value(s.current, &key).cloned()).flatten()
}

fn integer_of(value: Option<Value>) -> i64 {
    match value {
        Some(Value::Integer(value)) => value,
        Some(Value::Bool(value)) => value as i64,
        Some(Value::Float(value)) => value as i64,
        Some(Value::Double(value)) => value as i64,
        _ => 0,
    }
}

fn double_of(value: Option<Value>) -> f64 {
    match value {
        Some(Value::Float(value)) => value as f64,
        Some(Value::Double(value)) => value,
        Some(Value::Integer(value)) => value as f64,
        Some(Value::Bool(value)) => value as i64 as f64,
        _ => 0.0,
    }
}

fn fields_for(coder: Id, key: Id) -> Vec<f64> {
    match value_for(coder, key) {
        Some(Value::Data(bytes)) => struct_fields(&bytes).unwrap_or_default(),
        _ => Vec::new(),
    }
}

extern "C" fn decode_object(this: Id, _: Sel, key: Id) -> Id {
    match value_for(this, key) {
        Some(Value::Object(index)) => object_at(this, index),
        _ => 0,
    }
}

extern "C" fn decode_bool(this: Id, _: Sel, key: Id) -> bool {
    integer_of(value_for(this, key)) != 0
}

extern "C" fn decode_integer(this: Id, _: Sel, key: Id) -> i64 {
    integer_of(value_for(this, key))
}

extern "C" fn decode_int32(this: Id, _: Sel, key: Id) -> i32 {
    integer_of(value_for(this, key)) as i32
}

extern "C" fn decode_float(this: Id, _: Sel, key: Id) -> f32 {
    double_of(value_for(this, key)) as f32
}

extern "C" fn decode_double(this: Id, _: Sel, key: Id) -> f64 {
    double_of(value_for(this, key))
}

extern "C" fn decode_point(this: Id, _: Sel, key: Id) -> Point {
    match fields_for(this, key)[..] {
        [x, y, ..] => Point { x, y },
        _ => Point::default(),
    }
}

extern "C" fn decode_size(this: Id, _: Sel, key: Id) -> Size {
    match fields_for(this, key)[..] {
        [width, height, ..] => Size { width, height },
        _ => Size::default(),
    }
}

extern "C" fn decode_rect(this: Id, _: Sel, key: Id) -> Rect {
    match fields_for(this, key)[..] {
        [x, y, width, height, ..] => Rect::new(x, y, width, height),
        _ => Rect::default(),
    }
}

extern "C" fn contains_value(this: Id, _: Sel, key: Id) -> bool {
    value_for(this, key).is_some()
}

extern "C" fn decode_bytes(this: Id, _: Sel, key: Id, length: *mut usize) -> *const u8 {
    let bytes = match value_for(this, key) {
        Some(Value::Data(bytes)) => Some(bytes),
        _ => None,
    };
    if !length.is_null() {
        unsafe { *length = bytes.as_ref().map(Vec::len).unwrap_or(0) };
    }
    let Some(bytes) = bytes else { return std::ptr::null() };
    with_state(this, |s| {
        s.kept.push(bytes);
        s.kept.last().unwrap().as_ptr()
    })
    .unwrap_or(std::ptr::null())
}

extern "C" fn allows_keyed_coding(_this: Id, _: Sel) -> bool {
    true
}

// ----- connections: UIRuntimeConnection { isa, label, source, destination, event mask } -----

fn connection_fields(this: Id) -> &'static mut [usize; 4] {
    unsafe { &mut *((this + 8) as *mut [usize; 4]) }
}

extern "C" fn connection_init_with_coder(this: Id, _: Sel, coder: Id) -> Id {
    let label = runtime::send(coder, "decodeObjectForKey:", &[string::make("UILabel")]);
    let source = runtime::send(coder, "decodeObjectForKey:", &[string::make("UISource")]);
    let destination = runtime::send(coder, "decodeObjectForKey:", &[string::make("UIDestination")]);
    let mask = decode_integer(coder, 0, string::make("UIEventMask")) as usize;
    *connection_fields(this) = [label, source, destination, mask];
    this
}

extern "C" fn connection_label(this: Id, _: Sel) -> Id {
    connection_fields(this)[0]
}

extern "C" fn connection_source(this: Id, _: Sel) -> Id {
    connection_fields(this)[1]
}

extern "C" fn connection_destination(this: Id, _: Sel) -> Id {
    connection_fields(this)[2]
}

extern "C" fn connection_event_mask(this: Id, _: Sel) -> usize {
    connection_fields(this)[3]
}

/// Sets an outlet the way key-value coding does: the object's own
/// -setValue:forKey:, else the setter -setName:, else the field _name or name.
fn set_outlet(object: Id, key: &str, value: Id) {
    if object == 0 || key.is_empty() {
        return;
    }
    if runtime::responds(object, "setValue:forKey:") {
        runtime::send(object, "setValue:forKey:", &[value, string::make(key)]);
        return;
    }
    let mut letters = key.chars();
    let first = letters.next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default();
    let setter = format!("set{first}{}:", letters.as_str());
    if runtime::responds(object, &setter) {
        runtime::send(object, &setter, &[value]);
        return;
    }
    let class = runtime::lock().class_of(object);
    for field in [format!("_{key}"), key.to_string()] {
        let Ok(name) = std::ffi::CString::new(field) else { return };
        let ivar = crate::objc::reflect::get_instance_variable(class, name.as_ptr());
        if ivar != 0 {
            unsafe {
                let offset_cell = *(ivar as *const usize);
                let offset = if offset_cell == 0 { 0 } else { *(offset_cell as *const i32) as isize };
                *((object as isize + offset) as *mut Id) = if value == 0 { 0 } else { runtime::send(value, "retain", &[]) };
            }
            return;
        }
    }
    let class_name = runtime::lock().class_name(class);
    log_once(format!("UINib: outlet {key} of {class_name} has no setter or field; not set"));
}

extern "C" fn outlet_connect(this: Id, _: Sel) {
    let [label, source, destination, _] = *connection_fields(this);
    set_outlet(source, &string::get(label), destination);
}

extern "C" fn event_connect(this: Id, _: Sel) {
    let [label, source, destination, mask] = *connection_fields(this);
    let action = string::get(label);
    if source != 0 && runtime::responds(source, "addTarget:action:forControlEvents:") {
        runtime::send(source, "addTarget:action:forControlEvents:", &[destination, runtime::sel(&action), mask]);
    } else if source != 0 {
        let class_name = {
            let rt = runtime::lock();
            rt.class_name(rt.class_of(source))
        };
        log_once(format!("UINib: action {action} on {class_name}, which takes no targets; not connected"));
    }
}

// ----- UIImageNibPlaceholder: a picture named in the nib -----

/// Gives the UIImage itself: from the app (as +imageNamed:) or the nib's
/// own bundle.
extern "C" fn image_placeholder_init_with_coder(_this: Id, _: Sel, coder: Id) -> Id {
    let name = runtime::send(coder, "decodeObjectForKey:", &[string::make("UIResourceName")]);
    if name == 0 {
        return 0;
    }
    let bundle = with_state(coder, |s| s.bundle).unwrap_or(0);
    let folder = if bundle == 0 { String::new() } else { string::get(runtime::send(bundle, "bundlePath", &[])) };
    let image_class = runtime::class_named("UIImage");
    if bundle == 0 || crate::loader::try_app_dir().is_some_and(|app| app.trim_end_matches('/') == folder.trim_end_matches('/')) {
        return runtime::send(image_class, "imageNamed:", &[name]);
    }
    let name = string::get(name);
    let (stem, kind) = match name.rsplit_once('.') {
        Some((stem, kind)) if !stem.is_empty() => (stem.to_string(), kind.to_string()),
        _ => (name.clone(), "png".to_string()),
    };
    for place in ["", "en.lproj/"] {
        for file in [format!("{stem}@2x.{kind}"), format!("{stem}.{kind}")] {
            let path = format!("{folder}/{place}{file}");
            if std::path::Path::new(&path).is_file() {
                let image = runtime::send(image_class, "alloc", &[]);
                return runtime::send(image, "initWithContentsOfFile:", &[string::make(&path)]);
            }
        }
    }
    log_once(format!("UINib: picture {name} is not in {folder}"));
    0
}

// ----- UINib { isa, file } -----

struct NibFile {
    archive: Arc<Archive>,
    bundle: Id,
}

fn nib_file(nib: Id) -> Option<&'static NibFile> {
    let file = unsafe { *((nib + 8) as *const *const NibFile) };
    (nib != 0 && !file.is_null()).then(|| unsafe { &*file })
}

fn set_nib_file(nib: Id, archive: Archive, bundle: Id) -> Id {
    unsafe { *((nib + 8) as *mut *const NibFile) = Box::into_raw(Box::new(NibFile { archive: Arc::new(archive), bundle })) };
    nib
}

fn nib_from_bytes(bytes: &[u8], bundle: Id, place: &str) -> Id {
    match parse(bytes) {
        Ok(archive) => set_nib_file(runtime::alloc_instance(runtime::class_named("UINib")), archive, bundle),
        Err(error) => {
            crate::log!("UINib: {place} can't be read: {error}");
            0
        }
    }
}

/// The bytes of nib `name` in a bundle folder: Name~iphone.nib (or ~ipad)
/// before Name.nib, the folder itself before en.lproj, English.lproj and
/// Base.lproj. The name may have folders in it (a storyboard asks for
/// "Main.storyboardc/UIViewController-1"). A nib that is a folder holds
/// runtime.nib (or objects-*.nib).
fn find_nib(folder: &str, name: &str) -> Option<(String, Vec<u8>)> {
    let name = name.strip_suffix(".nib").unwrap_or(name);
    let device = if crate::libsystem::io::device_model().starts_with("iPad") { "~ipad" } else { "~iphone" };
    let places: Vec<String> = if name.starts_with('/') {
        vec![String::new()]
    } else {
        ["", "en.lproj/", "English.lproj/", "Base.lproj/"].iter().map(|p| format!("{folder}/{p}")).collect()
    };
    for place in &places {
        for file in [format!("{name}{device}.nib"), format!("{name}.nib")] {
            let path = std::path::PathBuf::from(format!("{place}{file}"));
            let inner = if path.is_dir() {
                let mut files: Vec<_> = std::fs::read_dir(&path)
                    .ok()?
                    .filter_map(|entry| entry.ok().map(|e| e.path()))
                    .filter(|p| p.extension().is_some_and(|e| e == "nib"))
                    .collect();
                files.sort();
                let runtime_nib = path.join("runtime.nib");
                if runtime_nib.is_file() { Some(runtime_nib) } else { files.pop() }
            } else {
                path.is_file().then_some(path)
            };
            if let Some(found) = inner {
                if let Ok(bytes) = std::fs::read(&found) {
                    return Some((found.to_string_lossy().into_owned(), bytes));
                }
            }
        }
    }
    None
}

extern "C" fn nib_with_name(_: Class, _: Sel, name: Id, bundle: Id) -> Id {
    let bundle = if bundle == 0 { runtime::send(runtime::class_named("NSBundle"), "mainBundle", &[]) } else { bundle };
    let folder = string::get(runtime::send(bundle, "bundlePath", &[]));
    let name = string::get(name);
    match find_nib(&folder, &name) {
        Some((path, bytes)) => nib_from_bytes(&bytes, bundle, &path),
        None => {
            crate::log!("UINib: no nib named {name} in {folder}");
            0
        }
    }
}

extern "C" fn nib_with_data(_: Class, _: Sel, bytes: Id, bundle: Id) -> Id {
    nib_from_bytes(&data::bytes(bytes), bundle, "nib data")
}

/// A nib stored inside a nib (archiveData), for a table's cells.
extern "C" fn nib_init_with_coder(this: Id, _: Sel, coder: Id) -> Id {
    let bytes = runtime::send(coder, "decodeObjectForKey:", &[string::make("archiveData")]);
    let bundle = with_state(coder, |s| s.bundle).unwrap_or(0);
    match parse(&data::bytes(bytes)) {
        Ok(archive) => set_nib_file(this, archive, bundle),
        Err(error) => {
            crate::log!("UINib: a nib inside a nib can't be read: {error}");
            0
        }
    }
}

/// Makes the nib's objects: placeholders resolved, outlets and actions
/// connected, then -awakeFromNib to each object; gives the top-level objects.
fn instantiate(file: &NibFile, owner: Id, options: Id) -> Id {
    let externals = if options == 0 { 0 } else { runtime::send(options, "objectForKey:", &[string::make("UINibExternalObjects")]) };
    let coder = new_coder(file.archive.clone(), file.bundle, owner, externals);
    let root = |key: &str| collections::array_items(decode_object(coder, 0, string::make(key)));
    let objects = root("UINibObjectsKey");
    let top = root("UINibTopLevelObjectsKey");
    let connections = root("UINibConnectionsKey");
    let windows = root("UINibVisibleWindowsKey");
    for connection in connections {
        if runtime::responds(connection, "connect") {
            runtime::send(connection, "connect", &[]);
        }
    }
    let placeholders = with_state(coder, |s| s.placeholders.clone()).unwrap_or_default();
    let real = |object: &Id| *object != 0 && !placeholders.contains(object);
    for object in objects.iter().filter(|o| real(o)) {
        if runtime::responds(*object, "awakeFromNib") {
            runtime::send(*object, "awakeFromNib", &[]);
        }
    }
    for window in windows.iter().filter(|o| real(o)) {
        if runtime::responds(*window, "makeKeyAndVisible") {
            runtime::send(*window, "makeKeyAndVisible", &[]);
        }
    }
    let top: Vec<Id> = top.into_iter().filter(|o| real(o)).collect();
    finish_coder(coder);
    crate::fw::counted::autorelease(collections::array_from(&top))
}

/// Lets a coder go once its nib is made: its state is dropped (a late
/// message reads nothing; bytes it handed out are gone, as on Apple) and it
/// is announced as finished, so the 32-bit bridge frees its guest copies.
fn finish_coder(coder: Id) {
    let slot = (coder + 8) as *mut *mut Mutex<Decoding>;
    let state = unsafe { std::ptr::replace(slot, std::ptr::null_mut()) };
    if !state.is_null() {
        drop(unsafe { Box::from_raw(state) });
    }
    runtime::object_finished(coder);
}

extern "C" fn nib_instantiate(this: Id, _: Sel, owner: Id, options: Id) -> Id {
    match nib_file(this) {
        Some(file) => instantiate(file, owner, options),
        None => 0,
    }
}

extern "C" fn bundle_load_nib(this: Id, _: Sel, name: Id, owner: Id, options: Id) -> Id {
    let nib = nib_with_name(0, 0, name, this);
    if nib == 0 {
        return 0;
    }
    nib_instantiate(nib, 0, owner, options)
}

pub fn register(_: &mut Registry) {
    if runtime::class_named("NSCoder") == 0 {
        define_class("NSCoder", "NSObject", 8);
    }
    let decoder = define_class("UINibDecoder", "NSCoder", 16);
    methods(
        decoder,
        &[
            ("decodeObjectForKey:", decode_object as usize),
            ("decodeBoolForKey:", decode_bool as usize),
            ("decodeIntegerForKey:", decode_integer as usize),
            ("decodeInt64ForKey:", decode_integer as usize),
            ("decodeIntForKey:", decode_int32 as usize),
            ("decodeInt32ForKey:", decode_int32 as usize),
            ("decodeFloatForKey:", decode_float as usize),
            ("decodeDoubleForKey:", decode_double as usize),
            ("decodeCGPointForKey:", decode_point as usize),
            ("decodeCGSizeForKey:", decode_size as usize),
            ("decodeCGRectForKey:", decode_rect as usize),
            ("containsValueForKey:", contains_value as usize),
            ("decodeBytesForKey:returnedLength:", decode_bytes as usize),
            ("allowsKeyedCoding", allows_keyed_coding as usize),
        ],
    );

    let connection = define_class("UIRuntimeConnection", "NSObject", 40);
    methods(
        connection,
        &[
            ("initWithCoder:", connection_init_with_coder as usize),
            ("label", connection_label as usize),
            ("source", connection_source as usize),
            ("destination", connection_destination as usize),
        ],
    );
    let outlet = define_class("UIRuntimeOutletConnection", "UIRuntimeConnection", 40);
    methods(outlet, &[("connect", outlet_connect as usize)]);
    let event = define_class("UIRuntimeEventConnection", "UIRuntimeConnection", 40);
    methods(event, &[("connect", event_connect as usize), ("eventMask", connection_event_mask as usize)]);

    let image_placeholder = define_class("UIImageNibPlaceholder", "NSObject", 8);
    methods(image_placeholder, &[("initWithCoder:", image_placeholder_init_with_coder as usize)]);

    let nib = define_class("UINib", "NSObject", 16);
    class_methods(nib, &[("nibWithNibName:bundle:", nib_with_name as usize), ("nibWithData:bundle:", nib_with_data as usize)]);
    methods(nib, &[("initWithCoder:", nib_init_with_coder as usize), ("instantiateWithOwner:options:", nib_instantiate as usize)]);
    methods(runtime::class_named("NSBundle"), &[("loadNibNamed:owner:options:", bundle_load_nib as usize)]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fw::{collections, data, object, string};
    use crate::objc::runtime::{self, ClassObject};
    use crate::symbols::{leak, leak_cstr};
    use std::collections::HashMap;
    use std::sync::Mutex;

    // ----- writing archives for the tests, laid out as Apple's compiler does -----

    #[derive(Clone)]
    enum Put {
        I8(i8),
        I16(i16),
        I32(i32),
        I64(i64),
        False,
        True,
        F32(f32),
        F64(f64),
        Data(Vec<u8>),
        Nil,
        Ref(usize),
    }

    /// Object 0 is the root (set with `root`); others are numbered in the
    /// order they are added.
    struct Builder {
        objects: Vec<(String, Vec<String>, Vec<(String, Put)>)>,
    }

    impl Builder {
        fn new() -> Builder {
            Builder { objects: vec![("NSObject".into(), Vec::new(), Vec::new())] }
        }

        fn object(&mut self, class: &str, values: Vec<(&str, Put)>) -> usize {
            self.object_with_fallbacks(class, &[], values)
        }

        fn object_with_fallbacks(&mut self, class: &str, fallbacks: &[&str], values: Vec<(&str, Put)>) -> usize {
            let values = values.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
            self.objects.push((class.into(), fallbacks.iter().map(|f| f.to_string()).collect(), values));
            self.objects.len() - 1
        }

        fn root(&mut self, values: Vec<(&str, Put)>) {
            self.objects[0].2 = values.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        }

        fn string(&mut self, text: &str) -> usize {
            self.object("NSString", vec![("NS.bytes", Put::Data(text.as_bytes().to_vec()))])
        }

        fn list(&mut self, class: &str, entries: &[usize]) -> usize {
            let mut values = vec![("NSInlinedValue", Put::True)];
            values.extend(entries.iter().map(|&e| ("UINibEncoderEmptyKey", Put::Ref(e))));
            self.object(class, values)
        }

        fn proxy(&mut self, identifier: &str) -> usize {
            let name = self.string(identifier);
            self.object("UIProxyObject", vec![("UIProxiedObjectIdentifier", Put::Ref(name))])
        }

        /// The usual root: every object, the top-level ones and the connections.
        fn standard_root(&mut self, objects: &[usize], top: &[usize], connections: &[usize]) {
            let objects = self.list("NSArray", objects);
            let top = self.list("NSArray", top);
            let connections = self.list("NSArray", connections);
            let empty = self.list("NSArray", &[]);
            self.root(vec![
                ("UINibTopLevelObjectsKey", Put::Ref(top)),
                ("UINibObjectsKey", Put::Ref(objects)),
                ("UINibConnectionsKey", Put::Ref(connections)),
                ("UINibVisibleWindowsKey", Put::Ref(empty)),
            ]);
        }

        fn bytes(&self) -> Vec<u8> {
            fn varint(out: &mut Vec<u8>, mut value: usize) {
                loop {
                    let low = (value & 0x7f) as u8;
                    value >>= 7;
                    if value == 0 {
                        out.push(low | 0x80);
                        return;
                    }
                    out.push(low);
                }
            }
            let mut class_names: Vec<(String, Vec<String>)> = Vec::new();
            let mut keys: Vec<String> = Vec::new();
            let class_index = |names: &mut Vec<(String, Vec<String>)>, name: &str, fallbacks: &[String]| {
                match names.iter().position(|(n, _)| n == name) {
                    Some(index) => index,
                    None => {
                        names.push((name.to_string(), fallbacks.to_vec()));
                        names.len() - 1
                    }
                }
            };
            for (class, fallbacks, _) in &self.objects {
                for fallback in fallbacks {
                    class_index(&mut class_names, fallback, &[]);
                }
                class_index(&mut class_names, class, fallbacks);
            }
            let (mut objects, mut values) = (Vec::new(), Vec::new());
            let mut value_count = 0;
            for (class, fallbacks, list) in &self.objects {
                varint(&mut objects, class_index(&mut class_names, class, fallbacks));
                varint(&mut objects, value_count);
                varint(&mut objects, list.len());
                for (key, put) in list {
                    let key_index = keys.iter().position(|k| k == key).unwrap_or_else(|| {
                        keys.push(key.clone());
                        keys.len() - 1
                    });
                    varint(&mut values, key_index);
                    match put {
                        Put::I8(v) => values.extend([0, *v as u8]),
                        Put::I16(v) => (values.push(1), values.extend(v.to_le_bytes())).1,
                        Put::I32(v) => (values.push(2), values.extend(v.to_le_bytes())).1,
                        Put::I64(v) => (values.push(3), values.extend(v.to_le_bytes())).1,
                        Put::False => values.push(4),
                        Put::True => values.push(5),
                        Put::F32(v) => (values.push(6), values.extend(v.to_le_bytes())).1,
                        Put::F64(v) => (values.push(7), values.extend(v.to_le_bytes())).1,
                        Put::Data(bytes) => {
                            values.push(8);
                            varint(&mut values, bytes.len());
                            values.extend(bytes);
                        }
                        Put::Nil => values.push(9),
                        Put::Ref(index) => (values.push(10), values.extend((*index as u32).to_le_bytes())).1,
                    }
                    value_count += 1;
                }
            }
            let mut key_bytes = Vec::new();
            for key in &keys {
                varint(&mut key_bytes, key.len());
                key_bytes.extend(key.as_bytes());
            }
            let mut class_bytes = Vec::new();
            for (name, fallbacks) in &class_names {
                varint(&mut class_bytes, name.len() + 1);
                varint(&mut class_bytes, fallbacks.len());
                for fallback in fallbacks {
                    let index = class_names.iter().position(|(n, _)| n == fallback).unwrap() as u32;
                    class_bytes.extend(index.to_le_bytes());
                }
                class_bytes.extend(name.as_bytes());
                class_bytes.push(0);
            }
            let object_offset = 50;
            let key_offset = object_offset + objects.len();
            let value_offset = key_offset + key_bytes.len();
            let class_offset = value_offset + values.len();
            let mut out = b"NIBArchive".to_vec();
            for word in [
                1,
                9,
                self.objects.len(),
                object_offset,
                keys.len(),
                key_offset,
                value_count,
                value_offset,
                class_names.len(),
                class_offset,
            ] {
                out.extend((word as u32).to_le_bytes());
            }
            out.extend(objects);
            out.extend(key_bytes);
            out.extend(values);
            out.extend(class_bytes);
            out
        }
    }

    fn floats(prefix: u8, list: &[f32]) -> Vec<u8> {
        let mut out = vec![prefix];
        for v in list {
            out.extend(v.to_le_bytes());
        }
        out
    }

    // ----- the reader -----

    #[test]
    fn nib_reader_reads_every_value_kind() {
        let mut b = Builder::new();
        let thing = b.object_with_fallbacks("NibTestThing", &["UIView"], vec![("name", Put::Data(b"x".to_vec()))]);
        b.root(vec![
            ("small", Put::I8(-5)),
            ("medium", Put::I16(300)),
            ("large", Put::I32(-70000)),
            ("huge", Put::I64(1 << 40)),
            ("no", Put::False),
            ("yes", Put::True),
            ("single", Put::F32(1.5)),
            ("double", Put::F64(2.25)),
            ("UICenter", Put::Data(floats(6, &[1.0, 2.0]))),
            ("nothing", Put::Nil),
            ("thing", Put::Ref(thing)),
        ]);
        let archive = parse(&b.bytes()).expect("archive reads");
        assert_eq!(archive.objects.len(), 2);
        assert_eq!(archive.value(0, "small"), Some(&Value::Integer(-5)));
        assert_eq!(archive.value(0, "medium"), Some(&Value::Integer(300)));
        assert_eq!(archive.value(0, "large"), Some(&Value::Integer(-70000)));
        assert_eq!(archive.value(0, "huge"), Some(&Value::Integer(1 << 40)));
        assert_eq!(archive.value(0, "no"), Some(&Value::Bool(false)));
        assert_eq!(archive.value(0, "yes"), Some(&Value::Bool(true)));
        assert_eq!(archive.value(0, "single"), Some(&Value::Float(1.5)));
        assert_eq!(archive.value(0, "double"), Some(&Value::Double(2.25)));
        assert_eq!(archive.value(0, "nothing"), Some(&Value::Nil));
        assert_eq!(archive.value(0, "thing"), Some(&Value::Object(1)));
        assert_eq!(archive.value(0, "missing"), None);
        assert_eq!(archive.class_name(0), "NSObject");
        assert_eq!(archive.class_name(1), "NibTestThing");
        let class = &archive.classes[archive.objects[1].class];
        let fallbacks: Vec<&str> = class.fallbacks.iter().map(|&f| archive.classes[f].name.as_str()).collect();
        assert_eq!(fallbacks, ["UIView"]);
        let Some(Value::Data(center)) = archive.value(0, "UICenter") else { panic!("UICenter is data") };
        assert_eq!(struct_fields(center), Some(vec![1.0, 2.0]));
        // Marker 7: 64-bit floats.
        let mut doubles = vec![7];
        doubles.extend(3.5f64.to_le_bytes());
        doubles.extend((-1.0f64).to_le_bytes());
        assert_eq!(struct_fields(&doubles), Some(vec![3.5, -1.0]));
        assert_eq!(struct_fields(&[9, 0, 0]), None);
        assert_eq!(struct_fields(&[6, 0, 0]), None, "not a whole number of fields");
    }

    #[test]
    fn nib_reader_rejects_damaged_archives() {
        let mut b = Builder::new();
        let name = b.string("hello");
        b.root(vec![("name", Put::Ref(name))]);
        let good = b.bytes();
        assert!(parse(&good).is_ok());
        let mut wrong_magic = good.clone();
        wrong_magic[0] = b'X';
        assert!(parse(&wrong_magic).is_err());
        assert!(parse(&good[..good.len() - 3]).is_err());
        assert!(parse(&good[..20]).is_err());
        let mut b = Builder::new();
        b.root(vec![("name", Put::Ref(99))]);
        assert!(parse(&b.bytes()).is_err(), "a reference past the last object is refused");
    }

    const VIEW_CONTROLLER_NIB: &[u8] = include_bytes!("../../tests/data/UIViewController-cD2-js-hwE.nib");
    const VIEW_NIB: &[u8] = include_bytes!("../../tests/data/cD2-js-hwE-view-QKQ-Dp-xpW.nib");

    fn text_of(archive: &Archive, value: Option<&Value>) -> String {
        let Some(&Value::Object(index)) = value else { panic!("not an object: {value:?}") };
        let Some(Value::Data(bytes)) = archive.value(index, "NS.bytes") else { panic!("not a string") };
        String::from_utf8(bytes.clone()).unwrap()
    }

    /// A storyboard's view controller nib from Dungeon Plunder.
    #[test]
    fn nib_reader_reads_a_real_view_controller_nib() {
        let archive = parse(VIEW_CONTROLLER_NIB).expect("real nib reads");
        assert_eq!(archive.objects.len(), 22);
        assert_eq!(archive.classes.len(), 7);
        for key in ["UINibTopLevelObjectsKey", "UINibObjectsKey", "UINibConnectionsKey", "UINibVisibleWindowsKey"] {
            assert!(matches!(archive.value(0, key), Some(Value::Object(_))), "root has {key}");
        }
        assert_eq!(archive.class_name(2), "UIClassSwapper");
        assert_eq!(text_of(&archive, archive.value(2, "UIClassName")), "HardcoreDeathViewController");
        assert_eq!(text_of(&archive, archive.value(2, "UIOriginalClassName")), "UIViewController");
        assert_eq!(text_of(&archive, archive.value(2, "UINibName")), "cD2-js-hwE-view-QKQ-Dp-xpW");
    }

    /// Its view nib: positions and sizes are data with 32-bit floats.
    #[test]
    fn nib_reader_reads_a_real_view_nib() {
        let archive = parse(VIEW_NIB).expect("real nib reads");
        assert_eq!(archive.objects.len(), 94);
        assert_eq!(archive.class_name(71), "UIView");
        let fields = |object: usize, key: &str| match archive.value(object, key) {
            Some(Value::Data(bytes)) => struct_fields(bytes).unwrap(),
            other => panic!("{key} is {other:?}"),
        };
        assert_eq!(fields(71, "UIBounds"), [0.0, 0.0, 480.0, 320.0]);
        assert_eq!(fields(71, "UICenter"), [240.0, 160.0]);
        assert_eq!(archive.class_name(52), "UILabel");
        assert_eq!(fields(52, "UIShadowOffset"), [0.0, -1.0]);
        assert_eq!(archive.value(50, "UIAlpha"), Some(&Value::Float(0.8)));
        assert_eq!(archive.value(71, "UIAutoresizingMask"), Some(&Value::Integer(36)));
        assert_eq!(archive.value(28, "UIFontPointSize"), Some(&Value::Double(15.0)));
    }

    // ----- test classes that record what they decoded -----

    #[derive(Default, Clone)]
    struct Record {
        count: i64,
        int32: i32,
        flag: bool,
        alpha: f32,
        size: f64,
        center: Point,
        precise: Point,
        bounds: Rect,
        offset: Size,
        title: String,
        items: Vec<i64>,
        table: Vec<(String, String)>,
        shared: Id,
        image: Id,
        has_count: bool,
        has_missing: bool,
        bytes: Vec<u8>,
        partner: Id,
        thing: Id,
        /// Order of -awakeFromNib among all objects (0: never woken).
        awake_order: usize,
        /// The coder its -initWithCoder: was given.
        coder: Id,
        partner_when_woken: Id,
        targets: Vec<(Id, String, usize)>,
    }

    static RECORDS: Mutex<Option<HashMap<Id, Record>>> = Mutex::new(None);
    static AWAKE_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn record(object: Id) -> Record {
        RECORDS.lock().unwrap().get_or_insert_with(HashMap::new).get(&object).cloned().unwrap_or_default()
    }

    fn change(object: Id, f: impl FnOnce(&mut Record)) {
        f(RECORDS.lock().unwrap().get_or_insert_with(HashMap::new).entry(object).or_default())
    }

    fn ask<R>(coder: Id, selector: &str, key: &str) -> R {
        let imp = runtime::imp_for(coder, runtime::sel(selector)).unwrap_or_else(|| panic!("the coder can't answer {selector}"));
        let f: extern "C" fn(Id, Sel, Id) -> R = unsafe { std::mem::transmute_copy(&imp) };
        f(coder, runtime::sel(selector), string::make(key))
    }

    extern "C" fn recorder_init_with_coder(this: Id, _: Sel, coder: Id) -> Id {
        let mut r = record(this);
        r.count = ask::<i64>(coder, "decodeIntegerForKey:", "count");
        r.int32 = ask::<i32>(coder, "decodeInt32ForKey:", "count");
        r.flag = ask::<bool>(coder, "decodeBoolForKey:", "flag");
        r.alpha = ask::<f32>(coder, "decodeFloatForKey:", "alpha");
        r.size = ask::<f64>(coder, "decodeDoubleForKey:", "size");
        r.center = ask::<Point>(coder, "decodeCGPointForKey:", "UICenter");
        r.precise = ask::<Point>(coder, "decodeCGPointForKey:", "precise");
        r.bounds = ask::<Rect>(coder, "decodeCGRectForKey:", "UIBounds");
        r.offset = ask::<Size>(coder, "decodeCGSizeForKey:", "UIShadowOffset");
        let title = ask::<Id>(coder, "decodeObjectForKey:", "title");
        r.title = if title == 0 { String::new() } else { string::get(title) };
        r.items = collections::array_items(ask::<Id>(coder, "decodeObjectForKey:", "items"))
            .into_iter()
            .map(collections::number_as_i64)
            .collect();
        let table = ask::<Id>(coder, "decodeObjectForKey:", "table");
        r.table = collections::dictionary_pairs(table).into_iter().map(|(k, v)| (string::get(k), string::get(v))).collect();
        r.shared = ask::<Id>(coder, "decodeObjectForKey:", "shared");
        r.image = ask::<Id>(coder, "decodeObjectForKey:", "image");
        r.coder = coder;
        r.has_count = ask::<bool>(coder, "containsValueForKey:", "count");
        r.has_missing = ask::<bool>(coder, "containsValueForKey:", "missing");
        let blob = ask::<Id>(coder, "decodeObjectForKey:", "missing");
        assert_eq!(blob, 0);
        let imp = runtime::imp_for(coder, runtime::sel("decodeBytesForKey:returnedLength:")).expect("decodeBytesForKey:returnedLength:");
        let f: extern "C" fn(Id, Sel, Id, *mut usize) -> *const u8 = unsafe { std::mem::transmute(imp) };
        let mut length = 0usize;
        let pointer = f(coder, runtime::sel("decodeBytesForKey:returnedLength:"), string::make("blob"), &mut length);
        r.bytes = if pointer.is_null() { Vec::new() } else { unsafe { std::slice::from_raw_parts(pointer, length) }.to_vec() };
        change(this, |old| *old = Record { awake_order: old.awake_order, ..r });
        this
    }

    extern "C" fn recorder_set_partner(this: Id, _: Sel, partner: Id) {
        change(this, |r| r.partner = partner);
    }

    extern "C" fn recorder_set_thing(this: Id, _: Sel, thing: Id) {
        change(this, |r| r.thing = thing);
    }

    extern "C" fn recorder_awake(this: Id, _: Sel) {
        let order = AWAKE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        change(this, |r| {
            r.awake_order = order;
            r.partner_when_woken = r.partner;
        });
    }

    extern "C" fn recorder_add_target(this: Id, _: Sel, target: Id, action: Sel, events: usize) {
        let name = runtime::lock().sel_name(action);
        change(this, |r| r.targets.push((target, name, events)));
    }

    /// An object with a field named `_extra` and no setter for it.
    fn fielded_class() -> Class {
        let class = object::define_class("NibTestFielded", "NSObject", 24);
        unsafe {
            let ro = (*(class as *const ClassObject)).data & !7;
            if *((ro + 48) as *const usize) == 0 {
                // ivar_list_t { entsize 32, count 1 } + ivar_t { offset*, name, type, alignment 3, size 8 }
                let ivars = leak([32 | (1usize << 32), leak(16i32), leak_cstr("_extra"), leak_cstr("@"), 3 | (8usize << 32)]);
                *((ro + 48) as *mut usize) = ivars;
            }
        }
        class
    }

    fn test_classes() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            crate::run::setup();
            let recorder = object::define_class("NibTestRecorder", "NSObject", 16);
            object::methods(
                recorder,
                &[
                    ("initWithCoder:", recorder_init_with_coder as usize),
                    ("setPartner:", recorder_set_partner as usize),
                    ("setThing:", recorder_set_thing as usize),
                    ("awakeFromNib", recorder_awake as usize),
                    ("addTarget:action:forControlEvents:", recorder_add_target as usize),
                ],
            );
            object::define_class("NibTestRecorderChild", "NibTestRecorder", 16);
            // The real nib's controller class, made here as a recorder.
            object::define_class("HardcoreDeathViewController", "NibTestRecorder", 16);
            let owner = object::define_class("NibTestOwner", "NSObject", 16);
            object::methods(
                owner,
                &[("setThing:", recorder_set_thing as usize), ("setSceneViewController:", recorder_set_partner as usize)],
            );
            object::methods(runtime::class_named("HardcoreDeathViewController"), &[("setStoryboard:", recorder_set_thing as usize)]);
            fielded_class();
        });
    }

    fn new_object(class: &str) -> Id {
        let object = runtime::send(runtime::class_named(class), "alloc", &[]);
        runtime::send(object, "init", &[])
    }

    fn class_of(object: Id) -> String {
        let rt = runtime::lock();
        rt.class_name(rt.class_of(object))
    }

    fn nib_from(bytes: &[u8]) -> Id {
        let nib = runtime::send(runtime::class_named("UINib"), "nibWithData:bundle:", &[data::make(bytes), 0]);
        assert_ne!(nib, 0, "+[UINib nibWithData:bundle:] gives a nib");
        nib
    }

    // ----- the coder -----

    #[test]
    fn nib_coder_answers_every_decode_message() {
        test_classes();
        let mut b = Builder::new();
        let title = b.string("hello");
        let three = b.object("NSNumber", vec![("NS.intval", Put::I8(3))]);
        let four = b.object("NSNumber", vec![("NS.intval", Put::I32(4))]);
        let items = b.list("NSMutableArray", &[three, four]);
        let key = b.string("key");
        let value = b.object("NSMutableString", vec![("NS.bytes", Put::Data(b"value".to_vec()))]);
        let table = b.list("NSDictionary", &[key, value]);
        let shared = b.object("NibTestRecorder", vec![]);
        let mut precise = vec![7];
        precise.extend(0.25f64.to_le_bytes());
        precise.extend(0.75f64.to_le_bytes());
        let first = b.object(
            "NibTestRecorder",
            vec![
                ("count", Put::I32(7)),
                ("flag", Put::True),
                ("alpha", Put::F32(0.5)),
                ("size", Put::F64(12.5)),
                ("UICenter", Put::Data(floats(6, &[10.0, 20.0]))),
                ("precise", Put::Data(precise)),
                ("UIBounds", Put::Data(floats(6, &[0.0, 0.0, 100.0, 50.0]))),
                ("UIShadowOffset", Put::Data(floats(6, &[1.0, -1.0]))),
                ("title", Put::Ref(title)),
                ("items", Put::Ref(items)),
                ("table", Put::Ref(table)),
                ("shared", Put::Ref(shared)),
                ("blob", Put::Data(vec![1, 2, 3])),
            ],
        );
        let second = b.object("NibTestRecorder", vec![("shared", Put::Ref(shared)), ("count", Put::I8(2)), ("flag", Put::False)]);
        b.standard_root(&[first, second, shared], &[first, second], &[]);
        let nib = nib_from(&b.bytes());
        let top = runtime::send(nib, "instantiateWithOwner:options:", &[0, 0]);
        let top = collections::array_items(top);
        assert_eq!(top.len(), 2);
        let (a, b) = (record(top[0]), record(top[1]));
        assert_eq!(class_of(top[0]), "NibTestRecorder");
        assert_eq!((a.count, a.int32, a.flag, a.alpha, a.size), (7, 7, true, 0.5, 12.5));
        assert_eq!(a.center, Point { x: 10.0, y: 20.0 });
        assert_eq!(a.precise, Point { x: 0.25, y: 0.75 });
        assert_eq!(a.bounds, Rect::new(0.0, 0.0, 100.0, 50.0));
        assert_eq!(a.offset, Size { width: 1.0, height: -1.0 });
        assert_eq!(a.title, "hello");
        assert_eq!(a.items, [3, 4]);
        assert_eq!(a.table, [("key".to_string(), "value".to_string())]);
        assert_eq!(a.bytes, [1, 2, 3]);
        assert!(a.has_count && !a.has_missing);
        // One archive object is one live object, however often it is named.
        assert_ne!(a.shared, 0);
        assert_eq!(a.shared, b.shared);
        assert_eq!(class_of(a.shared), "NibTestRecorder");
        assert_eq!((b.count, b.flag), (2, false));
        assert_eq!(b.title, "");
        assert!(a.awake_order != 0 && b.awake_order != 0 && record(a.shared).awake_order != 0);
    }

    #[test]
    fn nib_resolves_placeholders_connects_and_wakes() {
        test_classes();
        let mut b = Builder::new();
        let owner_proxy = b.proxy("IBFilesOwner");
        let outside_proxy = b.proxy("Upstream-1");
        let responder_proxy = b.proxy("IBFirstResponder");
        let first = b.object("NibTestRecorder", vec![]);
        let second = b.object("NibTestRecorder", vec![]);
        let child_name = b.string("NibTestRecorderChild");
        let original_name = b.string("NibTestRecorder");
        let swapped = b.object("UIClassSwapper", vec![("UIClassName", Put::Ref(child_name)), ("UIOriginalClassName", Put::Ref(original_name))]);
        let missing_name = b.string("NibTestNoSuchClass");
        let swapped_back =
            b.object("UIClassSwapper", vec![("UIClassName", Put::Ref(missing_name)), ("UIOriginalClassName", Put::Ref(original_name))]);
        let fallen_back = b.object_with_fallbacks("NibTestAlsoMissing", &["NibTestRecorder"], vec![]);
        let fielded = b.object("NibTestFielded", vec![]);
        let partner = b.string("partner");
        let thing = b.string("thing");
        let extra = b.string("extra");
        let tapped = b.string("tapped:");
        let outlet = |b: &mut Builder, label: usize, source: usize, destination: usize| {
            b.object(
                "UIRuntimeOutletConnection",
                vec![("UILabel", Put::Ref(label)), ("UISource", Put::Ref(source)), ("UIDestination", Put::Ref(destination))],
            )
        };
        let connections = vec![
            outlet(&mut b, partner, first, second),
            outlet(&mut b, thing, owner_proxy, first),
            outlet(&mut b, partner, second, outside_proxy),
            outlet(&mut b, extra, fielded, first),
            b.object(
                "UIRuntimeEventConnection",
                vec![
                    ("UILabel", Put::Ref(tapped)),
                    ("UISource", Put::Ref(second)),
                    ("UIDestination", Put::Ref(owner_proxy)),
                    ("UIEventMask", Put::I8(64)),
                ],
            ),
            b.object(
                "UIRuntimeEventConnection",
                vec![
                    ("UILabel", Put::Ref(tapped)),
                    ("UISource", Put::Ref(first)),
                    ("UIDestination", Put::Ref(responder_proxy)),
                    ("UIEventMask", Put::I16(4096)),
                ],
            ),
        ];
        b.standard_root(
            &[owner_proxy, outside_proxy, responder_proxy, first, second, swapped, swapped_back, fallen_back, fielded],
            &[owner_proxy, first, swapped, swapped_back, fallen_back],
            &connections,
        );
        let owner = new_object("NibTestOwner");
        let outside = new_object("NibTestRecorder");
        let externals = collections::dictionary_from(&[(string::make("Upstream-1"), outside)]);
        let options = collections::dictionary_from(&[(string::make("UINibExternalObjects"), externals)]);
        let nib = nib_from(&b.bytes());
        let top = collections::array_items(runtime::send(nib, "instantiateWithOwner:options:", &[owner, options]));
        // Placeholders are not in the top-level list.
        assert_eq!(top.len(), 4);
        assert_eq!(class_of(top[0]), "NibTestRecorder");
        assert_eq!(class_of(top[1]), "NibTestRecorderChild", "the swapper makes the game's class");
        assert_eq!(class_of(top[2]), "NibTestRecorder", "a missing class falls back to the original one");
        assert_eq!(class_of(top[3]), "NibTestRecorder", "a missing class falls back to the archive's fallback");
        let (a, second_object) = (top[0], record(top[0]).partner);
        assert_ne!(second_object, 0, "the outlet was set through setPartner:");
        assert_eq!(record(owner).thing, a, "the owner's outlet was set");
        assert_eq!(record(second_object).partner, outside, "an external object stands in for its placeholder");
        assert_eq!(record(second_object).targets, [(owner, "tapped:".to_string(), 64)]);
        assert_eq!(record(a).targets, [(0, "tapped:".to_string(), 4096)], "the first responder is a nil target");
        // Every object wakes after all connections are made; placeholders don't.
        assert_eq!(record(a).partner_when_woken, second_object);
        assert_eq!(record(second_object).partner_when_woken, outside);
        for object in &top {
            assert_ne!(record(*object).awake_order, 0, "{} woke", class_of(*object));
        }
        assert_eq!(record(outside).awake_order, 0);
    }

    /// An outlet with no setter writes the object's field `_name`.
    #[test]
    fn nib_outlet_without_a_setter_sets_the_field() {
        test_classes();
        let mut b = Builder::new();
        let fielded = b.object("NibTestFielded", vec![]);
        let target = b.object("NibTestRecorder", vec![]);
        let extra = b.string("extra");
        let outlet = b.object(
            "UIRuntimeOutletConnection",
            vec![("UILabel", Put::Ref(extra)), ("UISource", Put::Ref(fielded)), ("UIDestination", Put::Ref(target))],
        );
        b.standard_root(&[fielded, target], &[fielded, target], &[outlet]);
        let top = collections::array_items(runtime::send(nib_from(&b.bytes()), "instantiateWithOwner:options:", &[0, 0]));
        assert_eq!(top.len(), 2);
        assert_eq!(unsafe { *((top[0] + 16) as *const Id) }, top[1]);
    }

    /// The real view controller nib, with its placeholders given as a
    /// storyboard gives them.
    #[test]
    fn nib_instantiates_the_real_view_controller_nib() {
        test_classes();
        let scene = new_object("NibTestOwner");
        let storyboard = new_object("NibTestRecorder");
        let externals = collections::dictionary_from(&[(string::make("UIStoryboardPlaceholder"), storyboard)]);
        let options = collections::dictionary_from(&[(string::make("UINibExternalObjects"), externals)]);
        let top = collections::array_items(runtime::send(nib_from(VIEW_CONTROLLER_NIB), "instantiateWithOwner:options:", &[scene, options]));
        assert_eq!(top.len(), 1, "one controller; the placeholders are left out");
        let controller = top[0];
        assert_eq!(class_of(controller), "HardcoreDeathViewController");
        assert_eq!(record(controller).thing, storyboard, "-setStoryboard: got the placeholder's object");
        assert_eq!(record(scene).partner, controller, "-setSceneViewController: on the owner");
        assert_ne!(record(controller).awake_order, 0);
    }

    /// A nib inside a nib (a table's cell nib) becomes a UINib.
    #[test]
    fn nib_inside_a_nib_becomes_a_nib() {
        test_classes();
        let mut inner = Builder::new();
        let thing = inner.object("NibTestRecorder", vec![("count", Put::I8(5))]);
        inner.standard_root(&[thing], &[thing], &[]);
        let mut b = Builder::new();
        let bytes = b.object("NSData", vec![("NS.bytes", Put::Data(inner.bytes()))]);
        let nested = b.object("UINib", vec![("captureEnclosingNIBBundleOnDecode", Put::True), ("archiveData", Put::Ref(bytes))]);
        b.standard_root(&[nested], &[nested], &[]);
        let top = collections::array_items(runtime::send(nib_from(&b.bytes()), "instantiateWithOwner:options:", &[0, 0]));
        assert_eq!(top.len(), 1);
        assert_eq!(class_of(top[0]), "UINib");
        let cells = collections::array_items(runtime::send(top[0], "instantiateWithOwner:options:", &[0, 0]));
        assert_eq!(cells.len(), 1);
        assert_eq!(record(cells[0]).count, 5);
    }

    // ----- finding nib files -----

    fn small_nib(count: i8) -> Vec<u8> {
        let mut b = Builder::new();
        let thing = b.object("NibTestRecorder", vec![("count", Put::I8(count))]);
        b.standard_root(&[thing], &[thing], &[]);
        b.bytes()
    }

    #[test]
    fn nib_files_are_found_in_the_bundle() {
        test_classes();
        let folder = std::env::temp_dir().join(format!("gamebridge-nib-{}", std::process::id()));
        let app = folder.join("Game.app");
        std::fs::create_dir_all(app.join("en.lproj/Main.storyboardc")).unwrap();
        std::fs::create_dir_all(app.join("Folder.nib")).unwrap();
        std::fs::write(app.join("Plain.nib"), small_nib(1)).unwrap();
        std::fs::write(app.join("en.lproj/Localized.nib"), small_nib(2)).unwrap();
        std::fs::write(app.join("en.lproj/Main.storyboardc/UIViewController-7.nib"), small_nib(3)).unwrap();
        std::fs::write(app.join("Device.nib"), small_nib(4)).unwrap();
        std::fs::write(app.join("Device~iphone.nib"), small_nib(5)).unwrap();
        std::fs::write(app.join("Folder.nib/runtime.nib"), small_nib(6)).unwrap();
        let bundle = runtime::send(runtime::class_named("NSBundle"), "bundleWithPath:", &[string::make(&app.to_string_lossy())]);
        assert_ne!(bundle, 0);
        let count_in = |name: &str| -> Option<i64> {
            let nib = runtime::send(runtime::class_named("UINib"), "nibWithNibName:bundle:", &[string::make(name), bundle]);
            if nib == 0 {
                return None;
            }
            let top = collections::array_items(runtime::send(nib, "instantiateWithOwner:options:", &[0, 0]));
            Some(record(top[0]).count)
        };
        assert_eq!(count_in("Plain"), Some(1));
        assert_eq!(count_in("Plain.nib"), Some(1));
        assert_eq!(count_in("Localized"), Some(2));
        assert_eq!(count_in("Main.storyboardc/UIViewController-7"), Some(3));
        assert_eq!(count_in("Device"), Some(5), "the iPhone version is preferred");
        assert_eq!(count_in("Folder"), Some(6));
        assert_eq!(count_in("Nothing"), None);
        // -[NSBundle loadNibNamed:owner:options:] gives the top-level objects.
        let top = runtime::send(bundle, "loadNibNamed:owner:options:", &[string::make("Localized"), 0, 0]);
        let top = collections::array_items(top);
        assert_eq!(top.len(), 1);
        assert_eq!(record(top[0]).count, 2);
        assert_eq!(runtime::send(bundle, "loadNibNamed:owner:options:", &[string::make("Nothing"), 0, 0]), 0);
        std::fs::remove_dir_all(&folder).unwrap();
    }

    /// A picture named in a nib becomes a UIImage from the nib's bundle.
    #[test]
    fn nib_image_placeholders_become_images() {
        test_classes();
        let folder = std::env::temp_dir().join(format!("gamebridge-nib-image-{}", std::process::id()));
        let app = folder.join("Pictures.bundle");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join("hero.png"), crate::fw::png::encode(2, 3, &[255; 2 * 3 * 4], false)).unwrap();
        let mut b = Builder::new();
        let name = b.string("hero.png");
        let missing = b.string("nothing.png");
        let picture = b.object(
            "UIImageNibPlaceholder",
            vec![("UIImageWidth", Put::F32(2.0)), ("UIImageHeight", Put::F32(3.0)), ("UIResourceName", Put::Ref(name))],
        );
        let lost = b.object("UIImageNibPlaceholder", vec![("UIResourceName", Put::Ref(missing))]);
        let with_picture = b.object("NibTestRecorder", vec![("image", Put::Ref(picture))]);
        let without = b.object("NibTestRecorder", vec![("image", Put::Ref(lost))]);
        b.standard_root(&[with_picture, without], &[with_picture, without], &[]);
        let bytes = data::make(&b.bytes());
        let bundle = runtime::send(runtime::class_named("NSBundle"), "bundleWithPath:", &[string::make(&app.to_string_lossy())]);
        let nib = runtime::send(runtime::class_named("UINib"), "nibWithData:bundle:", &[bytes, bundle]);
        let top = collections::array_items(runtime::send(nib, "instantiateWithOwner:options:", &[0, 0]));
        let image = record(top[0]).image;
        assert_ne!(image, 0);
        assert_eq!(class_of(image), "UIImage");
        assert_eq!(crate::fw::uikit::image_points(image), Size { width: 2.0, height: 3.0 });
        assert_eq!(record(top[1]).image, 0, "a missing picture is nil");
        std::fs::remove_dir_all(&folder).unwrap();
    }

    // ----- the coder seen from a 32-bit game, through the bridge -----

    use crate::guest32::objc::bridge::{install, send_from_guest, Bridge, GuestSide, SendKind};
    use crate::guest32::objc::testing::Fake;

    /// A coder reading the root object of `b`'s archive.
    fn coder_for(b: &Builder) -> Id {
        test_classes();
        new_coder(Arc::new(parse(&b.bytes()).unwrap()), 0, 0, 0)
    }

    struct Guest {
        fake: &'static Fake,
        bridge: &'static Mutex<Bridge>,
    }

    impl Guest {
        fn new() -> Guest {
            let fake = Fake::new();
            let md = guest32_image::ObjcMetadata32::default();
            let bridge: &'static Mutex<Bridge> = Box::leak(Box::new(Mutex::new(Bridge::new(&md).unwrap())));
            install(bridge, fake, &md).unwrap();
            Guest { fake, bridge }
        }

        fn object(&self, host: Id) -> u32 {
            self.bridge.lock().unwrap().guest_object(self.fake, host).unwrap()
        }

        /// [receiver selector argument...] sent by guest code; r0 | r1 << 32.
        fn send(&self, kind: SendKind, words: &[u32]) -> u64 {
            let get = |i: u32| words.get(i as usize).copied().unwrap_or(0);
            let result = send_from_guest(self.bridge, self.fake, kind, &get).unwrap();
            u64::from(result.r0) | u64::from(result.r1) << 32
        }

        fn selector(&self, name: &str) -> u32 {
            self.bridge.lock().unwrap().guest_selector(self.fake, name)
        }

        fn key(&self, name: &str) -> u32 {
            self.object(string::make(name))
        }
    }

    /// Numbers and structs reach 32-bit code as armv7 values (a C float in
    /// r0, a double in r0 and r1, a CGRect of floats in memory).
    #[test]
    fn nib_coder_values_cross_the_32_bit_bridge() {
        let mut b = Builder::new();
        b.root(vec![
            ("alpha", Put::F32(0.5)),
            ("size", Put::F64(12.5)),
            ("count", Put::I16(-300)),
            ("flag", Put::True),
            ("UIBounds", Put::Data(floats(6, &[1.0, 2.0, 30.0, 40.0]))),
        ]);
        let coder = coder_for(&b);
        let guest = Guest::new();
        let receiver = guest.object(coder);
        let float = guest.send(SendKind::Normal, &[receiver, guest.selector("decodeFloatForKey:"), guest.key("alpha")]);
        assert_eq!(f32::from_bits(float as u32), 0.5, "decodeFloatForKey: gives a C float");
        let double = guest.send(SendKind::Normal, &[receiver, guest.selector("decodeDoubleForKey:"), guest.key("size")]);
        assert_eq!(f64::from_bits(double), 12.5);
        let count = guest.send(SendKind::Normal, &[receiver, guest.selector("decodeIntegerForKey:"), guest.key("count")]);
        assert_eq!(count as u32 as i32, -300);
        let flag = guest.send(SendKind::Normal, &[receiver, guest.selector("decodeBoolForKey:"), guest.key("flag")]);
        assert_eq!(flag as u8, 1);
        let out = guest.fake.alloc_permanent(16);
        guest.send(SendKind::Stret, &[out, receiver, guest.selector("decodeCGRectForKey:"), guest.key("UIBounds")]);
        let fields: Vec<f32> = guest.fake.read(out, 16).chunks(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
        assert_eq!(fields, [1.0, 2.0, 30.0, 40.0]);
    }

    /// The bytes are copied into guest memory and stay there while the coder
    /// lives (each key its own copy); the length is a 4-byte guest value.
    #[test]
    fn nib_coder_bytes_cross_the_32_bit_bridge() {
        let mut b = Builder::new();
        b.root(vec![("blob", Put::Data(vec![1, 2, 0, 4, 5])), ("other", Put::Data(vec![9, 8]))]);
        let coder = coder_for(&b);
        let guest = Guest::new();
        let receiver = guest.object(coder);
        let length = guest.fake.words(&[0xAAAA_AAAA, 0xBBBB_BBBB]);
        let selector = guest.selector("decodeBytesForKey:returnedLength:");
        let first = guest.send(SendKind::Normal, &[receiver, selector, guest.key("blob"), length]) as u32;
        assert_ne!(first, 0);
        assert_eq!(guest.fake.read(first, 5), [1, 2, 0, 4, 5]);
        assert_eq!(guest.fake.u32_at(length), 5);
        assert_eq!(guest.fake.u32_at(length + 4), 0xBBBB_BBBB, "only the 4-byte length is written");
        let second = guest.send(SendKind::Normal, &[receiver, selector, guest.key("other"), length]) as u32;
        assert_eq!(guest.fake.read(second, 2), [9, 8]);
        assert_eq!(guest.fake.u32_at(length), 2);
        assert_eq!(guest.fake.read(first, 5), [1, 2, 0, 4, 5], "the first copy is still there");
        let missing = guest.send(SendKind::Normal, &[receiver, selector, guest.key("missing"), length]) as u32;
        assert_eq!((missing, guest.fake.u32_at(length)), (0, 0));
        // When the coder is finished with, its copies go back to the guest heap.
        guest.bridge.lock().unwrap().release_host_copies(guest.fake, coder);
        let freed = guest.fake.freed.lock().unwrap().clone();
        assert!(freed.contains(&first) && freed.contains(&second), "freed {freed:x?}");
    }

    /// A list or dictionary that holds itself is one object, not endless.
    #[test]
    fn nib_collections_that_hold_themselves_decode() {
        let mut b = Builder::new();
        let name = b.string("name");
        let list = b.objects.len();
        b.object("NSMutableArray", vec![("NSInlinedValue", Put::True), ("UINibEncoderEmptyKey", Put::Ref(list)), ("UINibEncoderEmptyKey", Put::Ref(name))]);
        let table = b.objects.len();
        b.object("NSMutableDictionary", vec![("NSInlinedValue", Put::True), ("UINibEncoderEmptyKey", Put::Ref(name)), ("UINibEncoderEmptyKey", Put::Ref(table))]);
        b.root(vec![("list", Put::Ref(list)), ("table", Put::Ref(table))]);
        let coder = coder_for(&b);
        let array = decode_object(coder, 0, string::make("list"));
        let items = collections::array_items(array);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], array);
        assert_eq!(string::get(items[1]), "name");
        assert_eq!(class_of(array), "NSMutableArray");
        let dictionary = decode_object(coder, 0, string::make("table"));
        assert_eq!(collections::dictionary_pairs(dictionary), [(items[1], dictionary)]);
        assert_eq!(class_of(dictionary), "NSMutableDictionary");
    }

    static FINISHED: Mutex<Vec<Id>> = Mutex::new(Vec::new());

    fn note_finished(object: Id) {
        FINISHED.lock().unwrap().push(object);
    }

    /// After -instantiateWithOwner:options: the coder is let go: its state is
    /// gone, it is announced as finished (the 32-bit bridge frees its copies),
    /// and a late message to it reads nothing.
    #[test]
    fn nib_coder_is_let_go_after_instantiation() {
        test_classes();
        runtime::on_object_finished(note_finished);
        let mut b = Builder::new();
        let thing = b.object("NibTestRecorder", vec![("count", Put::I8(4))]);
        b.standard_root(&[thing], &[thing], &[]);
        let top = collections::array_items(runtime::send(nib_from(&b.bytes()), "instantiateWithOwner:options:", &[0, 0]));
        let r = record(top[0]);
        assert_eq!(r.count, 4);
        assert_ne!(r.coder, 0);
        assert_eq!(unsafe { *((r.coder + 8) as *const usize) }, 0, "the coder's state was dropped");
        assert!(FINISHED.lock().unwrap().contains(&r.coder), "the coder was announced as finished");
        assert_eq!(ask::<i64>(r.coder, "decodeIntegerForKey:", "count"), 0);
        assert_eq!(ask::<Id>(r.coder, "decodeObjectForKey:", "count"), 0);
    }

    /// 32-bit games reach the coder and UINib through the bridge's table.
    #[test]
    fn nib_messages_have_32_bit_encodings() {
        let table = crate::guest32::objc::selectors::FrameworkSelectors::builtin();
        for (selector, encoding) in [
            ("decodeCGPointForKey:", "{CGPoint=ff}@:@"),
            ("decodeCGSizeForKey:", "{CGSize=ff}@:@"),
            ("decodeCGRectForKey:", "{CGRect={CGPoint=ff}{CGSize=ff}}@:@"),
            ("decodeIntegerForKey:", "i@:@"),
            ("decodeInt32ForKey:", "i@:@"),
            ("containsValueForKey:", "c@:@"),
            ("nibWithNibName:bundle:", "@@:@@"),
            ("nibWithData:bundle:", "@@:@@"),
            ("instantiateWithOwner:options:", "@@:@@"),
            ("loadNibNamed:owner:options:", "@@:@@@"),
        ] {
            let entry = table.get(selector).unwrap_or_else(|| panic!("{selector} is in framework_selectors.tsv"));
            assert_eq!(entry.encoding, encoding, "{selector}");
        }
        // The coder answers -decodeFloatForKey: with a C float, not CGFloat.
        assert!(table.get("decodeFloatForKey:").unwrap().real_float, "decodeFloatForKey: is marked float");
        let bytes = table.get("decodeBytesForKey:returnedLength:").expect("decodeBytesForKey:returnedLength: is listed");
        assert!(bytes.host_bytes, "its result is copied into guest memory");
    }
}
