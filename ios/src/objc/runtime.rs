//! A small Objective-C runtime that reads Apple's 64-bit class layout, so
//! the game's own classes and our stand-in Apple classes live side by side.

use crate::loader::Loaded;
use crate::symbols::{leak, leak_cstr};
use std::collections::{HashMap, HashSet};
use std::ffi::CStr;
use std::sync::atomic::Ordering;
use std::sync::{Mutex, MutexGuard, OnceLock};

pub type Id = usize;
pub type Sel = usize;
pub type Class = usize;
pub type Imp = usize;

/// Apple's class object: isa, superclass, cache, vtable, data.
#[repr(C)]
pub struct ClassObject {
    pub isa: usize,
    pub superclass: usize,
    pub cache: usize,
    pub vtable: usize,
    pub data: usize,
}

#[repr(C)]
struct ClassRo {
    flags: u32,
    instance_start: u32,
    instance_size: u32,
    reserved: u32,
    ivar_layout: usize,
    name: usize,
    base_methods: usize,
    base_protocols: usize,
    ivars: usize,
    weak_ivar_layout: usize,
    base_properties: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Init {
    No,
    /// +initialize is running on this thread.
    Running(u64),
    Done,
}

static INIT_DONE: std::sync::Condvar = std::sync::Condvar::new();

fn this_thread() -> u64 {
    unsafe { libc::pthread_self() as u64 }
}

pub struct ClassRec {
    pub name: String,
    pub superclass: Class,
    pub meta: bool,
    /// The class for a metaclass, the metaclass for a class.
    pub partner: Class,
    pub methods: HashMap<Sel, Imp>,
    pub instance_size: usize,
    pub protocols: Vec<usize>,
    pub from_game: bool,
    pub placeholder: bool,
    init: Init,
}

#[derive(Default)]
pub struct Runtime {
    pub classes: HashMap<Class, ClassRec>,
    pub by_name: HashMap<String, Class>,
    selectors: HashMap<String, Sel>,
    selector_names: HashMap<Sel, String>,
    cache: HashMap<(Class, Sel), Imp>,
    protocols: HashMap<String, usize>,
    missing_methods: HashMap<String, u64>,
    /// Method handles given out, one per (defining class, selector).
    method_objects: HashMap<(Class, Sel), usize>,
    method_owner: HashMap<usize, (Class, Sel)>,
}

/// Apple's method_t { SEL name; const char *types; IMP imp; }: the Method
/// handle the game gets from class_getInstanceMethod.
#[repr(C)]
pub struct MethodObject {
    pub name: Sel,
    pub types: usize,
    pub imp: Imp,
}

fn runtime() -> &'static Mutex<Runtime> {
    static RUNTIME: OnceLock<Mutex<Runtime>> = OnceLock::new();
    RUNTIME.get_or_init(|| Mutex::new(Runtime::default()))
}

pub fn lock() -> MutexGuard<'static, Runtime> {
    runtime().lock().unwrap_or_else(|e| e.into_inner())
}

pub unsafe fn cstr(address: usize) -> String {
    if address == 0 {
        return String::new();
    }
    CStr::from_ptr(address as *const libc::c_char).to_string_lossy().into_owned()
}

impl Runtime {
    pub fn sel(&mut self, name: &str) -> Sel {
        if let Some(&sel) = self.selectors.get(name) {
            return sel;
        }
        let sel = leak_cstr(name);
        self.selectors.insert(name.to_string(), sel);
        self.selector_names.insert(sel, name.to_string());
        sel
    }

    /// The selector named `name`, if any code registered it.
    pub fn selector(&self, name: &str) -> Option<Sel> {
        self.selectors.get(name).copied()
    }

    pub fn sel_name(&self, sel: Sel) -> String {
        self.selector_names
            .get(&sel)
            .cloned()
            .unwrap_or_else(|| unsafe { cstr(sel) })
    }

    fn find(&self, mut class: Class, sel: Sel) -> Option<Imp> {
        let mut guard = 0;
        while class != 0 && guard < 256 {
            let rec = self.classes.get(&class)?;
            if let Some(&imp) = rec.methods.get(&sel) {
                return Some(imp);
            }
            class = rec.superclass;
            guard += 1;
        }
        None
    }

    pub fn lookup(&mut self, class: Class, sel: Sel) -> Option<Imp> {
        if let Some(&imp) = self.cache.get(&(class, sel)) {
            return Some(imp);
        }
        let imp = self.find(class, sel)?;
        self.cache.insert((class, sel), imp);
        Some(imp)
    }

    pub fn add_method(&mut self, class: Class, sel: Sel, imp: Imp) {
        if let Some(rec) = self.classes.get_mut(&class) {
            rec.methods.insert(sel, imp);
        }
        if let Some(&method) = self.method_objects.get(&(class, sel)) {
            unsafe { (*(method as *mut MethodObject)).imp = imp };
        }
        self.cache.clear();
    }

    /// The class in `class`'s parent chain that defines `sel` itself.
    pub fn owner_of(&self, mut class: Class, sel: Sel) -> Option<Class> {
        let mut guard = 0;
        while class != 0 && guard < 256 {
            let rec = self.classes.get(&class)?;
            if rec.methods.contains_key(&sel) {
                return Some(class);
            }
            class = rec.superclass;
            guard += 1;
        }
        None
    }

