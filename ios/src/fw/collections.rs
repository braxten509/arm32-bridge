//! NSArray and NSDictionary (and their mutable forms), plus NSNumber.
//! Arrays: { isa, *mut Vec<Id> }. Dictionaries: { isa, *mut Vec<(key, value)> }.
//! What goes in is kept (fw/counted.rs `keep`) and never let go.

use crate::fw::geometry::{Point, Rect, Size, Transform, Transform3D};
use crate::fw::object::{class_methods, define_class, methods};
use crate::objc::runtime::{self, lock, Class, Id, Sel};
use crate::symbols::Registry;
use std::sync::OnceLock;

static CLASSES: OnceLock<Classes> = OnceLock::new();

struct Classes {
    array: Class,
    mutable_array: Class,
    dictionary: Class,
    mutable_dictionary: Class,
    number: Class,
}

fn classes() -> &'static Classes {
    CLASSES.get().unwrap()
}

fn items(object: Id) -> &'static mut Vec<Id> {
    unsafe {
        let slot = (object + 8) as *mut *mut Vec<Id>;
        if (*slot).is_null() {
            *slot = Box::into_raw(Box::new(Vec::new()));
        }
        &mut **slot
    }
}

/// Holds on to objects put in a collection (as Apple's retain does).
fn keep_all(list: &[Id]) {
    list.iter().for_each(|&o| crate::fw::counted::keep(o));
}

fn pairs(object: Id) -> &'static mut Vec<(Id, Id)> {
    unsafe {
        let slot = (object + 8) as *mut *mut Vec<(Id, Id)>;
        if (*slot).is_null() {
            *slot = Box::into_raw(Box::new(Vec::new()));
        }
        &mut **slot
    }
}

pub fn array_from(list: &[Id]) -> Id {
    let object = runtime::alloc_instance(classes().array);
    keep_all(list);
    *items(object) = list.to_vec();
    object
}

/// The most entries read from a foreign collection through its primitive
/// methods (see array_items).
const MAX_FOREIGN_ITEMS: usize = 16 * 1024 * 1024;

pub fn array_items(object: Id) -> Vec<Id> {
    if object == 0 {
        return Vec::new();
    }
    if !keeps_our_storage(object, "count", array_count as usize) && answers(object, "objectAtIndex:") {
        let count = runtime::send(object, "count", &[]);
        // A foreign count is the game's word: a wrong one (a non-collection,
        // or a count read before the entries exist) must not loop for ages.
        if count > MAX_FOREIGN_ITEMS {
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| crate::log!("a foreign collection says it has {count} entries; reading none (more than {MAX_FOREIGN_ITEMS})"));
            return Vec::new();
        }
        return (0..count).map(|index| runtime::send(object, "objectAtIndex:", &[index])).collect();
    }
    items(object).clone()
}

/// Whether a collection keeps its entries in our storage: its `primitive`
/// method is our `ours`. A game's class-cluster subclass (its own primitive
/// methods) or a game proxy standing in for a collection doesn't, and is read
/// through its primitive methods, as Foundation reads any collection.
fn keeps_our_storage(object: Id, primitive: &str, ours: usize) -> bool {
    let sel = runtime::sel(primitive);
    runtime::imp_for(object, sel) == Some(ours as runtime::Imp)
}

/// -respondsToSelector:, asked by message (a proxy forwards it).
fn answers(object: Id, selector: &str) -> bool {
    runtime::send(object, "respondsToSelector:", &[runtime::sel(selector) as usize]) & 0xFF != 0
}

/// Makes a mutable array hold exactly `list` (kept, as its other setters do).
pub fn set_array_items(object: Id, list: Vec<Id>) {
    keep_all(&list);
    *items(object) = list;
}

pub fn dictionary_from(list: &[(Id, Id)]) -> Id {
    let object = runtime::alloc_instance(classes().dictionary);
    list.iter().for_each(|&(k, v)| keep_all(&[k, v]));
    *pairs(object) = list.to_vec();
    object
}

pub fn is_array(object: Id) -> bool {
    let rt = lock();
    object != 0 && rt.is_subclass(rt.class_of(object), classes().array)
}

pub fn is_dictionary(object: Id) -> bool {
    let rt = lock();
    object != 0 && rt.is_subclass(rt.class_of(object), classes().dictionary)
}

fn keys_equal(a: Id, b: Id) -> bool {
    a == b || (a != 0 && b != 0 && runtime::send(a, "isEqual:", &[b]) & 0xFF != 0)
}

/// Finds a key without holding on to the entry list while the game's isEqual: runs
/// (that code may change this same dictionary).
fn find_key(object: Id, key: Id) -> Option<Id> {
    let keys: Vec<Id> = pairs(object).iter().map(|(k, _)| *k).collect();
    if let Some(&same) = keys.iter().find(|&&k| k == key) {
        return Some(same);
    }
    keys.into_iter().find(|&k| keys_equal(k, key))
}

pub fn dictionary_get(object: Id, key: Id) -> Id {
    if object == 0 {
        return 0;
    }
    match find_key(object, key) {
        Some(found) => pairs(object).iter().find(|(k, _)| *k == found).map(|(_, v)| *v).unwrap_or(0),
        None => 0,
    }
}

pub fn dictionary_pairs(object: Id) -> Vec<(Id, Id)> {
    if object == 0 {
        return Vec::new();
    }
    if !keeps_our_storage(object, "count", dict_count as usize) && answers(object, "keyEnumerator") {
        let keys = runtime::send(object, "keyEnumerator", &[]);
        return std::iter::from_fn(|| Some(runtime::send(keys, "nextObject", &[])).filter(|&key| key != 0))
            .map(|key| (key, runtime::send(object, "objectForKey:", &[key])))
            .collect();
    }
    pairs(object).clone()
}

// ----- NSArray -----

extern "C" fn array_init(this: Id, _: Sel) -> Id {
    items(this);
    this
}

extern "C" fn array_empty(class: Class, _: Sel) -> Id {
    let object = runtime::alloc_instance(class);
    items(object);
    object
}

extern "C" fn array_with_object(class: Class, _: Sel, item: Id) -> Id {
    let object = runtime::alloc_instance(class);
    crate::fw::counted::keep(item);
    items(object).push(item);
    object
}

extern "C" fn array_with_objects_count(class: Class, _: Sel, list: *const Id, count: usize) -> Id {
    let object = runtime::alloc_instance(class);
    *items(object) = unsafe { std::slice::from_raw_parts(list, count) }.to_vec();
    keep_all(items(object));
    object
}

extern "C" fn array_init_with_objects_count(this: Id, _: Sel, list: *const Id, count: usize) -> Id {
    *items(this) = unsafe { std::slice::from_raw_parts(list, count) }.to_vec();
    keep_all(items(this));
    this
}

extern "C" fn array_with_array(class: Class, _: Sel, other: Id) -> Id {
    let object = runtime::alloc_instance(class);
    *items(object) = array_items(other);
    keep_all(items(object));
    object
}

extern "C" fn array_init_with_array(this: Id, _: Sel, other: Id) -> Id {
    *items(this) = array_items(other);
    keep_all(items(this));
    this
}

crate::libsystem::libc::variadic!(ios_array_with_objects, 3, ios_array_with_objects_impl);
crate::libsystem::libc::variadic!(ios_array_init_with_objects, 3, ios_array_init_with_objects_impl);

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
    keep_all(&list);
    list
}

#[no_mangle]
extern "C" fn ios_array_with_objects_impl(class: Class, _: Sel, first: Id, va: *const u64) -> Id {
    let object = runtime::alloc_instance(class);
    *items(object) = nil_terminated(first, va);
    object
}

#[no_mangle]
extern "C" fn ios_array_init_with_objects_impl(this: Id, _: Sel, first: Id, va: *const u64) -> Id {
    *items(this) = nil_terminated(first, va);
    this
}

extern "C" fn array_count(this: Id, _: Sel) -> usize {
    items(this).len()
}

extern "C" fn array_object_at(this: Id, _: Sel, index: usize) -> Id {
    match items(this).get(index) {
        Some(&item) => item,
        None => {
            crate::log!("STOP: list index {index} is past the end ({} items)", items(this).len());
            crate::report::finish(1);
        }
    }
}

extern "C" fn array_first(this: Id, _: Sel) -> Id {
    items(this).first().copied().unwrap_or(0)
}

extern "C" fn array_last(this: Id, _: Sel) -> Id {
    items(this).last().copied().unwrap_or(0)
}

extern "C" fn array_contains(this: Id, _: Sel, item: Id) -> bool {
    items(this).iter().any(|&i| keys_equal(i, item))
}

extern "C" fn array_index_of(this: Id, _: Sel, item: Id) -> usize {
    items(this).iter().position(|&i| keys_equal(i, item)).unwrap_or(isize::MAX as usize)
}

extern "C" fn array_copy(this: Id, _: Sel, _zone: usize) -> Id {
    array_from(&array_items(this))
}

extern "C" fn array_mutable_copy(this: Id, _: Sel, _zone: usize) -> Id {
    let object = runtime::alloc_instance(classes().mutable_array);
    let list = array_items(this);
    keep_all(&list);
    *items(object) = list;
    object
}

