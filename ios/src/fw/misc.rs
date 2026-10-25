//! Smaller Foundation pieces: NSSet, NSUUID, NSNull, NSDate, NSError and
//! the class/selector name functions.

use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::{collections, string};
use crate::objc::runtime::{self, lock, Class, Id, Sel};
use crate::symbols::{leak, Registry};
use std::sync::OnceLock;

// ----- NSSet / NSMutableSet: { isa, *mut Vec<Id> } (equal items kept once) -----

fn items(object: Id) -> &'static mut Vec<Id> {
    unsafe {
        let slot = (object + 8) as *mut *mut Vec<Id>;
        if (*slot).is_null() {
            *slot = Box::into_raw(Box::new(Vec::new()));
        }
        &mut **slot
    }
}

fn same(a: Id, b: Id) -> bool {
    a == b || (a != 0 && b != 0 && runtime::send(a, "isEqual:", &[b]) & 0xFF != 0)
}

/// A new NSSet holding these objects.
pub fn set_from(list: &[Id]) -> Id {
    let set = runtime::alloc_instance(runtime::class_named("NSSet"));
    for &item in list {
        add(set, item);
    }
    set
}

fn add(set: Id, item: Id) {
    if item != 0 && !items(set).iter().any(|&i| same(i, item)) {
        // Kept, as Apple's set retains (fw/counted.rs; never let go).
        crate::fw::counted::keep(item);
        items(set).push(item);
    }
}

extern "C" fn set_empty(class: Class, _: Sel) -> Id {
    let object = runtime::alloc_instance(class);
    items(object);
    object
}

extern "C" fn set_with_capacity(class: Class, s: Sel, _capacity: usize) -> Id {
    set_empty(class, s)
}

extern "C" fn set_init(this: Id, _: Sel) -> Id {
    items(this);
    this
}

extern "C" fn set_init_capacity(this: Id, _: Sel, _capacity: usize) -> Id {
    items(this);
    this
}

extern "C" fn set_with_object(class: Class, _: Sel, item: Id) -> Id {
    let object = runtime::alloc_instance(class);
    add(object, item);
    object
}

extern "C" fn set_with_array(class: Class, _: Sel, array: Id) -> Id {
    let object = runtime::alloc_instance(class);
    for item in collections::array_items(array) {
        add(object, item);
    }
    object
}

extern "C" fn set_with_set(class: Class, s: Sel, other: Id) -> Id {
    let all = if other == 0 { collections::array_from(&[]) } else { runtime::send(other, "allObjects", &[]) };
    set_with_array(class, s, all)
}

extern "C" fn set_count(this: Id, _: Sel) -> usize {
    items(this).len()
}

extern "C" fn set_contains(this: Id, _: Sel, item: Id) -> bool {
    items(this).iter().any(|&i| same(i, item))
}

extern "C" fn set_member(this: Id, _: Sel, item: Id) -> Id {
    items(this).iter().copied().find(|&i| same(i, item)).unwrap_or(0)
}

extern "C" fn set_any(this: Id, _: Sel) -> Id {
    items(this).first().copied().unwrap_or(0)
}

extern "C" fn set_all(this: Id, _: Sel) -> Id {
    collections::array_from(&items(this).clone())
}

extern "C" fn set_add(this: Id, _: Sel, item: Id) {
    add(this, item);
}

extern "C" fn set_remove(this: Id, _: Sel, item: Id) {
    items(this).retain(|&i| !same(i, item));
}

extern "C" fn set_remove_all(this: Id, _: Sel) {
    items(this).clear();
}

extern "C" fn set_enumerator(this: Id, _: Sel) -> Id {
    collections::enumerator(items(this).clone())
}

extern "C" fn set_copy(this: Id, _: Sel, _zone: usize) -> Id {
    let object = runtime::alloc_instance(runtime::class_named("NSSet"));
    *items(object) = items(this).clone();
    object
}

extern "C" fn set_mutable_copy(this: Id, _: Sel, _zone: usize) -> Id {
    let object = runtime::alloc_instance(runtime::class_named("NSMutableSet"));
    *items(object) = items(this).clone();
    object
}

extern "C" fn set_enumerate(this: Id, _: Sel, state: *mut [usize; 8], buffer: *mut Id, length: usize) -> usize {
    unsafe {
        let list = items(this);
        let start = (*state)[0];
        if start >= list.len() {
            return 0;
        }
        let count = (list.len() - start).min(length.max(1));
        for index in 0..count {
            *buffer.add(index) = list[start + index];
        }
        (*state)[0] = start + count;
        (*state)[1] = buffer as usize;
        (*state)[2] = (&(*state)[3]) as *const usize as usize;
        count
    }
}

