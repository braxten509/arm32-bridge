//! Apple's Objective-C runtime functions the game imports.

pub mod runtime;

use crate::symbols::{leak, Registry};
use runtime::{lock, Id, Sel};
use std::collections::HashMap;
use std::sync::{Condvar, Mutex};

// Pools only matter for objects with real reference counts (fw/counted.rs).
extern "C" fn autorelease_pool_push() -> usize {
    crate::fw::counted::pool_push()
}

extern "C" fn autorelease_pool_pop(token: usize) {
    crate::fw::counted::pool_pop(token)
}

extern "C" fn get_property(this: Id, _: Sel, offset: isize, _atomic: bool) -> Id {
    if this == 0 {
        return 0;
    }
    unsafe { *((this as isize + offset) as *const usize) }
}

extern "C" fn set_property(this: Id, _: Sel, offset: isize, value: Id, _atomic: bool, copy: i8) {
    if this == 0 {
        return;
    }
    let stored = match copy {
        _ if value == 0 => 0,
        0 => value,
        2 => runtime::send(value, "mutableCopy", &[]),
        _ => runtime::send(value, "copy", &[]),
    };
    if copy == 0 {
        crate::fw::counted::retain(stored);
    }
    let slot = (this as isize + offset) as *mut usize;
    let old = unsafe { std::mem::replace(&mut *slot, stored) };
    crate::fw::counted::release(old);
}

extern "C" fn set_property_plain(this: Id, _: Sel, value: Id, offset: isize) {
    if this != 0 {
        let old = unsafe { std::mem::replace(&mut *((this as isize + offset) as *mut usize), value) };
        crate::fw::counted::replace(old, value);
    }
}

extern "C" fn set_property_copy(this: Id, _: Sel, value: Id, offset: isize) {
    if this != 0 {
        let copied = runtime::send(value, "copy", &[]);
        let old = unsafe { std::mem::replace(&mut *((this as isize + offset) as *mut usize), copied) };
        crate::fw::counted::release(old);
    }
}

/// Like Apple's runtime: atomic struct properties are guarded by one of a
/// few locks picked by the property's address.
static STRUCT_LOCKS: [Mutex<()>; 64] = [const { Mutex::new(()) }; 64];

fn struct_lock(address: usize) -> &'static Mutex<()> {
    &STRUCT_LOCKS[(address >> 4) % STRUCT_LOCKS.len()]
}

/// objc_copyStruct: one of `dest` or `src` is the property; with `atomic`
/// the copy can't be seen half done.
extern "C" fn copy_struct(dest: *mut u8, src: *const u8, size: usize, atomic: bool, _strong: bool) {
    if !atomic {
        unsafe { std::ptr::copy(src, dest, size) };
        return;
    }
    let (first, second) = {
        let (a, b) = (struct_lock(dest as usize) as *const Mutex<()>, struct_lock(src as usize) as *const Mutex<()>);
        if a <= b { (a, b) } else { (b, a) }
    };
    let _first = unsafe { &*first }.lock().unwrap_or_else(|e| e.into_inner());
    let _second = (first != second).then(|| unsafe { &*second }.lock().unwrap_or_else(|e| e.into_inner()));
    unsafe { std::ptr::copy(src, dest, size) };
}

// objc_sync_enter / exit: one re-entrant lock per object.
struct SyncState {
    owners: HashMap<usize, (u64, usize)>,
}

static SYNC: Mutex<Option<SyncState>> = Mutex::new(None);
static SYNC_WAIT: Condvar = Condvar::new();

fn thread_number() -> u64 {
    unsafe { libc::pthread_self() as u64 }
}

extern "C" fn sync_enter(object: Id) -> i32 {
    if object == 0 {
        return 0;
    }
    let me = thread_number();
    let mut guard = SYNC.lock().unwrap();
    loop {
        let state = guard.get_or_insert_with(|| SyncState { owners: HashMap::new() });
        match state.owners.get_mut(&object) {
            None => {
                state.owners.insert(object, (me, 1));
                return 0;
            }
            Some((owner, count)) if *owner == me => {
                *count += 1;
                return 0;
            }
            Some(_) => guard = SYNC_WAIT.wait(guard).unwrap(),
        }
    }
}

extern "C" fn sync_exit(object: Id) -> i32 {
    if object == 0 {
        return 0;
    }
    let mut guard = SYNC.lock().unwrap();
    if let Some(state) = guard.as_mut() {
        if let Some((_, count)) = state.owners.get_mut(&object) {
            *count -= 1;
            if *count == 0 {
                state.owners.remove(&object);
                SYNC_WAIT.notify_all();
            }
        }
    }
    0
}

extern "C" fn enumeration_mutation(object: Id) {
    crate::log!("STOP: a list changed while the game was going through it ({object:#x})");
    crate::report::finish(1);
}

extern "C" fn exception_throw(exception: Id) -> ! {
    let reason = crate::fw::string::describe(exception);
    crate::log!("STOP: the game threw an Objective-C exception: {reason}");
    crate::report::finish(1);
}