/// Fast enumeration: { state, itemsPtr, mutationsPtr, extra[5] }.
extern "C" fn array_enumerate(this: Id, _: Sel, state: *mut [usize; 8], buffer: *mut Id, length: usize) -> usize {
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

extern "C" fn array_add(this: Id, _: Sel, item: Id) {
    crate::fw::counted::keep(item);
    items(this).push(item);
}

extern "C" fn array_insert(this: Id, _: Sel, item: Id, index: usize) {
    crate::fw::counted::keep(item);
    let list = items(this);
    list.insert(index.min(list.len()), item);
}

extern "C" fn array_remove_at(this: Id, _: Sel, index: usize) {
    let list = items(this);
    if index < list.len() {
        list.remove(index);
    }
}

extern "C" fn array_remove_object(this: Id, _: Sel, item: Id) {
    items(this).retain(|&i| !keys_equal(i, item));
}

extern "C" fn array_remove_all(this: Id, _: Sel) {
    items(this).clear();
}

extern "C" fn array_remove_last(this: Id, _: Sel) {
    items(this).pop();
}

extern "C" fn array_replace(this: Id, _: Sel, index: usize, item: Id) {
    if let Some(slot) = items(this).get_mut(index) {
        crate::fw::counted::keep(item);
        *slot = item;
    }
}

extern "C" fn array_add_all(this: Id, _: Sel, other: Id) {
    let extra = array_items(other);
    items(this).extend(extra);
}

extern "C" fn array_init_capacity(this: Id, _: Sel, _capacity: usize) -> Id {
    items(this);
    this
}

extern "C" fn array_with_capacity(class: Class, _: Sel, _capacity: usize) -> Id {
    let object = runtime::alloc_instance(class);
    items(object);
    object
}

extern "C" fn array_description(this: Id, _: Sel) -> Id {
    let parts: Vec<String> = array_items(this).into_iter().map(crate::fw::string::describe).collect();
    crate::fw::string::make(&format!("({})", parts.join(", ")))
}

fn compare_with_selector(a: Id, b: Id, selector: Sel) -> std::cmp::Ordering {
    let name = lock().sel_name(selector);
    let result = runtime::send(a, &name, &[b]) as isize;
    result.cmp(&0)
}

extern "C" fn array_sorted_selector(this: Id, _: Sel, selector: Sel) -> Id {
    let mut list = array_items(this);
    list.sort_by(|&a, &b| compare_with_selector(a, b, selector));
    array_from(&list)
}

extern "C" fn array_sort_selector(this: Id, _: Sel, selector: Sel) {
    items(this).sort_by(|&a, &b| compare_with_selector(a, b, selector));
}

fn compare_with_block(a: Id, b: Id, block: usize) -> std::cmp::Ordering {
    let header = block as *const crate::libsystem::mach::BlockHeader;
    let invoke: extern "C" fn(usize, Id, Id) -> isize = unsafe { std::mem::transmute((*header).invoke) };
    invoke(block, a, b).cmp(&0)
}

extern "C" fn array_sorted_comparator(this: Id, _: Sel, block: usize) -> Id {
    let mut list = array_items(this);
    list.sort_by(|&a, &b| compare_with_block(a, b, block));
    array_from(&list)
}

extern "C" fn array_sort_comparator(this: Id, _: Sel, block: usize) {
    items(this).sort_by(|&a, &b| compare_with_block(a, b, block));
}

extern "C" fn array_sorted_function(this: Id, _: Sel, function: usize, context: usize) -> Id {
    let f: extern "C" fn(Id, Id, usize) -> isize = unsafe { std::mem::transmute(function) };
    let mut list = array_items(this);
    list.sort_by(|&a, &b| f(a, b, context).cmp(&0));
    array_from(&list)
}

extern "C" fn array_sort_function(this: Id, _: Sel, function: usize, context: usize) {
    let f: extern "C" fn(Id, Id, usize) -> isize = unsafe { std::mem::transmute(function) };
    items(this).sort_by(|&a, &b| f(a, b, context).cmp(&0));
}

extern "C" fn array_joined(this: Id, _: Sel, separator: Id) -> Id {
    let parts: Vec<String> = array_items(this).into_iter().map(crate::fw::string::describe).collect();
    crate::fw::string::make(&parts.join(&crate::fw::string::get(separator)))
}

extern "C" fn array_adding(this: Id, _: Sel, item: Id) -> Id {
    let mut list = array_items(this);
    list.push(item);
    array_from(&list)
}

extern "C" fn array_adding_array(this: Id, _: Sel, other: Id) -> Id {
    let mut list = array_items(this);
    list.extend(array_items(other));
    array_from(&list)
}

extern "C" fn array_subarray(this: Id, _: Sel, range: crate::fw::string::Range) -> Id {
    let list = array_items(this);
    let start = range.location.min(list.len());
    let end = (range.location + range.length).min(list.len());
    array_from(&list[start..end])
}

extern "C" fn array_index_identical(this: Id, _: Sel, item: Id) -> usize {
    items(this).iter().position(|&i| i == item).unwrap_or(isize::MAX as usize)
}

extern "C" fn array_is_equal(this: Id, _: Sel, other: Id) -> bool {
    if !is_array(other) {
        return false;
    }
    let (a, b) = (array_items(this), array_items(other));
    a.len() == b.len() && a.iter().zip(b.iter()).all(|(&x, &y)| keys_equal(x, y))
}

extern "C" fn array_make_perform(this: Id, _: Sel, selector: Sel) {
    let name = lock().sel_name(selector);
    for item in array_items(this) {
        runtime::send(item, &name, &[]);
    }
}

extern "C" fn array_make_perform_with(this: Id, _: Sel, selector: Sel, argument: Id) {
    let name = lock().sel_name(selector);
    for item in array_items(this) {
        runtime::send(item, &name, &[argument]);
    }
}

extern "C" fn array_enumerate_block(this: Id, _: Sel, block: usize) {
    let header = block as *const crate::libsystem::mach::BlockHeader;
    let invoke: extern "C" fn(usize, Id, usize, *mut u8) = unsafe { std::mem::transmute((*header).invoke) };
    let mut stop = 0u8;
    for (index, item) in array_items(this).into_iter().enumerate() {
        invoke(block, item, index, &mut stop);
        if stop != 0 {
            break;
        }
    }
}

extern "C" fn array_remove_in(this: Id, _: Sel, other: Id) {
    let remove = array_items(other);
    items(this).retain(|&i| !remove.iter().any(|&r| keys_equal(i, r)));
}

extern "C" fn array_exchange(this: Id, _: Sel, a: usize, b: usize) {
    let list = items(this);
    if a < list.len() && b < list.len() {
        list.swap(a, b);
    }
}

extern "C" fn array_with_contents_of_file(_: Class, _: Sel, path: Id) -> Id {
    match std::fs::read(crate::fw::string::get(path)).ok().and_then(|b| crate::fw::plist::parse(&b)) {
        Some(value @ crate::fw::plist::Value::Array(_)) => crate::fw::plist::to_object(&value),
        _ => 0,
    }
}

extern "C" fn dict_with_contents_of_file(_: Class, _: Sel, path: Id) -> Id {
    match std::fs::read(crate::fw::string::get(path)).ok().and_then(|b| crate::fw::plist::parse(&b)) {
        Some(value @ crate::fw::plist::Value::Dict(_)) => crate::fw::plist::to_object(&value),
        _ => 0,
    }
}

extern "C" fn init_with_contents_of_file(this: Id, _: Sel, path: Id) -> Id {
    let is_dictionary = is_dictionary(this);
    let loaded = if is_dictionary { dict_with_contents_of_file(0, 0, path) } else { array_with_contents_of_file(0, 0, path) };
    if loaded == 0 {
        return 0;
    }
    if is_dictionary {
        *pairs(this) = dictionary_pairs(loaded);
    } else {
        *items(this) = array_items(loaded);
    }
    this
}

extern "C" fn write_to_file(this: Id, _: Sel, path: Id, _atomically: u8) -> bool {
    match crate::fw::plist::from_object(this) {
        Some(value) => {
            let path = crate::fw::string::get(path);
            let temporary = format!("{path}.part");
            std::fs::write(&temporary, crate::fw::plist::to_xml(&value)).and_then(|_| std::fs::rename(&temporary, &path)).is_ok()
        }
        None => false,
    }
}

// ----- NSDictionary -----

extern "C" fn dict_init(this: Id, _: Sel) -> Id {
    pairs(this);
    this
}

extern "C" fn dict_empty(class: Class, _: Sel) -> Id {
    let object = runtime::alloc_instance(class);
    pairs(object);
    object
}

extern "C" fn dict_with_capacity(class: Class, _: Sel, _capacity: usize) -> Id {
    dict_empty(class, 0)
}

extern "C" fn dict_init_capacity(this: Id, _: Sel, _capacity: usize) -> Id {
    pairs(this);
    this
}

extern "C" fn dict_with_object_for_key(class: Class, _: Sel, value: Id, key: Id) -> Id {
    let object = runtime::alloc_instance(class);
    keep_all(&[key, value]);
    pairs(object).push((key, value));
    object
}

extern "C" fn dict_with_objects_keys_count(class: Class, _: Sel, values: *const Id, keys: *const Id, count: usize) -> Id {
    let object = runtime::alloc_instance(class);
    dict_init_objects_keys_count(object, 0, values, keys, count)
}

extern "C" fn dict_init_objects_keys_count(this: Id, _: Sel, values: *const Id, keys: *const Id, count: usize) -> Id {
    let list = pairs(this);
    list.clear();
    for index in 0..count {
        let (key, value) = unsafe { (*keys.add(index), *values.add(index)) };
        list.retain(|(k, _)| !keys_equal(*k, key));
        keep_all(&[key, value]);
        list.push((key, value));
    }
    this
}

extern "C" fn dict_with_dictionary(class: Class, _: Sel, other: Id) -> Id {
    let object = runtime::alloc_instance(class);
    *pairs(object) = dictionary_pairs(other);
    pairs(object).iter().for_each(|&(k, v)| keep_all(&[k, v]));
    object
}

extern "C" fn dict_init_with_dictionary(this: Id, _: Sel, other: Id) -> Id {
    *pairs(this) = dictionary_pairs(other);
    pairs(this).iter().for_each(|&(k, v)| keep_all(&[k, v]));
    this
}

extern "C" fn dict_init_with_dictionary_copy(this: Id, _: Sel, other: Id, copy_items: u8) -> Id {
    let mut list = dictionary_pairs(other);
    for (key, value) in list.iter_mut() {
        // A copied value is already the dictionary's own.
        if copy_items != 0 {
            *value = runtime::send(*value, "copy", &[]);
            keep_all(&[*key]);
        } else {
            keep_all(&[*key, *value]);
        }
    }
    *pairs(this) = list;
    this
}

extern "C" fn dict_with_contents_of_url(class: Class, s: Sel, url: Id) -> Id {
    if url == 0 {
        return 0;
    }
    let path = runtime::send(url, "path", &[]);
    if path == 0 {
        return 0;
    }
    dict_with_contents_of_file(class, s, path)
}

// Apple arm64 passes every variadic argument on the stack, one 8-byte slot
// each: the trampolines hand the implementations the stack pointer.
crate::libsystem::libc::variadic!(ios_dict_with_objects_and_keys, 3, ios_dict_with_objects_and_keys_impl);
crate::libsystem::libc::variadic!(ios_dict_init_with_objects_and_keys, 3, ios_dict_init_with_objects_and_keys_impl);

/// Fills a dictionary from a nil-terminated value, key, value, key... list.
fn fill_objects_and_keys(object: Id, first: Id, va: *const u64) -> Id {
    let list = nil_terminated(first, va);
    let store = pairs(object);
    store.clear();
    for pair in list.chunks(2) {
        if let [value, key] = pair {
            store.retain(|(k, _)| !keys_equal(*k, *key));
            store.push((*key, *value));
        }
    }
    object
}

#[no_mangle]
extern "C" fn ios_dict_with_objects_and_keys_impl(class: Class, _: Sel, first: Id, va: *const u64) -> Id {
    fill_objects_and_keys(runtime::alloc_instance(class), first, va)
}

#[no_mangle]
extern "C" fn ios_dict_init_with_objects_and_keys_impl(this: Id, _: Sel, first: Id, va: *const u64) -> Id {
    fill_objects_and_keys(this, first, va)
}

extern "C" fn dict_object_for_key(this: Id, _: Sel, key: Id) -> Id {
    dictionary_get(this, key)
}

extern "C" fn dict_count(this: Id, _: Sel) -> usize {
    pairs(this).len()
}

extern "C" fn dict_all_keys(this: Id, _: Sel) -> Id {
    let keys: Vec<Id> = pairs(this).iter().map(|(k, _)| *k).collect();
    array_from(&keys)
}

extern "C" fn dict_all_values(this: Id, _: Sel) -> Id {
    let values: Vec<Id> = pairs(this).iter().map(|(_, v)| *v).collect();
    array_from(&values)
}

extern "C" fn dict_set(this: Id, _: Sel, value: Id, key: Id) {
    let key = if crate::fw::string::is_string(key) { runtime::send(key, "copy", &[]) } else { key };
    let found = find_key(this, key);
    keep_all(&[key, value]);
    let list = pairs(this);
    match found.and_then(|f| list.iter_mut().find(|(k, _)| *k == f)) {
        Some(entry) => entry.1 = value,
        None => list.push((key, value)),
    }
}

extern "C" fn dict_remove(this: Id, _: Sel, key: Id) {
    if let Some(found) = find_key(this, key) {
        pairs(this).retain(|(k, _)| *k != found);
    }
}

extern "C" fn dict_remove_keys(this: Id, s: Sel, keys: Id) {
    for key in array_items(keys) {
        dict_remove(this, s, key);
    }
}

extern "C" fn dict_remove_all(this: Id, _: Sel) {
    pairs(this).clear();
}

/// setValue:forKey: on a dictionary: nil removes the key.
extern "C" fn dict_set_value(this: Id, s: Sel, value: Id, key: Id) {
    if value == 0 {
        dict_remove(this, s, key);
    } else {
        dict_set(this, s, value, key);
    }
}

extern "C" fn dict_add_entries(this: Id, s: Sel, other: Id) {
    for (key, value) in dictionary_pairs(other) {
        dict_set(this, s, value, key);
    }
}

extern "C" fn dict_set_dictionary(this: Id, s: Sel, other: Id) {
    let entries = if other == 0 { Vec::new() } else { dictionary_pairs(other) };
    dict_remove_all(this, s);
    for (key, value) in entries {
        dict_set(this, s, value, key);
    }
}

extern "C" fn dict_copy(this: Id, _: Sel, _zone: usize) -> Id {
    dictionary_from(&dictionary_pairs(this))
}

extern "C" fn dict_mutable_copy(this: Id, _: Sel, _zone: usize) -> Id {
    let object = runtime::alloc_instance(classes().mutable_dictionary);
    let entries = dictionary_pairs(this);
    entries.iter().for_each(|&(k, v)| keep_all(&[k, v]));
    *pairs(object) = entries;
    object
}

extern "C" fn dict_enumerate(this: Id, _: Sel, state: *mut [usize; 8], buffer: *mut Id, length: usize) -> usize {
    unsafe {
        let list = pairs(this);
        let start = (*state)[0];
        if start >= list.len() {
            return 0;
        }
        let count = (list.len() - start).min(length.max(1));
        for index in 0..count {
            *buffer.add(index) = list[start + index].0;
        }
        (*state)[0] = start + count;
        (*state)[1] = buffer as usize;
        (*state)[2] = (&(*state)[3]) as *const usize as usize;
        count
    }
}

extern "C" fn dict_enumerate_block(this: Id, _: Sel, block: usize) {
    let header = block as *const crate::libsystem::mach::BlockHeader;
    let invoke: extern "C" fn(usize, Id, Id, *mut u8) = unsafe { std::mem::transmute((*header).invoke) };
    let mut stop = 0u8;
    for (key, value) in dictionary_pairs(this) {
        invoke(block, key, value, &mut stop);
        if stop != 0 {
            break;
        }
    }
}

extern "C" fn dict_objects_for_keys(this: Id, _: Sel, keys: Id, marker: Id) -> Id {
    let values: Vec<Id> = array_items(keys)
        .into_iter()
        .map(|k| {
            let v = dictionary_get(this, k);
            if v == 0 { marker } else { v }
        })
        .collect();
    array_from(&values)
}

extern "C" fn dict_is_equal(this: Id, _: Sel, other: Id) -> bool {
    if !is_dictionary(other) {
        return false;
    }
    let (a, b) = (dictionary_pairs(this), dictionary_pairs(other));
    a.len() == b.len() && a.iter().all(|&(k, v)| keys_equal(dictionary_get(other, k), v))
}

extern "C" fn dict_keys_for_object(this: Id, _: Sel, value: Id) -> Id {
    let keys: Vec<Id> = dictionary_pairs(this).into_iter().filter(|&(_, v)| keys_equal(v, value)).map(|(k, _)| k).collect();
    array_from(&keys)
}

extern "C" fn dict_description(this: Id, _: Sel) -> Id {
    let parts: Vec<String> = dictionary_pairs(this)
        .into_iter()
        .map(|(k, v)| format!("{} = {}", crate::fw::string::describe(k), crate::fw::string::describe(v)))
        .collect();
    crate::fw::string::make(&format!("{{{}}}", parts.join("; ")))
}

// NSCache may be shared between threads; one lock covers every cache.
// The same thread may take it again (a key's isEqual: can use a cache).
static CACHE_LOCK: crate::libsystem::libc::parking::Reentrant = crate::libsystem::libc::parking::Reentrant::new();

fn with_cache<T>(work: impl FnOnce() -> T) -> T {
    CACHE_LOCK.enter();
    let result = work();
    CACHE_LOCK.exit();
    result
}

extern "C" fn cache_object_for_key(this: Id, s: Sel, key: Id) -> Id {
    with_cache(|| dict_object_for_key(this, s, key))
}

extern "C" fn cache_set(this: Id, s: Sel, value: Id, key: Id) {
    with_cache(|| dict_set(this, s, value, key))
}

extern "C" fn cache_set_with_cost(this: Id, s: Sel, value: Id, key: Id, _cost: usize) {
    cache_set(this, s, value, key)
}

extern "C" fn cache_remove(this: Id, s: Sel, key: Id) {
    with_cache(|| dict_remove(this, s, key))
}

extern "C" fn cache_remove_all(this: Id, s: Sel) {
    with_cache(|| dict_remove_all(this, s))
}

extern "C" fn ignore_value(_this: Id, _: Sel, _value: usize) {}

// ----- NSEnumerator: { isa, *mut (items, next index) } -----

static ENUMERATOR_CLASS: OnceLock<Class> = OnceLock::new();

pub fn enumerator(list: Vec<Id>) -> Id {
    let object = runtime::alloc_instance(*ENUMERATOR_CLASS.get().unwrap());
    unsafe { *((object + 8) as *mut *mut (Vec<Id>, usize)) = Box::into_raw(Box::new((list, 0))) };
    object
}

fn enumerator_state(object: Id) -> &'static mut (Vec<Id>, usize) {
    unsafe { &mut **((object + 8) as *mut *mut (Vec<Id>, usize)) }
}

