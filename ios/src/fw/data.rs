//! NSData and NSMutableData: { isa, *mut Vec<u8> }.

use crate::fw::object::{class_methods, define_class, methods};
use crate::objc::runtime::{self, lock, Class, Id, Sel};
use crate::symbols::Registry;
use std::sync::OnceLock;

static CLASSES: OnceLock<[Class; 2]> = OnceLock::new();

fn store(object: Id) -> &'static mut Vec<u8> {
    unsafe {
        let slot = (object + 8) as *mut *mut Vec<u8>;
        if (*slot).is_null() {
            *slot = Box::into_raw(Box::new(Vec::new()));
        }
        &mut **slot
    }
}

pub fn make(bytes: &[u8]) -> Id {
    let object = runtime::alloc_instance(CLASSES.get().unwrap()[0]);
    *store(object) = bytes.to_vec();
    object
}

pub fn is_data(object: Id) -> bool {
    let rt = lock();
    object != 0 && rt.is_subclass(rt.class_of(object), CLASSES.get().unwrap()[0])
}

pub fn bytes(object: Id) -> Vec<u8> {
    if object == 0 {
        return Vec::new();
    }
    store(object).clone()
}

extern "C" fn data_empty(class: Class, _: Sel) -> Id {
    let object = runtime::alloc_instance(class);
    store(object);
    object
}

extern "C" fn data_with_bytes(class: Class, _: Sel, bytes: *const u8, length: usize) -> Id {
    let object = runtime::alloc_instance(class);
    if !bytes.is_null() {
        *store(object) = unsafe { std::slice::from_raw_parts(bytes, length) }.to_vec();
    }
    object
}

extern "C" fn data_init_with_bytes(this: Id, _: Sel, bytes: *const u8, length: usize) -> Id {
    *store(this) = if bytes.is_null() { Vec::new() } else { unsafe { std::slice::from_raw_parts(bytes, length) }.to_vec() };
    this
}

extern "C" fn data_with_bytes_no_copy(class: Class, s: Sel, bytes: *const u8, length: usize) -> Id {
    data_with_bytes(class, s, bytes, length)
}

extern "C" fn data_with_bytes_no_copy_free(class: Class, s: Sel, bytes: *const u8, length: usize, _free: bool) -> Id {
    data_with_bytes(class, s, bytes, length)
}

extern "C" fn data_with_contents_of_file(class: Class, _: Sel, path: Id) -> Id {
    match std::fs::read(crate::fw::string::get(path)) {
        Ok(bytes) => {
            let object = runtime::alloc_instance(class);
            *store(object) = bytes;
            object
        }
        Err(_) => 0,
    }
}

extern "C" fn data_init_with_contents_of_file(this: Id, _: Sel, path: Id) -> Id {
    match std::fs::read(crate::fw::string::get(path)) {
        Ok(bytes) => {
            *store(this) = bytes;
            this
        }
        Err(_) => 0,
    }
}

extern "C" fn data_bytes(this: Id, _: Sel) -> *const u8 {
    store(this).as_ptr()
}

extern "C" fn data_length(this: Id, _: Sel) -> usize {
    store(this).len()
}

/// -base64EncodedStringWithOptions: (no line breaks; options ignored).
extern "C" fn data_base64(this: Id, _: Sel, _options: usize) -> Id {
    crate::fw::string::make(&crate::fw::plist::base64_encode(store(this)))
}

extern "C" fn data_write_to_file(this: Id, _: Sel, path: Id, _atomically: bool) -> bool {
    let path = crate::fw::string::get(path);
    let temporary = format!("{path}.part");
    std::fs::write(&temporary, store(this)).and_then(|_| std::fs::rename(&temporary, &path)).is_ok()
}

extern "C" fn data_copy(this: Id, _: Sel, _zone: usize) -> Id {
    make(&bytes(this))
}

extern "C" fn data_mutable_copy(this: Id, _: Sel, _zone: usize) -> Id {
    let object = runtime::alloc_instance(CLASSES.get().unwrap()[1]);
    *store(object) = bytes(this);
    object
}

extern "C" fn data_is_equal(this: Id, _: Sel, other: Id) -> bool {
    is_data(other) && bytes(this) == bytes(other)
}

extern "C" fn data_append_bytes(this: Id, _: Sel, bytes: *const u8, length: usize) {
    if !bytes.is_null() {
        store(this).extend_from_slice(unsafe { std::slice::from_raw_parts(bytes, length) });
    }
}

extern "C" fn data_append_data(this: Id, _: Sel, other: Id) {
    let extra = bytes(other);
    store(this).extend(extra);
}

extern "C" fn data_set_length(this: Id, _: Sel, length: usize) {
    store(this).resize(length, 0);
}

extern "C" fn data_mutable_bytes(this: Id, _: Sel) -> *mut u8 {
    store(this).as_mut_ptr()
}

extern "C" fn data_with_capacity(class: Class, _: Sel, capacity: usize) -> Id {
    let object = runtime::alloc_instance(class);
    store(object).reserve(capacity);
    object
}

extern "C" fn data_with_length(class: Class, _: Sel, length: usize) -> Id {
    let object = runtime::alloc_instance(class);
    store(object).resize(length, 0);
    object
}

extern "C" fn data_with_data(class: Class, _: Sel, other: Id) -> Id {
    let object = runtime::alloc_instance(class);
    *store(object) = bytes(other);
    object
}

extern "C" fn data_init_with_data(this: Id, _: Sel, other: Id) -> Id {
    *store(this) = bytes(other);
    this
}

pub fn register(_: &mut Registry) {
    let data = define_class("NSData", "NSObject", 16);
    let mutable = define_class("NSMutableData", "NSData", 16);
    CLASSES.set([data, mutable]).ok();
    class_methods(
        data,
        &[
            ("data", data_empty as usize),
            ("dataWithBytes:length:", data_with_bytes as usize),
            ("dataWithBytesNoCopy:length:", data_with_bytes_no_copy as usize),
            ("dataWithBytesNoCopy:length:freeWhenDone:", data_with_bytes_no_copy_free as usize),
            ("dataWithContentsOfFile:", data_with_contents_of_file as usize),
            ("dataWithData:", data_with_data as usize),
        ],
    );
    methods(
        data,
        &[
            ("initWithBytes:length:", data_init_with_bytes as usize),
            ("initWithData:", data_init_with_data as usize),
            ("initWithContentsOfFile:", data_init_with_contents_of_file as usize),
            ("bytes", data_bytes as usize),
            ("length", data_length as usize),
            ("base64EncodedStringWithOptions:", data_base64 as usize),
            ("writeToFile:atomically:", data_write_to_file as usize),
            ("copyWithZone:", data_copy as usize),
            ("mutableCopyWithZone:", data_mutable_copy as usize),
            ("isEqual:", data_is_equal as usize),
            ("isEqualToData:", data_is_equal as usize),
        ],
    );
    class_methods(mutable, &[("dataWithCapacity:", data_with_capacity as usize), ("dataWithLength:", data_with_length as usize)]);
    methods(
        mutable,
        &[
            ("appendBytes:length:", data_append_bytes as usize),
            ("appendData:", data_append_data as usize),
            ("setLength:", data_set_length as usize),
            ("mutableBytes", data_mutable_bytes as usize),
        ],
    );
}
