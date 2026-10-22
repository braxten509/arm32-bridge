//! The C++ runtime for games that link Apple's GNU libstdc++ (libstdc++.6):
//! strings, streams, locales, containers' tree and list helpers, type
//! information and exceptions. It is Apple's own libstdc++ source with GCC's
//! libsupc++, built by ios/build.rs with Apple's arm64 layouts (see
//! ios/cxxrt/adapt/gamebridge_prelude.h); every name in it is private, and
//! this file hands the game the table of its Apple names.

use crate::symbols::Registry;
use std::ffi::{c_char, c_void, CStr};

#[repr(C)]
struct Export {
    name: *const c_char,
    address: *const c_void,
    data: i32,
}

extern "C" {
    static gamebridge_cxxrt_exports: [Export; 0];
}

/// Names libSystem already provides with its own behaviour (logging guards,
/// atexit, the "unfinished method" stop); those stay.
fn keep_existing(name: &str) -> bool {
    matches!(name, "__cxa_guard_acquire" | "__cxa_guard_release" | "__cxa_guard_abort" | "__cxa_pure_virtual")
}

/// Every runtime name, as (Apple name without its leading "_", address, is data).
pub fn exports() -> Vec<(&'static str, usize, bool)> {
    let mut list = Vec::new();
    unsafe {
        let mut row = gamebridge_cxxrt_exports.as_ptr();
        while !(*row).name.is_null() {
            let name = CStr::from_ptr((*row).name).to_str().unwrap();
            list.push((name, (*row).address as usize, (*row).data != 0));
            row = row.add(1);
        }
    }
    list
}

pub fn register(r: &mut Registry) {
    for (name, address, data) in exports() {
        if keep_existing(name) && r.get(name).is_some() {
            continue;
        }
        if data {
            r.data(name, address);
        } else {
            r.function(name, address);
        }
    }
}

/// Address of a runtime name (for tests and for code here that calls it).
pub fn address(name: &str) -> Option<usize> {
    exports().into_iter().find(|e| e.0 == name).map(|e| e.1)
}

// ----- What the runtime asks of Game Bridge -----

/// Apple's character classes, from the same table as the game's own ctype calls.
#[no_mangle]
extern "C" fn gamebridge_cxx_maskrune(c: i32, mask: u64) -> u64 {
    crate::libsystem::libc::maskrune(c, mask)
}

/// File streams open the game's paths the way its fopen does.
#[no_mangle]
extern "C" fn gamebridge_cxx_fopen(path: *const c_char, mode: *const c_char) -> *mut libc::FILE {
    crate::libsystem::io::ios_fopen(path, mode)
}

/// The unwinder's own consistency checks (GCC's gcc_assert) failed.
#[no_mangle]
extern "C" fn gamebridge_cxx_unwind_fault(line: i32) -> ! {
    crate::log!("STOP: the C++ exception unwinder found inconsistent unwind data (unwind-dw2.c check at line {line})");
    crate::report::finish(1);
}

/// A C++ exception's search for a handler ended at the frame returning to
/// `pc`: the end of the stack, or code that is neither the game's nor the
/// C++ runtime's (it is not unwound). std::terminate follows.
#[no_mangle]
extern "C" fn gamebridge_cxx_unwind_stopped(pc: usize) {
    crate::log!("C++ exception: no handler found up to {}", crate::loader::describe_address(pc));
}

// ----- Objective-C exceptions (ios/cxxrt/glue/objc_eh.cc) -----

/// Whether `object` is an instance of the class named `class_name` or of a subclass.
#[no_mangle]
extern "C" fn gamebridge_objc_is_kind_of(object: usize, class_name: *const c_char) -> i32 {
    let name = unsafe { CStr::from_ptr(class_name) }.to_string_lossy();
    let Some(class) = crate::objc::runtime::existing_class(&name, false) else { return 0 };
    let runtime = crate::objc::runtime::lock();
    i32::from(object != 0 && runtime.is_subclass(runtime.class_of(object), class))
}