extern "C" fn enumerator_next(this: Id, _: Sel) -> Id {
    let state = enumerator_state(this);
    let item = state.0.get(state.1).copied().unwrap_or(0);
    state.1 += 1;
    item
}

extern "C" fn enumerator_all(this: Id, _: Sel) -> Id {
    let state = enumerator_state(this);
    let rest = state.0.get(state.1..).unwrap_or(&[]).to_vec();
    state.1 = state.0.len();
    array_from(&rest)
}

extern "C" fn enumerator_enumerate(this: Id, _: Sel, state: *mut [usize; 8], buffer: *mut Id, length: usize) -> usize {
    let items = enumerator_state(this);
    let mut count = 0;
    while count < length.max(1) && items.1 < items.0.len() {
        unsafe { *buffer.add(count) = items.0[items.1] };
        items.1 += 1;
        count += 1;
    }
    unsafe {
        (*state)[1] = buffer as usize;
        (*state)[2] = (&(*state)[3]) as *const usize as usize;
    }
    count
}

extern "C" fn array_object_enumerator(this: Id, _: Sel) -> Id {
    enumerator(array_items(this))
}

extern "C" fn array_reverse_enumerator(this: Id, _: Sel) -> Id {
    let mut list = array_items(this);
    list.reverse();
    enumerator(list)
}

extern "C" fn dict_key_enumerator(this: Id, _: Sel) -> Id {
    enumerator(dictionary_pairs(this).into_iter().map(|(k, _)| k).collect())
}

extern "C" fn dict_object_enumerator(this: Id, _: Sel) -> Id {
    enumerator(dictionary_pairs(this).into_iter().map(|(_, v)| v).collect())
}

// ----- NSValue: { isa, kind, pointer } or { isa, KIND_STRUCT, *mut Boxed } -----

const KIND_POINTER: usize = 3;
const KIND_STRUCT: usize = 4;

/// A struct held by value: its type encoding (as the host lays it out) and bytes.
struct Boxed {
    kind: std::ffi::CString,
    bytes: Vec<u8>,
}

fn value_kind(object: Id) -> usize {
    unsafe { *((object + 8) as *const usize) }
}

fn boxed(object: Id) -> Option<&'static Boxed> {
    if object == 0 || value_kind(object) != KIND_STRUCT {
        return None;
    }
    unsafe { (*((object + 16) as *const *const Boxed)).as_ref() }
}

/// A new NSValue (of `class`) holding a copy of `bytes`, of type `kind`.
pub fn value_with_bytes(class: Class, kind: &str, bytes: &[u8]) -> Id {
    let object = runtime::alloc_instance(class);
    hold_struct(object, kind, bytes);
    object
}

fn hold_struct(object: Id, kind: &str, bytes: &[u8]) {
    let held = Boxed { kind: std::ffi::CString::new(kind).unwrap_or_default(), bytes: bytes.to_vec() };
    unsafe {
        *((object + 8) as *mut usize) = KIND_STRUCT;
        *((object + 16) as *mut *mut Boxed) = Box::into_raw(Box::new(held));
    }
}

/// The struct an NSValue holds, if it holds one of `kind`'s size.
pub fn value_struct<T: Copy + Default>(object: Id) -> T {
    match boxed(object) {
        Some(held) if held.bytes.len() == std::mem::size_of::<T>() => unsafe { (held.bytes.as_ptr() as *const T).read_unaligned() },
        _ => T::default(),
    }
}

fn bytes_of<T>(value: &T) -> &[u8] {
    unsafe { std::slice::from_raw_parts(value as *const T as *const u8, std::mem::size_of::<T>()) }
}

pub const POINT_TYPE: &str = "{CGPoint=dd}";
pub const SIZE_TYPE: &str = "{CGSize=dd}";
pub const RECT_TYPE: &str = "{CGRect={CGPoint=dd}{CGSize=dd}}";
pub const TRANSFORM_TYPE: &str = "{CGAffineTransform=dddddd}";
pub const TRANSFORM_3D_TYPE: &str = "{CATransform3D=dddddddddddddddd}";
pub const RANGE_TYPE: &str = "{_NSRange=QQ}";

pub fn value_with<T>(kind: &str, value: &T) -> Id {
    value_with_bytes(runtime::class_named("NSValue"), kind, bytes_of(value))
}

extern "C" fn value_with_point(class: Class, _: Sel, value: Point) -> Id {
    value_with_bytes(class, POINT_TYPE, bytes_of(&value))
}
extern "C" fn value_with_size(class: Class, _: Sel, value: Size) -> Id {
    value_with_bytes(class, SIZE_TYPE, bytes_of(&value))
}
extern "C" fn value_with_rect(class: Class, _: Sel, value: Rect) -> Id {
    value_with_bytes(class, RECT_TYPE, bytes_of(&value))
}
extern "C" fn value_with_transform(class: Class, _: Sel, value: Transform) -> Id {
    value_with_bytes(class, TRANSFORM_TYPE, bytes_of(&value))
}
extern "C" fn value_with_transform_3d(class: Class, _: Sel, value: Transform3D) -> Id {
    value_with_bytes(class, TRANSFORM_3D_TYPE, bytes_of(&value))
}
extern "C" fn value_with_range(class: Class, _: Sel, value: crate::fw::string::Range) -> Id {
    value_with_bytes(class, RANGE_TYPE, bytes_of(&value))
}
extern "C" fn value_point(this: Id, _: Sel) -> Point {
    value_struct(this)
}
extern "C" fn value_size(this: Id, _: Sel) -> Size {
    value_struct(this)
}
extern "C" fn value_rect(this: Id, _: Sel) -> Rect {
    value_struct(this)
}
extern "C" fn value_transform(this: Id, _: Sel) -> Transform {
    match boxed(this) {
        Some(held) if held.bytes.len() == std::mem::size_of::<Transform>() => unsafe { (held.bytes.as_ptr() as *const Transform).read_unaligned() },
        _ => crate::fw::geometry::IDENTITY,
    }
}
extern "C" fn value_transform_3d(this: Id, _: Sel) -> Transform3D {
    match boxed(this) {
        Some(held) if held.bytes.len() == std::mem::size_of::<Transform3D>() => unsafe { (held.bytes.as_ptr() as *const Transform3D).read_unaligned() },
        _ => crate::fw::geometry::IDENTITY_3D,
    }
}
extern "C" fn value_range(this: Id, _: Sel) -> crate::fw::string::Range {
    match boxed(this) {
        Some(held) if held.bytes.len() == 16 => unsafe { (held.bytes.as_ptr() as *const crate::fw::string::Range).read_unaligned() },
        _ => crate::fw::string::Range { location: 0, length: 0 },
    }
}

extern "C" fn value_with_pointer(class: Class, _: Sel, pointer: usize) -> Id {
    let object = runtime::alloc_instance(class);
    unsafe {
        *((object + 8) as *mut usize) = KIND_POINTER;
        *((object + 16) as *mut usize) = pointer;
    }
    object
}

extern "C" fn value_pointer(this: Id, _: Sel) -> usize {
    if boxed(this).is_some() {
        return 0;
    }
    unsafe { *((this + 16) as *const usize) }
}

/// Copies the value out: a struct's bytes, else the pointer.
extern "C" fn value_get(this: Id, s: Sel, out: *mut usize) {
    if out.is_null() {
        return;
    }
    match boxed(this) {
        Some(held) => unsafe { std::ptr::copy_nonoverlapping(held.bytes.as_ptr(), out as *mut u8, held.bytes.len()) },
        None => unsafe { out.write_unaligned(value_pointer(this, s)) },
    }
}

extern "C" fn value_objc_type(this: Id, _: Sel) -> *const std::ffi::c_char {
    match boxed(this) {
        Some(held) => held.kind.as_ptr(),
        None => c"^v".as_ptr(),
    }
}

extern "C" fn value_is_equal(this: Id, s: Sel, other: Id) -> bool {
    let same_class = {
        let rt = lock();
        other != 0 && rt.class_of(other) == rt.class_of(this)
    };
    if !same_class {
        return false;
    }
    match (boxed(this), boxed(other)) {
        (Some(a), Some(b)) => a.kind == b.kind && a.bytes == b.bytes,
        (None, None) => value_pointer(this, s) == value_pointer(other, s),
        _ => false,
    }
}

