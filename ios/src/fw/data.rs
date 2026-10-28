//! NSData and NSMutableData: { isa, *mut Bytes }.
//!
//! The bytes are where the game can reach them: a 32-bit game reads and
//! writes them through 32-bit pointers (-bytes, -mutableBytes,
//! CFDataGetBytePtr), so while one runs they come from its heap
//! ([`keep_bytes_in`]); otherwise from the host's malloc. As on Apple, the
//! pointer stays put while the length stays within the capacity, and a
//! data made over someone else's bytes (no copy) reads those bytes.
//!
//! Data count references (fw::counted) and give their bytes back at the
//! last release: our own bytes, and bytes handed over with no copy that
//! the data is to free (freeWhenDone, a CFAllocator that frees); never
//! bytes the caller keeps. Convenience constructors answer autoreleased
//! objects, alloc/init, copies and Create functions +1, as on Apple.

use crate::fw::counted;
use crate::fw::object::{class_methods, define_class, methods};
use crate::objc::runtime::{self, lock, Class, Id, Sel};
use crate::symbols::Registry;
use std::sync::{OnceLock, RwLock};

static CLASSES: OnceLock<[Class; 2]> = OnceLock::new();

/// Where new bytes come from.
#[derive(Clone, Copy)]
pub struct ByteHeap {
    pub alloc: fn(usize) -> *mut u8,
    pub free: fn(*mut u8),
    /// Frees a block from the game's own malloc (bytes handed over with no copy).
    pub free_malloced: fn(*mut u8),
}

const HOST_HEAP: ByteHeap = ByteHeap {
    alloc: |size| unsafe { libc::malloc(size) as *mut u8 },
    free: |pointer| unsafe { libc::free(pointer as *mut libc::c_void) },
    // A 64-bit game's malloc is the layer's (libsystem::heap).
    free_malloced: |pointer| crate::libsystem::heap::free(pointer as *mut libc::c_void),
};

static HEAP: RwLock<ByteHeap> = RwLock::new(HOST_HEAP);

fn heap() -> ByteHeap {
    *HEAP.read().unwrap_or_else(|e| e.into_inner())
}

/// How many hold the heap choice (see [`keep_bytes_in`]).
static HOLDERS: std::sync::Mutex<usize> = std::sync::Mutex::new(0);

/// New data bytes come from the chosen heap while this is held; when the
/// last holder lets go, from the host's again.
#[must_use]
pub struct HeapHeld(());

impl Drop for HeapHeld {
    fn drop(&mut self) {
        let mut holders = HOLDERS.lock().unwrap_or_else(|e| e.into_inner());
        *holders -= 1;
        if *holders == 0 {
            *HEAP.write().unwrap_or_else(|e| e.into_inner()) = HOST_HEAP;
        }
    }
}

/// Makes new data bytes come from `heap` (a 32-bit game's, so its 32-bit
/// pointers reach them) for as long as the answer is held. Bytes made
/// before or after keep their own heap.
pub fn keep_bytes_in(heap: ByteHeap) -> HeapHeld {
    let mut holders = HOLDERS.lock().unwrap_or_else(|e| e.into_inner());
    *holders += 1;
    *HEAP.write().unwrap_or_else(|e| e.into_inner()) = heap;
    HeapHeld(())
}

/// A data object's bytes; dropping them frees what is ours to free.
pub struct Bytes {
    pointer: *mut u8,
    length: usize,
    capacity: usize,
    /// How `pointer` is freed; None when the bytes stay someone else's.
    free: Option<fn(*mut u8)>,
}

impl Drop for Bytes {
    fn drop(&mut self) {
        if let (Some(free), false) = (self.free, self.pointer.is_null()) {
            free(self.pointer);
        }
    }
}

impl Bytes {
    const EMPTY: Bytes = Bytes { pointer: std::ptr::null_mut(), length: 0, capacity: 0, free: None };

