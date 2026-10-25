//! The AddressBook framework: address books, person records, multi values
//! and the name comparator. Game Bridge has no contacts store, so a book
//! holds no people; records and multi values are real objects a game can
//! build, keep values in and read back (Apple's property and label rules).

use crate::fw::object::define_class;
use crate::fw::{collections, counted, string};
use crate::objc::runtime::{self, Class, Id};
use crate::symbols::{leak, Registry};
use std::collections::HashMap;
use std::sync::OnceLock;

static BOOK_CLASS: OnceLock<Class> = OnceLock::new();
static PERSON_CLASS: OnceLock<Class> = OnceLock::new();
static MULTI_VALUE_CLASS: OnceLock<Class> = OnceLock::new();

/// ABPerson: { isa, values } where values is a HashMap<ABPropertyID, id>.
/// ABMultiValue: { isa, list } where list is a Vec<(value, label)> in order.
/// The objects are never freed (as elsewhere in this layer), so the boxes stay.
fn is_of(record: Id, class: &OnceLock<Class>) -> bool {
    record != 0 && class.get().is_some_and(|&class| runtime::lock().class_of(record) == class)
}

fn person_values(record: Id) -> Option<&'static HashMap<i32, Id>> {
    if !is_of(record, &PERSON_CLASS) {
        return None;
    }
    let map = unsafe { *((record + 8) as *const *const HashMap<i32, Id>) };
    (!map.is_null()).then(|| unsafe { &*map })
}

fn person_values_mut(record: Id) -> Option<&'static mut HashMap<i32, Id>> {
    if !is_of(record, &PERSON_CLASS) {
        return None;
    }
    let map = unsafe { *((record + 8) as *mut *mut HashMap<i32, Id>) };
    (!map.is_null()).then(|| unsafe { &mut *map })
}

fn multi_values(record: Id) -> Option<&'static Vec<(Id, Id)>> {
    if !is_of(record, &MULTI_VALUE_CLASS) {
        return None;
    }
    let list = unsafe { *((record + 8) as *const *const Vec<(Id, Id)>) };
    (!list.is_null()).then(|| unsafe { &*list })
}

fn multi_values_mut(record: Id) -> Option<&'static mut Vec<(Id, Id)>> {
    if !is_of(record, &MULTI_VALUE_CLASS) {
        return None;
    }
    let list = unsafe { *((record + 8) as *mut *mut Vec<(Id, Id)>) };
    (!list.is_null()).then(|| unsafe { &mut *list })
}

extern "C" fn address_book_create() -> Id {
    let Some(&class) = BOOK_CLASS.get() else { return 0 };
    runtime::alloc_instance(class)
}

/// There is no contacts store: every book is empty (+1, as Apple's copy).
extern "C" fn address_book_copy_all_people(book: Id) -> Id {
    if book == 0 {
        return 0;
    }
    collections::array_from(&[])
}

extern "C" fn person_create() -> Id {
    let Some(&class) = PERSON_CLASS.get() else { return 0 };
    let record = runtime::alloc_instance(class);
    unsafe { *((record + 8) as *mut *mut HashMap<i32, Id>) = Box::into_raw(Box::new(HashMap::new())) };
    record
}

/// -[ABAddressBook sortOrdering] for a game: first name.
extern "C" fn person_get_sort_ordering() -> i32 {
    0
}

/// ABRecordCopyValue: the stored value (+1), or NULL when the record has none.
extern "C" fn record_copy_value(record: Id, property: i32) -> Id {
    person_values(record).and_then(|values| values.get(&property).copied()).map_or(0, counted::retain)
}

/// ABRecordSetValue: stores the value (+1) for the property, replacing (and
/// letting go of) an older one. False for anything that isn't a person record.
extern "C" fn record_set_value(record: Id, property: i32, value: Id) -> bool {
    let Some(values) = person_values_mut(record) else { return false };
    let old = values.get(&property).copied().unwrap_or(0);
    counted::replace(old, value);
    values.insert(property, value);
    true
}