    /// The Method handle for `sel` as `class` itself defines it.
    pub fn method_object(&mut self, class: Class, sel: Sel, types: usize) -> usize {
        if let Some(&method) = self.method_objects.get(&(class, sel)) {
            return method;
        }
        let imp = self.classes.get(&class).and_then(|r| r.methods.get(&sel)).copied().unwrap_or(0);
        let types = if types != 0 { types } else { self.declared_types(class, sel) };
        let method = leak(MethodObject { name: sel, types, imp });
        self.method_objects.insert((class, sel), method);
        self.method_owner.insert(method, (class, sel));
        method
    }

    /// method_setImplementation: the class's method table changes with it.
    pub fn set_method_imp(&mut self, method: usize, imp: Imp) -> Imp {
        let old = unsafe { (*(method as *const MethodObject)).imp };
        match self.method_owner.get(&method).copied() {
            Some((class, sel)) => self.add_method(class, sel, imp),
            None => unsafe { (*(method as *mut MethodObject)).imp = imp },
        }
        old
    }

    /// The type string of a method in the class's own compiled method list.
    fn declared_types(&self, class: Class, sel: Sel) -> usize {
        if !self.classes.contains_key(&class) {
            return 0;
        }
        let name = self.sel_name(sel);
        unsafe {
            let ro = (*(class as *const ClassObject)).data & !7;
            let list = read::<usize>(ro + 32);
            if list == 0 {
                return 0;
            }
            let entry_size = (read::<u32>(list) & !3) as usize;
            let count = read::<u32>(list + 4) as usize;
            (0..count)
                .map(|i| list + 8 + i * entry_size)
                .find(|&entry| cstr(read::<usize>(entry)) == name)
                .map(|entry| read::<usize>(entry + 8))
                .unwrap_or(0)
        }
    }

    pub fn class_name(&self, class: Class) -> String {
        self.classes
            .get(&class)
            .map(|r| r.name.clone())
            .unwrap_or_else(|| format!("<unknown class {class:#x}>"))
    }

    pub fn is_subclass(&self, mut class: Class, of: Class) -> bool {
        while class != 0 {
            if class == of {
                return true;
            }
            class = self.classes.get(&class).map(|r| r.superclass).unwrap_or(0);
        }
        false
    }

    /// Makes a class and its metaclass; the class pointer is returned.
    pub fn define_class(&mut self, name: &str, superclass: Class, instance_size: usize) -> Class {
        if let Some(&existing) = self.by_name.get(name) {
            return existing;
        }
        let empty = || leak(ClassObject { isa: 0, superclass: 0, cache: 0, vtable: 0, data: 0 });
        let (class, meta) = (empty(), empty());
        self.build_class_pair(class, meta, name, superclass, instance_size);
        self.by_name.insert(name.to_string(), class);
        class
    }

    /// Fills in a class and its metaclass in the given memory (each at least
    /// a ClassObject) and records them; the name is not findable yet. As on
    /// Apple's runtime, the fields start where the parent's end (a root class
    /// has just its isa), and a metaclass's isa is its hierarchy's root
    /// metaclass.
    pub fn build_class_pair(&mut self, class: Class, meta: Class, name: &str, superclass: Class, instance_size: usize) {
        let name_c = leak_cstr(name);
        let super_meta = self.classes.get(&superclass).map(|r| r.partner).unwrap_or(0);
        let start = self.classes.get(&superclass).map(|r| r.instance_size).unwrap_or(0);
        let size = instance_size.max(start).max(8);
        // A metaclass has no fields of its own yet: it starts and ends where
        // the parent metaclass ends (a root metaclass is one class object).
        let meta_size = self.classes.get(&super_meta).map(|r| r.instance_size).unwrap_or(40).max(40);
        let make_ro = |meta: bool, start: usize, size: usize| {
            leak(ClassRo {
                flags: if meta { 1 } else { 0 } | if superclass == 0 { 2 } else { 0 },
                instance_start: start as u32,
                instance_size: size as u32,
                reserved: 0,
                ivar_layout: 0,
                name: name_c,
                base_methods: 0,
                base_protocols: 0,
                ivars: 0,
                weak_ivar_layout: 0,
                base_properties: 0,
            })
        };
        unsafe {
            std::ptr::write(
                class as *mut ClassObject,
                ClassObject { isa: 0, superclass, cache: 0, vtable: 0, data: make_ro(false, start, size) },
            );
            std::ptr::write(
                meta as *mut ClassObject,
                ClassObject { isa: 0, superclass: 0, cache: 0, vtable: 0, data: make_ro(true, meta_size, meta_size) },
            );
        }
        let (meta_isa, meta_super) = if superclass == 0 {
            (meta, class)
        } else if super_meta != 0 {
            (unsafe { (*(super_meta as *const ClassObject)).isa }, super_meta)
        } else {
            (meta, 0)
        };
        unsafe {
            (*(class as *mut ClassObject)).isa = meta;
            (*(meta as *mut ClassObject)).isa = meta_isa;
            (*(meta as *mut ClassObject)).superclass = meta_super;
        }
        self.classes.insert(
            class,
            ClassRec {
                name: name.to_string(),
                superclass,
                meta: false,
                partner: meta,
                methods: HashMap::new(),
                instance_size: size,
                protocols: Vec::new(),
                from_game: false,
                placeholder: false,
                init: Init::No,
            },
        );
        self.classes.insert(
            meta,
            ClassRec {
                name: name.to_string(),
                superclass: meta_super,
                meta: true,
                partner: class,
                methods: HashMap::new(),
                instance_size: meta_size,
                protocols: Vec::new(),
                from_game: false,
                placeholder: false,
                init: Init::No,
            },
        );
    }

