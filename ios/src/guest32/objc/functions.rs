//! The Objective-C runtime's C functions for 32-bit games (objc_msgSend
//! family, ARC and property helpers, class/ivar/method lookups, associated
//! objects, sync, exceptions) and the blocks runtime in its 32-bit layout.
//!
//! Each function takes the call's argument words (r0-r3, then the guest
//! stack) and returns r0 | r1 << 32. The adapter that registers them as
//! translator imports lives with the guest32 runner; nothing here needs it.
//!
//! 32-bit block layout: { isa, flags, reserved, invoke, descriptor } with
//! descriptor { reserved, size, [copy, dispose], [signature] };
//! __block variables: { isa, forwarding, flags, size, [keep, destroy], ... }.

use super::bridge::{
    call_host_function, framework_row, send_from_guest, Bridge, GuestSide, SendKind, Special, BLOCK_HAS_COPY_DISPOSE, BLOCK_IS_GLOBAL, BLOCK_NEEDS_FREE,
    BLOCK_REFCOUNT_MASK,
};
use super::classes::{Lookup, Super};
use super::convert::{GuestRet, HostCallOptions};
use super::encoding::Signature;
use super::selectors::Varargs;
use crate::objc::runtime;
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

pub struct Ctx<'a> {
    pub bridge: &'a Mutex<Bridge>,
    pub g: &'a dyn GuestSide,
}

type Words<'a> = &'a dyn Fn(u32) -> u32;
pub type ObjcFn = fn(&Ctx, Words) -> Result<u64, String>;

impl Ctx<'_> {
    fn lock(&self) -> MutexGuard<'_, Bridge> {
        self.bridge.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub(super) fn word(&self, addr: u32) -> u32 {
        u32::from_le_bytes(self.g.read(addr, 4).try_into().unwrap())
    }
    pub(super) fn set_word(&self, addr: u32, value: u32) {
        self.g.write(addr, &value.to_le_bytes())
    }
    pub(super) fn cstr(&self, addr: u32) -> String {
        if addr == 0 {
            return String::new();
        }
        unsafe { runtime::cstr(self.g.host_address(addr)) }
    }
    /// One guest C string per text (shared with selector names).
    pub(super) fn name(&self, text: &str) -> u32 {
        self.lock().guest_selector(self.g, text)
    }
    /// Sends `name` to a guest object from the bridge itself.
    pub fn send(&self, receiver: u32, name: &str, args: &[u32]) -> Result<u32, String> {
        if receiver == 0 {
            return Ok(0);
        }
        let sel = self.name(name);
        let words: Vec<u32> = [receiver, sel].iter().chain(args).copied().collect();
        let get = |i: u32| words.get(i as usize).copied().unwrap_or(0);
        Ok(send_from_guest(self.bridge, self.g, SendKind::Normal, &get)?.r0)
    }
    fn retain(&self, o: u32) -> Result<u32, String> {
        self.send(o, "retain", &[])?;
        Ok(o)
    }
    fn release(&self, o: u32) -> Result<(), String> {
        self.send(o, "release", &[]).map(|_| ())
    }
    fn autorelease(&self, o: u32) -> Result<u32, String> {
        self.send(o, "autorelease", &[])?;
        Ok(o)
    }
}

fn ok(value: u32) -> Result<u64, String> {
    Ok(u64::from(value))
}

