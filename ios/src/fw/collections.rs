//! NSArray and NSDictionary (and their mutable forms), plus NSNumber.
//! Arrays: { isa, *mut Vec<Id> }. Dictionaries: { isa, *mut Vec<(key, value)> }.

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
    *items(object) = list.to_vec();
    object
}

pub fn array_items(object: Id) -> Vec<Id> {
    if object == 0 {
        return Vec::new();
    }
    items(object).clone()
}

pub fn dictionary_from(list: &[(Id, Id)]) -> Id {
    let object = runtime::alloc_instance(classes().dictionary);
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
    items(object).push(item);
    object
}

extern "C" fn array_with_objects_count(class: Class, _: Sel, list: *const Id, count: usize) -> Id {
    let object = runtime::alloc_instance(class);
    *items(object) = unsafe { std::slice::from_raw_parts(list, count) }.to_vec();
    object
}

extern "C" fn array_init_with_objects_count(this: Id, _: Sel, list: *const Id, count: usize) -> Id {
    *items(this) = unsafe { std::slice::from_raw_parts(list, count) }.to_vec();
    this
}

extern "C" fn array_with_array(class: Class, _: Sel, other: Id) -> Id {
    let object = runtime::alloc_instance(class);
    *items(object) = array_items(other);
    object
}

extern "C" fn array_init_with_array(this: Id, _: Sel, other: Id) -> Id {
    *items(this) = array_items(other);
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
    *items(object) = array_items(this);
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
    items(this).push(item);
}

extern "C" fn array_insert(this: Id, _: Sel, item: Id, index: usize) {
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
        list.push((key, value));
    }
    this
}

extern "C" fn dict_with_dictionary(class: Class, _: Sel, other: Id) -> Id {
    let object = runtime::alloc_instance(class);
    *pairs(object) = dictionary_pairs(other);
    object
}

extern "C" fn dict_init_with_dictionary(this: Id, _: Sel, other: Id) -> Id {
    *pairs(this) = dictionary_pairs(other);
    this
}

extern "C" fn dict_init_with_dictionary_copy(this: Id, _: Sel, other: Id, copy_items: u8) -> Id {
    let mut list = dictionary_pairs(other);
    if copy_items != 0 {
        for (_, value) in list.iter_mut() {
            *value = runtime::send(*value, "copy", &[]);
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

crate::libsystem::libc::variadic!(ios_dict_with_objects_and_keys, 3, ios_dict_with_objects_and_keys_impl);

#[no_mangle]
extern "C" fn ios_dict_with_objects_and_keys_impl(class: Class, _: Sel, first: Id, va: *const u64) -> Id {
    let list = nil_terminated(first, va);
    let object = runtime::alloc_instance(class);
    let store = pairs(object);
    for pair in list.chunks(2) {
        if let [value, key] = pair {
            store.retain(|(k, _)| !keys_equal(*k, *key));
            store.push((*key, *value));
        }
    }
    object
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

extern "C" fn dict_copy(this: Id, _: Sel, _zone: usize) -> Id {
    dictionary_from(&dictionary_pairs(this))
}

extern "C" fn dict_mutable_copy(this: Id, _: Sel, _zone: usize) -> Id {
    let object = runtime::alloc_instance(classes().mutable_dictionary);
    *pairs(object) = dictionary_pairs(this);
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

pub fn register(_: &mut Registry) {
    let array = define_class("NSArray", "NSObject", 16);
    let mutable_array = define_class("NSMutableArray", "NSArray", 16);
    let dictionary = define_class("NSDictionary", "NSObject", 16);
    let mutable_dictionary = define_class("NSMutableDictionary", "NSDictionary", 16);
    let value = define_class("NSValue", "NSObject", 24);
    let number = define_class("NSNumber", "NSValue", 24);
    let _ = value;
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
        ],
    );
    class_methods(array, &[("arrayWithContentsOfFile:", array_with_contents_of_file as usize)]);
    methods(
        mutable_array,
        &[
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
        ],
    );
    class_methods(
        dictionary,
        &[
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
            ("removeAllObjects", dict_remove_all as usize),
            ("addEntriesFromDictionary:", dict_add_entries as usize),
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
