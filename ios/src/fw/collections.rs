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

/// Lets go of a 32-bit game's objects taken out of a collection (as Apple's
/// release does; one deallocs at the next safe point if nothing else holds
/// it). Pictures are still never let go of (fw/counted.rs): a holder of ours
/// that doesn't retain one would see it vanish.
fn let_go_all(list: &[Id]) {
    list.iter().for_each(|&o| crate::fw::counted::game_release(o));
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

/// A new array handed out as Apple's getters hand out a copy of a list
/// they keep ([[list copy] autorelease]): its hold on a game's objects
/// lasts until the pool drains (our arrays themselves never dealloc).
pub fn array_autoreleased(list: &[Id]) -> Id {
    let object = array_from(list);
    list.iter().for_each(|&o| crate::fw::counted::game_autorelease(o));
    object
}

/// The most entries read from a foreign collection through its primitive
/// methods (see array_items).
const MAX_FOREIGN_ITEMS: usize = 16 * 1024 * 1024;

pub fn array_items(object: Id) -> Vec<Id> {
    if object == 0 {
        return Vec::new();
    }
    if !our_list(object) && answers(object, "objectAtIndex:") {
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
fn keeps_our_storage(object: Id, primitive: &PrimitiveSel, ours: usize) -> bool {
    let sel = *primitive.1.get_or_init(|| runtime::sel(primitive.0));
    runtime::imp_for(object, sel) == Some(ours as runtime::Imp)
}

/// A primitive method's name and its selector, looked up once (these are
/// asked on every collection call, and a lookup takes the runtime lock).
struct PrimitiveSel(&'static str, OnceLock<Sel>);

static COUNT: PrimitiveSel = PrimitiveSel("count", OnceLock::new());
static OBJECT_AT: PrimitiveSel = PrimitiveSel("objectAtIndex:", OnceLock::new());
static OBJECT_FOR_KEY: PrimitiveSel = PrimitiveSel("objectForKey:", OnceLock::new());

/// The object's class, read without a lock (as counted.rs does).
fn isa(object: Id) -> Class {
    if object == 0 { 0 } else { unsafe { *(object as *const usize) } }
}

/// -respondsToSelector:, asked by message (a proxy forwards it).
fn answers(object: Id, selector: &str) -> bool {
    runtime::send(object, "respondsToSelector:", &[runtime::sel(selector) as usize]) & 0xFF != 0
}

/// Whether an array keeps its entries in our storage. Every other method
/// of a game's subclass with its own storage (JSONKit's KV_JKArray) goes
/// through its primitive methods (count, objectAtIndex:, insertObject:atIndex:,
/// removeObjectAtIndex:, replaceObjectAtIndex:withObject:), as Foundation's do.
/// Our own classes are known by their class pointer (no lookup: these run
/// many times a frame); a subclass is ours only while both reading
/// primitives (count and objectAtIndex:) are still ours.
fn our_list(object: Id) -> bool {
    let class = isa(object);
    let ours = classes();
    class == ours.array
        || class == ours.mutable_array
        || (keeps_our_storage(object, &COUNT, array_count as usize) && keeps_our_storage(object, &OBJECT_AT, array_object_at as usize))
}

/// The same for dictionaries (KV_JKDictionary, ApItems): count,
/// objectForKey:, keyEnumerator, setObject:forKey:, removeObjectForKey:.
fn our_table(object: Id) -> bool {
    let class = isa(object);
    let ours = classes();
    class == ours.dictionary
        || class == ours.mutable_dictionary
        || (keeps_our_storage(object, &COUNT, dict_count as usize) && keeps_our_storage(object, &OBJECT_FOR_KEY, dict_object_for_key as usize))
}

fn count_of(object: Id) -> usize {
    runtime::send(object, "count", &[])
}

/// A dictionary's keys (through keyEnumerator for one that isn't ours).
fn dictionary_keys(object: Id) -> Vec<Id> {
    if object == 0 {
        return Vec::new();
    }
    if !our_table(object) && answers(object, "keyEnumerator") {
        let keys = runtime::send(object, "keyEnumerator", &[]);
        return std::iter::from_fn(|| Some(runtime::send(keys, "nextObject", &[])).filter(|&key| key != 0)).collect();
    }
    hash_order(&pairs(object).iter().map(|(key, _)| *key).collect::<Vec<_>>())
}

/// A dictionary's value for a key (through objectForKey: for one that isn't ours).
fn table_get(object: Id, key: Id) -> Id {
    if object != 0 && !our_table(object) {
        return runtime::send(object, "objectForKey:", &[key]);
    }
    dictionary_get(object, key)
}

/// Raises the Foundation exception `name` with `reason` (for a 32-bit
/// game's message, the method then returns and the game gets the exception).
pub(crate) fn raise_named(name: &str, reason: String) {
    let exception = runtime::send(
        runtime::class_named("NSException"),
        "exceptionWithName:reason:userInfo:",
        &[crate::fw::string::make(name), crate::fw::string::make(&reason), 0],
    );
    crate::fw::exception::raise(exception);
}

/// NSRangeException for an index past an array's end, worded as Apple's.
fn out_of_range(method: &str, index: usize, count: usize) {
    let bounds = if count == 0 { "for empty array".to_string() } else { format!("[0 .. {}]", count - 1) };
    raise_named("NSRangeException", format!("*** -[NSArray {method}]: index {index} beyond bounds {bounds}"));
}

/// NSInvalidArgumentException for a nil entry put into a collection.
fn nil_entry(method: &str) {
    raise_named("NSInvalidArgumentException", format!("*** {method}: object cannot be nil"));
}

/// Removes the entries `gone` picks (by index and entry), through the
/// primitive removeObjectAtIndex: for an array that isn't ours.
fn remove_where(this: Id, mut gone: impl FnMut(usize, Id) -> bool) {
    if our_list(this) {
        // Read by index, not through a borrow of the list: `gone` may run
        // game code (-isEqual:) that touches this same array.
        let (mut kept, mut removed) = (Vec::new(), Vec::new());
        let mut index = 0;
        while let Some(&item) = items(this).get(index) {
            if gone(index, item) {
                removed.push(item);
            } else {
                kept.push(item);
            }
            index += 1;
        }
        *items(this) = kept;
        let_go_all(&removed);
    } else {
        let list = array_items(this);
        let doomed: Vec<usize> = list.iter().enumerate().filter(|&(index, &item)| gone(index, item)).map(|(index, _)| index).collect();
        for index in doomed.into_iter().rev() {
            runtime::send(this, "removeObjectAtIndex:", &[index]);
        }
    }
}

/// Appends entries, through insertObject:atIndex: for an array that isn't ours.
fn append_all(this: Id, extra: Vec<Id>) {
    if our_list(this) {
        keep_all(&extra);
        items(this).extend(extra);
    } else {
        for item in extra {
            runtime::send(this, "insertObject:atIndex:", &[item, count_of(this)]);
        }
    }
}

/// Makes an array hold `list` in that order (the same entries, sorted).
fn put_back(this: Id, list: Vec<Id>) {
    if our_list(this) {
        *items(this) = list;
    } else {
        for (index, item) in list.into_iter().enumerate() {
            runtime::send(this, "replaceObjectAtIndex:withObject:", &[index, item]);
        }
    }
}

/// A stable merge sort, as CFSortIndexes does: it finishes on any
/// comparator, even one that isn't a total order (Rust's sort may panic on
/// one, which here would end the app). `order(a, b)` is asked with a from
/// the earlier run, as Apple's merge asks.
fn merge_sort<T: Copy>(list: Vec<T>, order: &mut dyn FnMut(T, T) -> std::cmp::Ordering) -> Vec<T> {
    if list.len() < 2 {
        return list;
    }
    let mut right = list;
    let left = right.drain(..right.len() / 2).collect();
    let (left, right) = (merge_sort(left, order), merge_sort(right, order));
    let mut merged = Vec::with_capacity(left.len() + right.len());
    let (mut i, mut j) = (0, 0);
    while i < left.len() && j < right.len() {
        if order(left[i], right[j]) == std::cmp::Ordering::Greater {
            merged.push(right[j]);
            j += 1;
        } else {
            merged.push(left[i]);
            i += 1;
        }
    }
    merged.extend_from_slice(&left[i..]);
    merged.extend_from_slice(&right[j..]);
    merged
}

/// An NSComparisonResult as returned: a 32-bit game's -1 arrives as
/// 0xFFFFFFFF (its upper half empty), a 64-bit one's sign-extended.
fn comparison(result: usize) -> std::cmp::Ordering {
    let value = result as i64;
    let value = if value >> 32 == 0 { value as u32 as i32 as i64 } else { value };
    value.cmp(&0)
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
    // Our own strings (most keys) are compared by their text in place;
    // anything else is asked -isEqual:, as NSDictionary asks.
    // A key's own isEqual: may change the looked-up string: its text is read again after one.
    let mut memo = Vec::new();
    let mut key_text = crate::fw::string::plain_units(key);
    keys.into_iter().find(|&k| match key_text.zip(crate::fw::string::plain_units_memo(k, &mut memo)) {
        Some((a, b)) => unsafe { *a == *b },
        None => {
            let same = keys_equal(k, key);
            key_text = crate::fw::string::plain_units(key);
            same
        }
    })
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

/// A dictionary's entries as Apple's CFBasicHash holds them: the order its
/// hash table enumerates (see `hash_order`).
pub fn dictionary_pairs(object: Id) -> Vec<(Id, Id)> {
    if object == 0 {
        return Vec::new();
    }
    if !our_table(object) && answers(object, "keyEnumerator") {
        let keys = runtime::send(object, "keyEnumerator", &[]);
        return std::iter::from_fn(|| Some(runtime::send(keys, "nextObject", &[])).filter(|&key| key != 0))
            .map(|key| (key, runtime::send(object, "objectForKey:", &[key])))
            .collect();
    }
    let list = pairs(object).clone();
    let ordered = hash_order(&list.iter().map(|(key, _)| *key).collect::<Vec<_>>());
    ordered.into_iter().map(|key| list.iter().find(|(k, _)| *k == key).copied().unwrap_or((key, 0))).collect()
}

/// Apple's CFBasicHash growth table (CoreFoundation 6.1, 0x164828): the most
/// entries a table may hold before it grows (0, 3, 6, 11, 19, 32, ...), and
/// the bucket count of that table (0, 3, 7, 13, 23, 41, ...). A 7-bucket
/// table holds 6 entries; the 7th grows it to 13 buckets, as
/// CFBasicHashAddValue (0xc835) decides.
const HASH_TABLE_SIZES: &[(usize, usize)] = &[
    (0, 0), (3, 3), (6, 7), (11, 13), (19, 23), (32, 41), (52, 71), (85, 127), (118, 191), (155, 251),
    (237, 383), (390, 631), (672, 1087), (1065, 1723), (1732, 2803), (2795, 4523), (4543, 7351),
    (7391, 11959), (12019, 19447), (19302, 31231), (31324, 50683), (50629, 81919), (81956, 132607),
    (132580, 214519), (214215, 346607), (346784, 561109), (561026, 907759), (907847, 1468927),
    (1468567, 2376191), (2376414, 3845119), (3844982, 6221311), (6221390, 10066421),
    (10066379, 16287743), (16287773, 26354171), (26354132, 42641881), (42641916, 68996069),
    (68996399, 111638519), (111638327, 180634607), (180634415, 292272623), (292272755, 472907251),
];

/// The hash CFHash gives a key: CFString's and CFNumber's own, another
/// object's -hash (CFHash asks an Objective-C object's -hash).
fn entry_hash(key: Id) -> u32 {
    if crate::fw::string::is_string(key) {
        crate::fw::string::cf_hash(key)
    } else if is_number(key) {
        cf_number_hash(key)
    } else {
        runtime::send(key, "hash", &[]) as u32
    }
}

/// `keys` in the order Apple's CFBasicHash enumerates them. A key's bucket
/// is its hash modulo the table's bucket count; a taken bucket moves on one
/// (CFDictionary is made with CFBasicHash flags 0x2001 and CFSet with
/// 0x2000: both use linear probing, unlike the double hashing flags), and
/// enumeration walks the buckets from 0. Apple keeps the table's size when
/// entries are removed or a capacity was asked for (dictionaryWithCapacity:);
/// ours is worked out from the count now, so a dictionary that shrank or was
/// made with a capacity hint can come out in another order than on the phone.
pub(crate) fn hash_order(keys: &[Id]) -> Vec<Id> {
    let Some(&(_, buckets_used)) = HASH_TABLE_SIZES.iter().find(|&&(most, _)| most >= keys.len()) else { return keys.to_vec() };
    if buckets_used == 0 {
        return keys.to_vec();
    }
    let mut buckets = vec![usize::MAX; buckets_used];
    for (index, &key) in keys.iter().enumerate() {
        let mut bucket = entry_hash(key) as usize % buckets_used;
        while buckets[bucket] != usize::MAX {
            bucket = (bucket + 1) % buckets_used;
        }
        buckets[bucket] = index;
    }
    buckets.into_iter().filter(|&index| index != usize::MAX).map(|index| keys[index]).collect()
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

extern "C" fn array_with_objects_count(class: Class, s: Sel, list: *const Id, count: usize) -> Id {
    array_init_with_objects_count(runtime::alloc_instance(class), s, list, count)
}

/// A nil entry raises NSInvalidArgumentException, as Apple's does.
extern "C" fn array_init_with_objects_count(this: Id, _: Sel, list: *const Id, count: usize) -> Id {
    let entries = if count == 0 { Vec::new() } else { unsafe { std::slice::from_raw_parts(list, count) }.to_vec() };
    if let Some(index) = entries.iter().position(|&item| item == 0) {
        raise_named("NSInvalidArgumentException", format!("*** -[NSArray initWithObjects:count:]: attempt to insert nil object from objects[{index}]"));
        return 0;
    }
    keep_all(&entries);
    *items(this) = entries;
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
    list
}

#[no_mangle]
extern "C" fn ios_array_with_objects_impl(class: Class, _: Sel, first: Id, va: *const u64) -> Id {
    let object = runtime::alloc_instance(class);
    *items(object) = nil_terminated(first, va);
    keep_all(items(object));
    object
}

#[no_mangle]
extern "C" fn ios_array_init_with_objects_impl(this: Id, _: Sel, first: Id, va: *const u64) -> Id {
    *items(this) = nil_terminated(first, va);
    keep_all(items(this));
    this
}

extern "C" fn array_count(this: Id, _: Sel) -> usize {
    items(this).len()
}

/// Past the end raises NSRangeException (a game's @catch can take it).
extern "C" fn array_object_at(this: Id, _: Sel, index: usize) -> Id {
    match items(this).get(index) {
        Some(&item) => item,
        None => {
            out_of_range("objectAtIndex:", index, items(this).len());
            0
        }
    }
}

/// The entry at `index` of the first or last kind (nil for an empty array).
fn end_entry(this: Id, last: bool) -> Id {
    if our_list(this) {
        let list = items(this);
        return if last { list.last() } else { list.first() }.copied().unwrap_or(0);
    }
    match count_of(this) {
        0 => 0,
        count => runtime::send(this, "objectAtIndex:", &[if last { count - 1 } else { 0 }]),
    }
}

extern "C" fn array_first(this: Id, _: Sel) -> Id {
    end_entry(this, false)
}

extern "C" fn array_last(this: Id, _: Sel) -> Id {
    end_entry(this, true)
}

extern "C" fn array_contains(this: Id, s: Sel, item: Id) -> bool {
    array_index_of(this, s, item) != NOT_FOUND
}

extern "C" fn array_index_of(this: Id, _: Sel, item: Id) -> usize {
    position_of(this, |i| keys_equal(i, item))
}

/// The first index whose entry `hit` picks (NSNotFound for none); our own
/// arrays are read in place, by index (`hit` may run game code).
fn position_of(this: Id, mut hit: impl FnMut(Id) -> bool) -> usize {
    if !our_list(this) {
        return array_items(this).into_iter().position(hit).unwrap_or(NOT_FOUND);
    }
    let mut index = 0;
    while let Some(&entry) = items(this).get(index) {
        if hit(entry) {
            return index;
        }
        index += 1;
    }
    NOT_FOUND
}

/// An immutable array's copy is the array itself, as Apple's is.
extern "C" fn array_copy(this: Id, _: Sel, _zone: usize) -> Id {
    if runtime::lock().class_of(this) == classes().array {
        return crate::fw::counted::retain(this);
    }
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
        let start = (*state)[0];
        let ours = our_list(this);
        let total = if ours { items(this).len() } else { count_of(this) };
        if start >= total {
            return 0;
        }
        let count = (total - start).min(length.max(1));
        for index in 0..count {
            *buffer.add(index) = if ours { items(this)[start + index] } else { runtime::send(this, "objectAtIndex:", &[start + index]) };
        }
        (*state)[0] = start + count;
        (*state)[1] = buffer as usize;
        (*state)[2] = (&(*state)[3]) as *const usize as usize;
        count
    }
}

/// -getObjects:range: copies a range of entries out (past the end raises).
extern "C" fn array_get_objects(this: Id, _: Sel, out: *mut Id, range: crate::fw::string::Range) {
    let list = array_items(this);
    let end = range.location.saturating_add(range.length);
    if end > list.len() {
        out_of_range("getObjects:range:", end.saturating_sub(1), list.len());
        return;
    }
    if !out.is_null() {
        unsafe { std::ptr::copy_nonoverlapping(list[range.location..end].as_ptr(), out, range.length) };
    }
}

extern "C" fn array_add(this: Id, _: Sel, item: Id) {
    if item == 0 {
        return nil_entry("-[NSMutableArray addObject:]");
    }
    if our_list(this) {
        crate::fw::counted::keep(item);
        items(this).push(item);
    } else {
        runtime::send(this, "insertObject:atIndex:", &[item, count_of(this)]);
    }
}

extern "C" fn array_insert(this: Id, _: Sel, item: Id, index: usize) {
    if item == 0 {
        return nil_entry("-[NSMutableArray insertObject:atIndex:]");
    }
    let list = items(this);
    if index > list.len() {
        return out_of_range("insertObject:atIndex:", index, list.len());
    }
    crate::fw::counted::keep(item);
    list.insert(index, item);
}

extern "C" fn array_remove_at(this: Id, _: Sel, index: usize) {
    let list = items(this);
    if index >= list.len() {
        return out_of_range("removeObjectAtIndex:", index, list.len());
    }
    let removed = list.remove(index);
    let_go_all(&[removed]);
}

/// Removes every entry equal to `item`.
extern "C" fn array_remove_object(this: Id, _: Sel, item: Id) {
    remove_where(this, |_, i| keys_equal(i, item));
}

/// Removes only `item` itself (by address), not entries equal to it.
extern "C" fn array_remove_identical(this: Id, _: Sel, item: Id) {
    remove_where(this, |_, i| i == item);
}

extern "C" fn array_remove_all(this: Id, _: Sel) {
    if our_list(this) {
        let removed = std::mem::take(items(this));
        let_go_all(&removed);
    } else {
        remove_where(this, |_, _| true);
    }
}

extern "C" fn array_remove_last(this: Id, _: Sel) {
    if our_list(this) {
        let removed = items(this).pop();
        let_go_all(removed.as_slice());
    } else if let Some(last) = count_of(this).checked_sub(1) {
        runtime::send(this, "removeObjectAtIndex:", &[last]);
    }
}

extern "C" fn array_replace(this: Id, _: Sel, index: usize, item: Id) {
    if item == 0 {
        return nil_entry("-[NSMutableArray replaceObjectAtIndex:withObject:]");
    }
    let list = items(this);
    match list.get_mut(index) {
        Some(slot) => {
            crate::fw::counted::keep(item);
            let old = std::mem::replace(slot, item);
            let_go_all(&[old]);
        }
        None => out_of_range("replaceObjectAtIndex:withObject:", index, list.len()),
    }
}

/// list[index] = item: replaces, or adds at index == count.
extern "C" fn array_set_subscript(this: Id, s: Sel, item: Id, index: usize) {
    if index == count_of(this) {
        array_add(this, s, item);
    } else {
        runtime::send(this, "replaceObjectAtIndex:withObject:", &[index, item]);
    }
}

/// Adds (and keeps) every entry of another array.
extern "C" fn array_add_all(this: Id, _: Sel, other: Id) {
    append_all(this, array_items(other));
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
    comparison(runtime::send(a, &name, &[b]))
}

fn sorted(list: Vec<Id>, mut order: impl FnMut(Id, Id) -> std::cmp::Ordering) -> Vec<Id> {
    merge_sort(list, &mut order)
}

extern "C" fn array_sorted_selector(this: Id, _: Sel, selector: Sel) -> Id {
    array_from(&sorted(array_items(this), |a, b| compare_with_selector(a, b, selector)))
}

extern "C" fn array_sort_selector(this: Id, _: Sel, selector: Sel) {
    put_back(this, sorted(array_items(this), |a, b| compare_with_selector(a, b, selector)));
}

fn compare_with_block(a: Id, b: Id, block: usize) -> std::cmp::Ordering {
    let header = block as *const crate::libsystem::mach::BlockHeader;
    let invoke: extern "C" fn(usize, Id, Id) -> usize = unsafe { std::mem::transmute((*header).invoke) };
    comparison(invoke(block, a, b))
}

extern "C" fn array_sorted_comparator(this: Id, _: Sel, block: usize) -> Id {
    array_from(&sorted(array_items(this), |a, b| compare_with_block(a, b, block)))
}

extern "C" fn array_sort_comparator(this: Id, _: Sel, block: usize) {
    put_back(this, sorted(array_items(this), |a, b| compare_with_block(a, b, block)));
}

extern "C" fn array_sorted_function(this: Id, _: Sel, function: usize, context: usize) -> Id {
    let f: extern "C" fn(Id, Id, usize) -> usize = unsafe { std::mem::transmute(function) };
    array_from(&sorted(array_items(this), |a, b| comparison(f(a, b, context))))
}

extern "C" fn array_sort_function(this: Id, _: Sel, function: usize, context: usize) {
    let f: extern "C" fn(Id, Id, usize) -> usize = unsafe { std::mem::transmute(function) };
    put_back(this, sorted(array_items(this), |a, b| comparison(f(a, b, context))));
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
    position_of(this, |i| i == item)
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
    remove_where(this, |_, i| remove.iter().any(|&r| keys_equal(i, r)));
}

extern "C" fn array_exchange(this: Id, _: Sel, a: usize, b: usize) {
    let mut list = array_items(this);
    if let Some(&past) = [a, b].iter().find(|&&i| i >= list.len()) {
        return out_of_range("exchangeObjectAtIndex:withObjectAtIndex:", past, list.len());
    }
    list.swap(a, b);
    if our_list(this) {
        *items(this) = list;
    } else {
        runtime::send(this, "replaceObjectAtIndex:withObject:", &[a, list[a]]);
        runtime::send(this, "replaceObjectAtIndex:withObject:", &[b, list[b]]);
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

/// A nil key or value raises NSInvalidArgumentException, as Apple's does.
extern "C" fn dict_init_objects_keys_count(this: Id, _: Sel, values: *const Id, keys: *const Id, count: usize) -> Id {
    for index in 0..count {
        let (key, value) = unsafe { (*keys.add(index), *values.add(index)) };
        if key == 0 || value == 0 {
            let what = if value == 0 { "object" } else { "key" };
            raise_named("NSInvalidArgumentException", format!("*** -[NSDictionary initWithObjects:forKeys:count:]: attempt to insert nil {what} from objects[{index}]"));
            return 0;
        }
    }
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
            keep_all(&[*key, *value]);
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
    array_from(&dictionary_keys(this))
}

extern "C" fn dict_all_values(this: Id, _: Sel) -> Id {
    let values: Vec<Id> = dictionary_pairs(this).into_iter().map(|(_, v)| v).collect();
    array_from(&values)
}

/// -valueForKey: is -objectForKey:, except that a key starting with "@"
/// asks NSObject's key-value coding for the rest ("@count").
extern "C" fn dict_value_for_key(this: Id, _: Sel, key: Id) -> Id {
    if crate::fw::string::is_string(key) {
        if let Some(rest) = crate::fw::string::get(key).strip_prefix('@') {
            let sel = runtime::sel("valueForKey:");
            let object_class = runtime::class_named("NSObject");
            let found = lock().lookup(object_class, sel);
            if let Some(imp) = found {
                let value_for_key: extern "C" fn(Id, Sel, Id) -> Id = unsafe { std::mem::transmute(imp) };
                return value_for_key(this, sel, crate::fw::string::make(rest));
            }
        }
    }
    runtime::send(this, "objectForKey:", &[key])
}

/// Stores a pair as it is (NSCache doesn't copy its keys).
fn store_pair(this: Id, value: Id, key: Id) {
    let found = find_key(this, key);
    keep_all(&[key, value]);
    let list = pairs(this);
    match found.and_then(|f| list.iter_mut().find(|(k, _)| *k == f)) {
        Some(entry) => {
            // The value it replaces is let go of (the old key stays).
            let old = std::mem::replace(&mut entry.1, value);
            let_go_all(&[old, key]);
        }
        None => list.push((key, value)),
    }
}

/// Every key is copied (NSCopying), as Apple's does; a nil key or value raises.
extern "C" fn dict_set(this: Id, _: Sel, value: Id, key: Id) {
    if value == 0 || key == 0 {
        let what = if value == 0 { "object" } else { "key" };
        return raise_named("NSInvalidArgumentException", format!("*** -[NSMutableDictionary setObject:forKey:]: {what} cannot be nil"));
    }
    let key = if crate::fw::string::is_string(key) || runtime::responds(key, "copyWithZone:") {
        runtime::send(key, "copyWithZone:", &[0])
    } else {
        // Apple raises here (the key doesn't adopt NSCopying); kept as it is.
        static WARNED: std::sync::Once = std::sync::Once::new();
        WARNED.call_once(|| crate::log!("NSMutableDictionary: a {} key can't be copied; kept as it is", crate::fw::string::describe_class_of(key)));
        key
    };
    store_pair(this, value, key);
}

extern "C" fn dict_remove(this: Id, _: Sel, key: Id) {
    if let Some(found) = find_key(this, key) {
        let list = pairs(this);
        if let Some(at) = list.iter().position(|(k, _)| *k == found) {
            let (key, value) = list.remove(at);
            let_go_all(&[key, value]);
        }
    }
}

extern "C" fn dict_remove_keys(this: Id, _: Sel, keys: Id) {
    for key in array_items(keys) {
        runtime::send(this, "removeObjectForKey:", &[key]);
    }
}

extern "C" fn dict_remove_all(this: Id, _: Sel) {
    if our_table(this) {
        let removed = std::mem::take(pairs(this));
        removed.iter().for_each(|&(key, value)| let_go_all(&[key, value]));
    } else {
        for key in dictionary_keys(this) {
            runtime::send(this, "removeObjectForKey:", &[key]);
        }
    }
}

/// setValue:forKey: on a dictionary: nil removes the key.
extern "C" fn dict_set_value(this: Id, _: Sel, value: Id, key: Id) {
    if value == 0 {
        runtime::send(this, "removeObjectForKey:", &[key]);
    } else {
        runtime::send(this, "setObject:forKey:", &[value, key]);
    }
}

extern "C" fn dict_add_entries(this: Id, _: Sel, other: Id) {
    for (key, value) in dictionary_pairs(other) {
        runtime::send(this, "setObject:forKey:", &[value, key]);
    }
}

extern "C" fn dict_set_dictionary(this: Id, s: Sel, other: Id) {
    let entries = if other == 0 { Vec::new() } else { dictionary_pairs(other) };
    dict_remove_all(this, s);
    for (key, value) in entries {
        runtime::send(this, "setObject:forKey:", &[value, key]);
    }
}

/// An immutable dictionary's copy is the dictionary itself, as Apple's is.
extern "C" fn dict_copy(this: Id, _: Sel, _zone: usize) -> Id {
    if runtime::lock().class_of(this) == classes().dictionary {
        return crate::fw::counted::retain(this);
    }
    dictionary_from(&dictionary_pairs(this))
}

/// -getObjects:andKeys: copies the values and keys out, in the same order.
extern "C" fn dict_get_objects_and_keys(this: Id, _: Sel, values: *mut Id, keys: *mut Id) {
    for (index, (key, value)) in dictionary_pairs(this).into_iter().enumerate() {
        unsafe {
            if !values.is_null() {
                *values.add(index) = value;
            }
            if !keys.is_null() {
                *keys.add(index) = key;
            }
        }
    }
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
        let start = (*state)[0];
        let keys = dictionary_keys(this);
        let total = keys.len();
        if start >= total {
            return 0;
        }
        let count = (total - start).min(length.max(1));
        for index in 0..count {
            *buffer.add(index) = keys[start + index];
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
            let v = table_get(this, k);
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
    a.len() == b.len() && a.iter().all(|&(k, v)| keys_equal(table_get(other, k), v))
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

extern "C" fn cache_set(this: Id, _: Sel, value: Id, key: Id) {
    with_cache(|| store_pair(this, value, key))
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

/// Our keyEnumerator reads our own storage (a subclass that keeps its own
/// overrides it; asking keyEnumerator here would call this again).
extern "C" fn dict_key_enumerator(this: Id, _: Sel) -> Id {
    enumerator(hash_order(&pairs(this).iter().map(|(key, _)| *key).collect::<Vec<_>>()))
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

/// The bytes at `bytes` of the C type `kind` (its size from the encoding, in
/// the memory's layout: in a 32-bit game's, long and pointers are 4 bytes).
fn bytes_of_type(bytes: *const u8, kind: *const std::ffi::c_char) -> (String, Vec<u8>) {
    let kind = if kind.is_null() { String::new() } else { unsafe { runtime::cstr(kind as usize) }.to_string() };
    let size = crate::fw::system::type_size(&kind, crate::fw::system::guest_memory_at(bytes as usize).is_some()).0;
    let held = if bytes.is_null() { vec![0; size] } else { unsafe { std::slice::from_raw_parts(bytes, size) }.to_vec() };
    (kind, held)
}

/// +valueWithBytes:objCType: / +value:withObjCType:
extern "C" fn value_with_bytes_type(class: Class, _: Sel, bytes: *const u8, kind: *const std::ffi::c_char) -> Id {
    let (kind, held) = bytes_of_type(bytes, kind);
    value_with_bytes(class, &kind, &held)
}

extern "C" fn value_init_with_bytes_type(this: Id, _: Sel, bytes: *const u8, kind: *const std::ffi::c_char) -> Id {
    let (kind, held) = bytes_of_type(bytes, kind);
    hold_struct(this, &kind, &held);
    this
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
        // A 32-bit game's pointer is 4 bytes.
        None if crate::fw::system::guest_memory_at(out as usize).is_some() => unsafe { (out as *mut u32).write_unaligned(value_pointer(this, s) as u32) },
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

// ----- NSNumber: { isa, kind | C type << 8, value bits } -----
// The C type the number was made from ('f', 'i', 'Q'...) is kept above the
// kind for -objCType; 0 means the kind's own ('d', 'c', 'q').

const KIND_INT: usize = 0;
const KIND_FLOAT: usize = 1;
const KIND_BOOL: usize = 2;

/// A number of `kind` made from the C type `code`.
fn typed_number(kind: usize, code: u8, bits: u64) -> Id {
    number(kind | (code as usize) << 8, bits)
}

/// The C type an integer keeps: unsigned values past i64::MAX are 'Q'.
fn unsigned_code(value: u64) -> u8 {
    if value > i64::MAX as u64 { b'Q' } else { b'q' }
}

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
    unsafe { (*((object + 8) as *const usize) & 0xFF, *((object + 16) as *const u64)) }
}

/// The C type the number was made from, as -objCType gives it.
fn number_code(object: Id) -> u8 {
    match unsafe { *((object + 8) as *const usize) } >> 8 & 0xFF {
        0 => match number_parts(object).0 {
            KIND_FLOAT => b'd',
            KIND_BOOL => b'c',
            _ => b'q',
        },
        code => code as u8,
    }
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
    if kind == KIND_FLOAT {
        f64::from_bits(bits)
    } else if number_code(object) == b'Q' {
        bits as f64
    } else {
        bits as i64 as f64
    }
}

pub fn number_as_i64(object: Id) -> i64 {
    let (kind, bits) = number_parts(object);
    if kind == KIND_FLOAT { f64::from_bits(bits) as i64 } else { bits as i64 }
}

/// Apple's __CFNumberHash (CoreFoundation 6.1, 0xddc0): an 8-, 16- or
/// 32-bit number is its magnitude times 0x9E3779B1; anything else is taken
/// as a double, rounded, and the fraction times 2^32-1 added to the whole
/// part modulo 2^32-1 times 0x9E3779B1.
fn cf_number_hash(object: Id) -> u32 {
    match number_code(object) {
        // kCFNumberSInt8/16/32Type (the C types and 64-bit ones take the
        // double path in CFNumberHash).
        b'c' | b'i' | b's' => {
            let value = number_as_i64(object) as i32;
            let sign = value >> 31;
            (value.wrapping_add(sign) ^ sign).wrapping_mul(0x9e_3779_b1u32 as i32) as u32
        }
        _ => {
            let value = number_as_f64(object).abs();
            let rounded = (value + 0.5).floor();
            let fraction = ((value - rounded) * 4_294_967_295.0) as i64 as i32;
            let whole = (rounded % 4_294_967_295.0) as i64 as i32;
            whole.wrapping_mul(0x9e_3779_b1u32 as i32).wrapping_add(fraction) as u32
        }
    }
}

extern "C" fn number_with_int(_: Class, _: Sel, value: i32) -> Id {
    typed_number(KIND_INT, b'i', value as i64 as u64)
}
extern "C" fn number_with_unsigned_int(_: Class, _: Sel, value: u32) -> Id {
    number_int(value as i64)
}
extern "C" fn number_with_long(_: Class, _: Sel, value: i64) -> Id {
    number_int(value)
}
/// numberWithShort: 's'; CF keeps an unsigned short as an int ('i').
extern "C" fn number_with_short(_: Class, _: Sel, value: i16) -> Id {
    typed_number(KIND_INT, b's', value as i64 as u64)
}
extern "C" fn number_with_unsigned_short(_: Class, _: Sel, value: u16) -> Id {
    typed_number(KIND_INT, b'i', value as u64)
}
/// numberWithChar: 'c'; CF keeps an unsigned char as a short ('s').
extern "C" fn number_with_char(_: Class, _: Sel, value: i8) -> Id {
    typed_number(KIND_INT, b'c', value as i64 as u64)
}
extern "C" fn number_with_unsigned_char(_: Class, _: Sel, value: u8) -> Id {
    typed_number(KIND_INT, b's', value as u64)
}
extern "C" fn number_with_bool(_: Class, _: Sel, value: u8) -> Id {
    number_bool(value != 0)
}
extern "C" fn number_with_float(_: Class, _: Sel, value: f32) -> Id {
    typed_number(KIND_FLOAT, b'f', (value as f64).to_bits())
}
extern "C" fn number_with_double(_: Class, _: Sel, value: f64) -> Id {
    number_float(value)
}
extern "C" fn number_init_int(this: Id, _: Sel, value: i32) -> Id {
    unsafe {
        *((this + 8) as *mut usize) = KIND_INT | (b'i' as usize) << 8;
        *((this + 16) as *mut i64) = value as i64;
    }
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
        *((this + 8) as *mut usize) = KIND_FLOAT | (b'f' as usize) << 8;
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
    cf_number_hash(this) as usize
}
extern "C" fn number_compare(this: Id, _: Sel, other: Id) -> isize {
    let (a, b) = (number_as_f64(this), number_as_f64(other));
    if a < b { -1 } else if a > b { 1 } else { 0 }
}
/// CFNumber's text: a floating-point number as "%.17g" (NaN and the
/// infinities by name), an unsigned one past i64::MAX unsigned.
extern "C" fn number_string_value(this: Id, _: Sel) -> Id {
    let (kind, bits) = number_parts(this);
    let text = match kind {
        KIND_FLOAT => float_text(f64::from_bits(bits)),
        _ if number_code(this) == b'Q' => bits.to_string(),
        _ => format!("{}", bits as i64),
    };
    crate::fw::string::make(&text)
}

fn float_text(value: f64) -> String {
    if value.is_nan() {
        return "nan".into();
    }
    if value.is_infinite() {
        return if value > 0.0 { "+infinity" } else { "-infinity" }.into();
    }
    let mut buffer = [0u8; 64];
    let written = unsafe { libc::snprintf(buffer.as_mut_ptr() as *mut libc::c_char, buffer.len(), c"%.17g".as_ptr(), value) };
    String::from_utf8_lossy(&buffer[..(written.max(0) as usize).min(buffer.len() - 1)]).into_owned()
}
extern "C" fn return_self(this: Id, _: Sel, _zone: usize) -> Id {
    this
}
extern "C" fn number_with_unsigned_long(_: Class, _: Sel, value: u64) -> Id {
    typed_number(KIND_INT, unsigned_code(value), value)
}
extern "C" fn number_init_unsigned_long_long(this: Id, _: Sel, value: u64) -> Id {
    unsafe {
        *((this + 8) as *mut usize) = KIND_INT | (unsigned_code(value) as usize) << 8;
        *((this + 16) as *mut u64) = value;
    }
    this
}
extern "C" fn number_unsigned_short_value(this: Id, _: Sel) -> u16 {
    number_as_i64(this) as u16
}
/// The C type the number was made from, as Apple reports it.
extern "C" fn number_objc_type(this: Id, _: Sel) -> *const std::ffi::c_char {
    match number_code(this) {
        b'f' => c"f".as_ptr(),
        b'd' => c"d".as_ptr(),
        b'c' => c"c".as_ptr(),
        b's' => c"s".as_ptr(),
        b'i' => c"i".as_ptr(),
        b'Q' => c"Q".as_ptr(),
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
        out_of_range("objectsAtIndexes:", last, list.len());
        return 0;
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
    let count = count_of(this);
    if let Some(last) = remove.last().filter(|&last| last >= count) {
        return out_of_range("removeObjectsAtIndexes:", last, count);
    }
    remove_where(this, |index, _| remove.contains(index));
}

extern "C" fn array_remove_range(this: Id, _: Sel, range: crate::fw::string::Range) {
    let (start, end) = (range.location, range.location.saturating_add(range.length));
    let count = count_of(this);
    if end > count {
        return out_of_range("removeObjectsInRange:", end - 1, count);
    }
    remove_where(this, |index, _| (start..end).contains(&index));
}

extern "C" fn array_set_array(this: Id, s: Sel, other: Id) {
    let list = array_items(other);
    if our_list(this) {
        set_array_items(this, list);
    } else {
        array_remove_all(this, s);
        append_all(this, list);
    }
}

extern "C" fn dict_with_objects_for_keys(class: Class, s: Sel, values: Id, keys: Id) -> Id {
    dict_init_objects_for_keys(runtime::alloc_instance(class), s, values, keys)
}

extern "C" fn dict_init_objects_for_keys(this: Id, s: Sel, values: Id, keys: Id) -> Id {
    let (values, keys) = (array_items(values), array_items(keys));
    if values.len() != keys.len() {
        raise_named(
            "NSInvalidArgumentException",
            format!("*** -[NSDictionary initWithObjects:forKeys:]: count of objects ({}) differs from count of keys ({})", values.len(), keys.len()),
        );
        return 0;
    }
    dict_init_objects_keys_count(this, s, values.as_ptr(), keys.as_ptr(), keys.len())
}

fn keys_sorted_by_value(this: Id, mut order: impl FnMut(Id, Id) -> std::cmp::Ordering) -> Id {
    let entries = merge_sort(dictionary_pairs(this), &mut |a: (Id, Id), b: (Id, Id)| order(a.1, b.1));
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
            ("valueWithBytes:objCType:", value_with_bytes_type as usize),
            ("value:withObjCType:", value_with_bytes_type as usize),
        ],
    );
    methods(
        value,
        &[
            ("pointerValue", value_pointer as usize),
            ("nonretainedObjectValue", value_pointer as usize),
            ("getValue:", value_get as usize),
            ("initWithBytes:objCType:", value_init_with_bytes_type as usize),
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
            ("removeObjectIdenticalTo:", array_remove_identical as usize),
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
            ("getObjects:range:", array_get_objects as usize),
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
            ("setObject:atIndexedSubscript:", array_set_subscript as usize),
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
            ("valueForKey:", dict_value_for_key as usize),
            ("getObjects:andKeys:", dict_get_objects_and_keys as usize),
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
            ("numberWithUnsignedLongLong:", number_with_unsigned_long as usize),
            ("numberWithUnsignedInteger:", number_with_unsigned_long as usize),
            ("numberWithUnsignedLong:", number_with_unsigned_long as usize),
            ("numberWithShort:", number_with_short as usize),
            ("numberWithUnsignedShort:", number_with_unsigned_short as usize),
            ("numberWithChar:", number_with_char as usize),
            ("numberWithUnsignedChar:", number_with_unsigned_char as usize),
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

    static PICKY_ASKED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    extern "C" fn picky_is_equal(_this: Id, _: Sel, _other: Id) -> bool {
        PICKY_ASKED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        true
    }

    /// Our own string keys are compared by their text in place (SpellCraft's
    /// screen changes spent ~30 ms sending -isEqual: for every key); a key
    /// class with its own -isEqual: is still asked, as NSDictionary asks it.
    #[test]
    fn string_keys_compare_their_text_and_a_keys_own_is_equal_is_still_asked() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let key = |name: &str| crate::fw::string::make(name);
        let picky = {
            let mut rt = runtime::lock();
            let string = rt.by_name["NSString"];
            let class = rt.define_class("PickyKeyString", string, 16);
            let is_equal = rt.sel("isEqual:");
            rt.add_method(class, is_equal, picky_is_equal as usize);
            class
        };
        let odd = runtime::alloc_instance(picky);
        assert!(crate::fw::string::plain_units(key("wand")).is_some());
        assert!(crate::fw::string::plain_units(odd).is_none(), "its own -isEqual: answers");
        let (one, two, three) = (number_int(1), number_int(2), number_int(3));
        let words = dictionary_from(&[(key("hat"), one), (key("wand"), two)]);
        assert_eq!(dictionary_get(words, key("wand")), two, "an equal text, another object");
        assert_eq!(dictionary_get(words, key("staff")), 0);
        let picky_words = dictionary_from(&[(odd, three)]);
        assert_eq!(dictionary_get(picky_words, key("anything")), three);
        assert!(PICKY_ASKED.load(std::sync::atomic::Ordering::SeqCst) > 0, "the key's own -isEqual: was asked");
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

    /// -addObjectsFromArray: keeps each entry it adds, as every other way in does.
    #[test]
    fn apple_adding_from_another_array_keeps_each_entry() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        use crate::fw::counted;
        let data = runtime::send(runtime::send(runtime::class_named("NSMutableData"), "alloc", &[]), "initWithLength:", &[1]);
        let source = array_from(&[data]);
        assert_eq!(counted::count(data), Some(2));
        let list = runtime::send(runtime::class_named("NSMutableArray"), "array", &[]);
        runtime::send(list, "addObjectsFromArray:", &[source]);
        assert_eq!(counted::count(data), Some(3), "the new array keeps its entry");
    }

    /// The nil-terminated makers (arrayWithObjects:, initWithObjects:,
    /// dictionaryWithObjectsAndKeys:) keep each entry once, as Apple's retain.
    #[test]
    fn nil_terminated_makers_keep_each_entry_once() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        use crate::fw::counted;
        let data = runtime::send(runtime::send(runtime::class_named("NSMutableData"), "alloc", &[]), "initWithLength:", &[1]);
        ios_array_with_objects_impl(runtime::class_named("NSArray"), 0, data, [0u64].as_ptr());
        assert_eq!(counted::count(data), Some(2), "+[NSArray arrayWithObjects:]");
        ios_array_init_with_objects_impl(runtime::alloc_instance(runtime::class_named("NSArray")), 0, data, [0u64].as_ptr());
        assert_eq!(counted::count(data), Some(3), "-[NSArray initWithObjects:]");
        let key = crate::fw::string::make("key");
        ios_dict_with_objects_and_keys_impl(runtime::class_named("NSDictionary"), 0, data, [key as u64, 0].as_ptr());
        assert_eq!(counted::count(data), Some(4), "+[NSDictionary dictionaryWithObjectsAndKeys:]");
    }

    /// -removeObjectIdenticalTo: removes the same object only, not an equal one.
    #[test]
    fn apple_remove_identical_removes_only_that_object() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let (first, second) = (crate::fw::string::make("same"), crate::fw::string::make("same"));
        let list = runtime::send(array_from(&[first, second]), "mutableCopy", &[]);
        runtime::send(list, "removeObjectIdenticalTo:", &[first]);
        assert_eq!(array_items(list), [second]);
    }

    static OWN_LIST: std::sync::Mutex<Vec<Id>> = std::sync::Mutex::new(Vec::new());
    static OWN_WORDS: std::sync::Mutex<Vec<(Id, Id)>> = std::sync::Mutex::new(Vec::new());

    extern "C" fn own_count(_this: Id, _: Sel) -> usize {
        OWN_LIST.lock().unwrap().len()
    }
    extern "C" fn own_object_at(_this: Id, _: Sel, index: usize) -> Id {
        OWN_LIST.lock().unwrap()[index]
    }
    extern "C" fn own_insert(_this: Id, _: Sel, item: Id, index: usize) {
        OWN_LIST.lock().unwrap().insert(index, item);
    }
    extern "C" fn own_remove_at(_this: Id, _: Sel, index: usize) {
        OWN_LIST.lock().unwrap().remove(index);
    }
    extern "C" fn own_replace(_this: Id, _: Sel, index: usize, item: Id) {
        OWN_LIST.lock().unwrap()[index] = item;
    }
    extern "C" fn own_words_count(_this: Id, _: Sel) -> usize {
        OWN_WORDS.lock().unwrap().len()
    }
    extern "C" fn own_words_get(_this: Id, _: Sel, key: Id) -> Id {
        let words = OWN_WORDS.lock().unwrap().clone();
        words.into_iter().find(|(k, _)| keys_equal(*k, key)).map_or(0, |(_, v)| v)
    }
    extern "C" fn own_words_keys(_this: Id, _: Sel) -> Id {
        let keys: Vec<Id> = OWN_WORDS.lock().unwrap().iter().map(|(k, _)| *k).collect();
        enumerator(keys)
    }
    extern "C" fn own_words_set(_this: Id, _: Sel, value: Id, key: Id) {
        let mut words = OWN_WORDS.lock().unwrap();
        let text = crate::fw::string::get(key);
        words.retain(|(k, _)| crate::fw::string::get(*k) != text);
        words.push((key, value));
    }
    extern "C" fn own_words_remove(_this: Id, _: Sel, key: Id) {
        let text = crate::fw::string::get(key);
        OWN_WORDS.lock().unwrap().retain(|(k, _)| crate::fw::string::get(*k) != text);
    }

    fn texts(list: Id) -> Vec<String> {
        array_items(list).into_iter().map(crate::fw::string::get).collect()
    }

    /// A game's class-cluster subclass (JSONKit's KV_JKArray/KV_JKDictionary,
    /// ApItems) keeps its own storage and overrides only the primitive
    /// methods: every other NSArray/NSDictionary method goes through them, as
    /// Foundation's do.
    #[test]
    fn apple_collection_methods_go_through_a_subclass_primitives() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let s = crate::fw::string::make;
        let class = define_class("TestOwnStorageList", "NSMutableArray", 16);
        methods(
            class,
            &[
                ("count", own_count as usize),
                ("objectAtIndex:", own_object_at as usize),
                ("insertObject:atIndex:", own_insert as usize),
                ("removeObjectAtIndex:", own_remove_at as usize),
                ("replaceObjectAtIndex:withObject:", own_replace as usize),
            ],
        );
        let list = runtime::alloc_instance(class);
        let (a, b, c) = (s("a"), s("b"), s("c"));
        runtime::send(list, "addObject:", &[b]);
        assert_eq!(*OWN_LIST.lock().unwrap(), [b], "addObject: inserts through the primitive");
        assert_eq!(runtime::send(list, "lastObject", &[]), b);
        assert_eq!(runtime::send(list, "firstObject", &[]), b);
        assert_eq!(runtime::send(list, "containsObject:", &[s("b")]) & 0xFF, 1);
        assert_eq!(runtime::send(list, "indexOfObject:", &[s("b")]), 0);
        runtime::send(list, "addObjectsFromArray:", &[array_from(&[c, a])]);
        runtime::send(list, "sortUsingSelector:", &[runtime::sel("compare:") as usize]);
        assert_eq!(texts(list), ["a", "b", "c"]);
        runtime::send(list, "exchangeObjectAtIndex:withObjectAtIndex:", &[0, 2]);
        assert_eq!(texts(list), ["c", "b", "a"]);
        let mut state = [0usize; 8];
        let mut buffer = [0 as Id; 4];
        let got = runtime::send(list, "countByEnumeratingWithState:objects:count:", &[state.as_mut_ptr() as usize, buffer.as_mut_ptr() as usize, 4]);
        assert_eq!(got, 3, "fast enumeration reads the primitives");
        runtime::send(list, "removeObject:", &[s("b")]);
        runtime::send(list, "removeLastObject", &[]);
        assert_eq!(texts(list), ["c"]);
        runtime::send(list, "removeAllObjects", &[]);
        assert!(OWN_LIST.lock().unwrap().is_empty());

        let class = define_class("TestOwnStorageWords", "NSMutableDictionary", 16);
        methods(
            class,
            &[
                ("count", own_words_count as usize),
                ("objectForKey:", own_words_get as usize),
                ("keyEnumerator", own_words_keys as usize),
                ("setObject:forKey:", own_words_set as usize),
                ("removeObjectForKey:", own_words_remove as usize),
            ],
        );
        let words = runtime::alloc_instance(class);
        runtime::send(words, "setValue:forKey:", &[s("gold"), s("price")]);
        runtime::send(words, "addEntriesFromDictionary:", &[dictionary_from(&[(s("icon"), s("hex.jpg"))])]);
        assert_eq!(OWN_WORDS.lock().unwrap().len(), 2, "both went through setObject:forKey:");
        assert_eq!(crate::fw::string::get(runtime::send(words, "valueForKey:", &[s("price")])), "gold");
        assert_eq!(texts(runtime::send(words, "allKeys", &[])), ["price", "icon"]);
        assert_eq!(texts(runtime::send(words, "allValues", &[])), ["gold", "hex.jpg"]);
        let mut state = [0usize; 8];
        let got = runtime::send(words, "countByEnumeratingWithState:objects:count:", &[state.as_mut_ptr() as usize, buffer.as_mut_ptr() as usize, 4]);
        assert_eq!(got, 2);
        runtime::send(words, "setValue:forKey:", &[0, s("price")]);
        assert_eq!(OWN_WORDS.lock().unwrap().len(), 1, "nil removes through removeObjectForKey:");
        runtime::send(words, "removeAllObjects", &[]);
        assert!(OWN_WORDS.lock().unwrap().is_empty());
    }

    extern "C" fn shouting_object_for_key(this: Id, _: Sel, key: Id) -> Id {
        let value = dictionary_get(this, key);
        if value == 0 { 0 } else { crate::fw::string::make(&crate::fw::string::get(value).to_uppercase()) }
    }

    /// A subclass that overrides only objectForKey: (its count, and storage,
    /// still ours) is read through its objectForKey:, not around it.
    #[test]
    fn apple_a_subclass_overriding_one_reading_primitive_is_read_through_it() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let s = crate::fw::string::make;
        let class = define_class("TestShoutingWords", "NSMutableDictionary", 16);
        methods(class, &[("objectForKey:", shouting_object_for_key as usize)]);
        let words = runtime::alloc_instance(class);
        runtime::send(words, "setObject:forKey:", &[s("gold"), s("price")]);
        assert_eq!(texts(runtime::send(words, "allValues", &[])), ["GOLD"]);
        assert_eq!(crate::fw::string::get(runtime::send(words, "valueForKey:", &[s("price")])), "GOLD");
        assert_eq!(texts(runtime::send(words, "allKeys", &[])), ["price"]);
    }

    static ORDER_STEP: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);

    /// A comparator that answers at random: not a total order.
    extern "C" fn fickle_order(_a: Id, _b: Id, _context: usize) -> isize {
        let step = ORDER_STEP.fetch_add(1, std::sync::atomic::Ordering::SeqCst).wrapping_mul(2_654_435_761) >> 7;
        (step % 3) as isize - 1
    }

    /// A 32-bit game's NSOrderedAscending (-1) comes back as 0xFFFFFFFF.
    extern "C" fn narrow_order(a: Id, b: Id, _context: usize) -> usize {
        match number_as_i64(a).cmp(&number_as_i64(b)) {
            std::cmp::Ordering::Less => 0xFFFF_FFFF,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        }
    }

    /// Sorting never stops the app on a comparator that isn't a total order
    /// (Apple's merge sort just finishes), and reads NSComparisonResult as the
    /// 32-bit value it is.
    #[test]
    fn apple_sorting_takes_any_comparator() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let list = runtime::send(array_from(&(0..60).map(number_int).collect::<Vec<_>>()), "mutableCopy", &[]);
        for _ in 0..20 {
            runtime::send(list, "sortUsingFunction:context:", &[fickle_order as usize, 0]);
        }
        assert_eq!(array_items(list).len(), 60);
        let sorted = runtime::send(array_from(&[number_int(3), number_int(1), number_int(2)]), "sortedArrayUsingFunction:context:", &[narrow_order as usize, 0]);
        assert_eq!(array_items(sorted).into_iter().map(number_as_i64).collect::<Vec<_>>(), [1, 2, 3]);
    }

    /// NSNumber's text for floating-point numbers is CFNumber's: "%.17g",
    /// with "nan" and "+infinity"/"-infinity"; whole numbers as before.
    #[test]
    fn apple_number_text_matches_cfnumber() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let text = |n: Id| crate::fw::string::get(runtime::send(n, "stringValue", &[]));
        assert_eq!(text(number_float(0.1)), "0.10000000000000001");
        assert_eq!(text(number_float(1e20)), "1e+20");
        assert_eq!(text(number_float(2.5)), "2.5");
        assert_eq!(text(number_float(f64::NAN)), "nan");
        assert_eq!(text(number_float(f64::INFINITY)), "+infinity");
        assert_eq!(text(number_float(f64::NEG_INFINITY)), "-infinity");
        assert_eq!(text(number_int(-42)), "-42");
        let number = runtime::class_named("NSNumber");
        assert_eq!(text(runtime::send(number, "numberWithUnsignedLongLong:", &[u64::MAX as usize])), "18446744073709551615");
    }

    /// objCType reports the C type the number was made from.
    #[test]
    fn apple_numbers_report_their_own_type() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let number = runtime::class_named("NSNumber");
        let kind = |n: Id| unsafe { runtime::cstr(runtime::send(n, "objCType", &[])) }.to_string();
        let float: extern "C" fn(Class, Sel, f32) -> Id = unsafe { std::mem::transmute(runtime::imp_for(number, runtime::sel("numberWithFloat:")).unwrap()) };
        assert_eq!(kind(float(number, 0, 0.5)), "f");
        assert_eq!(kind(runtime::send(number, "numberWithInt:", &[7])), "i");
        assert_eq!(kind(runtime::send(number, "numberWithBool:", &[1])), "c");
        assert_eq!(kind(runtime::send(number, "numberWithLongLong:", &[7])), "q");
        assert!(runtime::responds(number, "numberWithShort:"), "numberWithShort: is missing");
        assert_eq!(kind(runtime::send(number, "numberWithShort:", &[7])), "s");
        assert_eq!(kind(number_float(0.5)), "d");
    }

    /// Immutable collections' -copy is the collection itself; out-of-range
    /// indexes and nil entries raise (catchable) instead of being clamped or
    /// ending the app; every key is copied.
    #[test]
    fn apple_collections_copy_raise_and_copy_keys_as_foundation_does() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let s = crate::fw::string::make;
        let list = array_from(&[s("a")]);
        assert_eq!(runtime::send(list, "copy", &[]), list);
        let words = dictionary_from(&[(s("k"), s("v"))]);
        assert_eq!(runtime::send(words, "copy", &[]), words);
        assert_ne!(runtime::send(list, "mutableCopy", &[]), list);
        let set = runtime::send(runtime::class_named("NSSet"), "setWithObject:", &[s("a")]);
        assert_eq!(runtime::send(set, "copy", &[]), set);

        let name_of = |raised: Option<Id>| raised.map(|e| crate::fw::string::get(runtime::send(e, "name", &[])));
        let mutable = runtime::send(list, "mutableCopy", &[]);
        let (_, raised) = crate::fw::exception::catching(|| runtime::send(mutable, "insertObject:atIndex:", &[s("b"), 5]));
        assert_eq!(name_of(raised).as_deref(), Some("NSRangeException"));
        assert_eq!(array_items(mutable).len(), 1, "nothing inserted");
        let (_, raised) = crate::fw::exception::catching(|| runtime::send(mutable, "addObject:", &[0]));
        assert_eq!(name_of(raised).as_deref(), Some("NSInvalidArgumentException"));
        let (got, raised) = crate::fw::exception::catching(|| runtime::send(mutable, "objectAtIndex:", &[5]));
        assert_eq!((got, name_of(raised).as_deref()), (0, Some("NSRangeException")));
        let table = runtime::send(runtime::class_named("NSMutableDictionary"), "dictionary", &[]);
        let (_, raised) = crate::fw::exception::catching(|| runtime::send(table, "setObject:forKey:", &[0, s("k")]));
        assert_eq!(name_of(raised).as_deref(), Some("NSInvalidArgumentException"));
        assert_eq!(dictionary_pairs(table).len(), 0);

        // A mutable key is copied, so changing it later changes nothing here.
        let key = runtime::send(array_from(&[s("x")]), "mutableCopy", &[]);
        runtime::send(table, "setObject:forKey:", &[s("v"), key]);
        assert_ne!(dictionary_pairs(table)[0].0, key);
    }

    /// The missing NSArray/NSDictionary/NSValue methods games call.
    #[test]
    fn apple_get_objects_and_bytes_values() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let s = crate::fw::string::make;
        let value = runtime::class_named("NSValue");
        for (receiver, name) in [(value, "valueWithBytes:objCType:"), (value, "value:withObjCType:"), (dictionary_from(&[]), "getObjects:andKeys:")] {
            assert!(runtime::responds(receiver, name), "{name} is missing");
        }
        let list = array_from(&[s("a"), s("b"), s("c")]);
        let mut out = [0 as Id; 2];
        let get: extern "C" fn(Id, Sel, *mut Id, crate::fw::string::Range) =
            unsafe { std::mem::transmute(runtime::imp_for(list, runtime::sel("getObjects:range:")).expect("getObjects:range:")) };
        get(list, 0, out.as_mut_ptr(), crate::fw::string::Range { location: 1, length: 2 });
        assert_eq!(out.map(crate::fw::string::get), ["b", "c"]);
        let words = dictionary_from(&[(s("k1"), s("v1")), (s("k2"), s("v2"))]);
        let (mut values, mut keys) = ([0 as Id; 2], [0 as Id; 2]);
        runtime::send(words, "getObjects:andKeys:", &[values.as_mut_ptr() as usize, keys.as_mut_ptr() as usize]);
        // Apple's hash table order: hash("k2") % 3 puts k2 in bucket 1 and k1 in 2.
        assert_eq!((keys.map(crate::fw::string::get), values.map(crate::fw::string::get)), (["k2", "k1"].map(String::from), ["v2", "v1"].map(String::from)));

        let pair = [7i32, -3i32];
        let kind = c"{pair=ii}";
        let boxed = runtime::send(runtime::class_named("NSValue"), "valueWithBytes:objCType:", &[pair.as_ptr() as usize, kind.as_ptr() as usize]);
        assert_eq!(unsafe { runtime::cstr(runtime::send(boxed, "objCType", &[])) }, "{pair=ii}");
        let mut back = [0i32; 2];
        runtime::send(boxed, "getValue:", &[back.as_mut_ptr() as usize]);
        assert_eq!(back, pair);
        let same = runtime::send(runtime::alloc_instance(runtime::class_named("NSValue")), "initWithBytes:objCType:", &[pair.as_ptr() as usize, kind.as_ptr() as usize]);
        assert_eq!(runtime::send(same, "isEqualToValue:", &[boxed]) & 0xFF, 1);
        let old = runtime::send(runtime::class_named("NSValue"), "value:withObjCType:", &[pair.as_ptr() as usize, kind.as_ptr() as usize]);
        assert_eq!(runtime::send(old, "isEqualToValue:", &[boxed]) & 0xFF, 1);
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

    /// In a 32-bit game's memory long and pointers are 4 bytes: NSValue
    /// reads and writes back only those, never the word after.
    #[test]
    fn apple_values_from_a_32_bit_game_use_its_sizes() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        // Stand-in game memory: this buffer (the game's rules, as wire.rs sets them).
        static GAME: std::sync::Mutex<[u8; 24]> = std::sync::Mutex::new([0; 24]);
        let start = GAME.lock().unwrap().as_ptr() as usize;
        static START: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        START.store(start, std::sync::atomic::Ordering::Relaxed);
        crate::fw::system::set_guest_memory(crate::fw::system::GuestMemory {
            owns: |at| (START.load(std::sync::atomic::Ordering::Relaxed)..START.load(std::sync::atomic::Ordering::Relaxed) + 24).contains(&at),
            object_to_host: |guest| guest as Id,
            object_to_guest: |host| host as u32,
        });
        let class = runtime::class_named("NSValue");
        let kind = c"l";
        let mut game = GAME.lock().unwrap();
        game[..8].copy_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);
        game[8..].fill(0xee);
        let long = runtime::send(class, "valueWithBytes:objCType:", &[start, kind.as_ptr() as usize]);
        let pointer = runtime::send(class, "valueWithPointer:", &[0x1234_5670]);
        runtime::send(long, "getValue:", &[start + 8]);
        runtime::send(pointer, "getValue:", &[start + 16]);
        assert_eq!(game[8..16], [0x11, 0x22, 0x33, 0x44, 0xee, 0xee, 0xee, 0xee]);
        assert_eq!(game[16..], [0x70, 0x56, 0x34, 0x12, 0xee, 0xee, 0xee, 0xee]);
        // Host memory keeps the host's sizes.
        let mut host = [0xeeu8; 8];
        runtime::send(pointer, "getValue:", &[host.as_mut_ptr() as usize]);
        assert_eq!(usize::from_le_bytes(host), 0x1234_5670);
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

    /// -[NSString hash] is Apple's __CFStringHash (CoreFoundation 6.1,
    /// 0xa7c0): the length, then 4 characters at a time with fixed
    /// multipliers, then the text length's shift added back.
    #[test]
    fn string_hashes_are_apples_cfstring_hashes() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let hash = |text: &str| runtime::send(crate::fw::string::make(text), "hash", &[]) as u32;
        assert_eq!(hash("a"), 1062);
        assert_eq!(hash("d"), 1071);
        assert_eq!(hash("hello"), 2741432633);
        // Over 96 characters, Apple hashes the first 32, 32 around the middle
        // and the last 32 only (97 x 'a' as CoreFoundation 6.1 hashes it).
        assert_eq!(hash(&"a".repeat(97)), 1518140995);
    }

    /// -[NSNumber hash] is Apple's __CFNumberHash (CoreFoundation 0xddc0):
    /// an integer is its magnitude times 0x9E3779B1; a floating one rounds,
    /// keeps the fraction and the integral part modulo 2^32-1.
    #[test]
    fn number_hashes_are_apples_cfnumber_hashes() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let hash = |number: Id| runtime::send(number, "hash", &[]) as u32;
        assert_eq!(hash(number_int(1)), 2654435761);
        assert_eq!(hash(number_int(-1)), 2654435761);
        assert_eq!(hash(number_float(1.5)), 3161387875);
    }

    /// Dictionaries come out in Apple's CFBasicHash bucket order (the bucket
    /// a key lands in is its hash modulo the table's size, collisions move
    /// one bucket on, and enumeration walks the buckets from 0), not in the
    /// order the keys went in.
    #[test]
    fn dictionaries_enumerate_in_cfbasichash_bucket_order() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let table = runtime::send(runtime::class_named("NSMutableDictionary"), "dictionary", &[]);
        for key in ["a", "b", "c", "d", "e"] {
            runtime::send(table, "setObject:forKey:", &[crate::fw::string::make(key), crate::fw::string::make(key)]);
        }
        let keys: Vec<String> = array_items(runtime::send(table, "allKeys", &[])).into_iter().map(crate::fw::string::get).collect();
        assert_eq!(keys, ["d", "b", "e", "c", "a"]);
        // Apple's growth table: a 7-bucket table holds 6 entries; the 7th
        // grows it to 13, which moves the order.
        for key in ["f", "g"] {
            runtime::send(table, "setObject:forKey:", &[crate::fw::string::make(key), crate::fw::string::make(key)]);
        }
        let keys: Vec<String> = array_items(runtime::send(table, "allKeys", &[])).into_iter().map(crate::fw::string::get).collect();
        assert_eq!(keys, ["g", "c", "d", "e", "a", "f", "b"]);
        let set = runtime::send(runtime::class_named("NSSet"), "setWithArray:", &[array_from(&["a", "b", "c", "d", "e"].map(crate::fw::string::make))]);
        let ordered: Vec<String> = array_items(runtime::send(set, "allObjects", &[])).into_iter().map(crate::fw::string::get).collect();
        assert_eq!(ordered, ["d", "b", "e", "c", "a"], "sets come out the same way");
        assert_eq!(crate::fw::string::get(runtime::send(set, "anyObject", &[])), "d", "anyObject is the first bucket");
    }
}