    pub fn class_of(&self, object: Id) -> Class {
        if object == 0 {
            return 0;
        }
        unsafe { *(object as *const usize) }
    }

    pub fn is_class_object(&self, object: Id) -> bool {
        let isa = self.class_of(object);
        self.classes.get(&isa).map(|r| r.meta).unwrap_or(false)
    }
}

pub fn sel(name: &str) -> Sel {
    lock().sel(name)
}

pub fn class_named(name: &str) -> Class {
    lock().by_name.get(name).copied().unwrap_or(0)
}

/// One of our real (not placeholder) classes, or its metaclass.
pub fn existing_class(name: &str, want_meta: bool) -> Option<Class> {
    let rt = lock();
    let class = *rt.by_name.get(name)?;
    let rec = rt.classes.get(&class)?;
    if rec.placeholder {
        return None;
    }
    Some(if want_meta { rec.partner } else { class })
}

/// Makes a stand-in for an Apple class we have not written yet; any message
/// to it is reported as missing.
pub fn placeholder_class(name: &str, want_meta: bool) -> usize {
    let mut rt = lock();
    let class = match rt.by_name.get(name) {
        Some(&class) => class,
        None => {
            let root = rt.by_name.get("NSObject").copied().unwrap_or(0);
            let class = rt.define_class(name, root, 8);
            if let Some(rec) = rt.classes.get_mut(&class) {
                rec.placeholder = true;
            }
            let meta = rt.classes[&class].partner;
            if let Some(rec) = rt.classes.get_mut(&meta) {
                rec.placeholder = true;
            }
            class
        }
    };
    if want_meta {
        rt.classes[&class].partner
    } else {
        class
    }
}

// ---------- message sending ----------

std::arch::global_asm!(
    ".globl ios_objc_msgSend",
    ".p2align 2",
    "ios_objc_msgSend:",
    "cbz x0, 9f",
    "stp x29, x30, [sp, #-224]!",
    "mov x29, sp",
    "stp x0, x1, [sp, #16]",
    "stp x2, x3, [sp, #32]",
    "stp x4, x5, [sp, #48]",
    "stp x6, x7, [sp, #64]",
    "str x8, [sp, #80]",
    "stp q0, q1, [sp, #96]",
    "stp q2, q3, [sp, #128]",
    "stp q4, q5, [sp, #160]",
    "stp q6, q7, [sp, #192]",
    "mov x2, x30",
    // The saved receiver: forwarding may replace it (see forward.rs).
    "add x3, sp, #16",
    "bl ios_objc_lookup",
    "mov x16, x0",
    "ldp q6, q7, [sp, #192]",
    "ldp q4, q5, [sp, #160]",
    "ldp q2, q3, [sp, #128]",
    "ldp q0, q1, [sp, #96]",
    "ldr x8, [sp, #80]",
    "ldp x6, x7, [sp, #64]",
    "ldp x4, x5, [sp, #48]",
    "ldp x2, x3, [sp, #32]",
    "ldp x0, x1, [sp, #16]",
    "ldp x29, x30, [sp], #224",
    "br x16",
    "9:",
    "mov x1, #0",
    "movi d0, #0",
    "movi d1, #0",
    "movi d2, #0",
    "movi d3, #0",
    "ret",
    // objc_msgSendSuper2(struct { id receiver; Class current; } *, SEL, ...)
    ".globl ios_objc_msgSendSuper2",
    ".p2align 2",
    "ios_objc_msgSendSuper2:",
    "mov x9, #1",
    "b 8f",
    ".globl ios_objc_msgSendSuper",
    ".p2align 2",
    "ios_objc_msgSendSuper:",
    "mov x9, #0",
    "8:",
    "stp x29, x30, [sp, #-224]!",
    "mov x29, sp",
    "stp x0, x1, [sp, #16]",
    "stp x2, x3, [sp, #32]",
    "stp x4, x5, [sp, #48]",
    "stp x6, x7, [sp, #64]",
    "str x8, [sp, #80]",
    "stp q0, q1, [sp, #96]",
    "stp q2, q3, [sp, #128]",
    "stp q4, q5, [sp, #160]",
    "stp q6, q7, [sp, #192]",
    "mov x2, x9",
    "mov x3, x30",
    // The saved x0 (the pair) becomes the receiver the method gets.
    "add x4, sp, #16",
    "bl ios_objc_lookup_super",
    "mov x16, x0",
    "ldp q6, q7, [sp, #192]",
    "ldp q4, q5, [sp, #160]",
    "ldp q2, q3, [sp, #128]",
    "ldp q0, q1, [sp, #96]",
    "ldr x8, [sp, #80]",
    "ldp x6, x7, [sp, #64]",
    "ldp x4, x5, [sp, #48]",
    "ldp x2, x3, [sp, #32]",
    "ldp x0, x1, [sp, #16]",
    "ldp x29, x30, [sp], #224",
    "br x16",
);