crate::libsystem::libc::variadic!(ios_set_with_objects, 3, ios_set_with_objects_impl);
crate::libsystem::libc::variadic!(ios_set_init_with_objects, 3, ios_set_init_with_objects_impl);

fn nil_terminated(first: Id, va: *const u64) -> Vec<Id> {
    let mut list = Vec::new();
    if first == 0 {
        return list;
    }
    list.push(first);
    let mut index = 0;
    loop {
        let item = unsafe { *va.add(index) } as Id;
        if item == 0 {
            break;
        }
        list.push(item);
        index += 1;
    }
    list
}

#[no_mangle]
extern "C" fn ios_set_with_objects_impl(class: Class, _: Sel, first: Id, va: *const u64) -> Id {
    let object = runtime::alloc_instance(class);
    for item in nil_terminated(first, va) {
        add(object, item);
    }
    object
}

#[no_mangle]
extern "C" fn ios_set_init_with_objects_impl(this: Id, _: Sel, first: Id, va: *const u64) -> Id {
    for item in nil_terminated(first, va) {
        add(this, item);
    }
    this
}

extern "C" fn set_init_with_array(this: Id, _: Sel, array: Id) -> Id {
    for item in collections::array_items(array) {
        add(this, item);
    }
    this
}

extern "C" fn set_with_objects_count(class: Class, _: Sel, list: *const Id, count: usize) -> Id {
    let object = runtime::alloc_instance(class);
    for index in 0..count {
        add(object, unsafe { *list.add(index) });
    }
    object
}

extern "C" fn set_add_from_array(this: Id, _: Sel, array: Id) {
    for item in collections::array_items(array) {
        add(this, item);
    }
}

extern "C" fn set_union(this: Id, _: Sel, other: Id) {
    for item in items(other).clone() {
        add(this, item);
    }
}

extern "C" fn set_minus(this: Id, _: Sel, other: Id) {
    let remove = items(other).clone();
    items(this).retain(|&i| !remove.iter().any(|&r| same(i, r)));
}

extern "C" fn set_intersects(this: Id, _: Sel, other: Id) -> bool {
    items(this).iter().any(|&i| items(other).iter().any(|&o| same(i, o)))
}

extern "C" fn set_is_subset(this: Id, _: Sel, other: Id) -> bool {
    items(this).iter().all(|&i| items(other).iter().any(|&o| same(i, o)))
}

extern "C" fn set_adding(this: Id, _: Sel, item: Id) -> Id {
    let object = set_copy(this, 0, 0);
    add(object, item);
    object
}

extern "C" fn set_adding_set(this: Id, _: Sel, other: Id) -> Id {
    let object = set_copy(this, 0, 0);
    let more = if other == 0 { Vec::new() } else { collections::array_items(runtime::send(other, "allObjects", &[])) };
    more.into_iter().for_each(|item| add(object, item));
    object
}

extern "C" fn set_adding_array(this: Id, _: Sel, array: Id) -> Id {
    let object = set_copy(this, 0, 0);
    collections::array_items(array).into_iter().for_each(|item| add(object, item));
    object
}

extern "C" fn set_intersect(this: Id, _: Sel, other: Id) {
    let keep = if other == 0 { Vec::new() } else { collections::array_items(runtime::send(other, "allObjects", &[])) };
    items(this).retain(|&i| keep.iter().any(|&k| same(i, k)));
}

/// The members a test block (object, &stop) -> BOOL picks, as a new set.
extern "C" fn set_objects_passing(this: Id, _: Sel, block: usize) -> Id {
    let header = block as *const crate::libsystem::mach::BlockHeader;
    let invoke: extern "C" fn(usize, Id, *mut u8) -> u8 = unsafe { std::mem::transmute((*header).invoke) };
    let result = set_empty(runtime::class_named("NSSet"), 0);
    let mut stop = 0u8;
    for item in items(this).clone() {
        if invoke(block, item, &mut stop) != 0 {
            add(result, item);
        }
        if stop != 0 {
            break;
        }
    }
    result
}

extern "C" fn set_enumerate_block(this: Id, _: Sel, block: usize) {
    let header = block as *const crate::libsystem::mach::BlockHeader;
    let invoke: extern "C" fn(usize, Id, *mut u8) = unsafe { std::mem::transmute((*header).invoke) };
    let mut stop = 0u8;
    for item in items(this).clone() {
        invoke(block, item, &mut stop);
        if stop != 0 {
            break;
        }
    }
}

// ----- NSUUID: { isa, [u8; 16] } -----