extern "C" fn value_hash(this: Id, s: Sel) -> usize {
    match boxed(this) {
        Some(held) => held.bytes.iter().fold(0xcbf29ce484222325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3)) as usize,
        None => value_pointer(this, s),
    }
}

extern "C" fn value_description(this: Id, _: Sel) -> Id {
    let text = match boxed(this) {
        Some(held) => match held.kind.to_str().unwrap_or("") {
            POINT_TYPE => {
                let p: Point = value_struct(this);
                format!("NSPoint: {{{}, {}}}", p.x, p.y)
            }
            SIZE_TYPE => {
                let s: Size = value_struct(this);
                format!("NSSize: {{{}, {}}}", s.width, s.height)
            }
            RECT_TYPE => {
                let r: Rect = value_struct(this);
                format!("NSRect: {{{{{}, {}}}, {{{}, {}}}}}", r.origin.x, r.origin.y, r.size.width, r.size.height)
            }
            _ => format!("<{}>", held.bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()),
        },
        None => format!("<{:x}>", value_pointer(this, 0)),
    };
    crate::fw::string::make(&text)
}

/// The keyed archive form Foundation writes: "NS.special" 1-4 with the
/// text form of a point, size or rect ("{1.5, -2}", full precision) or a
/// range's two numbers; other structs by their type encoding. A pointer
/// means nothing in another run, so nothing is written for it (Apple
/// raises) and it reads back as NULL.
extern "C" fn value_encode_with_coder(this: Id, _: Sel, coder: Id) {
    let Some(held) = boxed(this) else { return };
    let key = crate::fw::string::make;
    let put_text = |name: &str, text: String| {
        runtime::send(coder, "encodeObject:forKey:", &[crate::fw::string::make(&text), key(name)]);
    };
    let put_number = |name: &str, n: i64| {
        runtime::send(coder, "encodeInt64:forKey:", &[n as usize, key(name)]);
    };
    match held.kind.to_str().unwrap_or("") {
        POINT_TYPE => {
            let p: Point = value_struct(this);
            put_number("NS.special", 1);
            put_text("NS.pointval", format!("{{{}, {}}}", p.x, p.y));
        }
        SIZE_TYPE => {
            let s: Size = value_struct(this);
            put_number("NS.special", 2);
            put_text("NS.sizeval", format!("{{{}, {}}}", s.width, s.height));
        }
        RECT_TYPE => {
            let r: Rect = value_struct(this);
            put_number("NS.special", 3);
            put_text("NS.rectval", format!("{{{{{}, {}}}, {{{}, {}}}}}", r.origin.x, r.origin.y, r.size.width, r.size.height));
        }
        RANGE_TYPE => {
            let r = value_range(this, 0);
            put_number("NS.special", 4);
            put_number("NS.rangeval.location", r.location as i64);
            put_number("NS.rangeval.length", r.length as i64);
        }
        kind => {
            put_text("NS.objctype", kind.to_string());
            runtime::send(coder, "encodeValueOfObjCType:at:", &[held.kind.as_ptr() as usize, held.bytes.as_ptr() as usize]);
        }
    }
}

extern "C" fn value_init_with_coder(this: Id, _: Sel, coder: Id) -> Id {
    let key = crate::fw::string::make;
    let text = |name: &str| {
        let object = runtime::send(coder, "decodeObjectForKey:", &[key(name)]);
        if crate::fw::string::is_string(object) { crate::fw::string::get(object) } else { String::new() }
    };
    let number = |name: &str| runtime::send(coder, "decodeInt64ForKey:", &[key(name)]) as i64;
    match number("NS.special") {
        1 => {
            let [x, y] = crate::fw::geometry::numbers(&text("NS.pointval"));
            hold_struct(this, POINT_TYPE, bytes_of(&Point { x, y }));
        }
        2 => {
            let [width, height] = crate::fw::geometry::numbers(&text("NS.sizeval"));
            hold_struct(this, SIZE_TYPE, bytes_of(&Size { width, height }));
        }
        3 => {
            let [x, y, width, height] = crate::fw::geometry::numbers(&text("NS.rectval"));
            hold_struct(this, RECT_TYPE, bytes_of(&Rect::new(x, y, width, height)));
        }
        4 => {
            let range = crate::fw::string::Range { location: number("NS.rangeval.location") as usize, length: number("NS.rangeval.length") as usize };
            hold_struct(this, RANGE_TYPE, bytes_of(&range));
        }
        _ => match text("NS.objctype") {
            kind if !kind.is_empty() => {
                let mut bytes = vec![0u8; crate::fw::system::type_size(&kind, false).0];
                let code = std::ffi::CString::new(kind.as_str()).unwrap_or_default();
                runtime::send(coder, "decodeValueOfObjCType:at:", &[code.as_ptr() as usize, bytes.as_mut_ptr() as usize]);
                hold_struct(this, &kind, &bytes);
            }
            _ => unsafe {
                *((this + 8) as *mut usize) = KIND_POINTER;
                *((this + 16) as *mut usize) = 0;
            },
        },
    }
    this
}

// ----- NSNumber: { isa, kind, value bits } -----

const KIND_INT: usize = 0;
const KIND_FLOAT: usize = 1;
const KIND_BOOL: usize = 2;

pub fn number(kind: usize, bits: u64) -> Id {
    let object = runtime::alloc_instance(classes().number);
    unsafe {
        *((object + 8) as *mut usize) = kind;
        *((object + 16) as *mut u64) = bits;
    }
    object
}

pub fn number_int(value: i64) -> Id {
    number(KIND_INT, value as u64)
}

pub fn number_bool(value: bool) -> Id {
    number(KIND_BOOL, value as u64)
}

pub fn number_float(value: f64) -> Id {
    number(KIND_FLOAT, value.to_bits())
}

/// An NSDecimalNumber (store prices), kept like an NSNumber.
pub fn decimal_number(value: i64) -> Id {
    let object = runtime::alloc_instance(runtime::class_named("NSDecimalNumber"));
    unsafe {
        *((object + 8) as *mut usize) = KIND_INT;
        *((object + 16) as *mut i64) = value;
    }
    object
}

extern "C" fn decimal_zero(_: Class, _: Sel) -> Id {
    decimal_number(0)
}

extern "C" fn decimal_one(_: Class, _: Sel) -> Id {
    decimal_number(1)
}

pub fn is_number(object: Id) -> bool {
    let rt = lock();
    object != 0 && rt.is_subclass(rt.class_of(object), classes().number)
}

fn number_parts(object: Id) -> (usize, u64) {
    unsafe { (*((object + 8) as *const usize), *((object + 16) as *const u64)) }
}

/// Booleans (numberWithBool:, kCFBooleanTrue) are CFBoolean on Apple's side.
pub fn number_is_bool(object: Id) -> bool {
    number_parts(object).0 == KIND_BOOL
}

/// Made from a float or double (not an integer or a BOOL).
pub fn number_is_float(object: Id) -> bool {
    number_parts(object).0 == KIND_FLOAT
}

pub fn number_as_f64(object: Id) -> f64 {
    let (kind, bits) = number_parts(object);
    if kind == KIND_FLOAT { f64::from_bits(bits) } else { bits as i64 as f64 }
}

pub fn number_as_i64(object: Id) -> i64 {
    let (kind, bits) = number_parts(object);
    if kind == KIND_FLOAT { f64::from_bits(bits) as i64 } else { bits as i64 }
}

extern "C" fn number_with_int(_: Class, _: Sel, value: i32) -> Id {
    number_int(value as i64)
}
extern "C" fn number_with_unsigned_int(_: Class, _: Sel, value: u32) -> Id {
    number_int(value as i64)
}
extern "C" fn number_with_long(_: Class, _: Sel, value: i64) -> Id {
    number_int(value)
}
extern "C" fn number_with_bool(_: Class, _: Sel, value: u8) -> Id {
    number_bool(value != 0)
}
extern "C" fn number_with_float(_: Class, _: Sel, value: f32) -> Id {
    number_float(value as f64)
}
extern "C" fn number_with_double(_: Class, _: Sel, value: f64) -> Id {
    number_float(value)
}
extern "C" fn number_init_int(this: Id, _: Sel, value: i32) -> Id {
    unsafe { *((this + 16) as *mut i64) = value as i64 };
    this
}
extern "C" fn number_init_bool(this: Id, _: Sel, value: u8) -> Id {
    unsafe {
        *((this + 8) as *mut usize) = KIND_BOOL;
        *((this + 16) as *mut u64) = (value != 0) as u64;
    }
    this
}
extern "C" fn number_init_float(this: Id, _: Sel, value: f32) -> Id {
    unsafe {
        *((this + 8) as *mut usize) = KIND_FLOAT;
        *((this + 16) as *mut u64) = (value as f64).to_bits();
    }
    this
}
extern "C" fn number_int_value(this: Id, _: Sel) -> i64 {
    number_as_i64(this)
}
extern "C" fn number_bool_value(this: Id, _: Sel) -> bool {
    number_as_f64(this) != 0.0
}
extern "C" fn number_float_value(this: Id, _: Sel) -> f32 {
    number_as_f64(this) as f32
}
extern "C" fn number_double_value(this: Id, _: Sel) -> f64 {
    number_as_f64(this)
}
extern "C" fn number_is_equal(this: Id, _: Sel, other: Id) -> bool {
    is_number(other) && number_as_f64(this) == number_as_f64(other)
}
extern "C" fn number_hash(this: Id, _: Sel) -> usize {
    number_as_i64(this) as usize
}
extern "C" fn number_compare(this: Id, _: Sel, other: Id) -> isize {
    let (a, b) = (number_as_f64(this), number_as_f64(other));
    if a < b { -1 } else if a > b { 1 } else { 0 }
}
extern "C" fn number_string_value(this: Id, _: Sel) -> Id {
    let (kind, bits) = number_parts(this);
    let text = match kind {
        KIND_FLOAT => format!("{}", f64::from_bits(bits)),
        _ => format!("{}", bits as i64),
    };
    crate::fw::string::make(&text)
}
extern "C" fn return_self(this: Id, _: Sel, _zone: usize) -> Id {
    this
}
extern "C" fn number_with_unsigned_long(_: Class, _: Sel, value: u64) -> Id {
    number_int(value as i64)
}
extern "C" fn number_init_unsigned_long_long(this: Id, _: Sel, value: u64) -> Id {
    unsafe {
        *((this + 8) as *mut usize) = KIND_INT;
        *((this + 16) as *mut u64) = value;
    }
    this
}
extern "C" fn number_unsigned_short_value(this: Id, _: Sel) -> u16 {
    number_as_i64(this) as u16
}
/// The C type the number was made from, as Apple reports it for each kind.
extern "C" fn number_objc_type(this: Id, _: Sel) -> *const std::ffi::c_char {
    match number_parts(this).0 {
        KIND_FLOAT => c"d".as_ptr(),
        KIND_BOOL => c"c".as_ptr(),
        _ => c"q".as_ptr(),
    }
}

// ----- NSDecimalNumber: kept as a whole number, or a double when it has a fraction -----

fn decimal_from(value: f64) -> Id {
    if value.fract() == 0.0 && value.abs() < 9.0e15 {
        return decimal_number(value as i64);
    }
    let object = runtime::alloc_instance(runtime::class_named("NSDecimalNumber"));
    unsafe {
        *((object + 8) as *mut usize) = KIND_FLOAT;
        *((object + 16) as *mut u64) = value.to_bits();
    }
    object
}

extern "C" fn decimal_with_mantissa(_: Class, _: Sel, mantissa: u64, exponent: i16, negative: u8) -> Id {
    if exponent >= 0 {
        if let Some(whole) = 10i64.checked_pow(exponent as u32).and_then(|p| i64::try_from(mantissa).ok()?.checked_mul(p)) {
            return decimal_number(if negative != 0 { -whole } else { whole });
        }
    }
    let value = mantissa as f64 * 10f64.powi(exponent as i32);
    decimal_from(if negative != 0 { -value } else { value })
}

extern "C" fn decimal_not_a_number(_: Class, _: Sel) -> Id {
    decimal_from(f64::NAN)
}