extern "C" {
    pub fn ios_objc_msgSend();
    pub fn ios_objc_msgSendSuper2();
    pub fn ios_objc_msgSendSuper();
}

extern "C" fn missing_method_returns_zero() -> usize {
    0
}

fn missing_method(class: Class, receiver_is_class: bool, sel: Sel, caller: usize) -> Imp {
    let (text, first) = {
        let mut rt = lock();
        let class_name = rt.class_name(class);
        let text = format!(
            "{}[{} {}]",
            if receiver_is_class { "+" } else { "-" },
            class_name,
            rt.sel_name(sel)
        );
        let count = rt.missing_methods.entry(text.clone()).or_default();
        *count += 1;
        (text, *count == 1)
    };
    let place = if caller == 0 { "our own code".to_string() } else { crate::loader::describe_address(caller) };
    if crate::trap::KEEP_GOING.load(Ordering::Relaxed) {
        if first {
            crate::log!("missing method {text} (called from {place}), returning 0");
        }
        return missing_method_returns_zero as usize;
    }
    crate::log!("STOP: missing method {text} (called from {place})");
    crate::report::finish(1);
}

/// Counts a call to a method nobody has (named like "-[Class selector");
/// says whether it is the first.
pub fn note_missing_method(text: &str) -> bool {
    let mut rt = lock();
    let count = rt.missing_methods.entry(text.to_string()).or_default();
    *count += 1;
    *count == 1
}

/// Methods asked for that don't exist. Gives nothing if the runtime is busy
/// (a crash report must not wait on a lock the crashed code held).
pub fn missing_methods() -> Vec<(String, u64)> {
    let Some(rt) = (0..100).find_map(|_| match runtime().try_lock() {
        Ok(guard) => Some(guard),
        Err(std::sync::TryLockError::Poisoned(e)) => Some(e.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => {
            std::thread::sleep(std::time::Duration::from_millis(2));
            None
        }
    }) else {
        return Vec::new();
    };
    let mut list: Vec<_> = rt.missing_methods.iter().map(|(k, v)| (k.clone(), *v)).collect();
    list.sort();
    list
}

/// Sends +initialize the first time a class is used: parents first, one
/// class at a time. Other threads wait until it finishes; the thread running
/// it may use the class meanwhile (as on Apple's runtime).
fn ensure_initialized(class: Class) {
    let chain: Vec<Class> = {
        let rt = lock();
        let mut chain = Vec::new();
        let mut current = class;
        while current != 0 {
            match rt.classes.get(&current) {
                Some(rec) => {
                    if rec.init != Init::Done {
                        chain.push(current);
                    }
                    current = rec.superclass;
                }
                None => break,
            }
        }
        chain
    };
    let me = this_thread();
    let initialize = sel("initialize");
    for &c in chain.iter().rev() {
        let mut rt = lock();
        loop {
            match rt.classes[&c].init {
                Init::Done => break,
                Init::Running(owner) if owner == me => break,
                Init::Running(_) => rt = INIT_DONE.wait(rt).unwrap_or_else(|e| e.into_inner()),
                Init::No => {
                    rt.classes.get_mut(&c).unwrap().init = Init::Running(me);
                    let meta = rt.classes[&c].partner;
                    let imp = rt.lookup(meta, initialize);
                    drop(rt);
                    if let Some(imp) = imp {
                        let f: extern "C" fn(Id, Sel) -> Id = unsafe { std::mem::transmute(imp) };
                        f(c, initialize);
                    }
                    rt = lock();
                    rt.classes.get_mut(&c).unwrap().init = Init::Done;
                    INIT_DONE.notify_all();
                    break;
                }
            }
        }
    }
}

#[no_mangle]
extern "C" fn ios_objc_lookup(receiver: Id, sel: Sel, caller: usize, saved_receiver: *mut Id) -> Imp {
    let mut receiver = receiver;
    let imp = find_method(&mut receiver, sel, caller);
    unsafe { *saved_receiver = receiver };
    imp
}

/// objc_msgSend's search: the method `*receiver` runs for `sel`, forwarded
/// when there is none (which may name another receiver; see forward.rs).
pub(crate) fn find_method(receiver: &mut Id, sel: Sel, caller: usize) -> Imp {
    let receiver_now = *receiver;
    let (isa, is_class) = class_for_message(receiver_now, sel, caller);
    let found = lock().lookup(isa, sel);
    match found {
        Some(imp) => imp,
        None => super::forward::after_miss(receiver, isa, sel, caller).unwrap_or_else(|| missing_method(isa, is_class, sel, caller)),
    }
}

/// The receiver's class (initialized) and whether the receiver is a class.
fn class_for_message(receiver: Id, sel: Sel, caller: usize) -> (Class, bool) {
    let (isa, is_class, known) = {
        let rt = lock();
        let isa = rt.class_of(receiver);
        let rec = rt.classes.get(&isa);
        (isa, rec.map(|r| r.meta).unwrap_or(false), rec.is_some())
    };
    if !known {
        let name = lock().sel_name(sel);
        crate::log!("STOP: message {name} sent to {receiver:#x}, which is not an object we know (isa {isa:#x})");
        crate::report::finish(1);
    }
    ensure_initialized(if is_class { receiver } else { isa });
    if let Some(pattern) = message_trace() {
        trace_message(pattern, isa, is_class, sel, caller);
    }
    (isa, is_class)
}

/// Test switch IOS_TRACE_MESSAGES=<pattern>: logs the game's messages whose
/// "-[Class selector]" matches the pattern (each one up to 50 times), to see
/// what a game asks for at a point where it stops going on.
fn message_trace() -> Option<&'static regex::Regex> {
    static TRACE: std::sync::OnceLock<Option<regex::Regex>> = std::sync::OnceLock::new();
    TRACE
        .get_or_init(|| {
            let pattern = std::env::var("IOS_TRACE_MESSAGES").ok().filter(|p| !p.is_empty())?;
            match regex::Regex::new(&pattern) {
                Ok(regex) => Some(regex),
                Err(error) => {
                    crate::log!("IOS_TRACE_MESSAGES: not a pattern ({error})");
                    None
                }
            }
        })
        .as_ref()
}