pub fn uuid_from_string(text: &str) -> Id {
    let hex: String = text.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 32 {
        return 0;
    }
    let object = runtime::alloc_instance(runtime::class_named("NSUUID"));
    let bytes = unsafe { &mut *((object + 8) as *mut [u8; 16]) };
    for i in 0..16 {
        bytes[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap_or(0);
    }
    object
}

fn uuid_text(bytes: &[u8; 16]) -> String {
    let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02X}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        hex[0..4].concat(),
        hex[4..6].concat(),
        hex[6..8].concat(),
        hex[8..10].concat(),
        hex[10..16].concat()
    )
}

extern "C" fn uuid_init(this: Id, _: Sel) -> Id {
    let bytes = unsafe { &mut *((this + 8) as *mut [u8; 16]) };
    unsafe { libc::getrandom(bytes.as_mut_ptr() as *mut _, 16, 0) };
    bytes[6] = (bytes[6] & 0x0F) | 0x40;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    this
}

extern "C" fn uuid_init_with_string(_this: Id, _: Sel, text: Id) -> Id {
    uuid_from_string(&string::get(text))
}

extern "C" fn uuid_string(this: Id, _: Sel) -> Id {
    string::make(&uuid_text(unsafe { &*((this + 8) as *const [u8; 16]) }))
}

extern "C" fn uuid_get_bytes(this: Id, _: Sel, out: *mut [u8; 16]) {
    unsafe { *out = *((this + 8) as *const [u8; 16]) };
}

// ----- NSNull -----

static NULL: OnceLock<Id> = OnceLock::new();

extern "C" fn null_object(_: Class, _: Sel) -> Id {
    *NULL.get_or_init(|| runtime::alloc_instance(runtime::class_named("NSNull")))
}

// ----- NSDate: { isa, seconds since 2001-01-01 } -----

const SINCE_1970: f64 = 978_307_200.0;

pub fn now_since_2001() -> f64 {
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &mut t) };
    t.tv_sec as f64 + t.tv_nsec as f64 / 1e9 - SINCE_1970
}

pub fn date(seconds: f64) -> Id {
    let object = runtime::alloc_instance(runtime::class_named("NSDate"));
    unsafe { *((object + 8) as *mut f64) = seconds };
    object
}

pub fn date_value(object: Id) -> f64 {
    unsafe { *((object + 8) as *const f64) }
}

extern "C" fn date_now(_: Class, _: Sel) -> Id {
    date(now_since_2001())
}

extern "C" fn date_init(this: Id, _: Sel) -> Id {
    unsafe { *((this + 8) as *mut f64) = now_since_2001() };
    this
}

extern "C" fn date_init_since_1970(this: Id, _: Sel, seconds: f64) -> Id {
    unsafe { *((this + 8) as *mut f64) = seconds - SINCE_1970 };
    this
}

extern "C" fn date_init_since_now(this: Id, _: Sel, offset: f64) -> Id {
    unsafe { *((this + 8) as *mut f64) = now_since_2001() + offset };
    this
}

extern "C" fn date_init_since_reference(this: Id, _: Sel, seconds: f64) -> Id {
    unsafe { *((this + 8) as *mut f64) = seconds };
    this
}

extern "C" fn date_init_since_date(this: Id, _: Sel, seconds: f64, other: Id) -> Id {
    let base = if other == 0 { now_since_2001() } else { date_value(other) };
    unsafe { *((this + 8) as *mut f64) = base + seconds };
    this
}

extern "C" fn date_since_now(_: Class, _: Sel, offset: f64) -> Id {
    date(now_since_2001() + offset)
}

extern "C" fn date_since_1970(_: Class, _: Sel, seconds: f64) -> Id {
    date(seconds - SINCE_1970)
}

extern "C" fn date_since_reference(_: Class, _: Sel, seconds: f64) -> Id {
    date(seconds)
}

extern "C" fn date_distant_future(_: Class, _: Sel) -> Id {
    date(63_113_904_000.0)
}

extern "C" fn date_distant_past(_: Class, _: Sel) -> Id {
    date(-63_114_076_800.0)
}

extern "C" fn date_interval_1970(this: Id, _: Sel) -> f64 {
    date_value(this) + SINCE_1970
}

extern "C" fn date_interval_reference(this: Id, _: Sel) -> f64 {
    date_value(this)
}

extern "C" fn date_interval_now(this: Id, _: Sel) -> f64 {
    date_value(this) - now_since_2001()
}

extern "C" fn date_interval_since(this: Id, _: Sel, other: Id) -> f64 {
    date_value(this) - date_value(other)
}

extern "C" fn date_reference_now(_: Class, _: Sel) -> f64 {
    now_since_2001()
}

extern "C" fn date_by_adding(this: Id, _: Sel, seconds: f64) -> Id {
    date(date_value(this) + seconds)
}