/// Every function this file provides, by C name.
pub fn functions() -> &'static [(&'static str, ObjcFn)] {
    &[
        ("objc_msgSend", |c, w| send(c, w, SendKind::Normal)),
        ("objc_msgSend_stret", |c, w| send(c, w, SendKind::Stret)),
        ("objc_msgSendSuper2", |c, w| send(c, w, SendKind::Super2)),
        ("objc_msgSendSuper2_stret", |c, w| send(c, w, SendKind::Super2Stret)),
        ("objc_retain", |c, w| ok(c.retain(w(0))?)),
        ("objc_retainAutoreleasedReturnValue", |c, w| ok(c.retain(w(0))?)),
        ("objc_release", |c, w| c.release(w(0)).map(|_| 0)),
        ("objc_autorelease", |c, w| ok(c.autorelease(w(0))?)),
        ("objc_autoreleaseReturnValue", |c, w| ok(c.autorelease(w(0))?)),
        ("objc_retainAutorelease", |c, w| ok(c.autorelease(c.retain(w(0))?)?)),
        ("objc_retainAutoreleaseReturnValue", |c, w| ok(c.autorelease(c.retain(w(0))?)?)),
        ("objc_retainBlock", |c, w| ok(block_copy(c, w(0))?)),
        ("objc_storeStrong", store_strong),
        // Weak references: the slots are listed per object and cleared when a
        // game object deallocs (object_dying).
        ("objc_storeWeak", |c, w| ok(store_weak(c, w(0), w(1)))),
        ("objc_initWeak", |c, w| ok(store_weak(c, w(0), w(1)))),
        ("objc_loadWeak", |c, w| ok(c.word(w(0)))),
        ("objc_loadWeakRetained", |c, w| ok(c.retain(c.word(w(0)))?)),
        ("objc_destroyWeak", |c, w| ok(store_weak(c, w(0), 0))),
        ("objc_copyWeak", |c, w| {
            store_weak(c, w(0), c.word(w(1)));
            ok(0)
        }),
        ("objc_moveWeak", |c, w| {
            store_weak(c, w(0), c.word(w(1)));
            store_weak(c, w(1), 0);
            ok(0)
        }),
        ("objc_getProperty", get_property),
        ("objc_setProperty", set_property),
        ("objc_setProperty_atomic", |c, w| set_property_with(c, w(0), w(3), w(2), 0)),
        ("objc_setProperty_nonatomic", |c, w| set_property_with(c, w(0), w(3), w(2), 0)),
        ("objc_setProperty_atomic_copy", |c, w| set_property_with(c, w(0), w(3), w(2), 1)),
        ("objc_setProperty_nonatomic_copy", |c, w| set_property_with(c, w(0), w(3), w(2), 1)),
        ("objc_copyStruct", |c, w| {
            let bytes = c.g.read(w(1), w(2));
            c.g.write(w(0), &bytes);
            ok(0)
        }),
        ("objc_autoreleasePoolPush", pool_push),
        ("objc_autoreleasePoolPop", pool_pop),
        ("objc_sync_enter", |c, w| host_object_call(c, "_objc_sync_enter", w(0))),
        ("objc_sync_exit", |c, w| host_object_call(c, "_objc_sync_exit", w(0))),
        ("objc_enumerationMutation", |_, _| Err("a collection changed while it was being enumerated".into())),
        // objc_exception_throw needs the engine (exceptions.rs, registered by
        // wire.rs); catching and rethrowing bind to the guest C++ runtime.
        ("NSSetUncaughtExceptionHandler", |_, w| {
            super::exceptions::set_uncaught_handler(w(0));
            ok(0)
        }),
        ("NSGetUncaughtExceptionHandler", |_, _| ok(super::exceptions::uncaught_handler())),
        ("dispatch_once", super::dispatch::once),
        ("dispatch_async", super::dispatch::run_async),
        ("dispatch_sync", super::dispatch::run_sync),
        ("dispatch_get_global_queue", super::dispatch::global_queue),
        ("dispatch_queue_create", super::dispatch::queue_create),
        ("dispatch_release", |_, _| ok(0)),
        ("dispatch_retain", |_, _| ok(0)),
        ("dispatch_get_current_queue", super::dispatch::current_queue),
        ("dispatch_time", super::dispatch::time),
        ("dispatch_after", super::dispatch::after),
        ("dispatch_suspend", super::dispatch::suspend),
        ("dispatch_resume", super::dispatch::resume),
        ("dispatch_source_create", super::dispatch::source_create),
        ("dispatch_source_set_event_handler", super::dispatch::source_set_event_handler),
        ("dispatch_source_set_timer", super::dispatch::source_set_timer),
        ("dispatch_group_create", super::dispatch::group_create),
        ("dispatch_group_enter", super::dispatch::group_enter),
        ("dispatch_group_leave", super::dispatch::group_leave),
        ("dispatch_group_wait", super::dispatch::group_wait),
        ("dispatch_group_notify", super::dispatch::group_notify),
        ("dispatch_group_async", super::dispatch::group_async),
        ("objc_getAssociatedObject", |_, w| ok(with_associated(|m| m.get(&(w(0), w(1))).map_or(0, |v| v.0)))),
        ("objc_setAssociatedObject", set_associated),
        ("objc_removeAssociatedObjects", |c, w| remove_associated(c, w(0)).map(|_| 0)),
        ("objc_getClass", |c, w| ok(class_named(c, &c.cstr(w(0))))),
        ("objc_lookUpClass", |c, w| ok(class_named(c, &c.cstr(w(0))))),
        ("objc_getMetaClass", |c, w| {
            let class = class_named(c, &c.cstr(w(0)));
            ok(if class == 0 { 0 } else { c.word(class) })
        }),
        ("NSClassFromString", |c, w| {
            let text = c.lock().object_text(c.g, w(0)).unwrap_or_default();
            ok(class_named(c, &text))
        }),
        ("NSStringFromClass", |c, w| {
            let name = class_name(c, w(0));
            make_string(c, &name)
        }),
        ("NSSelectorFromString", |c, w| {
            let text = c.lock().object_text(c.g, w(0)).unwrap_or_default();
            ok(if text.is_empty() { 0 } else { c.name(&text) })
        }),
        ("NSStringFromSelector", |c, w| {
            let name = c.lock().selector_name(c.g, w(0));
            make_string(c, &name)
        }),
        ("sel_registerName", |c, w| ok(c.name(&c.cstr(w(0))))),
        ("sel_getUid", |c, w| ok(c.name(&c.cstr(w(0))))),
        ("sel_getName", |_, w| ok(w(0))),
        ("object_getClass", |c, w| ok(if w(0) == 0 { 0 } else { c.word(w(0)) })),
        ("object_getClassName", |c, w| {
            let name = if w(0) == 0 { "nil".to_string() } else { class_name(c, c.word(w(0))) };
            ok(c.name(&name))
        }),
        ("class_getName", |c, w| ok(c.name(&class_name(c, w(0))))),
        ("class_getSuperclass", |c, w| ok(superclass(c, w(0)))),
        ("class_getInstanceSize", |c, w| {
            let size = c.lock().classes.class(w(0)).map(|k| k.instance_size);
            ok(size.unwrap_or_else(|| framework_instance_size(c, w(0))))
        }),
        ("class_getInstanceVariable", instance_variable),
        ("ivar_getOffset", |c, w| ok(c.word(c.word(w(0))))),
        ("ivar_getName", |c, w| ok(c.word(w(0) + 4))),
        ("ivar_getTypeEncoding", |c, w| ok(c.word(w(0) + 8))),
        // Plain stores and loads at the ivar's offset (no retain, like Apple's
        // for ivars without a known ownership).
        ("object_getIvar", |c, w| ok(if w(0) == 0 || w(1) == 0 { 0 } else { c.word(w(0) + c.word(c.word(w(1)))) })),
        ("object_setIvar", |c, w| {
            if w(0) != 0 && w(1) != 0 {
                c.set_word(w(0) + c.word(c.word(w(1))), w(2));
            }
            ok(0)
        }),
        // Layouts only matter for garbage collection, which iOS never had.
        ("class_getIvarLayout", |_, _| ok(0)),
        ("class_addMethod", add_method),
        ("class_getInstanceMethod", |c, w| method(c, w(0), w(1))),
        ("class_getClassMethod", |c, w| method(c, if w(0) == 0 { 0 } else { c.word(w(0)) }, w(1))),
        ("method_getImplementation", |c, w| ok(c.word(w(0) + 8))),
        ("method_getTypeEncoding", |c, w| ok(c.word(w(0) + 4))),
        ("method_getName", |c, w| ok(c.word(w(0)))),
        ("protocol_getMethodDescription", method_description),
        ("class_copyPropertyList", copy_property_list),
        ("class_getProperty", class_property),
        ("property_getName", |c, w| ok(if w(0) == 0 { 0 } else { c.word(w(0)) })),
        ("property_getAttributes", |c, w| ok(if w(0) == 0 { 0 } else { c.word(w(0) + 4) })),
        ("NSAllocateObject", |c, w| {
            if w(0) == 0 {
                return ok(0);
            }
            let object = c.lock().allocate(c.g, w(0), w(1))?;
            super::bridge::construct_pending(c.bridge, c.g)?;
            ok(object)
        }),
        // Zones are only names (every allocation uses the one heap): a fixed block.
        ("NSDefaultMallocZone", |c, _| {
            static ZONE: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
            ok(*ZONE.get_or_init(|| {
                let zone = c.g.alloc_permanent(16);
                c.g.write(zone, &[0; 16]);
                zone
            }))
        }),
        ("NSGetSizeAndAlignment", get_size_and_alignment),
        ("_Block_copy", |c, w| ok(block_copy(c, w(0))?)),
        ("_Block_release", |c, w| block_release(c, w(0)).map(|_| 0)),
        ("_Block_object_assign", block_object_assign),
        ("_Block_object_dispose", block_object_dispose),
    ]
}