    pub fn as_slice(&self) -> &[u8] {
        if self.length == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(self.pointer, self.length) }
        }
    }

    pub fn len(&self) -> usize {
        self.length
    }

    /// The bytes' address (never NULL, also when empty).
    pub fn pointer(&mut self) -> *mut u8 {
        if self.pointer.is_null() {
            self.move_to(1, &[]);
        }
        self.pointer
    }

    /// Moves to new memory of at least `capacity` bytes that holds `start`;
    /// the old memory goes only after the copy, so `start` may lie in it.
    fn move_to(&mut self, capacity: usize, start: &[u8]) {
        let heap = heap();
        let capacity = capacity.max(16);
        let pointer = (heap.alloc)(capacity);
        assert!(!pointer.is_null(), "out of memory for {capacity} data bytes");
        unsafe { std::ptr::copy_nonoverlapping(start.as_ptr(), pointer, start.len()) };
        // The assignment drops (frees) the old bytes.
        *self = Bytes { pointer, length: start.len(), capacity, free: Some(heap.free) };
    }

    /// Room for `total` bytes in memory of our own (grown by doubling).
    pub fn reserve(&mut self, total: usize) {
        if self.free.is_none() || total > self.capacity {
            let old = (self.pointer, self.length);
            let keep = if old.1 == 0 { &[][..] } else { unsafe { std::slice::from_raw_parts(old.0, old.1) } };
            self.move_to(total.max(self.capacity * 2), keep);
        }
    }

    /// The new length; added bytes are zeros.
    pub fn resize(&mut self, length: usize) {
        if length > self.length {
            self.reserve(length);
            unsafe { std::ptr::write_bytes(self.pointer.add(self.length), 0, length - self.length) };
        }
        self.length = length;
    }

    /// `bytes` may be part of these bytes.
    pub fn extend(&mut self, bytes: &[u8]) {
        let total = self.length + bytes.len();
        if self.free.is_none() || total > self.capacity {
            let mut joined = self.as_slice().to_vec();
            joined.extend_from_slice(bytes);
            self.move_to(total.max(self.capacity * 2), &joined);
            return;
        }
        unsafe { std::ptr::copy(bytes.as_ptr(), self.pointer.add(self.length), bytes.len()) };
        self.length = total;
    }

    /// Replaces the bytes (`bytes` may be part of them).
    pub fn set(&mut self, bytes: &[u8]) {
        if self.free.is_none() || bytes.len() > self.capacity {
            self.move_to(bytes.len(), bytes);
            return;
        }
        unsafe { std::ptr::copy(bytes.as_ptr(), self.pointer, bytes.len()) };
        self.length = bytes.len();
    }
}

fn slot(object: Id) -> *mut *mut Bytes {
    (object + 8) as *mut *mut Bytes
}

fn store(object: Id) -> &'static mut Bytes {
    unsafe {
        let slot = slot(object);
        if (*slot).is_null() {
            *slot = Box::into_raw(Box::new(Bytes::EMPTY));
        }
        &mut **slot
    }
}

/// Gives back a data's bytes (at its last release, or a game subclass's
/// -dealloc); what is left reads as empty.
fn clean_up(object: Id) {
    let bytes = unsafe { std::mem::replace(&mut *slot(object), std::ptr::null_mut()) };
    if !bytes.is_null() {
        drop(unsafe { Box::from_raw(bytes) });
    }
    runtime::object_finished(object);
}

/// A new object of `class` with one reference (counted when it is ours;
/// a game's subclass counts in the game).
fn new_object(class: Class) -> Id {
    let object = runtime::alloc_instance(class);
    if CLASSES.get().is_some_and(|classes| classes.contains(&class)) {
        counted::track(object, clean_up);
    }
    object
}

/// A new NSData holding a copy of `bytes`, +1: the caller owns it
/// (autorelease it to answer a convenience method).
pub fn make(bytes: &[u8]) -> Id {
    let object = new_object(CLASSES.get().unwrap()[0]);
    store(object).set(bytes);
    object
}

/// A new NSMutableData (+1) with room for `capacity` bytes before its bytes move.
pub fn make_mutable(capacity: usize) -> Id {
    let object = new_object(CLASSES.get().unwrap()[1]);
    store(object).reserve(capacity);
    object
}