/// The class name of a thrown object (the exception's type name), kept for the rest of the run.
#[no_mangle]
extern "C" fn gamebridge_objc_class_name(object: usize) -> *const c_char {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static NAMES: Mutex<Option<HashMap<usize, usize>>> = Mutex::new(None);
    let class = if object == 0 { 0 } else { crate::objc::runtime::lock().class_of(object) };
    let mut names = NAMES.lock().unwrap();
    let map = names.get_or_insert_with(HashMap::new);
    *map.entry(class).or_insert_with(|| {
        let name = if class == 0 { "nil".to_string() } else { crate::objc::runtime::lock().class_name(class) };
        crate::symbols::leak_cstr(&name)
    }) as *const c_char
}

/// An Objective-C exception nobody caught: the app ends, as on an iPhone.
#[no_mangle]
extern "C" fn gamebridge_objc_uncaught(object: usize) -> ! {
    let reason = crate::fw::string::describe(object);
    crate::log!("STOP: uncaught Objective-C exception: {reason}");
    crate::report::finish(1);
}

/// Tells the unwinder where a loaded game image's code and unwind tables are.
pub fn add_image(loaded: &crate::loader::Loaded) {
    extern "C" {
        fn gamebridge_unwind_add_image(
            header: usize,
            start: usize,
            end: usize,
            unwind_info: usize,
            unwind_info_size: usize,
            eh_frame: usize,
            eh_frame_size: usize,
        ) -> i32;
    }
    let image = &loaded.image;
    let base = image.base_address();
    let place = |segment: &str, name: &str| {
        image
            .section(segment, name)
            .map(|s| ((s.address - base) as usize + loaded.start, s.size as usize))
            .unwrap_or((0, 0))
    };
    let (unwind_info, unwind_info_size) = place("__TEXT", "__unwind_info");
    let (eh_frame, eh_frame_size) = place("__TEXT", "__eh_frame");
    let result = unsafe {
        gamebridge_unwind_add_image(loaded.start, loaded.start, loaded.end, unwind_info, unwind_info_size, eh_frame, eh_frame_size)
    };
    if result != 0 {
        crate::log!("C++ exceptions: too many images; this one's frames can't be unwound");
    }
}

/// std::terminate (an exception nobody catches, or a throw out of a
/// destructor or a no-throw function): on an iPhone the app ends here.
#[no_mangle]
extern "C" fn gamebridge_cxx_terminate(type_name: *const c_char, what: *const c_char) -> ! {
    let text = |p: *const c_char| unsafe { (!p.is_null()).then(|| CStr::from_ptr(p).to_string_lossy().into_owned()) };
    match (text(type_name), text(what)) {
        (Some(name), Some(what)) => crate::log!("STOP: C++ terminate: uncaught exception of type {name}: {what}"),
        (Some(name), None) => crate::log!("STOP: C++ terminate: uncaught exception of type {name}"),
        _ => crate::log!("STOP: C++ terminate called without an active exception"),
    }
    crate::report::finish(1);
}

#[cfg(test)]
mod tests {
    use super::address;
    use std::ffi::c_void;

    fn function<T: Copy>(name: &str) -> T {
        let at = address(name).unwrap_or_else(|| panic!("{name} is not in the runtime"));
        assert_eq!(std::mem::size_of::<T>(), 8);
        unsafe { std::mem::transmute_copy(&at) }
    }

    /// Apple's copy-on-write std::string: a pointer to the characters, with
    /// length, capacity and reference count in the 24 bytes before them;
    /// copies share until one is changed; "" is the shared empty string.
    #[test]
    fn strings_have_apples_shared_layout() {
        type Construct = extern "C" fn(*mut usize, *const u8, *const u8);
        type Copy = extern "C" fn(*mut usize, *const usize);
        type Append = extern "C" fn(*mut usize, *const u8, usize) -> *mut usize;
        type Destroy = extern "C" fn(*mut usize);
        let construct: Construct = function("_ZNSsC1EPKcRKSaIcE");
        let copy: Copy = function("_ZNSsC1ERKSs");
        let append: Append = function("_ZNSs6appendEPKcm");
        let destroy: Destroy = function("_ZNSsD1Ev");
        let allocator = 0u8;
        let (mut a, mut b, mut empty) = (0usize, 0usize, 0usize);
        construct(&mut a, b"sword\0".as_ptr(), &allocator);
        let header = |s: usize| unsafe { *((s - 24) as *const [u64; 3]) };
        assert_eq!(unsafe { std::slice::from_raw_parts(a as *const u8, 6) }, b"sword\0");
        assert_eq!(header(a)[0], 5);
        copy(&mut b, &a);
        assert_eq!(b, a, "a copy shares the characters");
        assert_eq!(header(a)[2] as u32, 1, "reference count is one more than the owners");
        append(&mut b, b" & shield".as_ptr(), 9);
        assert_ne!(b, a, "changing a shared copy gives it its own characters");
        assert_eq!(unsafe { std::slice::from_raw_parts(b as *const u8, 15) }, b"sword & shield\0");
        assert_eq!(header(a)[0], 5);
        construct(&mut empty, b"\0".as_ptr(), &allocator);
        assert_eq!(empty, address("_ZNSs4_Rep20_S_empty_rep_storageE").unwrap() + 24);
        destroy(&mut a);
        destroy(&mut b);
        destroy(&mut empty);
    }