/// Calls the function named `name` (C name).
/// Foundation C functions that take or return objects (and that the C
/// framework layer, guest32::fw, does not provide), answered by the
/// host layer's function of the same name with converted types: name,
/// armv7 encoding of the result then the arguments (as in
/// framework_selectors.tsv, without self/_cmd), variadic arguments.
pub const HOST_FUNCTIONS: &[(&str, &str, Varargs)] = &[
    ("NSLog", "v@", Varargs::Format),
    // class_respondsToSelector(Class, SEL): BOOL
    ("class_respondsToSelector", "c#::", Varargs::None),
    // CFStringAppendFormat(string, format options, format, ...)
    ("CFStringAppendFormat", "v@@@", Varargs::Format),
    // The keychain: CFTypeRef results come back through `id *`.
    ("SecItemAdd", "i@^@", Varargs::None),
    ("SecItemCopyMatching", "i@^@", Varargs::None),
    ("SecItemUpdate", "i@@", Varargs::None),
    ("SecItemDelete", "i@", Varargs::None),
];

/// NSGetSizeAndAlignment(type, &size, &alignment): armv7 sizes; returns
/// the type after it, past its offset digits.
fn get_size_and_alignment(c: &Ctx, w: Words) -> Result<u64, String> {
    let (types, size, align) = (w(0), w(1), w(2));
    if types == 0 {
        return ok(0);
    }
    let text = c.cstr(types);
    let (ty, used) = super::encoding::parse_one(&text)?;
    if size != 0 {
        c.set_word(size, ty.guest_size());
    }
    if align != 0 {
        c.set_word(align, ty.guest_align());
    }
    ok(types + used as u32)
}

pub fn call_function(ctx: &Ctx, name: &str, words: Words) -> Result<u64, String> {
    static TABLE: std::sync::OnceLock<HashMap<&'static str, ObjcFn>> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| functions().iter().copied().collect());
    if let Some(f) = table.get(name) {
        return f(ctx, words);
    }
    let &(_, encoding, varargs) =
        HOST_FUNCTIONS.iter().find(|f| f.0 == name).ok_or_else(|| format!("{name} is not an Objective-C runtime function"))?;
    let (address, _) = crate::symbols::lookup(&format!("_{name}")).ok_or_else(|| format!("the host layer has no {name}"))?;
    let sig = Signature::parse(encoding)?;
    let options = HostCallOptions { varargs, ..Default::default() };
    let r = call_host_function(ctx.bridge, ctx.g, address, &sig, options, words)?;
    Ok(u64::from(r.r0) | u64::from(r.r1) << 32)
}