extern "C" fn date_compare(this: Id, _: Sel, other: Id) -> isize {
    let (a, b) = (date_value(this), date_value(other));
    if a < b { -1 } else if a > b { 1 } else { 0 }
}

extern "C" fn date_is_equal(this: Id, _: Sel, other: Id) -> bool {
    other != 0 && date_value(this) == date_value(other)
}

extern "C" fn date_description(this: Id, _: Sel) -> Id {
    string::make(&format!("{:.3} seconds since 1970", date_value(this) + SINCE_1970))
}

extern "C" fn absolute_time_get_current() -> f64 {
    now_since_2001()
}

// ----- NSError: { isa, domain, code, userInfo } -----

pub fn error(domain: &str, code: isize, description: &str) -> Id {
    let object = runtime::alloc_instance(runtime::class_named("NSError"));
    let info = collections::dictionary_from(&[(string::make("NSLocalizedDescription"), string::make(description))]);
    unsafe {
        *((object + 8) as *mut Id) = string::make(domain);
        *((object + 16) as *mut isize) = code;
        *((object + 24) as *mut Id) = info;
    }
    object
}

extern "C" fn error_with_domain(_: Class, _: Sel, domain: Id, code: isize, info: Id) -> Id {
    let object = runtime::alloc_instance(runtime::class_named("NSError"));
    unsafe {
        *((object + 8) as *mut Id) = domain;
        *((object + 16) as *mut isize) = code;
        *((object + 24) as *mut Id) = info;
    }
    object
}

extern "C" fn error_init_with_domain(this: Id, _: Sel, domain: Id, code: isize, info: Id) -> Id {
    unsafe {
        *((this + 8) as *mut Id) = domain;
        *((this + 16) as *mut isize) = code;
        *((this + 24) as *mut Id) = info;
    }
    this
}

extern "C" fn error_domain(this: Id, _: Sel) -> Id {
    unsafe { *((this + 8) as *const Id) }
}

extern "C" fn error_code(this: Id, _: Sel) -> isize {
    unsafe { *((this + 16) as *const isize) }
}

extern "C" fn error_user_info(this: Id, _: Sel) -> Id {
    unsafe { *((this + 24) as *const Id) }
}

extern "C" fn error_description(this: Id, _: Sel) -> Id {
    let info = error_user_info(this, 0);
    let text = collections::dictionary_get(info, string::make("NSLocalizedDescription"));
    if text != 0 {
        return text;
    }
    string::make(&format!("{} error {}", string::get(error_domain(this, 0)), error_code(this, 0)))
}

/// A userInfo string (the reason, the suggestion), nil when the error has none.
fn error_info_text(this: Id, key: &str) -> Id {
    collections::dictionary_get(error_user_info(this, 0), string::make(key))
}

extern "C" fn error_failure_reason(this: Id, _: Sel) -> Id {
    error_info_text(this, "NSLocalizedFailureReason")
}

extern "C" fn error_recovery_suggestion(this: Id, _: Sel) -> Id {
    error_info_text(this, "NSLocalizedRecoverySuggestion")
}

// ----- NSURL: { isa, text } (file links keep the plain path) -----

fn url(text: &str, file: bool) -> Id {
    let object = runtime::alloc_instance(runtime::class_named("NSURL"));
    let full = if file && !text.starts_with("file://") { format!("file://{text}") } else { text.to_string() };
    unsafe { *((object + 8) as *mut Id) = string::make(&full) };
    object
}

fn url_text(object: Id) -> String {
    string::get(unsafe { *((object + 8) as *const Id) })
}

extern "C" fn url_file(_: Class, _: Sel, path: Id) -> Id {
    if path == 0 { 0 } else { url(&string::get(path), true) }
}

extern "C" fn url_file_dir(class: Class, s: Sel, path: Id, _directory: u8) -> Id {
    url_file(class, s, path)
}

extern "C" fn url_with_string(_: Class, _: Sel, text: Id) -> Id {
    if text == 0 { 0 } else { url(&string::get(text), false) }
}

extern "C" fn url_init_with_string(_this: Id, _: Sel, text: Id) -> Id {
    if text == 0 { 0 } else { url(&string::get(text), false) }
}

/// A link's parts: scheme, authority, path, query, fragment (RFC 3986 appendix B).
struct Parts<'a> {
    scheme: Option<&'a str>,
    authority: Option<&'a str>,
    path: &'a str,
    query: Option<&'a str>,
    fragment: Option<&'a str>,
}