/// How many times one message is logged before it is left out.
const TRACE_LIMIT: u32 = 50;

/// Whether `text` is the next message to log: the pattern names it and this
/// exact message has not been logged `limit` times yet.
fn trace_wants(pattern: &regex::Regex, limit: u32, text: &str) -> bool {
    static COUNTS: Mutex<Option<HashMap<String, u32>>> = Mutex::new(None);
    if !pattern.is_match(text) {
        return false;
    }
    let mut counts = COUNTS.lock().unwrap_or_else(|e| e.into_inner());
    let count = counts.get_or_insert_with(HashMap::new).entry(text.to_string()).or_default();
    *count += 1;
    *count <= limit
}

/// Whether the test switch IOS_TRACE_MESSAGES is set at all (a caller with
/// work to do before asking for the message's text checks this first).
pub(crate) fn message_trace_on() -> bool {
    message_trace().is_some()
}

/// The "-[Class selector]" text of a message, when the test switch wants it
/// logged. The 32-bit bridge asks for the same text with the guest class's
/// name, so one switch traces both kinds of game.
pub(crate) fn traced_message_text(class: &str, is_class: bool, sel: &str) -> Option<String> {
    let pattern = message_trace()?;
    let text = format!("{}[{class} {sel}]", if is_class { "+" } else { "-" });
    trace_wants(pattern, TRACE_LIMIT, &text).then_some(text)
}

fn trace_message(pattern: &regex::Regex, isa: Class, is_class: bool, sel: Sel, caller: usize) {
    let text = {
        let rt = lock();
        format!("{}[{} {}]", if is_class { "+" } else { "-" }, rt.class_name(isa), rt.sel_name(sel))
    };
    if !trace_wants(pattern, TRACE_LIMIT, &text) {
        return;
    }
    crate::log!("message {text} from {}", crate::loader::describe_address(caller));
}

#[cfg(test)]
mod trace_tests {
    use super::*;

    #[test]
    fn a_traced_message_is_logged_up_to_the_limit() {
        let pattern = regex::Regex::new(r"^-\[CDAudioManager ").unwrap();
        let wanted = "-[CDAudioManager playBackgroundMusic:loop:]";
        let logged = (1..=TRACE_LIMIT + 5).filter(|_| trace_wants(&pattern, TRACE_LIMIT, wanted)).count();
        assert_eq!(logged, TRACE_LIMIT as usize, "one message logged {logged} times");
        assert!(!trace_wants(&pattern, TRACE_LIMIT, "-[CCDirector run]"), "a message the pattern does not name");
    }
}

#[no_mangle]
extern "C" fn ios_objc_lookup_super(pair: *const [usize; 2], sel: Sel, second_form: usize, caller: usize, saved_x0: *mut Id) -> Imp {
    let [mut receiver, current] = unsafe { *pair };
    let start = if second_form != 0 {
        lock().classes.get(&current).map(|r| r.superclass).unwrap_or(0)
    } else {
        current
    };
    let is_class = lock().is_class_object(receiver);
    let found = lock().lookup(start, sel);
    let imp = match found {
        Some(imp) => imp,
        None => super::forward::after_miss(&mut receiver, start, sel, caller).unwrap_or_else(|| missing_method(start, is_class, sel, caller)),
    };
    unsafe { *saved_x0 = receiver };
    imp
}

/// Finds the code for a message, for calls from our own Rust code.
pub fn imp_for(receiver: Id, sel: Sel) -> Option<Imp> {
    if receiver == 0 {
        return None;
    }
    let (isa, is_class) = {
        let rt = lock();
        (rt.class_of(receiver), rt.is_class_object(receiver))
    };
    ensure_initialized(if is_class { receiver } else { isa });
    lock().lookup(isa, sel)
}