extern "C" fn exception_rethrow() -> ! {
    crate::log!("STOP: the game re-threw an Objective-C exception");
    crate::report::finish(1);
}

extern "C" fn terminate() -> ! {
    crate::log!("STOP: objc_terminate");
    crate::report::finish(1);
}

/// Associated objects: the value and whether it was retained (its policy isn't ASSIGN).
static ASSOCIATED: Mutex<Option<HashMap<(usize, usize), (usize, bool)>>> = Mutex::new(None);

extern "C" fn get_associated(object: Id, key: usize) -> Id {
    ASSOCIATED
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|m| m.get(&(object, key)).map(|&(value, _)| value))
        .unwrap_or(0)
}

extern "C" fn set_associated(object: Id, key: usize, value: Id, policy: usize) {
    // OBJC_ASSOCIATION_ASSIGN (0) holds without a retain; every other policy
    // holds a reference (counts matter for counted objects only, whose copy
    // is themselves).
    let (value, retained) = match policy {
        _ if value == 0 => (0, false),
        0 => (value, false),
        _ => (crate::fw::counted::retain(value), true),
    };
    let old = {
        let mut guard = ASSOCIATED.lock().unwrap();
        let map = guard.get_or_insert_with(HashMap::new);
        if value == 0 {
            map.remove(&(object, key))
        } else {
            map.insert((object, key), (value, retained))
        }
    };
    if let Some((old, true)) = old {
        crate::fw::counted::release(old);
    }
}

extern "C" fn protocol_conforms(protocol: usize, other: usize) -> bool {
    if protocol == other {
        return true;
    }
    unsafe {
        let name = runtime::cstr(*((other + 8) as *const usize));
        let list = *((protocol + 16) as *const usize);
        if list == 0 {
            return false;
        }
        let count = *(list as *const u64) as usize;
        (0..count).any(|i| {
            let p = *((list + 8 + i * 8) as *const usize);
            runtime::cstr(*((p + 8) as *const usize)) == name || protocol_conforms(p, other)
        })
    }
}

extern "C" fn protocol_copy_methods(_p: usize, _req: bool, _inst: bool, count: *mut u32) -> usize {
    if !count.is_null() {
        unsafe { *count = 0 };
    }
    0
}

extern "C" fn protocol_copy_protocols(_p: usize, count: *mut u32) -> usize {
    if !count.is_null() {
        unsafe { *count = 0 };
    }
    0
}

extern "C" fn objc_get_class(name: *const libc::c_char) -> usize {
    let name = unsafe { runtime::cstr(name as usize) };
    runtime::class_named(&name)
}

extern "C" fn sel_register_name(name: *const libc::c_char) -> Sel {
    runtime::sel(&unsafe { runtime::cstr(name as usize) })
}

extern "C" fn sel_get_name(sel: Sel) -> usize {
    sel
}

extern "C" fn object_get_class(object: Id) -> usize {
    lock().class_of(object)
}

extern "C" fn class_get_name(class: usize) -> usize {
    let name = lock().class_name(class);
    crate::symbols::leak_cstr(&name)
}

extern "C" fn personality() -> i32 {
    crate::log!("STOP: an exception reached the error handler (not supported yet)");
    crate::report::finish(1);
}

// ----- ARC entry points (compiler-made calls in ARC code) -----
// Objects are never freed here (retain/release do nothing), so these mostly
// pass objects along and store them; weak references are plain stores. Only
// objects with real counts (pictures, fw/counted.rs) are counted.

extern "C" fn arc_same(object: Id) -> Id {
    object
}

extern "C" fn arc_retain(object: Id) -> Id {
    crate::fw::counted::retain(object)
}

extern "C" fn arc_release(object: Id) {
    crate::fw::counted::release(object)
}

extern "C" fn arc_autorelease(object: Id) -> Id {
    crate::fw::counted::autorelease(object)
}

extern "C" fn arc_retain_autorelease(object: Id) -> Id {
    crate::fw::counted::autorelease(crate::fw::counted::retain(object))
}

// Weak references: zeroed when a counted object goes (fw/counted.rs);
// for every other object they are plain stores, as objects never go.

extern "C" fn arc_store_weak(location: *mut Id, object: Id) -> Id {
    crate::fw::counted::store_weak(location, object)
}

/// objc_initWeak: `location` holds nothing yet (not even a valid old value).
extern "C" fn arc_init_weak(location: *mut Id, object: Id) -> Id {
    if location.is_null() {
        return 0;
    }
    unsafe { *location = 0 };
    crate::fw::counted::store_weak(location, object)
}

extern "C" fn arc_store_strong(location: *mut Id, object: Id) {
    if !location.is_null() {
        crate::fw::counted::retain(object);
        let old = unsafe { std::mem::replace(&mut *location, object) };
        crate::fw::counted::release(old);
    }
}

