//! NSString and its mutable and constant forms.
//! Our strings: { isa, *mut Text }. Compiled-in constant strings:
//! { isa, flags, characters, length } with 8-bit or UTF-16 characters.

use crate::fw::object::{class_methods, define_class, methods};
use crate::objc::runtime::{self, lock, Class, Id, Sel};
use crate::symbols::Registry;
use std::collections::HashMap;
use std::ffi::CString;
use std::sync::{Mutex, OnceLock};

pub struct Text {
    pub units: Vec<u16>,
    utf8: Option<CString>,
}

impl Text {
    fn new(text: &str) -> Text {
        Text { units: text.encode_utf16().collect(), utf8: None }
    }

    pub fn string(&self) -> String {
        String::from_utf16_lossy(&self.units)
    }
}

static CLASSES: OnceLock<[Class; 3]> = OnceLock::new();
static CONSTANT_TEXT: Mutex<Option<HashMap<usize, usize>>> = Mutex::new(None);

fn string_class() -> Class {
    CLASSES.get().unwrap()[0]
}

fn mutable_class() -> Class {
    CLASSES.get().unwrap()[1]
}

fn constant_class() -> Class {
    CLASSES.get().unwrap()[2]
}

pub fn is_string(object: Id) -> bool {
    if object == 0 || CLASSES.get().is_none() {
        return false;
    }
    let rt = lock();
    let class = rt.class_of(object);
    rt.is_subclass(class, string_class())
}

/// The text behind any string object (ours or compiled-in).
pub fn text(object: Id) -> *mut Text {
    let class = lock().class_of(object);
    if class == constant_class() {
        let mut cache = CONSTANT_TEXT.lock().unwrap();
        let cache = cache.get_or_insert_with(HashMap::new);
        if let Some(&text) = cache.get(&object) {
            return text as *mut Text;
        }
        let text = unsafe {
            let flags = *((object + 8) as *const u64);
            let characters = *((object + 16) as *const usize);
            let length = *((object + 24) as *const usize);
            let units = if flags & 0x10 != 0 {
                std::slice::from_raw_parts(characters as *const u16, length).to_vec()
            } else {
                let bytes = std::slice::from_raw_parts(characters as *const u8, length);
                String::from_utf8_lossy(bytes).encode_utf16().collect()
            };
            Box::into_raw(Box::new(Text { units, utf8: None }))
        };
        cache.insert(object, text as usize);
        return text;
    }
    let slot = unsafe { *((object + 8) as *const *mut Text) };
    if slot.is_null() {
        let text = Box::into_raw(Box::new(Text::new("")));
        unsafe { *((object + 8) as *mut *mut Text) = text };
        return text;
    }
    slot
}

pub fn get(object: Id) -> String {
    if object == 0 {
        return String::new();
    }
    if !is_string(object) {
        return describe(object);
    }
    unsafe { (*text(object)).string() }
}

/// Text for %@ and logs: strings as they are, other objects by -description.
pub fn describe(object: Id) -> String {
    if object == 0 {
        return "(null)".into();
    }
    if is_string(object) {
        return unsafe { (*text(object)).string() };
    }
    if runtime::responds(object, "description") {
        let d = runtime::send(object, "description", &[]);
        if d != object && is_string(d) {
            return get(d);
        }
    }
    let rt = lock();
    format!("<{} {object:#x}>", rt.class_name(rt.class_of(object)))
}

/// The class name of an object (or the class itself) for logs.
pub fn describe_class_of(object: Id) -> String {
    if object == 0 {
        return "nil".into();
    }
    let rt = lock();
    let class = rt.class_of(object);
    let name = rt.class_name(class);
    if rt.classes.get(&class).map(|r| r.meta).unwrap_or(false) {
        format!("class {name}")
    } else {
        name
    }
}

fn make_in(class: Class, value: &str) -> Id {
    let object = runtime::alloc_instance(class);
    unsafe { *((object + 8) as *mut *mut Text) = Box::into_raw(Box::new(Text::new(value))) };
    object
}

/// A new string object.
pub fn make(value: &str) -> Id {
    make_in(string_class(), value)
}

fn set(object: Id, value: Vec<u16>) {
    unsafe {
        let text = text(object);
        (*text).units = value;
        (*text).utf8 = None;
    }
}

fn units(object: Id) -> Vec<u16> {
    if object == 0 {
        return Vec::new();
    }
    unsafe { (*text(object)).units.clone() }
}

unsafe fn c_string(pointer: usize) -> String {
    runtime::cstr(pointer)
}

// ----- methods -----

extern "C" fn init_with_utf8(this: Id, _: Sel, bytes: usize) -> Id {
    if bytes == 0 {
        return 0;
    }
    set(this, unsafe { c_string(bytes) }.encode_utf16().collect());
    this
}