/// A new NSData (+1) that reads `length` bytes at `pointer` without copying
/// them; `free` (if any) frees them when the data goes, else they stay the
/// caller's.
pub fn make_over(pointer: *const u8, length: usize, free: Option<fn(*mut u8)>) -> Id {
    let object = new_object(CLASSES.get().unwrap()[0]);
    take_over(object, pointer, length, free);
    object
}

/// How bytes handed over by a game are freed: with the game's free.
pub fn game_free() -> fn(*mut u8) {
    heap().free_malloced
}

fn take_over(object: Id, pointer: *const u8, length: usize, free: Option<fn(*mut u8)>) {
    if !pointer.is_null() {
        *store(object) = Bytes { pointer: pointer as *mut u8, length, capacity: length, free };
    }
}

pub fn is_data(object: Id) -> bool {
    let rt = lock();
    object != 0 && rt.is_subclass(rt.class_of(object), CLASSES.get().unwrap()[0])
}

pub fn bytes(object: Id) -> Vec<u8> {
    if object == 0 {
        return Vec::new();
    }
    if !keeps_our_storage(object) && runtime::send(object, "respondsToSelector:", &[runtime::sel("bytes") as usize]) & 0xFF != 0 {
        // Not ours (a game's NSData subclass, or a proxy standing in for a
        // data): read through its primitive methods, as Foundation does.
        let length = runtime::send(object, "length", &[]);
        let pointer = runtime::send(object, "bytes", &[]) as *const u8;
        return unsafe { slice(pointer, length) }.to_vec();
    }
    store(object).as_slice().to_vec()
}

/// Whether the data keeps its bytes in our storage (its -length is ours).
fn keeps_our_storage(object: Id) -> bool {
    let sel = runtime::sel("length");
    runtime::imp_for(object, sel) == Some(data_length as runtime::Imp)
}

// ----- class methods: autoreleased, as on Apple -----

extern "C" fn data_empty(class: Class, _: Sel) -> Id {
    counted::autorelease(new_object(class))
}

unsafe fn slice<'a>(bytes: *const u8, length: usize) -> &'a [u8] {
    if bytes.is_null() || length == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(bytes, length)
    }
}

extern "C" fn data_with_bytes(class: Class, _: Sel, bytes: *const u8, length: usize) -> Id {
    let object = new_object(class);
    store(object).set(unsafe { slice(bytes, length) });
    counted::autorelease(object)
}

/// The bytes are the data's from now on: freed when it goes (freeWhenDone).
extern "C" fn data_with_bytes_no_copy(class: Class, s: Sel, bytes: *const u8, length: usize) -> Id {
    data_with_bytes_no_copy_free(class, s, bytes, length, true)
}

extern "C" fn data_with_bytes_no_copy_free(class: Class, _: Sel, bytes: *const u8, length: usize, free: bool) -> Id {
    let object = new_object(class);
    take_over(object, bytes, length, free.then(game_free));
    counted::autorelease(object)
}

extern "C" fn data_with_contents_of_file(class: Class, _: Sel, path: Id) -> Id {
    match std::fs::read(crate::fw::string::get(path)) {
        Ok(bytes) => {
            let object = new_object(class);
            store(object).set(&bytes);
            counted::autorelease(object)
        }
        Err(_) => 0,
    }
}

/// The path of a file URL; 0 for any other URL (there is no network, so web
/// URLs fail as on an iPhone with no connection).
fn file_url_path(url: Id) -> Id {
    if url == 0 || !runtime::responds(url, "isFileURL") || runtime::send(url, "isFileURL", &[]) & 0xFF == 0 {
        return 0;
    }
    runtime::send(url, "path", &[])
}

extern "C" fn data_with_contents_of_url(class: Class, s: Sel, url: Id) -> Id {
    match file_url_path(url) {
        0 => 0,
        path => data_with_contents_of_file(class, s, path),
    }
}

extern "C" fn data_init_with_contents_of_url(this: Id, s: Sel, url: Id) -> Id {
    match file_url_path(url) {
        0 => {
            counted::release(this);
            0
        }
        path => data_init_with_contents_of_file(this, s, path),
    }
}

extern "C" fn data_with_capacity(class: Class, _: Sel, capacity: usize) -> Id {
    let object = new_object(class);
    store(object).reserve(capacity);
    counted::autorelease(object)
}