fn split_link(text: &str) -> Parts<'_> {
    let (rest, fragment) = match text.split_once('#') {
        Some((rest, fragment)) => (rest, Some(fragment)),
        None => (text, None),
    };
    let (rest, query) = match rest.split_once('?') {
        Some((rest, query)) => (rest, Some(query)),
        None => (rest, None),
    };
    let (scheme, rest) = match rest.find(':') {
        Some(colon) if colon > 0 && !rest[..colon].contains('/') => (Some(&rest[..colon]), &rest[colon + 1..]),
        _ => (None, rest),
    };
    let (authority, path) = match rest.strip_prefix("//") {
        Some(after) => {
            let end = after.find('/').unwrap_or(after.len());
            (Some(&after[..end]), &after[end..])
        }
        None => (None, rest),
    };
    Parts { scheme, authority, path, query, fragment }
}

fn remove_dot_segments(path: &str) -> String {
    let mut output: Vec<&str> = Vec::new();
    let segments: Vec<&str> = path.split('/').collect();
    for (index, segment) in segments.iter().enumerate() {
        let last = index + 1 == segments.len();
        match *segment {
            "." => {
                if last {
                    output.push("");
                }
            }
            ".." => {
                if output.len() > 1 || output.first().is_some_and(|first| !first.is_empty()) {
                    output.pop();
                }
                if last {
                    output.push("");
                }
            }
            _ => output.push(segment),
        }
    }
    let joined = output.join("/");
    if path.starts_with('/') && !joined.starts_with('/') { format!("/{joined}") } else { joined }
}

/// Resolves `reference` against `base` (RFC 3986 section 5.2.2).
fn resolve_link(base: &str, reference: &str) -> String {
    let (b, r) = (split_link(base), split_link(reference));
    let (scheme, authority, path, query);
    if r.scheme.is_some() {
        (scheme, authority, path, query) = (r.scheme, r.authority, remove_dot_segments(r.path), r.query);
    } else {
        scheme = b.scheme;
        if r.authority.is_some() {
            (authority, path, query) = (r.authority, remove_dot_segments(r.path), r.query);
        } else {
            authority = b.authority;
            if r.path.is_empty() {
                (path, query) = (b.path.to_string(), r.query.or(b.query));
            } else {
                let merged = if r.path.starts_with('/') {
                    r.path.to_string()
                } else if b.authority.is_some() && b.path.is_empty() {
                    format!("/{}", r.path)
                } else {
                    format!("{}{}", &b.path[..b.path.rfind('/').map_or(0, |slash| slash + 1)], r.path)
                };
                (path, query) = (remove_dot_segments(&merged), r.query);
            }
        }
    }
    let mut out = String::new();
    if let Some(scheme) = scheme {
        out += scheme;
        out.push(':');
    }
    if let Some(authority) = authority {
        out += "//";
        out += authority;
    }
    out += &path;
    if let Some(query) = query {
        out.push('?');
        out += query;
    }
    if let Some(fragment) = r.fragment {
        out.push('#');
        out += fragment;
    }
    out
}

extern "C" fn url_with_string_relative(_: Class, _: Sel, text: Id, base: Id) -> Id {
    match (text, base) {
        (0, _) => 0,
        (_, 0) => url(&string::get(text), false),
        _ => url(&resolve_link(&url_text(base), &string::get(text)), false),
    }
}

extern "C" fn url_init_with_string_relative(_this: Id, s: Sel, text: Id, base: Id) -> Id {
    url_with_string_relative(0, s, text, base)
}

extern "C" fn url_init_file(_this: Id, _: Sel, path: Id) -> Id {
    if path == 0 { 0 } else { url(&string::get(path), true) }
}

extern "C" fn url_absolute_string(this: Id, _: Sel) -> Id {
    string::make(&url_text(this))
}

extern "C" fn url_path(this: Id, _: Sel) -> Id {
    let text = url_text(this);
    let rest = match text.find("://") {
        Some(i) => &text[i + 3..],
        None => return string::make(&text),
    };
    let path = match rest.find('/') {
        Some(i) => &rest[i..],
        None => "",
    };
    string::make(path.split(['?', '#']).next().unwrap_or(""))
}

extern "C" fn url_scheme(this: Id, _: Sel) -> Id {
    let text = url_text(this);
    string::make(text.split("://").next().unwrap_or(""))
}

extern "C" fn url_host(this: Id, _: Sel) -> Id {
    let text = url_text(this);
    let rest = text.split("://").nth(1).unwrap_or("");
    string::make(rest.split(['/', '?', ':']).next().unwrap_or(""))
}

extern "C" fn url_query(this: Id, _: Sel) -> Id {
    let text = url_text(this);
    let before_fragment = text.split('#').next().unwrap_or("");
    match before_fragment.split_once('?') {
        Some((_, query)) => string::make(query),
        None => 0,
    }
}