extern "C" fn string_with_utf8(class: Class, _: Sel, bytes: usize) -> Id {
    if bytes == 0 {
        return 0;
    }
    make_in(if class == mutable_class() { class } else { string_class_for(class) }, &unsafe { c_string(bytes) })
}

fn string_class_for(class: Class) -> Class {
    if lock().is_subclass(class, mutable_class()) {
        mutable_class()
    } else {
        string_class()
    }
}

extern "C" fn string_with_string(class: Class, _: Sel, other: Id) -> Id {
    let object = make_in(string_class_for(class), "");
    set(object, units(other));
    object
}

extern "C" fn init_with_string(this: Id, _: Sel, other: Id) -> Id {
    set(this, units(other));
    this
}

extern "C" fn empty_string(class: Class, _: Sel) -> Id {
    make_in(string_class_for(class), "")
}

extern "C" fn init_empty(this: Id, _: Sel) -> Id {
    text(this);
    this
}

extern "C" fn init_with_capacity(this: Id, s: Sel, _capacity: usize) -> Id {
    init_empty(this, s)
}

extern "C" fn string_with_capacity(class: Class, s: Sel, _capacity: usize) -> Id {
    empty_string(class, s)
}

/// Takes the bytes over: copies them, then frees them if asked (as Apple would when done).
extern "C" fn init_with_bytes_no_copy(this: Id, s: Sel, bytes: *mut u8, length: usize, encoding: usize, free_when_done: u8) -> Id {
    let result = init_with_bytes(this, s, bytes, length, encoding);
    if free_when_done != 0 && !bytes.is_null() {
        crate::libsystem::heap::free(bytes as *mut _);
    }
    result
}

extern "C" fn init_with_characters(this: Id, _: Sel, characters: *const u16, length: usize) -> Id {
    let units = if characters.is_null() { Vec::new() } else { unsafe { std::slice::from_raw_parts(characters, length) }.to_vec() };
    set(this, units);
    this
}

extern "C" fn string_with_characters(class: Class, s: Sel, characters: *const u16, length: usize) -> Id {
    let object = make_in(string_class_for(class), "");
    init_with_characters(object, s, characters, length)
}

const NS_ASCII: usize = 1;
const NS_UTF8: usize = 4;
const NS_ISO_LATIN1: usize = 5;
const NS_UTF16: usize = 10;
const NS_UTF16_LE: usize = 0x9400_0100;
const NS_UTF32_LE: usize = 0x9C00_0100;
const NS_UTF32: usize = 0x8C00_0100;

fn decode(bytes: &[u8], encoding: usize) -> Option<Vec<u16>> {
    Some(match encoding {
        NS_ASCII | NS_UTF8 => std::str::from_utf8(bytes).ok()?.encode_utf16().collect(),
        NS_ISO_LATIN1 => bytes.iter().map(|&b| b as u16).collect(),
        NS_UTF16 | NS_UTF16_LE => {
            let mut units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            if units.first() == Some(&0xFEFF) {
                units.remove(0);
            }
            units
        }
        NS_UTF32 | NS_UTF32_LE => {
            let text: String = bytes
                .chunks_exact(4)
                .filter_map(|c| char::from_u32(u32::from_le_bytes([c[0], c[1], c[2], c[3]])))
                .filter(|&c| c != '\u{FEFF}')
                .collect();
            text.encode_utf16().collect()
        }
        _ => {
            crate::log!("string encoding {encoding:#x} not known, read as UTF-8");
            String::from_utf8_lossy(bytes).encode_utf16().collect()
        }
    })
}

pub fn encode(units: &[u16], encoding: usize) -> Vec<u8> {
    match encoding {
        NS_UTF16 | NS_UTF16_LE => units.iter().flat_map(|u| u.to_le_bytes()).collect(),
        NS_UTF32 | NS_UTF32_LE => String::from_utf16_lossy(units).chars().flat_map(|c| (c as u32).to_le_bytes()).collect(),
        NS_ISO_LATIN1 | NS_ASCII => units.iter().map(|&u| if u < 256 { u as u8 } else { b'?' }).collect(),
        _ => String::from_utf16_lossy(units).into_bytes(),
    }
}

extern "C" fn init_with_bytes(this: Id, _: Sel, bytes: *const u8, length: usize, encoding: usize) -> Id {
    let slice = if bytes.is_null() { &[][..] } else { unsafe { std::slice::from_raw_parts(bytes, length) } };
    match decode(slice, encoding) {
        Some(units) => {
            set(this, units);
            this
        }
        None => 0,
    }
}

extern "C" fn init_with_c_string(this: Id, _: Sel, bytes: usize, encoding: usize) -> Id {
    if bytes == 0 {
        return 0;
    }
    let raw = unsafe { std::ffi::CStr::from_ptr(bytes as *const libc::c_char) }.to_bytes();
    match decode(raw, encoding) {
        Some(units) => {
            set(this, units);
            this
        }
        None => 0,
    }
}