extern "C" fn data_with_length(class: Class, _: Sel, length: usize) -> Id {
    let object = new_object(class);
    store(object).resize(length);
    counted::autorelease(object)
}

extern "C" fn data_with_data(class: Class, _: Sel, other: Id) -> Id {
    let object = new_object(class);
    store(object).set(&bytes(other));
    counted::autorelease(object)
}

/// +alloc / +allocWithZone: one reference, the caller's.
extern "C" fn data_alloc(class: Class, _: Sel) -> Id {
    new_object(class)
}

extern "C" fn data_alloc_with_zone(class: Class, _: Sel, _zone: usize) -> Id {
    new_object(class)
}

// ----- initializers (on an alloc'd object; a failed one releases it) -----

extern "C" fn data_init_with_bytes(this: Id, _: Sel, bytes: *const u8, length: usize) -> Id {
    store(this).set(unsafe { slice(bytes, length) });
    this
}

extern "C" fn data_init_with_bytes_no_copy(this: Id, s: Sel, bytes: *const u8, length: usize) -> Id {
    data_init_with_bytes_no_copy_free(this, s, bytes, length, true)
}

extern "C" fn data_init_with_bytes_no_copy_free(this: Id, _: Sel, bytes: *const u8, length: usize, free: bool) -> Id {
    take_over(this, bytes, length, free.then(game_free));
    this
}

extern "C" fn data_init_with_contents_of_file(this: Id, _: Sel, path: Id) -> Id {
    match std::fs::read(crate::fw::string::get(path)) {
        Ok(bytes) => {
            store(this).set(&bytes);
            this
        }
        Err(_) => {
            counted::release(this);
            0
        }
    }
}

extern "C" fn data_init_with_data(this: Id, _: Sel, other: Id) -> Id {
    let copy = bytes(other);
    store(this).set(&copy);
    this
}

extern "C" fn data_init_with_capacity(this: Id, _: Sel, capacity: usize) -> Id {
    store(this).reserve(capacity);
    this
}

extern "C" fn data_init_with_length(this: Id, _: Sel, length: usize) -> Id {
    store(this).resize(length);
    this
}

// ----- reference counting -----

extern "C" fn data_retain(this: Id, _: Sel) -> Id {
    counted::retain(this)
}

extern "C" fn data_release(this: Id, _: Sel) {
    counted::release(this)
}

extern "C" fn data_autorelease(this: Id, _: Sel) -> Id {
    counted::autorelease(this)
}

extern "C" fn data_retain_count(this: Id, _: Sel) -> usize {
    counted::count(this).unwrap_or(1)
}

/// A game subclass's [super dealloc]: its bytes go (ours go at the last release).
extern "C" fn data_dealloc(this: Id, _: Sel) {
    if !counted::is_tracked(this) {
        clean_up(this);
    }
}

// ----- instance methods -----

extern "C" fn data_bytes(this: Id, _: Sel) -> *const u8 {
    store(this).pointer()
}

extern "C" fn data_length(this: Id, _: Sel) -> usize {
    store(this).len()
}

/// -base64EncodedStringWithOptions: (no line breaks; options ignored).
extern "C" fn data_base64(this: Id, _: Sel, _options: usize) -> Id {
    crate::fw::string::make(&crate::fw::plist::base64_encode(store(this).as_slice()))
}

extern "C" fn data_write_to_file(this: Id, _: Sel, path: Id, _atomically: bool) -> bool {
    let path = crate::fw::string::get(path);
    let temporary = format!("{path}.part");
    std::fs::write(&temporary, store(this).as_slice()).and_then(|_| std::fs::rename(&temporary, &path)).is_ok()
}

extern "C" fn data_copy(this: Id, _: Sel, _zone: usize) -> Id {
    make(store(this).as_slice())
}

extern "C" fn data_mutable_copy(this: Id, _: Sel, _zone: usize) -> Id {
    let object = new_object(CLASSES.get().unwrap()[1]);
    store(object).set(store(this).as_slice());
    object
}