extern "C" fn url_fragment(this: Id, _: Sel) -> Id {
    match url_text(this).split_once('#') {
        Some((_, fragment)) => string::make(fragment),
        None => 0,
    }
}

extern "C" fn url_port(this: Id, _: Sel) -> Id {
    let text = url_text(this);
    let rest = text.split("://").nth(1).unwrap_or("");
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    match authority.rsplit_once(':').and_then(|(_, p)| p.parse::<i64>().ok()) {
        Some(port) => collections::number_int(port),
        None => 0,
    }
}

/// Everything after the scheme's colon ("//host/path?query#fragment").
extern "C" fn url_resource_specifier(this: Id, _: Sel) -> Id {
    let text = url_text(this);
    match split_link(&text).scheme {
        Some(scheme) => string::make(&text[scheme.len() + 1..]),
        None => string::make(&text),
    }
}

extern "C" fn url_self(this: Id, _: Sel) -> Id {
    this
}

extern "C" fn url_nil(_this: Id, _: Sel) -> Id {
    0
}

extern "C" fn url_deleting_last(this: Id, _: Sel) -> Id {
    let text = url_text(this);
    let trimmed = text.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(i) => url(&text[..=i], false),
        None => this,
    }
}

extern "C" fn url_path_extension(this: Id, s: Sel) -> Id {
    runtime::send(url_path(this, s), "pathExtension", &[])
}

extern "C" fn url_path_components(this: Id, s: Sel) -> Id {
    runtime::send(url_path(this, s), "pathComponents", &[])
}

extern "C" fn url_is_file(this: Id, _: Sel) -> bool {
    url_text(this).starts_with("file://")
}

extern "C" fn url_last_component(this: Id, s: Sel) -> Id {
    let path = url_path(this, s);
    runtime::send(path, "lastPathComponent", &[])
}

extern "C" fn url_appending_component(this: Id, _: Sel, part: Id) -> Id {
    let text = url_text(this);
    let joined = if text.ends_with('/') { format!("{text}{}", string::get(part)) } else { format!("{text}/{}", string::get(part)) };
    url(&joined, false)
}

extern "C" fn url_is_equal(this: Id, _: Sel, other: Id) -> bool {
    other != 0 && runtime::class_named("NSURL") == lock().class_of(other) && url_text(this) == url_text(other)
}

extern "C" fn url_set_resource_value(_this: Id, _: Sel, _value: Id, _key: Id, _error: usize) -> bool {
    true
}

// ----- class and selector names -----

extern "C" fn class_from_string(name: Id) -> Class {
    if name == 0 {
        return 0;
    }
    let class = runtime::class_named(&string::get(name));
    if class != 0 && lock().classes.get(&class).map(|r| r.placeholder).unwrap_or(false) {
        return 0;
    }
    class
}

extern "C" fn string_from_class(class: Class) -> Id {
    if class == 0 {
        return 0;
    }
    let name = lock().class_name(class);
    string::make(&name)
}

extern "C" fn selector_from_string(name: Id) -> Sel {
    if name == 0 {
        return 0;
    }
    runtime::sel(&string::get(name))
}

extern "C" fn string_from_selector(sel: Sel) -> Id {
    if sel == 0 {
        return 0;
    }
    let name = lock().sel_name(sel);
    string::make(&name)
}