fn send(c: &Ctx, w: Words, kind: SendKind) -> Result<u64, String> {
    let keep_going = crate::trap::KEEP_GOING.load(std::sync::atomic::Ordering::Relaxed);
    let r = skip_missing(send_from_guest(c.bridge, c.g, kind, w), keep_going).map_err(|error| {
        // Which message failed, for the log.
        let stret = matches!(kind, SendKind::Stret | SendKind::Super2Stret);
        let name = c.lock().selector_name(c.g, w(u32::from(stret) + 1));
        format!("{error} (message {name})")
    })?;
    Ok(u64::from(r.r0) | u64::from(r.r1) << 32)
}

/// A message to a method nobody has: when the run keeps going (always on the
/// phone), only that message is skipped and the game gets 0 back, with an
/// error in the log (once per method) and the method in the missing list;
/// otherwise, and for every other failure, the game's call stops as before.
pub(super) fn skip_missing(result: Result<GuestRet, String>, keep_going: bool) -> Result<GuestRet, String> {
    match result {
        Err(error) if keep_going && error.starts_with(super::bridge::MISSING_METHOD) => {
            // "-[Class selector:]", from "missing method -[Class selector:]: why".
            let rest = &error[super::bridge::MISSING_METHOD.len()..];
            let name = rest.split(": ").next().unwrap_or(rest);
            let name = if name.ends_with(']') { name.to_string() } else { format!("{name}]") };
            if crate::objc::runtime::note_missing_method(&name) {
                crate::log!("ERROR: {error}; skipped, the game got 0");
            }
            Ok(GuestRet { r0: 0, r1: 0 })
        }
        other => other,
    }
}

fn store_strong(c: &Ctx, w: Words) -> Result<u64, String> {
    let (location, object) = (w(0), w(1));
    let old = c.word(location);
    if old != object {
        c.retain(object)?;
        c.set_word(location, object);
        c.release(old)?;
    }
    ok(0)
}

/// objc_getProperty(self, _cmd, offset, atomic)
fn get_property(c: &Ctx, w: Words) -> Result<u64, String> {
    let value = c.word(w(0).wrapping_add(w(2)));
    if w(3) & 0xff != 0 {
        return ok(c.autorelease(c.retain(value)?)?);
    }
    ok(value)
}

/// objc_setProperty(self, _cmd, offset, newValue, atomic, shouldCopy)
fn set_property(c: &Ctx, w: Words) -> Result<u64, String> {
    set_property_with(c, w(0), w(2), w(3), w(5) as i8)
}

fn set_property_with(c: &Ctx, object: u32, offset: u32, value: u32, copy: i8) -> Result<u64, String> {
    let new = match copy {
        0 => c.retain(value)?,
        2 => c.send(value, "mutableCopy", &[])?,
        _ => c.send(value, "copy", &[])?,
    };
    let slot = object.wrapping_add(offset);
    let old = c.word(slot);
    c.set_word(slot, new);
    c.release(old)?;
    ok(0)
}

/// Host autorelease pool tokens are 64-bit: the guest gets a number of its
/// own. Pools nest per thread, like Apple's.
static NEXT_POOL: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

thread_local! {
    /// This thread's pools, innermost last: (guest token, host token).
    static POOLS: std::cell::RefCell<Vec<(u32, usize)>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn host_function(name: &str) -> Option<usize> {
    crate::symbols::lookup(name).map(|s| s.0)
}

fn pool_push(_: &Ctx, _: Words) -> Result<u64, String> {
    let token = match host_function("_objc_autoreleasePoolPush") {
        Some(f) => super::mirror::call_host(f, &Default::default()).x[0] as usize,
        None => 0,
    };
    let guest = NEXT_POOL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    POOLS.with(|p| p.borrow_mut().push((guest, token)));
    ok(guest)
}

/// Pops this thread's pool `w(0)` and every pool pushed after it.
fn pool_pop(_: &Ctx, w: Words) -> Result<u64, String> {
    let guest = w(0);
    let token = POOLS.with(|p| {
        let mut pools = p.borrow_mut();
        let at = pools.iter().rposition(|(g, _)| *g == guest)?;
        let token = pools[at].1;
        pools.truncate(at);
        Some(token)
    });
    let Some(token) = token else {
        return Err(format!("objc_autoreleasePoolPop: pool {guest} is not one of this thread's"));
    };
    if let Some(f) = host_function("_objc_autoreleasePoolPop") {
        let mut args = super::convert::HostArgs::default();
        args.x[0] = token as u64;
        super::mirror::call_host(f, &args);
    }
    ok(0)
}

/// Calls a host C function that takes one object (sync enter/exit).
fn host_object_call(c: &Ctx, name: &str, object: u32) -> Result<u64, String> {
    let host = c.lock().host_object_for(c.g, object)?;
    let f = host_function(name).ok_or_else(|| format!("the host layer has no {name}"))?;
    let mut args = super::convert::HostArgs::default();
    args.x[0] = host as u64;
    ok(super::mirror::call_host(f, &args).x[0] as u32)
}

/// "Name: reason" of a guest exception object.
pub(super) fn describe_exception(c: &Ctx, exception: u32) -> String {
    let name = c.send(exception, "name", &[]).ok().and_then(|n| c.lock().object_text(c.g, n)).unwrap_or_default();
    let reason = c.send(exception, "reason", &[]).ok().and_then(|r| c.lock().object_text(c.g, r)).unwrap_or_default();
    format!("{name}: {reason}")
}

/// (object, key) -> (value, policy) for objc_setAssociatedObject.
fn with_associated<R>(f: impl FnOnce(&mut HashMap<(u32, u32), (u32, u32)>) -> R) -> R {
    static MAP: Mutex<Option<HashMap<(u32, u32), (u32, u32)>>> = Mutex::new(None);
    f(MAP.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new))
}