extern "C" fn data_is_equal(this: Id, _: Sel, other: Id) -> bool {
    is_data(other) && bytes(this) == bytes(other)
}

extern "C" fn data_append_bytes(this: Id, _: Sel, bytes: *const u8, length: usize) {
    store(this).extend(unsafe { slice(bytes, length) });
}

extern "C" fn data_append_data(this: Id, _: Sel, other: Id) {
    let extra = bytes(other);
    store(this).extend(&extra);
}

extern "C" fn data_set_length(this: Id, _: Sel, length: usize) {
    store(this).resize(length);
}

/// -setData: the bytes become a copy of the other data's.
extern "C" fn data_set_data(this: Id, _: Sel, other: Id) {
    let replacement = bytes(other);
    let store = store(this);
    store.resize(0);
    store.extend(&replacement);
}

extern "C" fn data_mutable_bytes(this: Id, _: Sel) -> *mut u8 {
    store(this).pointer()
}

extern "C" fn data_subdata(this: Id, _: Sel, range: crate::fw::string::Range) -> Id {
    let all = store(this);
    let end = range.location.saturating_add(range.length);
    if end > all.len() {
        crate::log!("STOP: subdataWithRange: {}..{end} is past the end ({} bytes)", range.location, all.len());
        crate::report::finish(1);
    }
    make(&all.as_slice()[range.location..end])
}

extern "C" fn data_get_bytes_length(this: Id, _: Sel, buffer: *mut u8, length: usize) {
    let all = store(this);
    let count = length.min(all.len());
    if !buffer.is_null() {
        unsafe { std::ptr::copy_nonoverlapping(all.as_slice().as_ptr(), buffer, count) };
    }
}

extern "C" fn data_get_bytes_range(this: Id, _: Sel, buffer: *mut u8, range: crate::fw::string::Range) {
    let all = store(this);
    let start = range.location.min(all.len());
    let end = range.location.saturating_add(range.length).min(all.len());
    if !buffer.is_null() {
        unsafe { std::ptr::copy_nonoverlapping(all.as_slice()[start..end].as_ptr(), buffer, end - start) };
    }
}

extern "C" fn data_increase_length(this: Id, _: Sel, extra: usize) {
    let all = store(this);
    let Some(length) = all.len().checked_add(extra) else {
        // Apple raises: no data can be that long.
        crate::log!("STOP: increaseLengthBy: {extra} more bytes than {} overflows the length", all.len());
        crate::report::finish(1);
    };
    all.resize(length);
}