pub fn register(r: &mut Registry) {
    let set = define_class("NSSet", "NSObject", 16);
    let mutable = define_class("NSMutableSet", "NSSet", 16);
    class_methods(
        set,
        &[
            ("set", set_empty as usize),
            ("setWithObject:", set_with_object as usize),
            ("setWithArray:", set_with_array as usize),
            ("setWithSet:", set_with_set as usize),
            ("setWithObjects:", ios_set_with_objects as usize),
            ("setWithObjects:count:", set_with_objects_count as usize),
        ],
    );
    methods(
        set,
        &[
            ("init", set_init as usize),
            ("count", set_count as usize),
            ("containsObject:", set_contains as usize),
            ("member:", set_member as usize),
            ("anyObject", set_any as usize),
            ("allObjects", set_all as usize),
            ("objectEnumerator", set_enumerator as usize),
            ("copyWithZone:", set_copy as usize),
            ("mutableCopyWithZone:", set_mutable_copy as usize),
            ("countByEnumeratingWithState:objects:count:", set_enumerate as usize),
            ("initWithObjects:", ios_set_init_with_objects as usize),
            ("initWithArray:", set_init_with_array as usize),
            ("intersectsSet:", set_intersects as usize),
            ("isSubsetOfSet:", set_is_subset as usize),
            ("setByAddingObject:", set_adding as usize),
            ("setByAddingObjectsFromSet:", set_adding_set as usize),
            ("setByAddingObjectsFromArray:", set_adding_array as usize),
            ("objectsPassingTest:", set_objects_passing as usize),
            ("enumerateObjectsUsingBlock:", set_enumerate_block as usize),
        ],
    );
    class_methods(mutable, &[("setWithCapacity:", set_with_capacity as usize)]);
    methods(
        mutable,
        &[
            ("initWithCapacity:", set_init_capacity as usize),
            ("addObject:", set_add as usize),
            ("removeObject:", set_remove as usize),
            ("removeAllObjects", set_remove_all as usize),
            ("addObjectsFromArray:", set_add_from_array as usize),
            ("unionSet:", set_union as usize),
            ("minusSet:", set_minus as usize),
            ("intersectSet:", set_intersect as usize),
        ],
    );

    let uuid = define_class("NSUUID", "NSObject", 24);
    class_methods(uuid, &[("UUID", uuid_new as usize)]);
    methods(
        uuid,
        &[
            ("init", uuid_init as usize),
            ("initWithUUIDString:", uuid_init_with_string as usize),
            ("UUIDString", uuid_string as usize),
            ("getUUIDBytes:", uuid_get_bytes as usize),
            ("description", uuid_string as usize),
        ],
    );

    let null = define_class("NSNull", "NSObject", 8);
    class_methods(null, &[("null", null_object as usize)]);
    // kCFNull is the one NSNull (toll-free bridged).
    r.data("kCFNull", leak(null_object(0, 0)));

    let date_class = define_class("NSDate", "NSObject", 16);
    class_methods(
        date_class,
        &[
            ("date", date_now as usize),
            ("dateWithTimeIntervalSinceNow:", date_since_now as usize),
            ("dateWithTimeIntervalSince1970:", date_since_1970 as usize),
            ("dateWithTimeIntervalSinceReferenceDate:", date_since_reference as usize),
            ("distantFuture", date_distant_future as usize),
            ("distantPast", date_distant_past as usize),
            ("timeIntervalSinceReferenceDate", date_reference_now as usize),
        ],
    );
    methods(
        date_class,
        &[
            ("init", date_init as usize),
            ("initWithTimeIntervalSince1970:", date_init_since_1970 as usize),
            ("initWithTimeIntervalSinceNow:", date_init_since_now as usize),
            ("initWithTimeIntervalSinceReferenceDate:", date_init_since_reference as usize),
            ("initWithTimeInterval:sinceDate:", date_init_since_date as usize),
            ("timeIntervalSince1970", date_interval_1970 as usize),
            ("timeIntervalSinceReferenceDate", date_interval_reference as usize),
            ("timeIntervalSinceNow", date_interval_now as usize),
            ("timeIntervalSinceDate:", date_interval_since as usize),
            ("dateByAddingTimeInterval:", date_by_adding as usize),
            ("compare:", date_compare as usize),
            ("isEqual:", date_is_equal as usize),
            ("isEqualToDate:", date_is_equal as usize),
            ("description", date_description as usize),
        ],
    );
    r.function("CFAbsoluteTimeGetCurrent", absolute_time_get_current as usize);

    let error_class = define_class("NSError", "NSObject", 32);
    class_methods(error_class, &[("errorWithDomain:code:userInfo:", error_with_domain as usize)]);
    methods(
        error_class,
        &[
            ("initWithDomain:code:userInfo:", error_init_with_domain as usize),
            ("domain", error_domain as usize),
            ("code", error_code as usize),
            ("userInfo", error_user_info as usize),
            ("localizedDescription", error_description as usize),
            ("localizedFailureReason", error_failure_reason as usize),
            ("localizedRecoverySuggestion", error_recovery_suggestion as usize),
            ("description", error_description as usize),
        ],
    );

    let url_class = define_class("NSURL", "NSObject", 16);
    class_methods(
        url_class,
        &[
            ("fileURLWithPath:", url_file as usize),
            ("fileURLWithPath:isDirectory:", url_file_dir as usize),
            ("URLWithString:", url_with_string as usize),
            ("URLWithString:relativeToURL:", url_with_string_relative as usize),
        ],
    );
    methods(
        url_class,
        &[
            ("initWithString:", url_init_with_string as usize),
            ("initWithString:relativeToURL:", url_init_with_string_relative as usize),
            ("initFileURLWithPath:", url_init_file as usize),
            ("absoluteString", url_absolute_string as usize),
            ("description", url_absolute_string as usize),
            ("path", url_path as usize),
            ("scheme", url_scheme as usize),
            ("host", url_host as usize),
            ("isFileURL", url_is_file as usize),
            ("query", url_query as usize),
            ("fragment", url_fragment as usize),
            ("port", url_port as usize),
            ("resourceSpecifier", url_resource_specifier as usize),
            ("relativeString", url_absolute_string as usize),
            ("relativePath", url_path as usize),
            ("standardizedURL", url_self as usize),
            ("absoluteURL", url_self as usize),
            ("baseURL", url_nil as usize),
            ("user", url_nil as usize),
            ("password", url_nil as usize),
            ("URLByDeletingLastPathComponent", url_deleting_last as usize),
            ("pathExtension", url_path_extension as usize),
            ("pathComponents", url_path_components as usize),
            ("lastPathComponent", url_last_component as usize),
            ("URLByAppendingPathComponent:", url_appending_component as usize),
            ("isEqual:", url_is_equal as usize),
            ("setResourceValue:forKey:error:", url_set_resource_value as usize),
        ],
    );

    r.function("NSClassFromString", class_from_string as usize);
    r.function("NSStringFromClass", string_from_class as usize);
    r.function("NSSelectorFromString", selector_from_string as usize);
    r.function("NSStringFromSelector", string_from_selector as usize);
    r.data("kCFBooleanTrue", leak(collections::number_bool(true)));
    r.data("kCFBooleanFalse", leak(collections::number_bool(false)));
    r.data("kCFAllocatorDefault", leak(0usize));
    r.function("CFUUIDCreate", cf_uuid_create as usize);
    r.function("CFUUIDCreateString", cf_uuid_create_string as usize);
    r.function("CFUUIDCreateFromString", cf_uuid_create_from_string as usize);
    r.function("CFUUIDGetUUIDBytes", cf_uuid_get_bytes as usize);
}