/// The number the text starts with (after spaces); NaN when it starts with none.
extern "C" fn decimal_with_string(class: Class, s: Sel, text: Id) -> Id {
    let text = crate::fw::string::get(text);
    let text = text.trim_start();
    let digits = text.trim_start_matches(['-', '+']);
    if !digits.starts_with(|c: char| c.is_ascii_digit() || c == '.') || !digits.trim_start_matches('.').starts_with(|c: char| c.is_ascii_digit()) {
        return decimal_not_a_number(class, s);
    }
    let number = crate::fw::string::make(text);
    let double: extern "C" fn(Id, Sel) -> f64 =
        unsafe { std::mem::transmute(runtime::imp_for(number, runtime::sel("doubleValue")).expect("NSString doubleValue")) };
    decimal_from(double(number, 0))
}

// ----- NSIndexSet / NSMutableIndexSet: { isa, *mut Ranges } -----

const NOT_FOUND: usize = isize::MAX as usize;

/// Indexes as separate, non-touching ranges (start -> end, end exclusive),
/// as Apple keeps them: a range of any size costs one entry.
#[derive(Clone, Default, PartialEq)]
pub struct Ranges(std::collections::BTreeMap<usize, usize>);

impl Ranges {
    fn add(&mut self, mut start: usize, mut end: usize) {
        if start >= end {
            return;
        }
        // Join every range that overlaps or touches [start, end).
        let touching: Vec<(usize, usize)> = self.0.range(..=end).rev().take_while(|&(_, &e)| e >= start).map(|(&s, &e)| (s, e)).collect();
        for (s, e) in touching {
            self.0.remove(&s);
            start = start.min(s);
            end = end.max(e);
        }
        self.0.insert(start, end);
    }

    fn remove(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let overlapping: Vec<(usize, usize)> = self.0.range(..end).rev().take_while(|&(_, &e)| e > start).map(|(&s, &e)| (s, e)).collect();
        for (s, e) in overlapping {
            self.0.remove(&s);
            if s < start {
                self.0.insert(s, start);
            }
            if e > end {
                self.0.insert(end, e);
            }
        }
    }

    fn contains(&self, index: usize) -> bool {
        self.0.range(..=index).next_back().is_some_and(|(_, &e)| index < e)
    }

    fn count(&self) -> usize {
        self.0.iter().fold(0usize, |n, (s, e)| n.saturating_add(e - s))
    }

    fn first(&self) -> Option<usize> {
        self.0.first_key_value().map(|(&s, _)| s)
    }

    fn last(&self) -> Option<usize> {
        self.0.last_key_value().map(|(_, &e)| e - 1)
    }

    /// The smallest index above `index`.
    fn after(&self, index: usize) -> Option<usize> {
        let next = index.checked_add(1)?;
        if self.contains(next) {
            return Some(next);
        }
        self.0.range(next..).next().map(|(&s, _)| s)
    }

    /// The largest index below `index`.
    fn before(&self, index: usize) -> Option<usize> {
        let previous = index.checked_sub(1)?;
        self.0.range(..=previous).next_back().map(|(_, &e)| (e - 1).min(previous))
    }

    /// Every index, in order (lazily: a huge set is walked, never stored).
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.0.iter().flat_map(|(&s, &e)| s..e)
    }

    pub fn ranges(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.0.iter().map(|(&s, &e)| (s, e))
    }
}

fn indexes(object: Id) -> &'static mut Ranges {
    unsafe {
        let slot = (object + 8) as *mut *mut Ranges;
        if (*slot).is_null() {
            *slot = Box::into_raw(Box::default());
        }
        &mut **slot
    }
}

/// An index set's indexes (empty for nil).
pub fn index_set(object: Id) -> Ranges {
    if object == 0 {
        return Ranges::default();
    }
    indexes(object).clone()
}

/// [start, end) of an NSRange, the end held at the largest index.
fn range_ends(range: crate::fw::string::Range) -> (usize, usize) {
    (range.location, range.location.saturating_add(range.length))
}

extern "C" fn index_set_empty(class: Class, _: Sel) -> Id {
    let object = runtime::alloc_instance(class);
    indexes(object);
    object
}
extern "C" fn index_set_with_index(class: Class, s: Sel, index: usize) -> Id {
    let object = index_set_empty(class, s);
    indexes(object).add(index, index.saturating_add(1));
    object
}
extern "C" fn index_set_with_range(class: Class, s: Sel, range: crate::fw::string::Range) -> Id {
    let object = index_set_empty(class, s);
    let (start, end) = range_ends(range);
    indexes(object).add(start, end);
    object
}
extern "C" fn index_set_init(this: Id, _: Sel) -> Id {
    indexes(this);
    this
}
extern "C" fn index_set_init_with_index(this: Id, _: Sel, index: usize) -> Id {
    indexes(this).add(index, index.saturating_add(1));
    this
}
extern "C" fn index_set_init_with_range(this: Id, _: Sel, range: crate::fw::string::Range) -> Id {
    let (start, end) = range_ends(range);
    indexes(this).add(start, end);
    this
}
extern "C" fn index_set_init_with_set(this: Id, _: Sel, other: Id) -> Id {
    *indexes(this) = index_set(other);
    this
}
extern "C" fn index_set_count(this: Id, _: Sel) -> usize {
    indexes(this).count()
}
extern "C" fn index_set_first(this: Id, _: Sel) -> usize {
    indexes(this).first().unwrap_or(NOT_FOUND)
}
extern "C" fn index_set_last(this: Id, _: Sel) -> usize {
    indexes(this).last().unwrap_or(NOT_FOUND)
}
extern "C" fn index_set_greater(this: Id, _: Sel, index: usize) -> usize {
    indexes(this).after(index).unwrap_or(NOT_FOUND)
}
extern "C" fn index_set_less(this: Id, _: Sel, index: usize) -> usize {
    indexes(this).before(index).unwrap_or(NOT_FOUND)
}
extern "C" fn index_set_contains(this: Id, _: Sel, index: usize) -> bool {
    indexes(this).contains(index)
}
extern "C" fn index_set_contains_set(this: Id, _: Sel, other: Id) -> bool {
    let mine = indexes(this);
    index_set(other).ranges().all(|(s, e)| mine.0.range(..=s).next_back().is_some_and(|(_, &end)| end >= e))
}
extern "C" fn index_set_is_equal(this: Id, _: Sel, other: Id) -> bool {
    other != 0 && *indexes(this) == index_set(other)
}
extern "C" fn index_set_copy(this: Id, _: Sel, _zone: usize) -> Id {
    let object = runtime::alloc_instance(runtime::class_named("NSIndexSet"));
    *indexes(object) = indexes(this).clone();
    object
}
extern "C" fn index_set_mutable_copy(this: Id, _: Sel, _zone: usize) -> Id {
    let object = runtime::alloc_instance(runtime::class_named("NSMutableIndexSet"));
    *indexes(object) = indexes(this).clone();
    object
}
/// block(index, &stop), in order.
extern "C" fn index_set_enumerate(this: Id, _: Sel, block: usize) {
    let header = block as *const crate::libsystem::mach::BlockHeader;
    let invoke: extern "C" fn(usize, usize, *mut u8) = unsafe { std::mem::transmute((*header).invoke) };
    let mut stop = 0u8;
    for index in index_set(this).iter() {
        invoke(block, index, &mut stop);
        if stop != 0 {
            break;
        }
    }
}
extern "C" fn index_set_description(this: Id, _: Sel) -> Id {
    let set = index_set(this);
    let parts: Vec<String> = set.ranges().map(|(s, e)| if e - s == 1 { s.to_string() } else { format!("{s}-{}", e - 1) }).collect();
    crate::fw::string::make(&format!("<NSIndexSet>[number of indexes: {} (in {} ranges), indexes: ({})]", set.count(), parts.len(), parts.join(" ")))
}
extern "C" fn index_set_add(this: Id, _: Sel, index: usize) {
    indexes(this).add(index, index.saturating_add(1));
}
extern "C" fn index_set_remove(this: Id, _: Sel, index: usize) {
    indexes(this).remove(index, index.saturating_add(1));
}
extern "C" fn index_set_add_range(this: Id, _: Sel, range: crate::fw::string::Range) {
    let (start, end) = range_ends(range);
    indexes(this).add(start, end);
}
extern "C" fn index_set_remove_range(this: Id, _: Sel, range: crate::fw::string::Range) {
    let (start, end) = range_ends(range);
    indexes(this).remove(start, end);
}
extern "C" fn index_set_add_set(this: Id, _: Sel, other: Id) {
    for (s, e) in index_set(other).ranges() {
        indexes(this).add(s, e);
    }
}
extern "C" fn index_set_remove_set(this: Id, _: Sel, other: Id) {
    for (s, e) in index_set(other).ranges() {
        indexes(this).remove(s, e);
    }
}
extern "C" fn index_set_remove_all(this: Id, _: Sel) {
    *indexes(this) = Ranges::default();
}

// ----- more NSArray / NSDictionary -----

/// Calls a test block (item, index, &stop) -> BOOL for each item until it stops.
fn passing_test(list: &[Id], block: usize, mut found: impl FnMut(usize)) {
    let header = block as *const crate::libsystem::mach::BlockHeader;
    let invoke: extern "C" fn(usize, Id, usize, *mut u8) -> u8 = unsafe { std::mem::transmute((*header).invoke) };
    let mut stop = 0u8;
    for (index, &item) in list.iter().enumerate() {
        if invoke(block, item, index, &mut stop) != 0 {
            found(index);
        }
        if stop != 0 {
            break;
        }
    }
}

extern "C" fn array_index_passing(this: Id, _: Sel, block: usize) -> usize {
    let mut first = None;
    let list = array_items(this);
    let header = block as *const crate::libsystem::mach::BlockHeader;
    let invoke: extern "C" fn(usize, Id, usize, *mut u8) -> u8 = unsafe { std::mem::transmute((*header).invoke) };
    let mut stop = 0u8;
    for (index, &item) in list.iter().enumerate() {
        if invoke(block, item, index, &mut stop) != 0 {
            first = Some(index);
            break;
        }
        if stop != 0 {
            break;
        }
    }
    first.unwrap_or(NOT_FOUND)
}

extern "C" fn array_indexes_passing(this: Id, _: Sel, block: usize) -> Id {
    let set = runtime::alloc_instance(runtime::class_named("NSIndexSet"));
    passing_test(&array_items(this), block, |index| {
        indexes(set).add(index, index + 1);
    });
    set
}

extern "C" fn array_objects_at_indexes(this: Id, _: Sel, set: Id) -> Id {
    let list = array_items(this);
    let wanted = index_set(set);
    if let Some(last) = wanted.last().filter(|&last| last >= list.len()) {
        crate::log!("STOP: objectsAtIndexes: index {last} is past the end ({} items)", list.len());
        crate::report::finish(1);
    }
    array_from(&wanted.iter().map(|index| list[index]).collect::<Vec<_>>())
}

extern "C" fn array_init_copy_items(this: Id, _: Sel, other: Id, copy_items: u8) -> Id {
    let mut list = array_items(other);
    if copy_items != 0 {
        list = list.into_iter().map(|item| if item == 0 { 0 } else { runtime::send(item, "copyWithZone:", &[0]) }).collect();
    }
    set_array_items(this, list);
    this
}

extern "C" fn array_remove_at_indexes(this: Id, _: Sel, set: Id) {
    let remove = index_set(set);
    let mut index = 0;
    items(this).retain(|_| {
        let keep = !remove.contains(index);
        index += 1;
        keep
    });
}

extern "C" fn array_remove_range(this: Id, _: Sel, range: crate::fw::string::Range) {
    let list = items(this);
    let start = range.location.min(list.len());
    let end = range.location.saturating_add(range.length).min(list.len());
    list.drain(start..end);
}

extern "C" fn array_set_array(this: Id, _: Sel, other: Id) {
    set_array_items(this, array_items(other));
}

extern "C" fn dict_with_objects_for_keys(class: Class, s: Sel, values: Id, keys: Id) -> Id {
    dict_init_objects_for_keys(runtime::alloc_instance(class), s, values, keys)
}