/// Removes every value associated with `object`, releasing each by its policy.
fn remove_associated(c: &Ctx, object: u32) -> Result<(), String> {
    let removed: Vec<(u32, u32)> = with_associated(|m| {
        let keys: Vec<_> = m.keys().filter(|k| k.0 == object).copied().collect();
        keys.iter().filter_map(|k| m.remove(k)).collect()
    });
    for old in removed {
        release_associated(c, old)?;
    }
    Ok(())
}

/// Object -> the guest slots holding weak references to it.
fn with_weak<R>(f: impl FnOnce(&mut HashMap<u32, Vec<u32>>) -> R) -> R {
    static MAP: Mutex<Option<HashMap<u32, Vec<u32>>>> = Mutex::new(None);
    f(MAP.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new))
}

/// objc_storeWeak(location, object): the slot now refers weakly to `object`.
fn store_weak(c: &Ctx, location: u32, object: u32) -> u32 {
    with_weak(|m| {
        let old = c.word(location);
        if let Some(slots) = m.get_mut(&old) {
            slots.retain(|&s| s != location);
            if slots.is_empty() {
                m.remove(&old);
            }
        }
        if object != 0 {
            m.entry(object).or_default().push(location);
        }
        c.set_word(location, object);
    });
    object
}

/// What Apple's objc_destructInstance does after the C++ destructors when a
/// game object deallocs: its associated objects are released and weak
/// references to it read nil from now on.
pub(super) fn object_dying(c: &Ctx, object: u32) -> Result<(), String> {
    remove_associated(c, object)?;
    for slot in with_weak(|m| m.remove(&object)).unwrap_or_default() {
        if c.word(slot) == object {
            c.set_word(slot, 0);
        }
    }
    Ok(())
}

/// A replaced or removed value is released by the policy it was set with.
fn release_associated(c: &Ctx, (value, policy): (u32, u32)) -> Result<(), String> {
    if matches!(policy & 0xff, 1 | 3) {
        c.release(value)?;
    }
    Ok(())
}

/// objc_setAssociatedObject(object, key, value, policy)
fn set_associated(c: &Ctx, w: Words) -> Result<u64, String> {
    let (object, key, value, policy) = (w(0), w(1), w(2), w(3));
    let new = match policy & 0xff {
        1 => c.retain(value)?,
        3 => c.send(value, "copy", &[])?,
        _ => value,
    };
    let old = with_associated(|m| if new == 0 { m.remove(&(object, key)) } else { m.insert((object, key), (new, policy)) });
    if let Some(old) = old {
        release_associated(c, old)?;
    }
    ok(0)
}

/// A class by name: the game's own, else the host's (as its guest copy), else nil.
fn class_named(c: &Ctx, name: &str) -> u32 {
    let mut b = c.lock();
    if let Some(class) = b.classes.class_named(name) {
        return class.address;
    }
    if runtime::existing_class(name, false).is_some() {
        return b.framework_class(c.g, name).0;
    }
    0
}

pub(super) fn class_name(c: &Ctx, class: u32) -> String {
    let b = c.lock();
    if let Some(k) = b.classes.class(class) {
        return k.name.clone();
    }
    drop(b);
    if class == 0 {
        return "nil".into();
    }
    // A framework class copy: class_t.data -> class_ro_t.name.
    let ro = c.word(class + 16) & !3;
    c.cstr(c.word(ro + 16))
}

pub(super) fn superclass(c: &Ctx, class: u32) -> u32 {
    if class == 0 {
        return 0;
    }
    let mut b = c.lock();
    match b.classes.class(class).map(|k| (k.superclass.clone(), k.meta)) {
        Some((Super::Game(parent), _)) => parent,
        Some((Super::Framework(name), meta)) => {
            let (cls, m) = b.framework_class(c.g, &name);
            if meta {
                m
            } else {
                cls
            }
        }
        Some((Super::None, _)) => 0,
        None => {
            drop(b);
            c.word(class + 4)
        }
    }
}

fn framework_instance_size(c: &Ctx, class: u32) -> u32 {
    if class == 0 {
        return 0;
    }
    c.word((c.word(class + 16) & !3) + 8)
}