extern "C" fn string_with_c_string(class: Class, s: Sel, bytes: usize, encoding: usize) -> Id {
    let object = make_in(string_class_for(class), "");
    init_with_c_string(object, s, bytes, encoding)
}

/// Reads a text file: UTF-16 or UTF-8 by its byte-order mark, else UTF-8
/// (Latin-1 when it isn't valid UTF-8). Returns the text and its encoding.
fn read_text_file(path: &str) -> Option<(Vec<u16>, usize)> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return Some((decode(&bytes[2..], NS_UTF16_LE)?, NS_UTF16));
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let units = bytes[2..].chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        return Some((units, NS_UTF16));
    }
    let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    match std::str::from_utf8(body) {
        Ok(text) => Some((text.encode_utf16().collect(), NS_UTF8)),
        Err(_) => Some((body.iter().map(|&b| b as u16).collect(), NS_ISO_LATIN1)),
    }
}

fn file_error(error: *mut Id, path: &str) {
    if !error.is_null() {
        unsafe { *error = crate::fw::misc::error("NSCocoaErrorDomain", 260, &format!("can't read {path}")) };
    }
}

extern "C" fn init_with_contents_used(this: Id, _: Sel, path: Id, used: *mut usize, error: *mut Id) -> Id {
    let path = get(path);
    match read_text_file(&path) {
        Some((value, encoding)) => {
            if !used.is_null() {
                unsafe { *used = encoding };
            }
            set(this, value);
            this
        }
        None => {
            file_error(error, &path);
            0
        }
    }
}

extern "C" fn init_with_contents_encoding(this: Id, _: Sel, path: Id, encoding: usize, error: *mut Id) -> Id {
    let path_text = get(path);
    match std::fs::read(&path_text).ok().and_then(|bytes| decode(&bytes, encoding)) {
        Some(value) => {
            set(this, value);
            this
        }
        None => {
            file_error(error, &path_text);
            0
        }
    }
}

extern "C" fn with_contents_used(class: Class, s: Sel, path: Id, used: *mut usize, error: *mut Id) -> Id {
    init_with_contents_used(make_in(string_class_for(class), ""), s, path, used, error)
}

extern "C" fn with_contents_encoding(class: Class, s: Sel, path: Id, encoding: usize, error: *mut Id) -> Id {
    init_with_contents_encoding(make_in(string_class_for(class), ""), s, path, encoding, error)
}

extern "C" fn with_contents(class: Class, s: Sel, path: Id) -> Id {
    with_contents_used(class, s, path, std::ptr::null_mut(), std::ptr::null_mut())
}

extern "C" fn init_with_data_encoding(this: Id, _: Sel, data: Id, encoding: usize) -> Id {
    match decode(&crate::fw::data::bytes(data), encoding) {
        Some(value) => {
            set(this, value);
            this
        }
        None => 0,
    }
}

extern "C" fn write_to_file_encoding(this: Id, _: Sel, path: Id, _atomically: u8, encoding: usize, _error: *mut Id) -> bool {
    let path = get(path);
    let temporary = format!("{path}.part");
    std::fs::write(&temporary, encode(&units(this), encoding)).and_then(|_| std::fs::rename(&temporary, &path)).is_ok()
}

extern "C" fn write_to_file(this: Id, s: Sel, path: Id, atomically: u8) -> bool {
    write_to_file_encoding(this, s, path, atomically, NS_UTF8, std::ptr::null_mut())
}

extern "C" fn utf8_string(this: Id, _: Sel) -> usize {
    unsafe {
        let text = text(this);
        if (*text).utf8.is_none() {
            let value = (*text).string().replace('\0', "");
            (*text).utf8 = Some(CString::new(value).unwrap());
        }
        (*text).utf8.as_ref().unwrap().as_ptr() as usize
    }
}

extern "C" fn c_string_using(this: Id, _: Sel, encoding: usize) -> usize {
    let mut bytes = encode(&units(this), encoding);
    bytes.extend_from_slice(&[0, 0, 0, 0]);
    Box::leak(bytes.into_boxed_slice()).as_ptr() as usize
}

extern "C" fn get_c_string(this: Id, _: Sel, buffer: *mut u8, size: usize, encoding: usize) -> bool {
    let bytes = encode(&units(this), encoding);
    if bytes.len() + 1 > size {
        return false;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer, bytes.len());
        *buffer.add(bytes.len()) = 0;
    }
    true
}

extern "C" fn data_using_encoding(this: Id, _: Sel, encoding: usize) -> Id {
    crate::fw::data::make(&encode(&units(this), encoding))
}