extern "C" fn dict_init_objects_for_keys(this: Id, s: Sel, values: Id, keys: Id) -> Id {
    let (values, keys) = (array_items(values), array_items(keys));
    if values.len() != keys.len() {
        crate::log!("STOP: dictionaryWithObjects:forKeys: has {} objects but {} keys", values.len(), keys.len());
        crate::report::finish(1);
    }
    dict_init_objects_keys_count(this, s, values.as_ptr(), keys.as_ptr(), keys.len())
}

fn keys_sorted_by_value(this: Id, mut order: impl FnMut(Id, Id) -> std::cmp::Ordering) -> Id {
    let mut entries = dictionary_pairs(this);
    entries.sort_by(|a, b| order(a.1, b.1));
    array_from(&entries.into_iter().map(|(k, _)| k).collect::<Vec<_>>())
}

extern "C" fn dict_keys_sorted_by_selector(this: Id, _: Sel, selector: Sel) -> Id {
    keys_sorted_by_value(this, |a, b| compare_with_selector(a, b, selector))
}

extern "C" fn dict_keys_sorted_by_comparator(this: Id, _: Sel, block: usize) -> Id {
    keys_sorted_by_value(this, |a, b| compare_with_block(a, b, block))
}

/// "key" = "value"; lines, as a .strings file holds them.
extern "C" fn dict_strings_file_format(this: Id, _: Sel) -> Id {
    let quote = |object: Id| {
        let text = crate::fw::string::describe(object);
        let escaped: String = text
            .chars()
            .flat_map(|c| match c {
                '"' => vec!['\\', '"'],
                '\\' => vec!['\\', '\\'],
                '\n' => vec!['\\', 'n'],
                '\t' => vec!['\\', 't'],
                other => vec![other],
            })
            .collect();
        format!("\"{escaped}\"")
    };
    let lines: String = dictionary_pairs(this).into_iter().map(|(k, v)| format!("{} = {};\n", quote(k), quote(v))).collect();
    crate::fw::string::make(&lines)
}