/// class_getInstanceVariable(class, name): a guest ivar_t copy.
fn instance_variable(c: &Ctx, w: Words) -> Result<u64, String> {
    static IVARS: Mutex<Option<HashMap<(u32, String), u32>>> = Mutex::new(None);
    let (mut class, name) = (w(0), c.cstr(w(1)));
    let found = {
        let b = c.lock();
        let mut found = None;
        while let Some(k) = b.classes.class(class) {
            if let Some(ivar) = k.ivars.iter().find(|i| i.name == name) {
                found = Some((class, ivar.clone()));
                break;
            }
            match k.superclass {
                Super::Game(parent) => class = parent,
                _ => break,
            }
        }
        found
    };
    let Some((owner, ivar)) = found else { return ok(0) };
    let mut cache = IVARS.lock().unwrap_or_else(|e| e.into_inner());
    let cache = cache.get_or_insert_with(HashMap::new);
    if let Some(&existing) = cache.get(&(owner, name.clone())) {
        return ok(existing);
    }
    let (name_c, type_c) = (c.name(&ivar.name), c.name(&ivar.type_encoding));
    let entry = c.g.alloc_permanent(20);
    let words = [ivar.offset_address, name_c, type_c, ivar.alignment, ivar.size];
    c.g.write(entry, &words.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>());
    cache.insert((owner, name), entry);
    ok(entry)
}

/// The guest property_t { name, attributes } of a declaration: the image's
/// own entry, so each declaration (a category may redeclare a property with
/// other attributes) is its own, as on Apple. Metadata without an address
/// gets an entry made once per declaration.
fn property_entry(c: &Ctx, class: u32, property: &guest32_image::Property32) -> u32 {
    static ENTRIES: Mutex<Option<HashMap<(u32, String, String), u32>>> = Mutex::new(None);
    if property.address != 0 {
        return property.address;
    }
    let key = (class, property.name.clone(), property.attributes.clone());
    if let Some(&entry) = ENTRIES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get(&key) {
        return entry;
    }
    let (name, attributes) = (c.name(&property.name), c.name(&property.attributes));
    let entry = c.g.alloc_permanent(8);
    c.g.write(entry, &[name.to_le_bytes(), attributes.to_le_bytes()].concat());
    *ENTRIES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).entry(key).or_insert(entry)
}

/// class_copyPropertyList(class, *count): the class's own properties (not
/// its superclasses') in a NULL-ended list from the game's heap, which the
/// game frees; NULL when there are none. Framework classes list none: the
/// host layer keeps no property declarations.
fn copy_property_list(c: &Ctx, w: Words) -> Result<u64, String> {
    let (class, count) = (w(0), w(1));
    let properties = c.lock().classes.class(class).map(|k| k.properties.clone()).unwrap_or_default();
    if count != 0 {
        c.set_word(count, properties.len() as u32);
    }
    if properties.is_empty() {
        return ok(0);
    }
    let entries: Vec<u32> = properties.iter().map(|p| property_entry(c, class, p)).chain([0]).collect();
    let list = c.g.alloc(4 * entries.len() as u32);
    c.g.write(list, &entries.iter().flat_map(|e| e.to_le_bytes()).collect::<Vec<_>>());
    ok(list)
}

/// class_getProperty(class, name): searches the superclasses too.
fn class_property(c: &Ctx, w: Words) -> Result<u64, String> {
    let (mut class, name) = (w(0), c.cstr(w(1)));
    for _ in 0..256 {
        let (found, parent) = {
            let b = c.lock();
            let Some(k) = b.classes.class(class) else { return ok(0) };
            (k.properties.iter().find(|p| p.name == name).cloned(), k.superclass.clone())
        };
        match (found, parent) {
            (Some(property), _) => return ok(property_entry(c, class, &property)),
            (None, Super::Game(next)) => class = next,
            _ => break,
        }
    }
    ok(0)
}

/// class_addMethod(class, sel, imp, types)
fn add_method(c: &Ctx, w: Words) -> Result<u64, String> {
    let (class, sel, imp) = (w(0), w(1), w(2));
    let types = c.cstr(w(3));
    let mut b = c.lock();
    let name = b.selector_name(c.g, sel);
    if let Some(k) = b.classes.class(class).map(|k| (k.meta, k.partner)) {
        let added = b.classes.add_method(class, &name, imp, &types);
        if added {
            let (meta, partner) = k;
            let mirror = b.mirror_of(if meta { partner } else { class });
            if let Some(mirror) = mirror {
                let host = if meta { runtime::lock().classes[&mirror].partner } else { mirror };
                super::mirror::add_mirror_method(host, &name);
            }
        }
        return ok(u32::from(added));
    }
    let framework = {
        let ro = c.word(class + 16) & !3;
        (c.cstr(c.word(ro + 16)), c.word(ro) & 1 != 0)
    };
    let added = b.classes.add_framework_method(&framework.0, framework.1, &name, imp, &types);
    if added {
        super::mirror::add_mirror_method(Bridge::host_class_named(&framework.0, framework.1), &name);
    }
    ok(u32::from(added))
}