/// objc_loadWeakRetained: +1, as ARC releases it after use.
extern "C" fn arc_load_weak_retained(location: *const Id) -> Id {
    crate::fw::counted::load_weak_retained(location)
}

/// objc_loadWeak: retained and autoreleased, as on Apple.
extern "C" fn arc_load_weak(location: *const Id) -> Id {
    crate::fw::counted::autorelease(crate::fw::counted::load_weak_retained(location))
}

extern "C" fn arc_destroy_weak(location: *mut Id) {
    crate::fw::counted::store_weak(location, 0);
}

/// objc_copyWeak: `to` (new) refers weakly to what `from` does.
extern "C" fn arc_copy_weak(to: *mut Id, from: *mut Id) {
    if !to.is_null() && !from.is_null() {
        let object = crate::fw::counted::load_weak_retained(from);
        arc_init_weak(to, object);
        crate::fw::counted::release(object);
    }
}

/// objc_moveWeak: as copy, then `from` is left nil.
extern "C" fn arc_move_weak(to: *mut Id, from: *mut Id) {
    arc_copy_weak(to, from);
    arc_destroy_weak(from);
}

extern "C" fn arc_retain_block(block: usize) -> usize {
    crate::libsystem::mach::block_copy(block as *mut _) as usize
}

pub fn register(r: &mut Registry) {
    r.function("objc_unsafeClaimAutoreleasedReturnValue", arc_same as usize);
    r.function("objc_retain", arc_retain as usize);
    r.function("objc_retainAutoreleasedReturnValue", arc_retain as usize);
    r.function("objc_autorelease", arc_autorelease as usize);
    r.function("objc_autoreleaseReturnValue", arc_autorelease as usize);
    r.function("objc_retainAutorelease", arc_retain_autorelease as usize);
    r.function("objc_retainAutoreleaseReturnValue", arc_retain_autorelease as usize);
    r.function("objc_release", arc_release as usize);
    r.function("objc_storeStrong", arc_store_strong as usize);
    r.function("objc_storeWeak", arc_store_weak as usize);
    r.function("objc_initWeak", arc_init_weak as usize);
    r.function("objc_loadWeak", arc_load_weak as usize);
    r.function("objc_loadWeakRetained", arc_load_weak_retained as usize);
    r.function("objc_destroyWeak", arc_destroy_weak as usize);
    r.function("objc_copyWeak", arc_copy_weak as usize);
    r.function("objc_moveWeak", arc_move_weak as usize);
    r.function("objc_retainBlock", arc_retain_block as usize);
    r.function("objc_msgSend", runtime::ios_objc_msgSend as usize);
    r.function("objc_msgSendSuper2", runtime::ios_objc_msgSendSuper2 as usize);
    r.function("objc_msgSendSuper", runtime::ios_objc_msgSendSuper as usize);
    r.function("objc_autoreleasePoolPush", autorelease_pool_push as usize);
    r.function("objc_autoreleasePoolPop", autorelease_pool_pop as usize);
    r.function("objc_getProperty", get_property as usize);
    r.function("objc_setProperty", set_property as usize);
    r.function("objc_setProperty_atomic", set_property_plain as usize);
    r.function("objc_setProperty_nonatomic", set_property_plain as usize);
    r.function("objc_setProperty_atomic_copy", set_property_copy as usize);
    r.function("objc_setProperty_nonatomic_copy", set_property_copy as usize);
    r.function("objc_copyStruct", copy_struct as usize);
    r.function("objc_sync_enter", sync_enter as usize);
    r.function("objc_sync_exit", sync_exit as usize);
    r.function("objc_enumerationMutation", enumeration_mutation as usize);
    r.function("objc_exception_throw", exception_throw as usize);
    r.function("objc_exception_rethrow", exception_rethrow as usize);
    r.function("objc_terminate", terminate as usize);
    r.function("objc_getAssociatedObject", get_associated as usize);
    r.function("objc_setAssociatedObject", set_associated as usize);
    r.function("objc_getClass", objc_get_class as usize);
    r.function("objc_lookUpClass", objc_get_class as usize);
    r.function("sel_registerName", sel_register_name as usize);
    r.function("sel_getUid", sel_register_name as usize);
    r.function("sel_getName", sel_get_name as usize);
    r.function("object_getClass", object_get_class as usize);
    r.function("class_getName", class_get_name as usize);
    r.function("protocol_conformsToProtocol", protocol_conforms as usize);
    r.function("protocol_copyMethodDescriptionList", protocol_copy_methods as usize);
    r.function("protocol_copyProtocolList", protocol_copy_protocols as usize);
    r.function("__objc_personality_v0", personality as usize);
    r.data("_objc_empty_cache", leak([0usize; 4]));
    r.data("_objc_empty_vtable", leak([0usize; 4]));
    // Exception type records; only read while an exception is in flight.
    r.data("OBJC_EHTYPE_id", leak([0usize; 4]));
    r.data("OBJC_EHTYPE_$_NSException", leak([0usize; 4]));
}