pub fn register(_: &mut Registry) {
    let array = define_class("NSArray", "NSObject", 16);
    let mutable_array = define_class("NSMutableArray", "NSArray", 16);
    let dictionary = define_class("NSDictionary", "NSObject", 16);
    let mutable_dictionary = define_class("NSMutableDictionary", "NSDictionary", 16);
    let value = define_class("NSValue", "NSObject", 24);
    let number = define_class("NSNumber", "NSValue", 24);
    let decimal = define_class("NSDecimalNumber", "NSNumber", 24);
    class_methods(
        decimal,
        &[
            ("zero", decimal_zero as usize),
            ("one", decimal_one as usize),
            ("notANumber", decimal_not_a_number as usize),
            ("decimalNumberWithMantissa:exponent:isNegative:", decimal_with_mantissa as usize),
            ("decimalNumberWithString:", decimal_with_string as usize),
        ],
    );
    class_methods(
        value,
        &[
            ("valueWithPointer:", value_with_pointer as usize),
            ("valueWithNonretainedObject:", value_with_pointer as usize),
            ("valueWithCGPoint:", value_with_point as usize),
            ("valueWithCGSize:", value_with_size as usize),
            ("valueWithCGRect:", value_with_rect as usize),
            ("valueWithCGAffineTransform:", value_with_transform as usize),
            ("valueWithCATransform3D:", value_with_transform_3d as usize),
            ("valueWithRange:", value_with_range as usize),
        ],
    );
    methods(
        value,
        &[
            ("pointerValue", value_pointer as usize),
            ("nonretainedObjectValue", value_pointer as usize),
            ("getValue:", value_get as usize),
            ("objCType", value_objc_type as usize),
            ("CGPointValue", value_point as usize),
            ("CGSizeValue", value_size as usize),
            ("CGRectValue", value_rect as usize),
            ("CGAffineTransformValue", value_transform as usize),
            ("CATransform3DValue", value_transform_3d as usize),
            ("rangeValue", value_range as usize),
            ("isEqual:", value_is_equal as usize),
            ("isEqualToValue:", value_is_equal as usize),
            ("hash", value_hash as usize),
            ("description", value_description as usize),
            ("copyWithZone:", return_self as usize),
            ("encodeWithCoder:", value_encode_with_coder as usize),
            ("initWithCoder:", value_init_with_coder as usize),
        ],
    );
    let index_set = define_class("NSIndexSet", "NSObject", 16);
    let mutable_index_set = define_class("NSMutableIndexSet", "NSIndexSet", 16);
    class_methods(
        index_set,
        &[
            ("indexSet", index_set_empty as usize),
            ("indexSetWithIndex:", index_set_with_index as usize),
            ("indexSetWithIndexesInRange:", index_set_with_range as usize),
        ],
    );
    methods(
        index_set,
        &[
            ("init", index_set_init as usize),
            ("initWithIndex:", index_set_init_with_index as usize),
            ("initWithIndexesInRange:", index_set_init_with_range as usize),
            ("initWithIndexSet:", index_set_init_with_set as usize),
            ("count", index_set_count as usize),
            ("firstIndex", index_set_first as usize),
            ("lastIndex", index_set_last as usize),
            ("indexGreaterThanIndex:", index_set_greater as usize),
            ("indexLessThanIndex:", index_set_less as usize),
            ("containsIndex:", index_set_contains as usize),
            ("containsIndexes:", index_set_contains_set as usize),
            ("isEqualToIndexSet:", index_set_is_equal as usize),
            ("isEqual:", index_set_is_equal as usize),
            ("enumerateIndexesUsingBlock:", index_set_enumerate as usize),
            ("copyWithZone:", index_set_copy as usize),
            ("mutableCopyWithZone:", index_set_mutable_copy as usize),
            ("description", index_set_description as usize),
        ],
    );
    methods(
        mutable_index_set,
        &[
            ("addIndex:", index_set_add as usize),
            ("removeIndex:", index_set_remove as usize),
            ("addIndexesInRange:", index_set_add_range as usize),
            ("removeIndexesInRange:", index_set_remove_range as usize),
            ("addIndexes:", index_set_add_set as usize),
            ("removeIndexes:", index_set_remove_set as usize),
            ("removeAllIndexes", index_set_remove_all as usize),
        ],
    );
    CLASSES.set(Classes { array, mutable_array, dictionary, mutable_dictionary, number }).ok();
    let enumerator_class = define_class("NSEnumerator", "NSObject", 16);
    ENUMERATOR_CLASS.set(enumerator_class).ok();
    methods(
        enumerator_class,
        &[
            ("nextObject", enumerator_next as usize),
            ("allObjects", enumerator_all as usize),
            ("countByEnumeratingWithState:objects:count:", enumerator_enumerate as usize),
        ],
    );
    methods(array, &[("objectEnumerator", array_object_enumerator as usize), ("reverseObjectEnumerator", array_reverse_enumerator as usize)]);
    methods(
        array,
        &[
            ("sortedArrayUsingSelector:", array_sorted_selector as usize),
            ("sortedArrayUsingComparator:", array_sorted_comparator as usize),
            ("sortedArrayUsingFunction:context:", array_sorted_function as usize),
            ("componentsJoinedByString:", array_joined as usize),
            ("arrayByAddingObject:", array_adding as usize),
            ("arrayByAddingObjectsFromArray:", array_adding_array as usize),
            ("subarrayWithRange:", array_subarray as usize),
            ("indexOfObjectIdenticalTo:", array_index_identical as usize),
            ("isEqualToArray:", array_is_equal as usize),
            ("isEqual:", array_is_equal as usize),
            ("makeObjectsPerformSelector:", array_make_perform as usize),
            ("makeObjectsPerformSelector:withObject:", array_make_perform_with as usize),
            ("enumerateObjectsUsingBlock:", array_enumerate_block as usize),
            ("initWithContentsOfFile:", init_with_contents_of_file as usize),
            ("writeToFile:atomically:", write_to_file as usize),
            ("indexOfObjectPassingTest:", array_index_passing as usize),
            ("indexesOfObjectsPassingTest:", array_indexes_passing as usize),
            ("objectsAtIndexes:", array_objects_at_indexes as usize),
            ("initWithArray:copyItems:", array_init_copy_items as usize),
        ],
    );
    class_methods(array, &[("arrayWithContentsOfFile:", array_with_contents_of_file as usize)]);
    methods(
        mutable_array,
        &[
            ("removeObjectsAtIndexes:", array_remove_at_indexes as usize),
            ("removeObjectsInRange:", array_remove_range as usize),
            ("setArray:", array_set_array as usize),
            ("sortUsingSelector:", array_sort_selector as usize),
            ("sortUsingComparator:", array_sort_comparator as usize),
            ("sortUsingFunction:context:", array_sort_function as usize),
            ("removeObjectsInArray:", array_remove_in as usize),
            ("exchangeObjectAtIndex:withObjectAtIndex:", array_exchange as usize),
            ("removeObjectIdenticalTo:", array_remove_object as usize),
        ],
    );
    methods(
        dictionary,
        &[
            ("enumerateKeysAndObjectsUsingBlock:", dict_enumerate_block as usize),
            ("objectsForKeys:notFoundMarker:", dict_objects_for_keys as usize),
            ("isEqualToDictionary:", dict_is_equal as usize),
            ("isEqual:", dict_is_equal as usize),
            ("allKeysForObject:", dict_keys_for_object as usize),
            ("initWithContentsOfFile:", init_with_contents_of_file as usize),
            ("writeToFile:atomically:", write_to_file as usize),
            ("initWithObjects:forKeys:", dict_init_objects_for_keys as usize),
            ("keysSortedByValueUsingSelector:", dict_keys_sorted_by_selector as usize),
            ("keysSortedByValueUsingComparator:", dict_keys_sorted_by_comparator as usize),
            ("descriptionInStringsFileFormat", dict_strings_file_format as usize),
        ],
    );
    class_methods(
        dictionary,
        &[
            ("dictionaryWithObjects:forKeys:", dict_with_objects_for_keys as usize),
            ("dictionaryWithContentsOfFile:", dict_with_contents_of_file as usize),
            ("dictionaryWithContentsOfURL:", dict_with_contents_of_url as usize),
        ],
    );
    // NSCache keeps its entries the same way (never evicts on its own).
    let cache = define_class("NSCache", "NSObject", 16);
    methods(
        cache,
        &[
            ("init", dict_init as usize),
            ("objectForKey:", cache_object_for_key as usize),
            ("setObject:forKey:", cache_set as usize),
            ("setObject:forKey:cost:", cache_set_with_cost as usize),
            ("removeObjectForKey:", cache_remove as usize),
            ("removeAllObjects", cache_remove_all as usize),
            ("setDelegate:", ignore_value as usize),
            ("setName:", ignore_value as usize),
            ("setCountLimit:", ignore_value as usize),
            ("setTotalCostLimit:", ignore_value as usize),
            ("setEvictsObjectsWithDiscardedContent:", ignore_value as usize),
        ],
    );
    methods(dictionary, &[("keyEnumerator", dict_key_enumerator as usize), ("objectEnumerator", dict_object_enumerator as usize)]);
    methods(
        array,
        &[
            ("init", array_init as usize),
            ("initWithObjects:count:", array_init_with_objects_count as usize),
            ("initWithObjects:", ios_array_init_with_objects as usize),
            ("initWithArray:", array_init_with_array as usize),
            ("count", array_count as usize),
            ("objectAtIndex:", array_object_at as usize),
            ("objectAtIndexedSubscript:", array_object_at as usize),
            ("firstObject", array_first as usize),
            ("lastObject", array_last as usize),
            ("containsObject:", array_contains as usize),
            ("indexOfObject:", array_index_of as usize),
            ("copyWithZone:", array_copy as usize),
            ("mutableCopyWithZone:", array_mutable_copy as usize),
            ("countByEnumeratingWithState:objects:count:", array_enumerate as usize),
            ("description", array_description as usize),
        ],
    );
    class_methods(
        array,
        &[
            ("array", array_empty as usize),
            ("arrayWithObject:", array_with_object as usize),
            ("arrayWithObjects:count:", array_with_objects_count as usize),
            ("arrayWithObjects:", ios_array_with_objects as usize),
            ("arrayWithArray:", array_with_array as usize),
        ],
    );
    methods(
        mutable_array,
        &[
            ("addObject:", array_add as usize),
            ("insertObject:atIndex:", array_insert as usize),
            ("removeObjectAtIndex:", array_remove_at as usize),
            ("removeObject:", array_remove_object as usize),
            ("removeAllObjects", array_remove_all as usize),
            ("removeLastObject", array_remove_last as usize),
            ("replaceObjectAtIndex:withObject:", array_replace as usize),
            ("setObject:atIndexedSubscript:", array_replace as usize),
            ("addObjectsFromArray:", array_add_all as usize),
            ("initWithCapacity:", array_init_capacity as usize),
        ],
    );
    class_methods(mutable_array, &[("arrayWithCapacity:", array_with_capacity as usize)]);
    methods(
        dictionary,
        &[
            ("init", dict_init as usize),
            ("initWithObjects:forKeys:count:", dict_init_objects_keys_count as usize),
            ("initWithDictionary:", dict_init_with_dictionary as usize),
            ("initWithDictionary:copyItems:", dict_init_with_dictionary_copy as usize),
            ("objectForKey:", dict_object_for_key as usize),
            ("objectForKeyedSubscript:", dict_object_for_key as usize),
            ("valueForKey:", dict_object_for_key as usize),
            ("count", dict_count as usize),
            ("allKeys", dict_all_keys as usize),
            ("allValues", dict_all_values as usize),
            ("copyWithZone:", dict_copy as usize),
            ("mutableCopyWithZone:", dict_mutable_copy as usize),
            ("countByEnumeratingWithState:objects:count:", dict_enumerate as usize),
            ("description", dict_description as usize),
            ("initWithObjectsAndKeys:", ios_dict_init_with_objects_and_keys as usize),
        ],
    );
    class_methods(
        dictionary,
        &[
            ("dictionary", dict_empty as usize),
            ("dictionaryWithObject:forKey:", dict_with_object_for_key as usize),
            ("dictionaryWithObjects:forKeys:count:", dict_with_objects_keys_count as usize),
            ("dictionaryWithObjectsAndKeys:", ios_dict_with_objects_and_keys as usize),
            ("dictionaryWithDictionary:", dict_with_dictionary as usize),
        ],
    );
    methods(
        mutable_dictionary,
        &[
            ("setObject:forKey:", dict_set as usize),
            ("setObject:forKeyedSubscript:", dict_set as usize),
            ("setValue:forKey:", dict_set_value as usize),
            ("removeObjectForKey:", dict_remove as usize),
            ("removeObjectsForKeys:", dict_remove_keys as usize),
            ("removeAllObjects", dict_remove_all as usize),
            ("addEntriesFromDictionary:", dict_add_entries as usize),
            ("setDictionary:", dict_set_dictionary as usize),
            ("initWithCapacity:", dict_init_capacity as usize),
        ],
    );
    class_methods(mutable_dictionary, &[("dictionaryWithCapacity:", dict_with_capacity as usize)]);
    class_methods(
        number,
        &[
            ("numberWithInt:", number_with_int as usize),
            ("numberWithUnsignedInt:", number_with_unsigned_int as usize),
            ("numberWithInteger:", number_with_long as usize),
            ("numberWithLong:", number_with_long as usize),
            ("numberWithLongLong:", number_with_long as usize),
            ("numberWithUnsignedLongLong:", number_with_long as usize),
            ("numberWithUnsignedInteger:", number_with_long as usize),
            ("numberWithUnsignedLong:", number_with_unsigned_long as usize),
            ("numberWithBool:", number_with_bool as usize),
            ("numberWithFloat:", number_with_float as usize),
            ("numberWithDouble:", number_with_double as usize),
        ],
    );
    methods(
        number,
        &[
            ("initWithInt:", number_init_int as usize),
            ("initWithBool:", number_init_bool as usize),
            ("initWithFloat:", number_init_float as usize),
            ("intValue", number_int_value as usize),
            ("integerValue", number_int_value as usize),
            ("longValue", number_int_value as usize),
            ("longLongValue", number_int_value as usize),
            ("unsignedIntValue", number_int_value as usize),
            ("unsignedIntegerValue", number_int_value as usize),
            ("unsignedLongLongValue", number_int_value as usize),
            ("unsignedLongValue", number_int_value as usize),
            ("unsignedShortValue", number_unsigned_short_value as usize),
            ("initWithUnsignedLongLong:", number_init_unsigned_long_long as usize),
            ("objCType", number_objc_type as usize),
            ("boolValue", number_bool_value as usize),
            ("floatValue", number_float_value as usize),
            ("doubleValue", number_double_value as usize),
            ("isEqual:", number_is_equal as usize),
            ("isEqualToNumber:", number_is_equal as usize),
            ("hash", number_hash as usize),
            ("compare:", number_compare as usize),
            ("stringValue", number_string_value as usize),
            ("description", number_string_value as usize),
            ("copyWithZone:", return_self as usize),
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// setDictionary: makes a mutable dictionary hold exactly the other one's entries.
    #[test]
    fn set_dictionary_replaces_every_entry() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let key = |name: &str| crate::fw::string::make(name);
        let mine = runtime::send(dictionary_from(&[(key("old"), number_int(1)), (key("kept"), number_int(2))]), "mutableCopy", &[]);
        let other = dictionary_from(&[(key("kept"), number_int(3)), (key("new"), number_int(4))]);
        runtime::send(mine, "setDictionary:", &[other]);
        assert!(runtime::send(mine, "isEqual:", &[other]) & 0xFF != 0, "{}", crate::fw::string::describe(mine));
        runtime::send(mine, "setDictionary:", &[0]);
        assert_eq!(dictionary_pairs(mine).len(), 0, "nil empties it");
    }

    /// mutableCopy keeps each entry, as -copy and arrayWithArray: /
    /// dictionaryWithDictionary: do: the copy outlives every other holder.
    #[test]
    fn mutable_copies_keep_their_entries() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        use crate::fw::counted;
        let data = runtime::send(runtime::class_named("NSMutableData"), "alloc", &[]);
        let data = runtime::send(data, "initWithLength:", &[1]);
        assert_eq!(counted::count(data), Some(1), "one reference, the caller's");
        let key = crate::fw::string::make("pixels");
        let list = array_from(&[data]);
        let words = dictionary_from(&[(key, data)]);
        assert_eq!(counted::count(data), Some(3), "the source collections keep it");
        let list_copy = runtime::send(list, "mutableCopy", &[]);
        let words_copy = runtime::send(words, "mutableCopy", &[]);
        assert_eq!(counted::count(data), Some(5), "both copies keep their entry");
        // The caller lets go, and so do the source collections (ours never
        // release, so their holds are dropped here): only the copies' holds remain.
        for _ in 0..4 {
            counted::release(data);
        }
        assert_eq!(counted::count(data), Some(1), "the copies' entries are alive");
        assert_eq!(runtime::send(list_copy, "objectAtIndex:", &[0]), data);
        assert_eq!(runtime::send(words_copy, "objectForKey:", &[key]), data);
    }

    static FOREIGN: std::sync::Mutex<Vec<(Id, Id)>> = std::sync::Mutex::new(Vec::new());

    extern "C" fn foreign_count(_this: Id, _: Sel) -> usize {
        FOREIGN.lock().unwrap().len()
    }
    extern "C" fn foreign_object_for_key(_this: Id, _: Sel, key: Id) -> Id {
        FOREIGN.lock().unwrap().iter().find(|(k, _)| keys_equal(*k, key)).map_or(0, |(_, v)| *v)
    }
    extern "C" fn foreign_key_enumerator(_this: Id, _: Sel) -> Id {
        let keys: Vec<Id> = FOREIGN.lock().unwrap().iter().map(|(k, _)| *k).collect();
        runtime::send(array_from(&keys), "objectEnumerator", &[])
    }
    extern "C" fn foreign_object_at(_this: Id, _: Sel, index: usize) -> Id {
        FOREIGN.lock().unwrap()[index].1
    }

    /// Collections that aren't ours (a game's class-cluster subclass with
    /// only the primitive methods, or a game proxy standing in for one) are
    /// read through those methods when copied, as Foundation does; reading
    /// our own storage from them found nothing (SpellCraft's inherited item
    /// specs lost every value of their parent spec).
    #[test]
    fn copies_read_other_collections_through_their_primitive_methods() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let s = crate::fw::string::make;
        *FOREIGN.lock().unwrap() = vec![(s("icon"), s("Card_Spell_Hex.jpg")), (s("price"), s("gold"))];
        let words = define_class("TestForeignWords", "NSDictionary", 16);
        methods(words, &[("count", foreign_count as usize), ("objectForKey:", foreign_object_for_key as usize), ("keyEnumerator", foreign_key_enumerator as usize)]);
        let list = define_class("TestForeignList", "NSArray", 16);
        methods(list, &[("count", foreign_count as usize), ("objectAtIndex:", foreign_object_at as usize)]);
        let (words, list) = (runtime::alloc_instance(words), runtime::alloc_instance(list));
        let read = |dictionary: Id, key: &str| crate::fw::string::get(runtime::send(dictionary, "objectForKey:", &[s(key)]));

        let copy = runtime::send(runtime::class_named("NSMutableDictionary"), "dictionaryWithDictionary:", &[words]);
        assert_eq!((read(copy, "icon").as_str(), read(copy, "price").as_str()), ("Card_Spell_Hex.jpg", "gold"));
        let merged = runtime::send(runtime::class_named("NSMutableDictionary"), "dictionary", &[]);
        runtime::send(merged, "addEntriesFromDictionary:", &[words]);
        assert_eq!(read(merged, "icon"), "Card_Spell_Hex.jpg");
        let items = runtime::send(runtime::class_named("NSArray"), "arrayWithArray:", &[list]);
        let texts: Vec<String> = array_items(items).into_iter().map(crate::fw::string::get).collect();
        assert_eq!(texts, ["Card_Spell_Hex.jpg", "gold"]);
    }

    /// Calls `f(x0, x1, x2, ...)` the way Apple arm64 code calls a variadic
    /// function: the variadic arguments in 8-byte stack slots, and x3-x7
    /// pointing at a decoy list (what a register-reading version would see).
    unsafe fn call_apple_variadic(f: usize, x0: usize, x1: usize, x2: usize, stack: &[usize], decoy: &[usize]) -> usize {
        let mut slots = stack.to_vec();
        slots.resize(stack.len().div_ceil(2) * 2, 0);
        let result: usize;
        std::arch::asm!(
            "mov x20, sp",
            "sub sp, sp, x13",
            "mov x9, #0",
            "2:",
            "cmp x9, x13",
            "b.ge 3f",
            "ldr x10, [x12, x9]",
            "str x10, [sp, x9]",
            "add x9, x9, #8",
            "b 2b",
            "3:",
            "blr x11",
            "mov sp, x20",
            inout("x0") x0 => result,
            in("x1") x1,
            in("x2") x2,
            in("x3") decoy.as_ptr(),
            in("x4") decoy.as_ptr(),
            in("x5") decoy.as_ptr(),
            in("x6") decoy.as_ptr(),
            in("x7") decoy.as_ptr(),
            in("x11") f,
            in("x12") slots.as_ptr(),
            in("x13") slots.len() * 8,
            out("x20") _,
            clobber_abi("C"),
        );
        result
    }

    #[test]
    fn nil_terminated_initializers_read_the_stack() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let [first, key1, value2, key2, decoy] = ["first", "key1", "second", "key2", "decoy"].map(crate::fw::string::make);
        let dictionary = runtime::alloc_instance(runtime::class_named("NSMutableDictionary"));
        let f = ios_dict_init_with_objects_and_keys as usize;
        let result = unsafe { call_apple_variadic(f, dictionary, 0, first, &[key1, value2, key2, 0], &[decoy, decoy, 0]) };
        assert_eq!(result, dictionary);
        assert_eq!(pairs(dictionary).len(), 2, "read the pairs from the stack, not the registers");
        assert_eq!(dictionary_get(dictionary, key1), first);
        assert_eq!(dictionary_get(dictionary, key2), value2);
    }

    /// NSValue boxes a pointer (or an object it doesn't keep); equal when the pointers match.
    #[test]
    fn values_box_pointers() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let class = runtime::class_named("NSValue");
        let boxed = runtime::send(class, "valueWithPointer:", &[0x1234_5670]);
        assert_ne!(boxed, 0, "+[NSValue valueWithPointer:] gave nil");
        assert_eq!(runtime::send(boxed, "pointerValue", &[]), 0x1234_5670);
        let same = runtime::send(class, "valueWithNonretainedObject:", &[0x1234_5670]);
        assert_eq!(runtime::send(same, "nonretainedObjectValue", &[]), 0x1234_5670);
        assert_eq!(runtime::send(boxed, "isEqual:", &[same]) & 0xFF, 1);
        assert_eq!(runtime::send(boxed, "hash", &[]), runtime::send(same, "hash", &[]));
        let other = runtime::send(class, "valueWithPointer:", &[0x99]);
        assert_eq!(runtime::send(boxed, "isEqualToValue:", &[other]) & 0xFF, 0);
        let mut out = 0usize;
        runtime::send(boxed, "getValue:", &[&mut out as *mut usize as usize]);
        assert_eq!(out, 0x1234_5670);
    }

    /// NSValue boxes geometry structs by value (CGFloat doubles here; a
    /// CATransform3D goes by address, too big for registers) and gives
    /// each back with its type encoding; equal boxes compare equal.
    #[test]
    fn values_box_geometry_structs() {
        use crate::fw::geometry::{Point, Rect, Size, Transform, Transform3D};
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let class = runtime::class_named("NSValue");
        let imp = |receiver: Id, name: &str| runtime::imp_for(receiver, runtime::sel(name)).unwrap_or_else(|| panic!("{name} is missing"));
        let meta = |name: &str| imp(class, name);
        let rect = Rect::new(1.5, -2.0, 320.0, 480.25);
        let make_rect: extern "C" fn(Class, Sel, Rect) -> Id = unsafe { std::mem::transmute(meta("valueWithCGRect:")) };
        let boxed = make_rect(class, runtime::sel("valueWithCGRect:"), rect);
        let get_rect: extern "C" fn(Id, Sel, ) -> Rect = unsafe { std::mem::transmute(imp(boxed, "CGRectValue")) };
        assert_eq!(get_rect(boxed, 0), rect);
        let kind = runtime::send(boxed, "objCType", &[]);
        assert_eq!(unsafe { runtime::cstr(kind) }, "{CGRect={CGPoint=dd}{CGSize=dd}}");
        let again = make_rect(class, 0, rect);
        assert_eq!(runtime::send(boxed, "isEqualToValue:", &[again]) & 0xFF, 1);
        assert_eq!(runtime::send(boxed, "isEqual:", &[make_rect(class, 0, Rect::default())]) & 0xFF, 0);

        let make_point: extern "C" fn(Class, Sel, Point) -> Id = unsafe { std::mem::transmute(meta("valueWithCGPoint:")) };
        let point = make_point(class, 0, Point { x: 3.0, y: 4.0 });
        let get_point: extern "C" fn(Id, Sel) -> Point = unsafe { std::mem::transmute(imp(point, "CGPointValue")) };
        assert_eq!(get_point(point, 0), Point { x: 3.0, y: 4.0 });
        let make_size: extern "C" fn(Class, Sel, Size) -> Id = unsafe { std::mem::transmute(meta("valueWithCGSize:")) };
        let size = make_size(class, 0, Size { width: 5.0, height: 6.0 });
        let get_size: extern "C" fn(Id, Sel) -> Size = unsafe { std::mem::transmute(imp(size, "CGSizeValue")) };
        assert_eq!(get_size(size, 0), Size { width: 5.0, height: 6.0 });
        let turn = Transform { a: 0.0, b: 1.0, c: -1.0, d: 0.0, tx: 7.0, ty: 8.0 };
        let make_transform: extern "C" fn(Class, Sel, Transform) -> Id = unsafe { std::mem::transmute(meta("valueWithCGAffineTransform:")) };
        let transform = make_transform(class, 0, turn);
        let get_transform: extern "C" fn(Id, Sel) -> Transform = unsafe { std::mem::transmute(imp(transform, "CGAffineTransformValue")) };
        assert_eq!(get_transform(transform, 0), turn);
        let mut cells = [0.0f64; 16];
        cells.iter_mut().enumerate().for_each(|(i, c)| *c = i as f64 + 0.5);
        let make_3d: extern "C" fn(Class, Sel, Transform3D) -> Id = unsafe { std::mem::transmute(meta("valueWithCATransform3D:")) };
        let three = make_3d(class, 0, Transform3D(cells));
        let get_3d: extern "C" fn(Id, Sel) -> Transform3D = unsafe { std::mem::transmute(imp(three, "CATransform3DValue")) };
        assert_eq!(get_3d(three, 0).0, cells);
        // getValue: copies the struct's bytes.
        let mut out = Point::default();
        runtime::send(point, "getValue:", &[&mut out as *mut Point as usize]);
        assert_eq!(out, Point { x: 3.0, y: 4.0 });
    }

    /// The NSNumber/NSDecimalNumber pieces games read back as other types.
    #[test]
    fn numbers_and_decimals_keep_their_values() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let number = runtime::class_named("NSNumber");
        let big = runtime::send(number, "numberWithUnsignedLong:", &[u64::MAX as usize]);
        assert_eq!(runtime::send(big, "unsignedLongValue", &[]), u64::MAX as usize);
        let short = runtime::send(number, "numberWithInt:", &[70_000]);
        assert_eq!(runtime::send(short, "unsignedShortValue", &[]) & 0xFFFF, 70_000 & 0xFFFF);
        let made = runtime::send(runtime::alloc_instance(number), "initWithUnsignedLongLong:", &[1 << 40]);
        assert_eq!(number_as_i64(made), 1 << 40);
        assert_eq!(unsafe { runtime::cstr(runtime::send(made, "objCType", &[])) }, "q");
        let decimal = runtime::class_named("NSDecimalNumber");
        // 1999 x 10^-2, negative: -19.99
        let price = runtime::send(decimal, "decimalNumberWithMantissa:exponent:isNegative:", &[1999, (-2i16) as u16 as usize, 1]);
        assert!((number_as_f64(price) + 19.99).abs() < 1e-9, "{}", number_as_f64(price));
        let whole = runtime::send(decimal, "decimalNumberWithMantissa:exponent:isNegative:", &[25, 3, 0]);
        assert_eq!(number_as_i64(whole), 25_000);
        let parsed = runtime::send(decimal, "decimalNumberWithString:", &[crate::fw::string::make("4.75")]);
        assert_eq!(number_as_f64(parsed), 4.75);
        let nan = runtime::send(decimal, "notANumber", &[]);
        assert!(number_as_f64(nan).is_nan());
        let bad = runtime::send(decimal, "decimalNumberWithString:", &[crate::fw::string::make("price")]);
        assert!(number_as_f64(bad).is_nan(), "text that isn't a number gives NaN");
    }

    /// Index sets, and the array and dictionary methods that take ranges,
    /// index sets, blocks and selectors.
    #[test]
    fn index_sets_and_collection_edits() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let s = crate::fw::string::make;
        let n = number_int;
        let indexes = runtime::send(runtime::class_named("NSMutableIndexSet"), "indexSet", &[]);
        for i in [5, 1, 3, 1] {
            runtime::send(indexes, "addIndex:", &[i]);
        }
        runtime::send(indexes, "removeIndex:", &[3]);
        assert_eq!(runtime::send(indexes, "count", &[]), 2);
        extern "C" fn note(block: usize, index: usize, _stop: *mut u8) {
            unsafe { (*((block + 32) as *const std::sync::Mutex<Vec<usize>>)).lock().unwrap().push(index) };
        }
        #[repr(C)]
        struct Block {
            header: crate::libsystem::mach::BlockHeader,
            seen: std::sync::Mutex<Vec<usize>>,
        }
        let descriptor: [usize; 4] = [0, std::mem::size_of::<Block>(), 0, 0];
        let block = Block {
            header: crate::libsystem::mach::BlockHeader { isa: 0, flags: 1 << 28, reserved: 0, invoke: note as usize, descriptor: &descriptor },
            seen: std::sync::Mutex::new(Vec::new()),
        };
        runtime::send(indexes, "enumerateIndexesUsingBlock:", &[&block as *const Block as usize]);
        assert_eq!(*block.seen.lock().unwrap(), [1, 5]);

        let list = runtime::send(array_from(&[s("a"), s("b"), s("c"), s("d"), s("e"), s("f")]), "mutableCopy", &[]);
        runtime::send(list, "removeObjectsAtIndexes:", &[indexes]);
        let remove_range: extern "C" fn(Id, Sel, crate::fw::string::Range) = unsafe { std::mem::transmute(runtime::imp_for(list, runtime::sel("removeObjectsInRange:")).unwrap()) };
        remove_range(list, 0, crate::fw::string::Range { location: 1, length: 2 });
        let left: Vec<String> = array_items(list).into_iter().map(crate::fw::string::get).collect();
        assert_eq!(left, ["a", "e"]);
        runtime::send(list, "setArray:", &[array_from(&[s("x")])]);
        assert_eq!(array_items(list).len(), 1);
        let copied = runtime::send(runtime::alloc_instance(runtime::class_named("NSArray")), "initWithArray:copyItems:", &[list, 1]);
        assert_eq!(crate::fw::string::get(array_items(copied)[0]), "x");

        let keys = array_from(&[s("low"), s("high"), s("mid")]);
        let dictionary = runtime::send(runtime::class_named("NSDictionary"), "dictionaryWithObjects:forKeys:", &[array_from(&[n(1), n(9), n(5)]), keys]);
        assert_eq!(number_as_i64(dictionary_get(dictionary, s("mid"))), 5);
        let sorted = runtime::send(dictionary, "keysSortedByValueUsingSelector:", &[runtime::sel("compare:")]);
        let order: Vec<String> = array_items(sorted).into_iter().map(crate::fw::string::get).collect();
        assert_eq!(order, ["low", "mid", "high"]);
        let text = crate::fw::string::get(runtime::send(dictionary_from(&[(s("k"), s("v \"q\""))]), "descriptionInStringsFileFormat", &[]));
        assert_eq!(text, "\"k\" = \"v \\\"q\\\"\";\n");
    }

    /// Index sets hold ranges: a huge range costs nothing, and range edits
    /// split and join them.
    #[test]
    fn index_sets_hold_huge_ranges() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        use crate::fw::string::Range;
        let class = runtime::class_named("NSMutableIndexSet");
        let with_range: extern "C" fn(Class, Sel, Range) -> Id =
            unsafe { std::mem::transmute(runtime::imp_for(class, runtime::sel("indexSetWithIndexesInRange:")).unwrap()) };
        let huge = 1usize << 40;
        let set = with_range(class, 0, Range { location: 5, length: huge });
        assert_eq!(runtime::send(set, "count", &[]), huge);
        assert_eq!(runtime::send(set, "lastIndex", &[]), 5 + huge - 1);
        let edit = |name: &str, location: usize, length: usize| {
            let f: extern "C" fn(Id, Sel, Range) = unsafe { std::mem::transmute(runtime::imp_for(set, runtime::sel(name)).unwrap()) };
            f(set, 0, Range { location, length });
        };
        edit("removeIndexesInRange:", 10, 5);
        assert_eq!(runtime::send(set, "count", &[]), huge - 5);
        assert_eq!(runtime::send(set, "containsIndex:", &[12]) & 0xFF, 0);
        assert_eq!(runtime::send(set, "indexGreaterThanIndex:", &[9]), 15);
        assert_eq!(runtime::send(set, "indexLessThanIndex:", &[15]), 9);
        edit("addIndexesInRange:", 12, 1);
        runtime::send(set, "addIndex:", &[4]);
        assert_eq!(runtime::send(set, "firstIndex", &[]), 4);
        let text = crate::fw::string::get(runtime::send(set, "description", &[]));
        assert!(text.contains("in 3 ranges") && text.contains("(4-9 12 15-"), "{text}");
        edit("removeIndexesInRange:", 0, usize::MAX);
        assert_eq!(runtime::send(set, "count", &[]), 0);
        assert_eq!(runtime::send(set, "firstIndex", &[]), isize::MAX as usize);
    }
}