/// The range's bytes become `length` new ones (the data grows or shrinks to fit).
extern "C" fn data_replace_bytes(this: Id, _: Sel, range: crate::fw::string::Range, bytes: *const u8, length: usize) {
    let all = store(this);
    let start = range.location.min(all.len());
    let end = range.location.saturating_add(range.length);
    if end > all.len() {
        all.resize(end);
    }
    let new: Vec<u8> = if bytes.is_null() { vec![0; length] } else { unsafe { std::slice::from_raw_parts(bytes, length) }.to_vec() };
    let mut joined = all.as_slice()[..start].to_vec();
    joined.extend_from_slice(&new);
    joined.extend_from_slice(&all.as_slice()[end..]);
    all.set(&joined);
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
            let object = new_object(class);
            store(object).set(&bytes);
            counted::autorelease(object)
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
            store(this).set(&bytes);
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
        std::fs::write(&temporary, store(this).as_slice()).and_then(|_| std::fs::rename(&temporary, &path))
    } else {
        std::fs::write(&path, store(this).as_slice())
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
    counted::count_class(data);
    counted::count_class(mutable);
    class_methods(
        data,
        &[
            ("alloc", data_alloc as usize),
            ("allocWithZone:", data_alloc_with_zone as usize),
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
            ("initWithBytesNoCopy:length:", data_init_with_bytes_no_copy as usize),
            ("initWithBytesNoCopy:length:freeWhenDone:", data_init_with_bytes_no_copy_free as usize),
            ("initWithData:", data_init_with_data as usize),
            ("initWithContentsOfFile:", data_init_with_contents_of_file as usize),
            ("initWithContentsOfMappedFile:", data_init_with_contents_of_file as usize),
            ("initWithContentsOfURL:", data_init_with_contents_of_url as usize),
            ("retain", data_retain as usize),
            ("release", data_release as usize),
            ("autorelease", data_autorelease as usize),
            ("retainCount", data_retain_count as usize),
            ("dealloc", data_dealloc as usize),
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
            ("initWithCapacity:", data_init_with_capacity as usize),
            ("initWithLength:", data_init_with_length as usize),
            ("appendBytes:length:", data_append_bytes as usize),
            ("appendData:", data_append_data as usize),
            ("setLength:", data_set_length as usize),
            ("setData:", data_set_data as usize),
            ("mutableBytes", data_mutable_bytes as usize),
            ("increaseLengthBy:", data_increase_length as usize),
            ("replaceBytesInRange:withBytes:length:", data_replace_bytes as usize),
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::objc::runtime::{class_named, send};
        use crate::libsystem::heap::{malloc, malloc_size};
    use crate::fw::string::Range;

    static FOREIGN_BYTES: &[u8] = b"0,#,1\r#,.,2";

    extern "C" fn foreign_length(_this: Id, _: Sel) -> usize {
        FOREIGN_BYTES.len()
    }
    extern "C" fn foreign_bytes(_this: Id, _: Sel) -> *const u8 {
        FOREIGN_BYTES.as_ptr()
    }

    /// Data that isn't ours (a game's NSData subclass with its own -length
    /// and -bytes, or a game proxy standing in for a data) is read through
    /// those methods, as Foundation does: SpellCraft's dungeon map text came
    /// from a proxy and read as empty.
    #[test]
    fn other_data_objects_are_read_through_length_and_bytes() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let class = define_class("TestForeignBytes", "NSData", 16);
        methods(class, &[("length", foreign_length as usize), ("bytes", foreign_bytes as usize)]);
        let foreign = runtime::alloc_instance(class);
        let text = send(send(class_named("NSString"), "alloc", &[]), "initWithData:encoding:", &[foreign, 1]);
        assert_eq!(crate::fw::string::get(text), "0,#,1\r#,.,2");
        assert_eq!(bytes(send(class_named("NSData"), "dataWithData:", &[foreign])), FOREIGN_BYTES);
    }

    /// Data from a URL: a file URL's file; a web URL is nil, as on an
    /// offline iPhone (never a local file at the URL's path).
    #[test]
    fn data_from_urls_reads_files_only() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let path = std::env::temp_dir().join(format!("data-url-{}", std::process::id()));
        std::fs::write(&path, b"saved").unwrap();
        let text = path.to_str().unwrap();
        let url_class = class_named("NSURL");
        let file = send(url_class, "fileURLWithPath:", &[crate::fw::string::make(text)]);
        let web = send(url_class, "URLWithString:", &[crate::fw::string::make(&format!("http://example.com{text}"))]);
        let made = send(send(class_named("NSData"), "alloc", &[]), "initWithContentsOfURL:", &[file]);
        assert_eq!(bytes(made), b"saved");
        assert_eq!(bytes(send(class_named("NSData"), "dataWithContentsOfURL:", &[file])), b"saved");
        assert_eq!(send(send(class_named("NSData"), "alloc", &[]), "initWithContentsOfURL:", &[web]), 0, "offline");
        assert_eq!(send(class_named("NSData"), "dataWithContentsOfURL:", &[web]), 0, "not the file at its path");
        std::fs::remove_file(&path).unwrap();
    }

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
    /// A 64-bit game's malloc block handed over without a copy is read in
    /// place and freed with the game's free at the last release, not before.
    #[test]
    fn handed_over_bytes_go_with_the_last_release() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let block = malloc(64);
        let data = send(send(class_named("NSData"), "alloc", &[]), "initWithBytesNoCopy:length:", &[block as usize, 64]);
        assert_eq!(send(data, "bytes", &[]), block as usize, "reads the block itself");
        send(data, "retain", &[]);
        send(data, "release", &[]);
        assert_ne!(malloc_size(block), 0, "still held once");
        send(data, "release", &[]);
        assert_eq!(malloc_size(block), 0, "freed with the game's free");
    }
}