pub fn responds(receiver: Id, sel_name: &str) -> bool {
    imp_for(receiver, sel(sel_name)).is_some()
}

/// Sends a message from Rust with up to four pointer-sized arguments.
pub fn send(receiver: Id, sel_name: &str, args: &[usize]) -> usize {
    let s = sel(sel_name);
    if receiver == 0 || crate::fw::exception::pending() {
        return 0;
    }
    let mut receiver = receiver;
    let imp = match imp_for(receiver, s) {
        Some(imp) => imp,
        None => {
            let (isa, is_class) = {
                let rt = lock();
                (rt.class_of(receiver), rt.is_class_object(receiver))
            };
            // Forwarded like objc_msgSend's (the arguments are in registers alike).
            super::forward::after_miss(&mut receiver, isa, s, 0).unwrap_or_else(|| missing_method(isa, is_class, s, 0))
        }
    };
    unsafe {
        match args {
            [] => std::mem::transmute::<Imp, extern "C" fn(Id, Sel) -> usize>(imp)(receiver, s),
            [a] => std::mem::transmute::<Imp, extern "C" fn(Id, Sel, usize) -> usize>(imp)(receiver, s, *a),
            [a, b] => std::mem::transmute::<Imp, extern "C" fn(Id, Sel, usize, usize) -> usize>(imp)(receiver, s, *a, *b),
            [a, b, c] => std::mem::transmute::<Imp, extern "C" fn(Id, Sel, usize, usize, usize) -> usize>(imp)(receiver, s, *a, *b, *c),
            [a, b, c, d] => std::mem::transmute::<Imp, extern "C" fn(Id, Sel, usize, usize, usize, usize) -> usize>(imp)(
                receiver, s, *a, *b, *c, *d,
            ),
            _ => panic!("send: too many arguments"),
        }
    }
}

// ---------- reading the game's classes ----------

unsafe fn read<T: Copy>(address: usize) -> T {
    std::ptr::read_unaligned(address as *const T)
}

fn section_words(loaded: &Loaded, segment_names: &[&str], name: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for segment in segment_names {
        if let Some(section) = loaded.image.section(segment, name) {
            let start = section.address.wrapping_add(loaded.slide) as usize;
            for index in 0..(section.size as usize / 8) {
                out.push(start + index * 8);
            }
        }
    }
    out
}

const DATA_SEGMENTS: &[&str] = &["__DATA", "__DATA_CONST", "__DATA_DIRTY"];

unsafe fn method_list(rt: &mut Runtime, list: usize) -> Vec<(Sel, Imp)> {
    if list == 0 {
        return Vec::new();
    }
    let entry_size = (read::<u32>(list) & !3) as usize;
    let count = read::<u32>(list + 4) as usize;
    let mut out = Vec::with_capacity(count);
    for index in 0..count {
        let entry = list + 8 + index * entry_size;
        let name = cstr(read::<usize>(entry));
        let imp = read::<usize>(entry + 16);
        out.push((rt.sel(&name), imp));
    }
    out
}

unsafe fn protocol_list(list: usize) -> Vec<usize> {
    if list == 0 {
        return Vec::new();
    }
    let count = read::<u64>(list) as usize;
    (0..count).map(|i| read::<usize>(list + 8 + i * 8)).collect()
}

fn realize(rt: &mut Runtime, class: Class, game_classes: &HashSet<Class>) -> Result<(), String> {
    if rt.classes.contains_key(&class) {
        return Ok(());
    }
    unsafe {
        let object = &*(class as *const ClassObject);
        let superclass = object.superclass;
        if superclass != 0 && !rt.classes.contains_key(&superclass) {
            if game_classes.contains(&superclass) {
                realize(rt, superclass, game_classes)?;
            } else {
                return Err(format!("class at {class:#x} has an unknown parent {superclass:#x}"));
            }
        }
        let ro = object.data & !7;
        let meta = object.isa;
        let meta_ro = (*(meta as *const ClassObject)).data & !7;
        let name = cstr(read::<usize>(ro + 24));
        let mut instance_start = read::<u32>(ro + 4) as usize;
        let mut instance_size = read::<u32>(ro + 8) as usize;
        let super_size = rt.classes.get(&superclass).map(|r| r.instance_size).unwrap_or(0);
        if superclass != 0 && super_size > instance_start {
            // Our parent class is bigger than the game expected: move the
            // game's fields up, as Apple's runtime does.
            let delta = (super_size - instance_start + 7) & !7;
            let ivars = read::<usize>(ro + 48);
            if ivars != 0 {
                let entry_size = read::<u32>(ivars) as usize;
                let count = read::<u32>(ivars + 4) as usize;
                for index in 0..count {
                    let offset_cell = read::<usize>(ivars + 8 + index * entry_size);
                    if offset_cell != 0 {
                        let value = read::<u32>(offset_cell);
                        std::ptr::write_unaligned(offset_cell as *mut u32, value + delta as u32);
                    }
                }
            }
            crate::log!("class {name}: fields moved up by {delta} bytes for a bigger parent");
            instance_start += delta;
            instance_size += delta;
        }
        let _ = instance_start;
        let methods = method_list(rt, read::<usize>(ro + 32));
        let class_methods = method_list(rt, read::<usize>(meta_ro + 32));
        let load = rt.sel("load");
        if let Some(&(_, imp)) = class_methods.iter().find(|(s, _)| *s == load) {
            BASE_LOAD.lock().unwrap().get_or_insert_with(HashMap::new).insert(class, imp);
        }
        let protocols = protocol_list(read::<usize>(ro + 40));
        let meta_super = (*(meta as *const ClassObject)).superclass;
        // A root metaclass's parent is its own class, recorded just below.
        if meta_super != 0 && meta_super != class && !rt.classes.contains_key(&meta_super) {
            return Err(format!("metaclass of {name} has an unknown parent {meta_super:#x}"));
        }
        rt.classes.insert(
            class,
            ClassRec {
                name: name.clone(),
                superclass,
                meta: false,
                partner: meta,
                methods: methods.into_iter().collect(),
                instance_size,
                protocols,
                from_game: true,
                placeholder: false,
                init: Init::No,
            },
        );
        rt.classes.insert(
            meta,
            ClassRec {
                name: name.clone(),
                superclass: meta_super,
                meta: true,
                partner: class,
                methods: class_methods.into_iter().collect(),
                instance_size: 40,
                protocols: Vec::new(),
                from_game: true,
                placeholder: false,
                init: Init::No,
            },
        );
        if let Some(old) = rt.by_name.insert(name.clone(), class) {
            crate::log!("class {name} from the game replaces our own version ({old:#x})");
        }
    }
    Ok(())
}