/// class_getInstanceMethod: a guest method_t { name, types, imp } copy.
fn method(c: &Ctx, class: u32, sel: u32) -> Result<u64, String> {
    static METHODS: Mutex<Option<HashMap<(u32, String), u32>>> = Mutex::new(None);
    if class == 0 || sel == 0 {
        return ok(0);
    }
    let mut b = c.lock();
    let name = b.selector_name(c.g, sel);
    let found = if b.classes.class(class).is_some() {
        match b.classes.lookup(class, &name) {
            Lookup::Guest(m) => Some((m.imp, m.types.clone())),
            Lookup::Framework { class, .. } => framework_row(&class, &name).map(|e| (super::bridge::message_stub(c.g, Some(e.encoding)), e.encoding.to_string())),
            Lookup::Missing => None,
        }
    } else {
        // A framework class: its own row, as its messages use.
        let (class, _) = b.class_display(class);
        framework_row(&class, &name).map(|e| (super::bridge::message_stub(c.g, Some(e.encoding)), e.encoding.to_string()))
    };
    let Some((imp, types)) = found else { return ok(0) };
    let mut cache = METHODS.lock().unwrap_or_else(|e| e.into_inner());
    let cache = cache.get_or_insert_with(HashMap::new);
    let key = (class, name.clone());
    let entry = match cache.get(&key) {
        Some(&entry) => entry,
        None => {
            let entry = c.g.alloc_permanent(12);
            cache.insert(key, entry);
            entry
        }
    };
    let types_c = b.guest_selector(c.g, &types);
    let canonical = b.guest_selector(c.g, &name);
    drop(b);
    c.g.write(entry, &[canonical, types_c, imp].iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>());
    ok(entry)
}

/// protocol_getMethodDescription(result, protocol, sel, required, instance):
/// struct objc_method_description { SEL name; char *types; } via stret.
fn method_description(c: &Ctx, w: Words) -> Result<u64, String> {
    let (out, protocol, sel, required, instance) = (w(0), w(1), w(2), w(3) & 0xff != 0, w(4) & 0xff != 0);
    let name = c.lock().selector_name(c.g, sel);
    let found = find_in_protocol(c, protocol, &name, required, instance, 0);
    let (sel_out, types) = found.map(|t| (c.name(&name), t)).unwrap_or((0, 0));
    c.g.write(out, &[sel_out.to_le_bytes(), types.to_le_bytes()].concat());
    ok(out)
}

/// protocol_t (32-bit): isa, name, protocols, instanceMethods, classMethods,
/// optionalInstanceMethods, optionalClassMethods, ...
fn find_in_protocol(c: &Ctx, protocol: u32, name: &str, required: bool, instance: bool, depth: u32) -> Option<u32> {
    if protocol == 0 || depth > 16 {
        return None;
    }
    let field = match (required, instance) {
        (true, true) => 3,
        (true, false) => 4,
        (false, true) => 5,
        (false, false) => 6,
    };
    let list = c.word(protocol + 4 * field);
    if list != 0 {
        let (entry_size, count) = (c.word(list) & !3, c.word(list + 4));
        for i in 0..count {
            let at = list + 8 + i * entry_size;
            if c.cstr(c.word(at)) == name {
                return Some(c.word(at + 4));
            }
        }
    }
    let parents = c.word(protocol + 8);
    if parents != 0 {
        for i in 0..c.word(parents) {
            if let Some(t) = find_in_protocol(c, c.word(parents + 4 + 4 * i), name, required, instance, depth + 1) {
                return Some(t);
            }
        }
    }
    None
}

fn make_string(c: &Ctx, text: &str) -> Result<u64, String> {
    let host = crate::fw::string::make(text);
    ok(c.lock().guest_object(c.g, host)?)
}

// ---------- blocks ----------

/// The flags word of a heap block or __block variable at guest `at`, as an
/// atomic: guest block functions and the host's copy/dispose helpers (which
/// run without the guest lock) change its count from any thread.
fn flags_word(g: &dyn GuestSide, at: u32) -> &std::sync::atomic::AtomicU32 {
    unsafe { &*(g.host_address(at) as *const std::sync::atomic::AtomicU32) }
}

/// The flags word of a heap block or __block variable, read atomically
/// (its count may be changing on another thread).
pub fn load_flags(g: &dyn GuestSide, at: u32) -> u32 {
    flags_word(g, at).load(std::sync::atomic::Ordering::Acquire)
}

/// One more reference (libclosure's latching increment): a saturated count
/// stays as it is.
pub fn count_retain(g: &dyn GuestSide, at: u32) {
    use std::sync::atomic::Ordering;
    let _ = flags_word(g, at).fetch_update(Ordering::AcqRel, Ordering::Acquire, |flags| {
        (flags & BLOCK_REFCOUNT_MASK != BLOCK_REFCOUNT_MASK).then(|| flags + 2)
    });
}

/// One reference less; true when it was the last (the caller disposes and
/// frees). Saturated and zero counts stay as they are.
pub fn count_release(g: &dyn GuestSide, at: u32) -> bool {
    use std::sync::atomic::Ordering;
    let before = flags_word(g, at).fetch_update(Ordering::AcqRel, Ordering::Acquire, |flags| {
        let count = flags & BLOCK_REFCOUNT_MASK;
        (count != BLOCK_REFCOUNT_MASK && count != 0).then(|| flags - 2)
    });
    before.is_ok_and(|flags| flags & BLOCK_REFCOUNT_MASK == 2)
}