extern "C" fn multi_value_create_mutable(_kind: i32) -> Id {
    let Some(&class) = MULTI_VALUE_CLASS.get() else { return 0 };
    let multi = runtime::alloc_instance(class);
    unsafe { *((multi + 8) as *mut *mut Vec<(Id, Id)>) = Box::into_raw(Box::new(Vec::new())) };
    multi
}

/// ABMultiValueAddValueAndLabel: appends the pair (retaining both) and writes
/// the new identifier (its index) when the caller wants it.
extern "C" fn multi_value_add_value_and_label(multi: Id, value: Id, label: Id, identifier: *mut i32) -> bool {
    let Some(values) = multi_values_mut(multi) else { return false };
    counted::keep(value);
    counted::keep(label);
    values.push((value, label));
    if !identifier.is_null() {
        unsafe { *identifier = (values.len() - 1) as i32 };
    }
    true
}

extern "C" fn multi_value_get_count(multi: Id) -> isize {
    multi_values(multi).map_or(0, |values| values.len() as isize)
}

/// ABMultiValueCopyValueAtIndex: the value (+1), or NULL past the end.
extern "C" fn multi_value_copy_value_at_index(multi: Id, index: isize) -> Id {
    if index < 0 {
        return 0;
    }
    multi_values(multi).and_then(|values| values.get(index as usize)).map_or(0, |&(value, _)| counted::retain(value))
}

fn property_text(record: Id, property: i32) -> String {
    person_values(record).and_then(|values| values.get(&property).copied()).map(string::get).unwrap_or_default()
}

/// Names sort the way Contacts sorted them on iPhone: case-insensitively,
/// first then last name (or last then first for kABPersonSortByLastName).
fn compare_text(a: &str, b: &str) -> isize {
    match a.to_lowercase().cmp(&b.to_lowercase()) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

extern "C" fn person_compare_people_by_name(a: Id, b: Id, ordering: i32) -> isize {
    const FIRST: i32 = 0;
    const LAST: i32 = 1;
    let order = if ordering == 1 { [LAST, FIRST] } else { [FIRST, LAST] };
    for property in order {
        let result = compare_text(&property_text(a, property), &property_text(b, property));
        if result != 0 {
            return result;
        }
    }
    0
}

pub fn register(r: &mut Registry) {
    let _ = BOOK_CLASS.set(define_class("ABAddressBook", "NSObject", 8));
    let _ = PERSON_CLASS.set(define_class("ABPerson", "NSObject", 16));
    let _ = MULTI_VALUE_CLASS.set(define_class("ABMultiValue", "NSObject", 16));
    r.function("ABAddressBookCreate", address_book_create as usize);
    r.function("ABAddressBookCopyArrayOfAllPeople", address_book_copy_all_people as usize);
    r.function("ABPersonCreate", person_create as usize);
    r.function("ABPersonGetSortOrdering", person_get_sort_ordering as usize);
    r.function("ABPersonComparePeopleByName", person_compare_people_by_name as usize);
    r.function("ABRecordCopyValue", record_copy_value as usize);
    r.function("ABRecordSetValue", record_set_value as usize);
    r.function("ABMultiValueCreateMutable", multi_value_create_mutable as usize);
    r.function("ABMultiValueAddValueAndLabel", multi_value_add_value_and_label as usize);
    r.function("ABMultiValueGetCount", multi_value_get_count as usize);
    r.function("ABMultiValueCopyValueAtIndex", multi_value_copy_value_at_index as usize);
    // ABPropertyID values from ABPerson.h.
    for (name, value) in [
        ("kABPersonFirstNameProperty", 0i32),
        ("kABPersonLastNameProperty", 1),
        ("kABPersonPhoneProperty", 3),
        ("kABPersonEmailProperty", 4),
    ] {
        r.data(name, leak(value));
    }
    r.data("kABWorkLabel", leak(string::make("_$!<Work>!$_")));
}