    /// An ostringstream formats numbers the way Apple's does and keeps its
    /// characters where a game's inlined code reads them (stringbuf's put area).
    #[test]
    fn ostringstream_formats_into_its_buffer() {
        type Construct = extern "C" fn(*mut c_void, u32);
        type PutInt = extern "C" fn(*mut c_void, i32) -> *mut c_void;
        type PutDouble = extern "C" fn(*mut c_void, f64) -> *mut c_void;
        type PutText = extern "C" fn(*mut c_void, *const u8, isize) -> *mut c_void;
        type Destroy = extern "C" fn(*mut c_void);
        let construct: Construct = function("_ZNSt19basic_ostringstreamIcSt11char_traitsIcESaIcEEC1ESt13_Ios_Openmode");
        let put_int: PutInt = function("_ZNSolsEi");
        let put_double: PutDouble = function("_ZNSolsEd");
        let put_text: PutText = function("_ZSt16__ostream_insertIcSt11char_traitsIcEERSt13basic_ostreamIT_T0_ES6_PKS3_l");
        let destroy: Destroy = function("_ZNSt19basic_ostringstreamIcSt11char_traitsIcESaIcEED1Ev");
        let mut storage = vec![0u64; 128];
        let stream = storage.as_mut_ptr() as *mut c_void;
        construct(stream, 16); // ios_base::out
        put_int(stream, -42);
        put_text(stream, b" coins, ".as_ptr(), 8);
        put_double(stream, 2.5);
        // The stringbuf follows the ostream's vtable pointer: its own vtable,
        // then eback, gptr, egptr, pbase, pptr, epptr.
        let buffer = unsafe { (stream as *const usize).add(1) };
        let (pbase, pptr) = unsafe { (*buffer.add(4), *buffer.add(5)) };
        let text = unsafe { std::slice::from_raw_parts(pbase as *const u8, pptr - pbase) };
        assert_eq!(text, b"-42 coins, 2.5");
        destroy(stream);
    }

    /// Real throw/catch in iPhone code (ios/cxxrt/test/throw_test.cc, built
    /// by ios/tools/build-cxx-test.sh): compact unwind frames, LSDAs and the
    /// personality routine bound through the game's GOT, cleanups on the way,
    /// rethrow, and an exception thrown inside this runtime caught by the game.
    #[test]
    fn game_code_throws_and_catches() {
        crate::symbols::with(|r| {
            crate::libsystem::register(r);
            super::register(r);
        });
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/cxxrt/test");
        let loaded = crate::loader::load(dir, &format!("{dir}/throw_test")).expect("load throw_test");
        let call = |name: &str| -> i32 {
            let symbol = loaded.image.symbols.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("{name}"));
            let f: extern "C" fn() -> i32 = unsafe { std::mem::transmute(symbol.address.wrapping_add(loaded.slide) as usize) };
            f()
        };
        assert_eq!(call("_test_catch_by_base"), 207);
        assert_eq!(call("_test_rethrow"), 19);
        assert_eq!(call("_test_runtime_throw"), 1);
        assert_eq!(call("_test_no_throw"), 1);
        assert_eq!(call("_test_catch_by_base"), 207, "a second round works the same");
    }
}