/// objc_readClassPair: takes in one class laid out the way the compiler
/// writes it. Gives 0 if its parent is unknown (and it is not a root class).
pub fn read_class_pair(class: Class) -> Class {
    let mut rt = lock();
    if rt.classes.contains_key(&class) {
        return class;
    }
    let (superclass, root) = unsafe {
        let object = &*(class as *const ClassObject);
        (object.superclass, read::<u32>(object.data & !7) & 2 != 0)
    };
    let parent_ok = match superclass {
        0 => root,
        _ => rt.classes.get(&superclass).map(|r| !r.meta).unwrap_or(false),
    };
    if !parent_ok {
        crate::log!("objc_readClassPair: class at {class:#x} has an unknown parent {superclass:#x}");
        return 0;
    }
    match realize(&mut rt, class, &HashSet::new()) {
        Ok(()) => class,
        Err(error) => {
            crate::log!("objc_readClassPair: {error}");
            0
        }
    }
}

/// The game's classes, in the order its class list gives them.
static IMAGE_CLASSES: Mutex<Vec<Class>> = Mutex::new(Vec::new());

pub fn image_classes() -> Vec<Class> {
    IMAGE_CLASSES.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

static LOAD_CALLS: Mutex<Vec<(Class, Imp, String)>> = Mutex::new(Vec::new());
/// Each game class's own +load (categories may add another one).
static BASE_LOAD: Mutex<Option<HashMap<Class, Imp>>> = Mutex::new(None);

pub fn load_image(loaded: &Loaded) -> Result<(), String> {
    let mut rt = lock();
    // Selector references: make every name point at one shared selector.
    for cell in section_words(loaded, DATA_SEGMENTS, "__objc_selrefs") {
        unsafe {
            let name = cstr(read::<usize>(cell));
            let s = rt.sel(&name);
            std::ptr::write_unaligned(cell as *mut usize, s);
        }
    }
    // Protocols.
    for cell in section_words(loaded, DATA_SEGMENTS, "__objc_protolist") {
        unsafe {
            let protocol = read::<usize>(cell);
            let name = cstr(read::<usize>(protocol + 8));
            rt.protocols.entry(name).or_insert(protocol);
        }
    }
    // Classes, parents first.
    let class_cells = section_words(loaded, DATA_SEGMENTS, "__objc_classlist");
    let game_classes: HashSet<Class> = class_cells.iter().map(|&c| unsafe { read::<usize>(c) }).collect();
    for &class in &game_classes {
        realize(&mut rt, class, &game_classes)?;
    }
    IMAGE_CLASSES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .extend(class_cells.iter().map(|&c| unsafe { read::<usize>(c) }));
    // Categories add methods to existing classes (ours or the game's).
    for cell in section_words(loaded, DATA_SEGMENTS, "__objc_catlist") {
        unsafe {
            let category = read::<usize>(cell);
            let class = read::<usize>(category + 8);
            if !rt.classes.contains_key(&class) {
                crate::log!("category {} is for an unknown class {class:#x}", cstr(read::<usize>(category)));
                continue;
            }
            let meta = rt.classes[&class].partner;
            for (s, imp) in method_list(&mut rt, read::<usize>(category + 16)) {
                rt.add_method(class, s, imp);
            }
            for (s, imp) in method_list(&mut rt, read::<usize>(category + 24)) {
                rt.add_method(meta, s, imp);
            }
            let protocols = protocol_list(read::<usize>(category + 32));
            rt.classes.get_mut(&class).unwrap().protocols.extend(protocols);
        }
    }
    // +load methods of classes that ask for them, parents first.
    let load = rt.sel("load");
    let mut order: Vec<Class> = Vec::new();
    for cell in section_words(loaded, DATA_SEGMENTS, "__objc_nlclslist") {
        let class = unsafe { read::<usize>(cell) };
        let mut chain = Vec::new();
        let mut current = class;
        while current != 0 && rt.classes.get(&current).map(|r| r.from_game).unwrap_or(false) {
            chain.push(current);
            current = rt.classes[&current].superclass;
        }
        for c in chain.into_iter().rev() {
            if !order.contains(&c) {
                order.push(c);
            }
        }
    }
    let mut calls = LOAD_CALLS.lock().unwrap();
    let base_load = BASE_LOAD.lock().unwrap().clone().unwrap_or_default();
    for class in order {
        if let Some(&imp) = base_load.get(&class) {
            calls.push((class, imp, rt.classes[&class].name.clone()));
        }
    }
    for cell in section_words(loaded, DATA_SEGMENTS, "__objc_nlcatlist") {
        unsafe {
            let category = read::<usize>(cell);
            let class = read::<usize>(category + 8);
            for (s, imp) in method_list(&mut rt, read::<usize>(category + 24)) {
                if s == load {
                    calls.push((class, imp, format!("category {}", cstr(read::<usize>(category)))));
                }
            }
        }
    }
    crate::log!(
        "Objective-C: {} game classes, {} selectors, {} protocols, {} +load calls",
        game_classes.len(),
        rt.selectors.len(),
        rt.protocols.len(),
        calls.len()
    );
    Ok(())
}

pub fn run_load_methods() {
    let calls: Vec<_> = LOAD_CALLS.lock().unwrap().drain(..).collect();
    let load = sel("load");
    for (class, imp, name) in calls {
        crate::log!("+load {name}");
        let f: extern "C" fn(Id, Sel) -> Id = unsafe { std::mem::transmute(imp) };
        f(class, load);
    }
}

/// "-[Class selector]" for each method the game's own classes define, by address.
pub fn game_method_names() -> Vec<(usize, String)> {
    let rt = lock();
    let mut out = Vec::new();
    for rec in rt.classes.values().filter(|r| r.from_game) {
        let sign = if rec.meta { '+' } else { '-' };
        for (&s, &imp) in &rec.methods {
            out.push((imp, format!("{sign}[{} {}]", rec.name, rt.sel_name(s))));
        }
    }
    out.sort();
    out
}

pub fn protocol_named(name: &str) -> usize {
    lock().protocols.get(name).copied().unwrap_or(0)
}

/// Makes `protocol` (a protocol_t) known by its name, unless one of that
/// name is known already; the one kept.
pub fn register_protocol(name: &str, protocol: usize) -> usize {
    *lock().protocols.entry(name.to_string()).or_insert(protocol)
}

/// Adds protocols to the ones `class` adopts (a protocol already there is not repeated).
pub fn add_protocols(class: Class, protocols: &[usize]) {
    let mut rt = lock();
    if let Some(rec) = rt.classes.get_mut(&class) {
        for &protocol in protocols {
            if protocol != 0 && !rec.protocols.contains(&protocol) {
                rec.protocols.push(protocol);
            }
        }
    }
}

pub fn conforms(class: Class, protocol: usize) -> bool {
    let rt = lock();
    let target = unsafe { cstr(read::<usize>(protocol + 8)) };
    let mut current = class;
    while current != 0 {
        let Some(rec) = rt.classes.get(&current) else { break };
        for &p in &rec.protocols {
            if protocol_matches(p, &target, 0) {
                return true;
            }
        }
        current = rec.superclass;
    }
    false
}

pub fn protocol_matches(protocol: usize, target: &str, depth: usize) -> bool {
    if protocol == 0 || depth > 16 {
        return false;
    }
    unsafe {
        if cstr(read::<usize>(protocol + 8)) == target {
            return true;
        }
        protocol_list(read::<usize>(protocol + 16))
            .into_iter()
            .any(|p| protocol_matches(p, target, depth + 1))
    }
}

// ---------- objects finished with ----------

static FINISHED_LISTENERS: Mutex<Vec<fn(Id)>> = Mutex::new(Vec::new());

/// Asks to hear of objects their owner is done with (see object_finished).
pub fn on_object_finished(listener: fn(Id)) {
    let mut listeners = FINISHED_LISTENERS.lock().unwrap_or_else(|e| e.into_inner());
    if !listeners.iter().any(|&l| l as usize == listener as usize) {
        listeners.push(listener);
    }
}

/// Tells the listeners that `object` is done with, so what they keep for it
/// can go (the 32-bit bridge frees the guest copies of its data).
pub fn object_finished(object: Id) {
    let listeners = FINISHED_LISTENERS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for listener in listeners {
        listener(object);
    }
}

pub fn alloc_instance(class: Class) -> Id {
    let size = lock().classes.get(&class).map(|r| r.instance_size).unwrap_or(16).max(16);
    let object = unsafe { libc::calloc(1, size) } as usize;
    unsafe { *(object as *mut usize) = class };
    object
}