/// _Block_copy: a heap block (a stack block is copied; heap blocks gain a
/// reference; global blocks stay as they are).
pub fn block_copy(c: &Ctx, block: u32) -> Result<u32, String> {
    if block == 0 {
        return Ok(0);
    }
    let flags = load_flags(c.g, block + 4);
    if flags & BLOCK_NEEDS_FREE != 0 {
        count_retain(c.g, block + 4);
        return Ok(block);
    }
    let malloc_isa = {
        let b = c.lock();
        if flags & BLOCK_IS_GLOBAL != 0 || b.special(c.word(block)) == Some(Special::GlobalBlock) {
            return Ok(block);
        }
        b.special_isa(Special::MallocBlock)
    };
    let descriptor = c.word(block + 16);
    let size = c.word(descriptor + 4).max(20);
    let copy = c.g.alloc(size);
    c.g.write(copy, &c.g.read(block, size));
    c.set_word(copy + 4, (flags & !BLOCK_REFCOUNT_MASK) | BLOCK_NEEDS_FREE | 2);
    if let Some(isa) = malloc_isa {
        c.set_word(copy, isa);
    }
    if flags & BLOCK_HAS_COPY_DISPOSE != 0 {
        c.g.call(c.word(descriptor + 8), &[copy, block])?;
    }
    Ok(copy)
}

/// _Block_release: drops a heap block's reference; the last one runs its
/// dispose helper and frees it.
pub fn block_release(c: &Ctx, block: u32) -> Result<(), String> {
    release_guest_block(c.g, block)
}

/// The same, with guest memory only (the host-side block helpers use it).
pub fn release_guest_block(g: &dyn GuestSide, block: u32) -> Result<(), String> {
    if block == 0 {
        return Ok(());
    }
    let word = |addr: u32| u32::from_le_bytes(g.read(addr, 4).try_into().unwrap());
    let flags = load_flags(g, block + 4);
    if flags & BLOCK_NEEDS_FREE == 0 {
        return Ok(());
    }
    if count_release(g, block + 4) {
        if flags & BLOCK_HAS_COPY_DISPOSE != 0 {
            let descriptor = word(block + 16);
            g.call(word(descriptor + 12), &[block])?;
        }
        g.free(block);
    }
    Ok(())
}

const FIELD_IS_OBJECT: u32 = 3;
const FIELD_IS_BLOCK: u32 = 7;
const FIELD_IS_BYREF: u32 = 8;
const BYREF_CALLER: u32 = 128;

/// _Block_object_assign(destination, object, flags): what a block's copy
/// helper does for each captured object, block or __block variable. A
/// __block variable's own value (BLOCK_BYREF_CALLER) is assigned as it is.
fn block_object_assign(c: &Ctx, w: Words) -> Result<u64, String> {
    let (destination, object, flags) = (w(0), w(1), w(2));
    let value = match flags & 0x1f {
        _ if flags & BYREF_CALLER != 0 => object,
        FIELD_IS_OBJECT => c.retain(object)?,
        FIELD_IS_BLOCK => block_copy(c, object)?,
        FIELD_IS_BYREF => byref_copy(c, object)?,
        _ => object,
    };
    c.set_word(destination, value);
    ok(0)
}

/// _Block_object_dispose(object, flags)
fn block_object_dispose(c: &Ctx, w: Words) -> Result<u64, String> {
    let (object, flags) = (w(0), w(1));
    match flags & 0x1f {
        _ if flags & BYREF_CALLER != 0 => {}
        FIELD_IS_OBJECT => c.release(object)?,
        FIELD_IS_BLOCK => block_release(c, object)?,
        FIELD_IS_BYREF => byref_release(c, object)?,
        _ => {}
    }
    ok(0)
}

fn byref_copy(c: &Ctx, byref: u32) -> Result<u32, String> {
    let current = c.word(byref + 4);
    let flags = load_flags(c.g, current + 8);
    if flags & BLOCK_NEEDS_FREE != 0 {
        count_retain(c.g, current + 8);
        return Ok(current);
    }
    let size = c.word(byref + 12).max(16);
    let copy = c.g.alloc(size);
    c.g.write(copy, &c.g.read(byref, size));
    // Two references: the stack frame's and the copier's.
    c.set_word(copy + 8, (flags & !BLOCK_REFCOUNT_MASK) | BLOCK_NEEDS_FREE | 4);
    c.set_word(copy + 4, copy);
    c.set_word(byref + 4, copy);
    if flags & BLOCK_HAS_COPY_DISPOSE != 0 {
        c.g.call(c.word(byref + 16), &[copy, byref])?;
    }
    Ok(copy)
}

fn byref_release(c: &Ctx, byref: u32) -> Result<(), String> {
    let current = c.word(byref + 4);
    let flags = load_flags(c.g, current + 8);
    if flags & BLOCK_NEEDS_FREE == 0 {
        return Ok(());
    }
    if count_release(c.g, current + 8) {
        if flags & BLOCK_HAS_COPY_DISPOSE != 0 {
            c.g.call(c.word(current + 20), &[current])?;
        }
        c.g.free(current);
    }
    Ok(())
}

/// A guest block as a host block (copied to the heap first), for C
/// functions that take blocks (dispatch_async, ...).
pub fn block_to_host(c: &Ctx, block: u32) -> Result<usize, String> {
    let heap = block_copy(c, block)?;
    let host = c.lock().host_object_for(c.g, heap);
    // The host wrapper holds its own reference; this copy's goes.
    block_release(c, heap)?;
    host
}
