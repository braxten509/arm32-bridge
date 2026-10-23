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
    send_from_guest, Bridge, GuestSide, SendKind, Special, BLOCK_HAS_COPY_DISPOSE, BLOCK_IS_GLOBAL, BLOCK_NEEDS_FREE,
    BLOCK_REFCOUNT_MASK,
};
use super::classes::{Lookup, Super};
use super::selectors::FrameworkSelectors;
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
    fn word(&self, addr: u32) -> u32 {
        u32::from_le_bytes(self.g.read(addr, 4).try_into().unwrap())
    }
    fn set_word(&self, addr: u32, value: u32) {
        self.g.write(addr, &value.to_le_bytes())
    }
    fn cstr(&self, addr: u32) -> String {
        if addr == 0 {
            return String::new();
        }
        unsafe { runtime::cstr(self.g.host_address(addr)) }
    }
    /// One guest C string per text (shared with selector names).
    fn name(&self, text: &str) -> u32 {
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
        // Weak references are plain stores here (not zeroed when the object goes).
        ("objc_storeWeak", |c, w| {
            c.set_word(w(0), w(1));
            ok(w(1))
        }),
        ("objc_initWeak", |c, w| {
            c.set_word(w(0), w(1));
            ok(w(1))
        }),
        ("objc_loadWeak", |c, w| ok(c.word(w(0)))),
        ("objc_loadWeakRetained", |c, w| ok(c.retain(c.word(w(0)))?)),
        ("objc_destroyWeak", |c, w| {
            c.set_word(w(0), 0);
            ok(0)
        }),
        ("objc_copyWeak", |c, w| {
            c.set_word(w(0), c.word(w(1)));
            ok(0)
        }),
        ("objc_moveWeak", |c, w| {
            c.set_word(w(0), c.word(w(1)));
            c.set_word(w(1), 0);
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
        ("objc_exception_throw", exception_throw),
        ("objc_exception_rethrow", |_, _| Err("objc_exception_rethrow: Objective-C exceptions are not supported yet".into())),
        ("objc_begin_catch", |_, _| Err("objc_begin_catch: Objective-C exceptions are not supported yet".into())),
        ("objc_end_catch", |_, _| Err("objc_end_catch: Objective-C exceptions are not supported yet".into())),
        ("objc_terminate", |_, _| Err("objc_terminate called".into())),
        ("objc_getAssociatedObject", |_, w| ok(with_associated(|m| m.get(&(w(0), w(1))).copied().unwrap_or(0)))),
        ("objc_setAssociatedObject", set_associated),
        ("objc_removeAssociatedObjects", |_, w| {
            with_associated(|m| m.retain(|k, _| k.0 != w(0)));
            ok(0)
        }),
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
        // Layouts only matter for garbage collection, which iOS never had.
        ("class_getIvarLayout", |_, _| ok(0)),
        ("class_addMethod", add_method),
        ("class_getInstanceMethod", |c, w| method(c, w(0), w(1))),
        ("class_getClassMethod", |c, w| method(c, if w(0) == 0 { 0 } else { c.word(w(0)) }, w(1))),
        ("method_getImplementation", |c, w| ok(c.word(w(0) + 8))),
        ("method_getTypeEncoding", |c, w| ok(c.word(w(0) + 4))),
        ("method_getName", |c, w| ok(c.word(w(0)))),
        ("protocol_getMethodDescription", method_description),
        ("_Block_copy", |c, w| ok(block_copy(c, w(0))?)),
        ("_Block_release", |c, w| block_release(c, w(0)).map(|_| 0)),
        ("_Block_object_assign", block_object_assign),
        ("_Block_object_dispose", block_object_dispose),
    ]
}

/// Calls the function named `name` (C name).
pub fn call_function(ctx: &Ctx, name: &str, words: Words) -> Result<u64, String> {
    static TABLE: std::sync::OnceLock<HashMap<&'static str, ObjcFn>> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| functions().iter().copied().collect());
    let f = table.get(name).ok_or_else(|| format!("{name} is not an Objective-C runtime function"))?;
    f(ctx, words)
}

fn send(c: &Ctx, w: Words, kind: SendKind) -> Result<u64, String> {
    let r = send_from_guest(c.bridge, c.g, kind, w)?;
    Ok(u64::from(r.r0) | u64::from(r.r1) << 32)
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

/// Host autorelease pool tokens are 64-bit: the guest gets an index.
static POOLS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

fn host_function(name: &str) -> Option<usize> {
    crate::symbols::lookup(name).map(|s| s.0)
}

fn pool_push(_: &Ctx, _: Words) -> Result<u64, String> {
    let token = match host_function("_objc_autoreleasePoolPush") {
        Some(f) => super::mirror::call_host(f, &Default::default()).x[0] as usize,
        None => 0,
    };
    let mut pools = POOLS.lock().unwrap_or_else(|e| e.into_inner());
    pools.push(token);
    ok(pools.len() as u32)
}

fn pool_pop(_: &Ctx, w: Words) -> Result<u64, String> {
    let index = w(0) as usize;
    let token = {
        let mut pools = POOLS.lock().unwrap_or_else(|e| e.into_inner());
        if index == 0 || index > pools.len() {
            return Err(format!("objc_autoreleasePoolPop: unknown pool {index}"));
        }
        let token = pools[index - 1];
        pools.truncate(index - 1);
        token
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

fn exception_throw(c: &Ctx, w: Words) -> Result<u64, String> {
    let exception = w(0);
    let name = c.send(exception, "name", &[]).ok().and_then(|n| c.lock().object_text(c.g, n)).unwrap_or_default();
    let reason = c.send(exception, "reason", &[]).ok().and_then(|r| c.lock().object_text(c.g, r)).unwrap_or_default();
    Err(format!("the game threw an Objective-C exception: {name}: {reason}"))
}

/// (object, key) -> value for objc_setAssociatedObject.
fn with_associated<R>(f: impl FnOnce(&mut HashMap<(u32, u32), u32>) -> R) -> R {
    static MAP: Mutex<Option<HashMap<(u32, u32), u32>>> = Mutex::new(None);
    f(MAP.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new))
}

/// objc_setAssociatedObject(object, key, value, policy)
fn set_associated(c: &Ctx, w: Words) -> Result<u64, String> {
    let (object, key, value, policy) = (w(0), w(1), w(2), w(3));
    let new = match policy & 0xff {
        1 => c.retain(value)?,
        3 => c.send(value, "copy", &[])?,
        _ => value,
    };
    let old = with_associated(|m| if new == 0 { m.remove(&(object, key)) } else { m.insert((object, key), new) });
    if let (Some(old), 1 | 3) = (old, policy & 0xff) {
        c.release(old)?;
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

fn class_name(c: &Ctx, class: u32) -> String {
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

fn superclass(c: &Ctx, class: u32) -> u32 {
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
    ok(u32::from(b.classes.add_framework_method(&framework.0, framework.1, &name, imp, &types)))
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
            Lookup::Framework { .. } => FrameworkSelectors::builtin().get(&name).map(|e| (b.msg_send_stub, e.encoding.to_string())),
            Lookup::Missing => None,
        }
    } else {
        FrameworkSelectors::builtin().get(&name).map(|e| (b.msg_send_stub, e.encoding.to_string()))
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

/// _Block_copy: a heap block (a stack block is copied; heap blocks gain a
/// reference; global blocks stay as they are).
pub fn block_copy(c: &Ctx, block: u32) -> Result<u32, String> {
    if block == 0 {
        return Ok(0);
    }
    let flags = c.word(block + 4);
    if flags & BLOCK_NEEDS_FREE != 0 {
        c.set_word(block + 4, (flags & !BLOCK_REFCOUNT_MASK) | ((flags & BLOCK_REFCOUNT_MASK) + 2).min(BLOCK_REFCOUNT_MASK));
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
    if block == 0 {
        return Ok(());
    }
    let flags = c.word(block + 4);
    if flags & BLOCK_NEEDS_FREE == 0 {
        return Ok(());
    }
    let count = (flags & BLOCK_REFCOUNT_MASK).saturating_sub(2);
    c.set_word(block + 4, (flags & !BLOCK_REFCOUNT_MASK) | count);
    if count == 0 {
        if flags & BLOCK_HAS_COPY_DISPOSE != 0 {
            let descriptor = c.word(block + 16);
            c.g.call(c.word(descriptor + 12), &[block])?;
        }
        c.g.free(block);
    }
    Ok(())
}

const FIELD_IS_OBJECT: u32 = 3;
const FIELD_IS_BLOCK: u32 = 7;
const FIELD_IS_BYREF: u32 = 8;
const BYREF_CALLER: u32 = 128;

/// _Block_object_assign(destination, object, flags): what a block's copy
/// helper does for each captured object, block or __block variable.
fn block_object_assign(c: &Ctx, w: Words) -> Result<u64, String> {
    let (destination, object, flags) = (w(0), w(1), w(2));
    let value = match flags & 0x1f {
        FIELD_IS_OBJECT if flags & BYREF_CALLER == 0 => c.retain(object)?,
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
        FIELD_IS_OBJECT if flags & BYREF_CALLER == 0 => c.release(object)?,
        FIELD_IS_BLOCK => block_release(c, object)?,
        FIELD_IS_BYREF => byref_release(c, object)?,
        _ => {}
    }
    ok(0)
}

fn byref_copy(c: &Ctx, byref: u32) -> Result<u32, String> {
    let current = c.word(byref + 4);
    let flags = c.word(current + 8);
    if flags & BLOCK_NEEDS_FREE != 0 {
        c.set_word(current + 8, flags + 2);
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
    let flags = c.word(current + 8);
    if flags & BLOCK_NEEDS_FREE == 0 {
        return Ok(());
    }
    let count = (flags & BLOCK_REFCOUNT_MASK).saturating_sub(2);
    c.set_word(current + 8, (flags & !BLOCK_REFCOUNT_MASK) | count);
    if count == 0 {
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
    c.lock().host_object_for(c.g, heap)
}