// CFUUID: the same object as NSUUID here (the game only uses CF functions on it).

extern "C" fn cf_uuid_create(_allocator: usize) -> Id {
    uuid_new(runtime::class_named("NSUUID"), 0)
}

extern "C" fn cf_uuid_create_string(_allocator: usize, uuid: Id) -> Id {
    if uuid == 0 {
        return 0;
    }
    uuid_string(uuid, 0)
}

extern "C" fn cf_uuid_create_from_string(_allocator: usize, text: Id) -> Id {
    if text == 0 {
        return 0;
    }
    uuid_from_string(&string::get(text))
}

#[repr(C)]
pub struct UuidBytes([u8; 16]);

extern "C" fn cf_uuid_get_bytes(uuid: Id) -> UuidBytes {
    if uuid == 0 {
        return UuidBytes([0; 16]);
    }
    UuidBytes(unsafe { *((uuid + 8) as *const [u8; 16]) })
}

extern "C" fn uuid_new(class: Class, _: Sel) -> Id {
    let object = runtime::alloc_instance(class);
    uuid_init(object, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A link relative to a base resolves the standard way (RFC 3986's own examples).
    #[test]
    fn relative_links_resolve_against_their_base() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let class = runtime::class_named("NSURL");
        assert!(runtime::responds(class, "URLWithString:relativeToURL:"), "+[NSURL URLWithString:relativeToURL:] is missing");
        let base = runtime::send(class, "URLWithString:", &[string::make("http://a/b/c/d;p?q")]);
        for (reference, expected) in [
            ("g", "http://a/b/c/g"),
            ("./g", "http://a/b/c/g"),
            ("g/", "http://a/b/c/g/"),
            ("/g", "http://a/g"),
            ("//g", "http://g"),
            ("?y", "http://a/b/c/d;p?y"),
            ("g?y", "http://a/b/c/g?y"),
            ("#s", "http://a/b/c/d;p?q#s"),
            ("g;x?y#s", "http://a/b/c/g;x?y#s"),
            ("", "http://a/b/c/d;p?q"),
            (".", "http://a/b/c/"),
            ("..", "http://a/b/"),
            ("../g", "http://a/b/g"),
            ("../../../g", "http://a/g"),
            ("https://other/x", "https://other/x"),
        ] {
            let link = runtime::send(class, "URLWithString:relativeToURL:", &[string::make(reference), base]);
            assert_eq!(string::get(runtime::send(link, "absoluteString", &[])), expected, "{reference:?}");
        }
        let host_only = runtime::send(class, "URLWithString:", &[string::make("https://app.example.com")]);
        let link = runtime::send(runtime::alloc_instance(class), "initWithString:relativeToURL:", &[string::make("/session"), host_only]);
        assert_eq!(string::get(runtime::send(link, "absoluteString", &[])), "https://app.example.com/session");
        let alone = runtime::send(class, "URLWithString:relativeToURL:", &[string::make("http://x/y"), 0]);
        assert_eq!(string::get(runtime::send(alone, "absoluteString", &[])), "http://x/y");
    }
}