extern "C" fn data_using_encoding_lossy(this: Id, s: Sel, encoding: usize, _lossy: u8) -> Id {
    data_using_encoding(this, s, encoding)
}

extern "C" fn length(this: Id, _: Sel) -> usize {
    unsafe { (*text(this)).units.len() }
}

extern "C" fn length_of_bytes(this: Id, _: Sel, encoding: usize) -> usize {
    encode(&units(this), encoding).len()
}

extern "C" fn character_at(this: Id, _: Sel, index: usize) -> u16 {
    unsafe { (&(*text(this)).units).get(index).copied().unwrap_or(0) }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Range {
    pub location: usize,
    pub length: usize,
}

extern "C" fn get_characters_range(this: Id, _: Sel, buffer: *mut u16, range: Range) {
    let units = units(this);
    let end = (range.location + range.length).min(units.len());
    if range.location < end {
        unsafe { std::ptr::copy_nonoverlapping(units[range.location..end].as_ptr(), buffer, end - range.location) };
    }
}

extern "C" fn get_characters(this: Id, _: Sel, buffer: *mut u16) {
    let units = units(this);
    unsafe { std::ptr::copy_nonoverlapping(units.as_ptr(), buffer, units.len()) };
}

extern "C" fn is_equal_to_string(this: Id, _: Sel, other: Id) -> bool {
    if other == 0 || !is_string(other) {
        return false;
    }
    units(this) == units(other)
}

extern "C" fn is_equal(this: Id, s: Sel, other: Id) -> bool {
    is_equal_to_string(this, s, other)
}

extern "C" fn hash(this: Id, _: Sel) -> usize {
    let mut h: u64 = 0xcbf29ce484222325;
    for unit in units(this) {
        h ^= unit as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h as usize
}

extern "C" fn copy_with_zone(this: Id, _: Sel, _zone: usize) -> Id {
    let class = lock().class_of(this);
    if class == string_class() || class == constant_class() {
        return this;
    }
    let object = make_in(string_class(), "");
    set(object, units(this));
    object
}

extern "C" fn mutable_copy_with_zone(this: Id, _: Sel, _zone: usize) -> Id {
    let object = make_in(mutable_class(), "");
    set(object, units(this));
    object
}

extern "C" fn description(this: Id, _: Sel) -> Id {
    this
}

fn compare_units(a: &[u16], b: &[u16], case_insensitive: bool) -> isize {
    let fold = |u: u16| if case_insensitive && (b'A' as u16..=b'Z' as u16).contains(&u) { u + 32 } else { u };
    for (x, y) in a.iter().zip(b.iter()) {
        let (x, y) = (fold(*x), fold(*y));
        if x != y {
            return if x < y { -1 } else { 1 };
        }
    }
    (a.len() as isize - b.len() as isize).signum()
}

extern "C" fn compare(this: Id, _: Sel, other: Id) -> isize {
    compare_units(&units(this), &units(other), false)
}

extern "C" fn compare_options(this: Id, _: Sel, other: Id, options: usize) -> isize {
    compare_units(&units(this), &units(other), options & 1 != 0)
}

extern "C" fn case_insensitive_compare(this: Id, _: Sel, other: Id) -> isize {
    compare_units(&units(this), &units(other), true)
}

extern "C" fn has_prefix(this: Id, _: Sel, other: Id) -> bool {
    let other = units(other);
    !other.is_empty() && units(this).starts_with(&other)
}

extern "C" fn has_suffix(this: Id, _: Sel, other: Id) -> bool {
    let other = units(other);
    !other.is_empty() && units(this).ends_with(&other)
}

extern "C" fn by_appending_string(this: Id, _: Sel, other: Id) -> Id {
    let mut value = units(this);
    value.extend(units(other));
    let object = make(""); 
    set(object, value);
    object
}

extern "C" fn append_string(this: Id, _: Sel, other: Id) {
    let mut value = units(this);
    value.extend(units(other));
    set(this, value);
}

extern "C" fn set_string(this: Id, _: Sel, other: Id) {
    set(this, units(other));
}

extern "C" fn lowercase(this: Id, _: Sel) -> Id {
    make(&get(this).to_lowercase())
}

extern "C" fn uppercase(this: Id, _: Sel) -> Id {
    make(&get(this).to_uppercase())
}

/// Words are split by whitespace (spaces, tabs, line ends), as Apple documents.
extern "C" fn capitalized(this: Id, _: Sel) -> Id {
    let mut out = String::new();
    let mut word_start = true;
    for c in get(this).chars() {
        if word_start {
            out.extend(c.to_uppercase());
        } else {
            out.extend(c.to_lowercase());
        }
        word_start = c.is_whitespace();
    }
    make(&out)
}

extern "C" fn substring_from(this: Id, _: Sel, index: usize) -> Id {
    let value = units(this);
    let object = make("");
    set(object, value.get(index..).unwrap_or(&[]).to_vec());
    object
}

extern "C" fn substring_to(this: Id, _: Sel, index: usize) -> Id {
    let value = units(this);
    let object = make("");
    set(object, value[..index.min(value.len())].to_vec());
    object
}

extern "C" fn substring_with_range(this: Id, _: Sel, range: Range) -> Id {
    let value = units(this);
    let start = range.location.min(value.len());
    let end = (range.location + range.length).min(value.len());
    let object = make("");
    set(object, value[start..end].to_vec());
    object
}

extern "C" fn range_of_string(this: Id, s: Sel, other: Id) -> Range {
    range_of_string_options(this, s, other, 0)
}

extern "C" fn range_of_string_options(this: Id, s: Sel, other: Id, options: usize) -> Range {
    range_of_string_in(this, s, other, options, Range { location: 0, length: units(this).len() })
}

/// NSStringCompareOptions: 1 case-insensitive (ASCII letters, as -compare:
/// here), 4 backwards (the last match), 8 anchored (only at the start, or at
/// the end when backwards); 2 (literal) changes nothing for plain units.
extern "C" fn range_of_string_in(this: Id, _: Sel, other: Id, options: usize, range: Range) -> Range {
    let (case_insensitive, backwards, anchored) = (options & 1 != 0, options & 4 != 0, options & 8 != 0);
    let fold = |u: u16| if case_insensitive && (b'A' as u16..=b'Z' as u16).contains(&u) { u + 32 } else { u };
    let haystack: Vec<u16> = units(this).into_iter().map(fold).collect();
    let needle: Vec<u16> = units(other).into_iter().map(fold).collect();
    let end = range.location.saturating_add(range.length).min(haystack.len());
    let begin = range.location.min(end);
    let not_found = Range { location: isize::MAX as usize, length: 0 };
    if needle.is_empty() || needle.len() > end - begin {
        return not_found;
    }
    let last = end - needle.len();
    let starts: Vec<usize> = match (backwards, anchored) {
        (false, false) => (begin..=last).collect(),
        (true, false) => (begin..=last).rev().collect(),
        (false, true) => vec![begin],
        (true, true) => vec![last],
    };
    starts
        .into_iter()
        .find(|&start| haystack[start..start + needle.len()] == needle[..])
        .map(|location| Range { location, length: needle.len() })
        .unwrap_or(not_found)
}

extern "C" fn int_value(this: Id, _: Sel) -> i32 {
    leading_number(&get(this)) as i32
}

extern "C" fn integer_value(this: Id, _: Sel) -> isize {
    leading_number(&get(this)) as isize
}

extern "C" fn float_value(this: Id, _: Sel) -> f32 {
    leading_float(&get(this)) as f32
}

extern "C" fn double_value(this: Id, _: Sel) -> f64 {
    leading_float(&get(this))
}

extern "C" fn bool_value(this: Id, _: Sel) -> bool {
    let text = get(this);
    let text = text.trim_start();
    matches!(text.chars().next(), Some('Y' | 'y' | 'T' | 't' | '1'..='9'))
}

fn leading_number(text: &str) -> i64 {
    let text = text.trim_start();
    let end = text
        .char_indices()
        .take_while(|(i, c)| c.is_ascii_digit() || (*i == 0 && (*c == '-' || *c == '+')))
        .count();
    text[..end].parse().unwrap_or(0)
}

fn leading_float(text: &str) -> f64 {
    let text = text.trim_start();
    let mut end = 0;
    for (i, c) in text.char_indices() {
        if c.is_ascii_digit() || c == '.' || ((c == '-' || c == '+') && (i == 0 || matches!(text.as_bytes()[i - 1], b'e' | b'E'))) || c == 'e' || c == 'E' {
            end = i + c.len_utf8();
        } else {
            break;
        }
    }
    let mut slice = &text[..end];
    while !slice.is_empty() && slice.parse::<f64>().is_err() {
        slice = &slice[..slice.len() - 1];
    }
    slice.parse().unwrap_or(0.0)
}

// stringWithFormat: and friends take extra arguments in stack slots.
crate::libsystem::libc::variadic!(ios_string_with_format, 3, ios_string_with_format_impl);
crate::libsystem::libc::variadic!(ios_init_with_format, 3, ios_init_with_format_impl);
crate::libsystem::libc::variadic!(ios_append_format, 3, ios_append_format_impl);
crate::libsystem::libc::variadic!(ios_by_appending_format, 3, ios_by_appending_format_impl);

pub fn format_object(format: Id, va: *const u64) -> String {
    let format = get(format);
    let bytes = crate::varargs::format(format.as_bytes(), &mut crate::varargs::Args::new(va), true);
    String::from_utf8_lossy(&bytes).into_owned()
}

#[no_mangle]
extern "C" fn ios_string_with_format_impl(class: Class, _: Sel, format: Id, va: *const u64) -> Id {
    make_in(string_class_for(class), &format_object(format, va))
}

#[no_mangle]
extern "C" fn ios_init_with_format_impl(this: Id, _: Sel, format: Id, va: *const u64) -> Id {
    set(this, format_object(format, va).encode_utf16().collect());
    this
}

#[no_mangle]
extern "C" fn ios_append_format_impl(this: Id, _: Sel, format: Id, va: *const u64) {
    let mut value = units(this);
    value.extend(format_object(format, va).encode_utf16());
    set(this, value);
}

#[no_mangle]
extern "C" fn ios_by_appending_format_impl(this: Id, _: Sel, format: Id, va: *const u64) -> Id {
    let mut value = get(this);
    value.push_str(&format_object(format, va));
    make(&value)
}

extern "C" fn init_with_format_arguments(this: Id, _: Sel, format: Id, va: *const u64) -> Id {
    ios_init_with_format_impl(this, 0, format, va)
}

extern "C" fn path_components(this: Id, _: Sel) -> Id {
    let value = get(this);
    let mut parts: Vec<Id> = Vec::new();
    if value.starts_with('/') {
        parts.push(make("/"));
    }
    for part in value.split('/').filter(|p| !p.is_empty()) {
        parts.push(make(part));
    }
    crate::fw::collections::array_from(&parts)
}

/// +pathWithComponents: the parts joined by "/"; a leading "/" part makes
/// the path absolute, empty parts and doubled or trailing slashes go away.
extern "C" fn path_with_components(_: Class, _: Sel, components: Id) -> Id {
    let parts: Vec<String> = crate::fw::collections::array_items(components).into_iter().map(get).collect();
    let absolute = parts.first().is_some_and(|p| p.starts_with('/'));
    let inner: Vec<&str> = parts.iter().flat_map(|p| p.split('/')).filter(|p| !p.is_empty()).collect();
    make(&format!("{}{}", if absolute { "/" } else { "" }, inner.join("/")))
}

extern "C" fn by_appending_path_component(this: Id, _: Sel, other: Id) -> Id {
    let base = get(this);
    let other = get(other);
    let joined = if base.is_empty() {
        other
    } else if base.ends_with('/') {
        format!("{base}{}", other.trim_start_matches('/'))
    } else {
        format!("{base}/{}", other.trim_start_matches('/'))
    };
    make(&joined)
}

extern "C" fn by_appending_path_extension(this: Id, _: Sel, other: Id) -> Id {
    make(&format!("{}.{}", get(this).trim_end_matches('/'), get(other)))
}

extern "C" fn last_path_component(this: Id, _: Sel) -> Id {
    let value = get(this);
    let trimmed = value.trim_end_matches('/');
    make(trimmed.rsplit('/').next().unwrap_or(""))
}

extern "C" fn by_deleting_last_path_component(this: Id, _: Sel) -> Id {
    let value = get(this);
    let trimmed = value.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) => make("/"),
        Some(index) => make(&trimmed[..index]),
        None => make(""),
    }
}

