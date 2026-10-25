//! AddressBook for 32-bit games: the C functions convert to the host
//! implementation (there is no contacts store, so a book is empty but its
//! records and multi values are real), plus the property and label
//! constants. The comparator is a function; a game taking its address gets
//! that stand-in's address from the loader.

use super::{host_functions, register_all, Bool, I32, Long, Obj, Ptr};
use arm32_bridge::runtime::mem;

/// The host's ABPropertyID variable `name` (an int32).
fn property_id(name: &str) -> u32 {
    super::host_data(name).map_or(0, |address| unsafe { *(address as *const i32) as u32 })
}

pub fn register() {
    // ABPersonComparePeopleByName is a function; a game that takes its
    // address (a data bind) gets this stand-in's address from the loader.
    register_all(host_functions! {
        "ABAddressBookCreate" => fn() -> Obj;
        "ABAddressBookCopyArrayOfAllPeople" => fn(book: Obj) -> Obj;
        "ABPersonCreate" => fn() -> Obj;
        "ABPersonGetSortOrdering" => fn() -> I32;
        "ABPersonComparePeopleByName" => fn(a: Obj, b: Obj, ordering: I32) -> Long;
        "ABRecordCopyValue" => fn(record: Obj, property: I32) -> Obj;
        "ABRecordSetValue" => fn(record: Obj, property: I32, value: Obj) -> Bool;
        "ABMultiValueCreateMutable" => fn(kind: I32) -> Obj;
        "ABMultiValueAddValueAndLabel" => fn(multi: Obj, value: Obj, label: Obj, identifier: Ptr) -> Bool;
        "ABMultiValueGetCount" => fn(multi: Obj) -> Long;
        "ABMultiValueCopyValueAtIndex" => fn(multi: Obj, index: Long) -> Obj;
    });
    // ABPropertyID constants are 4-byte int variables, as on Apple.
    for name in ["kABPersonFirstNameProperty", "kABPersonLastNameProperty", "kABPersonEmailProperty", "kABPersonPhoneProperty"] {
        super::super::data(name, move || {
            let variable = crate::guest32::alloc_permanent(4, 4);
            mem().write_u32(variable, property_id(name));
            variable
        });
    }
}
