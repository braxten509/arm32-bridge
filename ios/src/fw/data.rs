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

/// Only file URLs: the path of the URL.
extern "C" fn data_with_contents_of_url(class: Class, s: Sel, url: Id) -> Id {
    if url == 0 || !runtime::responds(url, "path") {
        return 0;
    }
    data_with_contents_of_file(class, s, runtime::send(url, "path", &[]))
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

extern "C" fn data_subdata(this: Id, _: Sel, range: crate::fw::string::Range) -> Id {
    let all = store(this);
    let end = range.location.saturating_add(range.length);
    if end > all.len() {
        crate::log!("STOP: subdataWithRange: {}..{end} is past the end ({} bytes)", range.location, all.len());
        crate::report::finish(1);
    }
    make(&all[range.location..end])
}

extern "C" fn data_get_bytes_length(this: Id, _: Sel, buffer: *mut u8, length: usize) {
    let all = store(this);
    let count = length.min(all.len());
    if !buffer.is_null() {
        unsafe { std::ptr::copy_nonoverlapping(all.as_ptr(), buffer, count) };
    }
}

extern "C" fn data_get_bytes_range(this: Id, _: Sel, buffer: *mut u8, range: crate::fw::string::Range) {
    let all = store(this);
    let start = range.location.min(all.len());
    let end = range.location.saturating_add(range.length).min(all.len());
    if !buffer.is_null() {
        unsafe { std::ptr::copy_nonoverlapping(all[start..end].as_ptr(), buffer, end - start) };
    }
}

extern "C" fn data_increase_length(this: Id, _: Sel, extra: usize) {
    let all = store(this);
    all.resize(all.len() + extra, 0);
}

extern "C" fn data_init_with_length(this: Id, _: Sel, length: usize) -> Id {
    store(this).resize(length, 0);
    this
}

/// The range's bytes become `length` new ones (the data grows or shrinks to fit).
extern "C" fn data_replace_bytes(this: Id, _: Sel, range: crate::fw::string::Range, bytes: *const u8, length: usize) {
    let all = store(this);
    let start = range.location.min(all.len());
    let end = range.location.saturating_add(range.length);
    if end > all.len() {
        all.resize(end, 0);
    }
    let new: Vec<u8> = if bytes.is_null() { vec![0; length] } else { unsafe { std::slice::from_raw_parts(bytes, length) }.to_vec() };
    all.splice(start..end, new);
}

/// NSFileReadNoSuchFileError and the other Cocoa file error codes.
fn file_error(error: &std::io::Error, path: &str, writing: bool) -> Id {
    let code = match (error.kind(), writing) {
        (std::io::ErrorKind::NotFound, false) => 260,
        (std::io::ErrorKind::PermissionDenied, false) => 257,
        (_, false) => 256,
        (std::io::ErrorKind::NotFound, true) => 4,
        (std::io::ErrorKind::PermissionDenied, true) => 513,
        (_, true) => 512,
    };
    crate::fw::misc::error("NSCocoaErrorDomain", code, &format!("The file “{}” couldn’t be {}.", path.rsplit('/').next().unwrap_or(path), if writing { "saved" } else { "opened" }))
}

fn set_error(out: *mut Id, error: Id) {
    if !out.is_null() {
        unsafe { *out = error };
    }
}

extern "C" fn data_with_contents_of_file_options(class: Class, _: Sel, path: Id, _options: usize, error: *mut Id) -> Id {
    let text = crate::fw::string::get(path);
    match std::fs::read(&text) {
        Ok(bytes) => {
            let object = runtime::alloc_instance(class);
            *store(object) = bytes;
            object
        }
        Err(e) => {
            set_error(error, file_error(&e, &text, false));
            0
        }
    }
}

extern "C" fn data_init_with_contents_of_file_options(this: Id, _: Sel, path: Id, _options: usize, error: *mut Id) -> Id {
    let text = crate::fw::string::get(path);
    match std::fs::read(&text) {
        Ok(bytes) => {
            *store(this) = bytes;
            this
        }
        Err(e) => {
            set_error(error, file_error(&e, &text, false));
            0
        }
    }
}

/// NSDataWritingAtomic (1) writes a side file first; NSDataWritingWithoutOverwriting (2) refuses an existing file.
extern "C" fn data_write_to_file_options(this: Id, _: Sel, path: Id, options: usize, error: *mut Id) -> bool {
    let path = crate::fw::string::get(path);
    if options & 2 != 0 && std::path::Path::new(&path).exists() {
        set_error(error, crate::fw::misc::error("NSCocoaErrorDomain", 516, "The file already exists."));
        return false;
    }
    let result = if options & 1 != 0 {
        let temporary = format!("{path}.part");
        std::fs::write(&temporary, store(this)).and_then(|_| std::fs::rename(&temporary, &path))
    } else {
        std::fs::write(&path, store(this))
    };
    match result {
        Ok(()) => true,
        Err(e) => {
            set_error(error, file_error(&e, &path, true));
            false
        }
    }
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
            // The old name: iOS maps the file, a plain read gives the same bytes.
            ("dataWithContentsOfMappedFile:", data_with_contents_of_file as usize),
            ("dataWithContentsOfURL:", data_with_contents_of_url as usize),
            ("dataWithData:", data_with_data as usize),
            ("dataWithContentsOfFile:options:error:", data_with_contents_of_file_options as usize),
        ],
    );
    methods(
        data,
        &[
            ("initWithBytes:length:", data_init_with_bytes as usize),
            ("initWithData:", data_init_with_data as usize),
            ("initWithContentsOfFile:", data_init_with_contents_of_file as usize),
            ("initWithContentsOfMappedFile:", data_init_with_contents_of_file as usize),
            ("bytes", data_bytes as usize),
            ("length", data_length as usize),
            ("base64EncodedStringWithOptions:", data_base64 as usize),
            ("writeToFile:atomically:", data_write_to_file as usize),
            ("copyWithZone:", data_copy as usize),
            ("mutableCopyWithZone:", data_mutable_copy as usize),
            ("isEqual:", data_is_equal as usize),
            ("isEqualToData:", data_is_equal as usize),
            ("initWithContentsOfFile:options:error:", data_init_with_contents_of_file_options as usize),
            ("writeToFile:options:error:", data_write_to_file_options as usize),
            ("subdataWithRange:", data_subdata as usize),
            ("getBytes:length:", data_get_bytes_length as usize),
            ("getBytes:range:", data_get_bytes_range as usize),
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
            ("increaseLengthBy:", data_increase_length as usize),
            ("initWithLength:", data_init_with_length as usize),
            ("replaceBytesInRange:withBytes:length:", data_replace_bytes as usize),
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fw::string::Range;

    /// Ranges of bytes: copied out, replaced (growing or shrinking the
    /// data), read into a buffer; length grows with zeros; files written
    /// and read with an error when they can't be.
    #[test]
    fn byte_ranges_and_files() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let imp = |object: Id, name: &str| runtime::imp_for(object, runtime::sel(name)).unwrap_or_else(|| panic!("{name} is missing"));
        let data = make(b"spellcraft");
        let sub: extern "C" fn(Id, Sel, Range) -> Id = unsafe { std::mem::transmute(imp(data, "subdataWithRange:")) };
        assert_eq!(bytes(sub(data, 0, Range { location: 5, length: 5 })), b"craft");
        let mut buffer = [0u8; 5];
        runtime::send(data, "getBytes:length:", &[buffer.as_mut_ptr() as usize, 5]);
        assert_eq!(&buffer, b"spell");
        let mutable = runtime::send(data, "mutableCopy", &[]);
        let replace: extern "C" fn(Id, Sel, Range, *const u8, usize) = unsafe { std::mem::transmute(imp(mutable, "replaceBytesInRange:withBytes:length:")) };
        replace(mutable, 0, Range { location: 5, length: 5 }, b"book".as_ptr(), 4);
        assert_eq!(bytes(mutable), b"spellbook");
        replace(mutable, 0, Range { location: 0, length: 2 }, b"SP".as_ptr(), 2);
        assert_eq!(bytes(mutable), b"SPellbook");
        runtime::send(mutable, "increaseLengthBy:", &[2]);
        assert_eq!(bytes(mutable), b"SPellbook\0\0");
        let sized = runtime::send(runtime::alloc_instance(runtime::class_named("NSMutableData")), "initWithLength:", &[3]);
        assert_eq!(bytes(sized), [0, 0, 0]);
        let path = std::env::temp_dir().join(format!("data-test-{}", std::process::id()));
        let path_text = crate::fw::string::make(path.to_str().unwrap());
        let mut error: Id = 0;
        assert_eq!(runtime::send(data, "writeToFile:options:error:", &[path_text, 1, &mut error as *mut Id as usize]) & 0xFF, 1);
        let class = runtime::class_named("NSData");
        let read = runtime::send(class, "dataWithContentsOfFile:options:error:", &[path_text, 0, &mut error as *mut Id as usize]);
        assert_eq!(bytes(read), b"spellcraft");
        std::fs::remove_file(&path).ok();
        let missing = runtime::send(class, "dataWithContentsOfFile:options:error:", &[path_text, 0, &mut error as *mut Id as usize]);
        assert_eq!(missing, 0);
        assert_ne!(error, 0, "a missing file sets the error");
        assert_eq!(runtime::send(error, "code", &[]), 260, "NSFileReadNoSuchFileError");
    }
}