extern "C" fn path_extension(this: Id, _: Sel) -> Id {
    let value = get(this);
    let last = value.rsplit('/').next().unwrap_or("");
    make(last.rfind('.').map(|i| &last[i + 1..]).unwrap_or(""))
}

extern "C" fn by_deleting_path_extension(this: Id, _: Sel) -> Id {
    let value = get(this);
    let slash = value.rfind('/').map(|i| i + 1).unwrap_or(0);
    match value[slash..].rfind('.') {
        Some(index) if index > 0 => make(&value[..slash + index]),
        _ => make(&value),
    }
}

extern "C" fn file_system_representation(this: Id, s: Sel) -> usize {
    utf8_string(this, s)
}

extern "C" fn is_absolute_path(this: Id, _: Sel) -> bool {
    get(this).starts_with(['/', '~'])
}

extern "C" fn by_standardizing_path(this: Id, _: Sel) -> Id {
    this
}

extern "C" fn components_separated_by_string(this: Id, _: Sel, separator: Id) -> Id {
    let value = get(this);
    let separator = get(separator);
    let parts: Vec<Id> = if separator.is_empty() {
        vec![make(&value)]
    } else {
        value.split(separator.as_str()).map(make).collect()
    };
    crate::fw::collections::array_from(&parts)
}

extern "C" fn by_replacing_occurrences(this: Id, _: Sel, target: Id, replacement: Id) -> Id {
    let target = get(target);
    if target.is_empty() {
        return make(&get(this));
    }
    make(&get(this).replace(target.as_str(), &get(replacement)))
}

extern "C" fn by_trimming(this: Id, _: Sel, _set: Id) -> Id {
    make(get(this).trim())
}

pub fn register(r: &mut Registry) {
    let string = define_class("NSString", "NSObject", 16);
    let mutable = define_class("NSMutableString", "NSString", 16);
    let constant = define_class("__NSCFConstantString", "NSString", 32);
    CLASSES.set([string, mutable, constant]).ok();
    r.data("__CFConstantStringClassReference", constant);
    methods(
        string,
        &[
            ("init", init_empty as usize),
            ("initWithUTF8String:", init_with_utf8 as usize),
            ("initWithString:", init_with_string as usize),
            ("initWithCharacters:length:", init_with_characters as usize),
            ("initWithBytes:length:encoding:", init_with_bytes as usize),
            ("initWithBytesNoCopy:length:encoding:freeWhenDone:", init_with_bytes_no_copy as usize),
            ("initWithCString:encoding:", init_with_c_string as usize),
            ("initWithFormat:", ios_init_with_format as usize),
            ("initWithFormat:arguments:", init_with_format_arguments as usize),
            ("UTF8String", utf8_string as usize),
            ("initWithContentsOfFile:usedEncoding:error:", init_with_contents_used as usize),
            ("initWithContentsOfFile:encoding:error:", init_with_contents_encoding as usize),
            ("initWithData:encoding:", init_with_data_encoding as usize),
            ("writeToFile:atomically:encoding:error:", write_to_file_encoding as usize),
            ("writeToFile:atomically:", write_to_file as usize),
            ("cStringUsingEncoding:", c_string_using as usize),
            ("getCString:maxLength:encoding:", get_c_string as usize),
            ("fileSystemRepresentation", file_system_representation as usize),
            ("length", length as usize),
            ("dataUsingEncoding:", data_using_encoding as usize),
            ("dataUsingEncoding:allowLossyConversion:", data_using_encoding_lossy as usize),
            ("lengthOfBytesUsingEncoding:", length_of_bytes as usize),
            ("characterAtIndex:", character_at as usize),
            ("getCharacters:range:", get_characters_range as usize),
            ("getCharacters:", get_characters as usize),
            ("isEqualToString:", is_equal_to_string as usize),
            ("isEqual:", is_equal as usize),
            ("hash", hash as usize),
            ("copyWithZone:", copy_with_zone as usize),
            ("mutableCopyWithZone:", mutable_copy_with_zone as usize),
            ("description", description as usize),
            ("compare:", compare as usize),
            ("compare:options:", compare_options as usize),
            ("caseInsensitiveCompare:", case_insensitive_compare as usize),
            ("hasPrefix:", has_prefix as usize),
            ("hasSuffix:", has_suffix as usize),
            ("stringByAppendingString:", by_appending_string as usize),
            ("stringByAppendingFormat:", ios_by_appending_format as usize),
            ("lowercaseString", lowercase as usize),
            ("uppercaseString", uppercase as usize),
            ("capitalizedString", capitalized as usize),
            ("substringFromIndex:", substring_from as usize),
            ("substringToIndex:", substring_to as usize),
            ("substringWithRange:", substring_with_range as usize),
            ("rangeOfString:", range_of_string as usize),
            ("rangeOfString:options:", range_of_string_options as usize),
            ("rangeOfString:options:range:", range_of_string_in as usize),
            ("intValue", int_value as usize),
            ("integerValue", integer_value as usize),
            ("longLongValue", integer_value as usize),
            ("floatValue", float_value as usize),
            ("doubleValue", double_value as usize),
            ("boolValue", bool_value as usize),
            ("pathComponents", path_components as usize),
            ("stringByAppendingPathComponent:", by_appending_path_component as usize),
            ("stringByAppendingPathExtension:", by_appending_path_extension as usize),
            ("lastPathComponent", last_path_component as usize),
            ("stringByDeletingLastPathComponent", by_deleting_last_path_component as usize),
            ("pathExtension", path_extension as usize),
            ("stringByDeletingPathExtension", by_deleting_path_extension as usize),
            ("isAbsolutePath", is_absolute_path as usize),
            ("stringByStandardizingPath", by_standardizing_path as usize),
            ("stringByExpandingTildeInPath", by_standardizing_path as usize),
            ("componentsSeparatedByString:", components_separated_by_string as usize),
            ("stringByReplacingOccurrencesOfString:withString:", by_replacing_occurrences as usize),
            ("stringByTrimmingCharactersInSet:", by_trimming as usize),
        ],
    );
    class_methods(
        string,
        &[
            ("string", empty_string as usize),
            ("stringWithUTF8String:", string_with_utf8 as usize),
            ("stringWithString:", string_with_string as usize),
            ("pathWithComponents:", path_with_components as usize),
            ("stringWithCharacters:length:", string_with_characters as usize),
            ("stringWithCString:encoding:", string_with_c_string as usize),
            ("stringWithFormat:", ios_string_with_format as usize),
            ("stringWithContentsOfFile:usedEncoding:error:", with_contents_used as usize),
            ("stringWithContentsOfFile:encoding:error:", with_contents_encoding as usize),
            ("stringWithContentsOfFile:", with_contents as usize),
        ],
    );
    class_methods(mutable, &[("stringWithCapacity:", string_with_capacity as usize)]);
    methods(
        mutable,
        &[
            ("initWithCapacity:", init_with_capacity as usize),
            ("appendString:", append_string as usize),
            ("appendFormat:", ios_append_format as usize),
            ("setString:", set_string as usize),
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each word (split by spaces, tabs or line ends) starts upper case, the rest lower case.
    #[test]
    fn capitalized_string_starts_each_word_upper_case() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let text = make("hELLO wORLD\tfoo-bar\nsuccess 1st");
        assert!(crate::objc::runtime::responds(text, "capitalizedString"), "-[NSString capitalizedString] is missing");
        let result = crate::objc::runtime::send(text, "capitalizedString", &[]);
        assert_eq!(get(result), "Hello World\tFoo-bar\nSuccess 1st");
    }

    /// -rangeOfString:options:(range:) honours case-insensitive, backwards and
    /// anchored searches and stays inside the range it is given.
    #[test]
    fn range_of_string_with_options_and_a_range() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        type Find = extern "C" fn(Id, Sel, Id, usize) -> Range;
        type FindIn = extern "C" fn(Id, Sel, Id, usize, Range) -> Range;
        let imp = |name: &str| crate::objc::runtime::imp_for(make("x"), crate::objc::runtime::sel(name)).unwrap_or_else(|| panic!("-[NSString {name}] is missing"));
        let find: Find = unsafe { std::mem::transmute(imp("rangeOfString:options:")) };
        let find_in: FindIn = unsafe { std::mem::transmute(imp("rangeOfString:options:range:")) };
        let at = |r: Range| (r.location as isize, r.length);
        let (text, sel) = (make("Spell=spell=SPELL"), crate::objc::runtime::sel("rangeOfString:options:"));
        let not_found = (isize::MAX, 0);
        assert_eq!(at(find(text, sel, make("spell"), 0)), (6, 5), "plain search is case-sensitive");
        assert_eq!(at(find(text, sel, make("spell"), 1)), (0, 5), "case-insensitive");
        assert_eq!(at(find(text, sel, make("spell"), 1 | 4)), (12, 5), "backwards finds the last one");
        assert_eq!(at(find(text, sel, make("spell"), 8)), not_found, "anchored: only at the start");
        assert_eq!(at(find(text, sel, make("SPELL"), 4 | 8)), (12, 5), "anchored backwards: only at the end");
        assert_eq!(at(find(text, sel, make(""), 0)), not_found, "an empty string is never found");
        assert_eq!(at(find_in(text, sel, make("spell"), 1, Range { location: 1, length: 9 })), not_found, "the match must fit the range");
        assert_eq!(at(find_in(text, sel, make("spell"), 1, Range { location: 1, length: 10 })), (6, 5));
    }

    /// Paths from the root or the home folder (~) are absolute.
    #[test]
    fn paths_from_the_root_or_home_are_absolute() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let absolute = |path: &str| crate::objc::runtime::send(make(path), "isAbsolutePath", &[]) as u8 != 0;
        assert_eq!([absolute("/a/b.tmx"), absolute("~/a"), absolute("a/b.tmx"), absolute("")], [true, true, false, false]);
    }

    /// +pathWithComponents: undoes -pathComponents (CocosDenshion finds its
    /// sound files this way; a file with no folder gives no components).
    #[test]
    fn path_with_components_joins_what_path_components_splits() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let class = crate::objc::runtime::class_named("NSString");
        let join = |parts: &[&str]| {
            let items: Vec<Id> = parts.iter().map(|p| make(p)).collect();
            get(crate::objc::runtime::send(class, "pathWithComponents:", &[crate::fw::collections::array_from(&items)]))
        };
        assert_eq!(join(&["/", "tmp", "scratch.tiff"]), "/tmp/scratch.tiff");
        assert_eq!(join(&["Sounds", "hit.caf"]), "Sounds/hit.caf");
        assert_eq!(join(&["a", "", "b/"]), "a/b");
        assert_eq!(join(&[]), "");
        assert_eq!(join(&["/"]), "/");
        let split = crate::objc::runtime::send(make("/x/y.png"), "pathComponents", &[]);
        assert_eq!(get(crate::objc::runtime::send(class, "pathWithComponents:", &[split])), "/x/y.png");
    }
}
