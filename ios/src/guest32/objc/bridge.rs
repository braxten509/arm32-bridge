//! The Objective-C bridge: which guest object is which host object, guest
//! copies of framework classes, and messages in both directions.
//!
//! - A framework object (NSString, UIView, ...) lives in the host. The guest
//!   sees an 8-byte proxy `{ isa, 0 }` whose isa is a guest copy of the
//!   framework class (objc2 32-bit layout, built on first use with its whole
//!   superclass chain), so `object->isa`, `object_getClass` and class
//!   comparisons work in guest code.
//! - A game object lives in guest memory (guest code reads its ivars) and
//!   has a host form, an instance of the game class's host mirror class
//!   (see mirror.rs). Allocation goes through the host (`+alloc` reaches the
//!   framework root with the mirror class as receiver); the guest form is
//!   made when the new host object first crosses to the guest.
//! - Guest messages: the game's own methods run as guest code; anything that
//!   reaches a framework class is converted by type encoding and calls the
//!   host implementation. Host messages to game objects come through the
//!   mirror trampoline into [`dispatch_from_host`].
//!
//! Locking: the bridge state is behind a mutex that is never held while
//! guest or host code runs (both may send messages back).

use super::classes::{CachedSend, GuestClasses, Lookup};
use super::convert::{self, Boundary, GuestRet, HostCallOptions, HostFrame};
use super::encoding::Signature;
use super::mirror;
use super::selectors::{FrameworkSelector, FrameworkSelectors};
use crate::objc::runtime;
use guest32_image::ObjcMetadata32;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

/// Guest memory and guest code, as the bridge needs them.
pub trait GuestSide: Sync {
    fn read(&self, addr: u32, len: u32) -> Vec<u8>;
    fn write(&self, addr: u32, data: &[u8]);
    /// The guest's heap (game instances, temporaries).
    fn alloc(&self, size: u32) -> u32;
    fn free(&self, addr: u32);
    /// Never freed (classes, selector names, proxies).
    fn alloc_permanent(&self, size: u32) -> u32;
    fn host_address(&self, addr: u32) -> usize;
    fn guest_address(&self, host: usize) -> Option<u32>;
    /// The guest address of the runtime function `name` (objc_msgSend, ...).
    fn function_address(&self, name: &str) -> u32;
    /// Runs guest code at `imp` (bit 0 = Thumb) with argument words; r0 | r1 << 32.
    fn call(&self, imp: u32, words: &[u32]) -> Result<u64, String>;
    /// Runs `wait` without this thread holding the guest lock (so other
    /// threads can run guest code meanwhile).
    fn without_guest_lock(&self, wait: &mut dyn FnMut()) {
        wait()
    }
}

/// The function a framework method's guest IMP is: objc_msgSend's stand-in,
/// or objc_msgSend_stret's when its result (by `types`, the method's
/// encoding) comes back through a hidden first pointer.
pub fn message_stub(g: &dyn GuestSide, types: Option<&str>) -> u32 {
    let in_memory = types.and_then(|t| Signature::parse_method(t).ok()).is_some_and(|sig| sig.ret.guest_returns_in_memory());
    g.function_address(if in_memory { "objc_msgSend_stret" } else { "objc_msgSend" })
}

fn u32_at(g: &dyn GuestSide, addr: u32) -> u32 {
    u32::from_le_bytes(g.read(addr, 4).try_into().unwrap())
}

fn guest_cstr(g: &dyn GuestSide, addr: u32) -> String {
    if addr == 0 {
        return String::new();
    }
    unsafe { runtime::cstr(g.host_address(addr)) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendKind {
    /// objc_msgSend(self, _cmd, ...)
    Normal,
    /// objc_msgSend_stret(result, self, _cmd, ...)
    Stret,
    /// objc_msgSendSuper2(&{self, current class}, _cmd, ...)
    Super2,
    /// objc_msgSendSuper2_stret(result, &{self, current class}, _cmd, ...)
    Super2Stret,
}

/// Guest objects of special kinds, by their isa.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Special {
    ConstantString,
    StackBlock,
    MallocBlock,
    GlobalBlock,
}

impl Special {
    pub fn is_block(self) -> bool {
        matches!(self, Special::StackBlock | Special::MallocBlock | Special::GlobalBlock)
    }
}

// Block flags (libclosure).
pub const BLOCK_REFCOUNT_MASK: u32 = 0xfffe;
pub const BLOCK_NEEDS_FREE: u32 = 1 << 24;
pub const BLOCK_HAS_COPY_DISPOSE: u32 = 1 << 25;
pub const BLOCK_IS_GLOBAL: u32 = 1 << 28;
pub const BLOCK_HAS_SIGNATURE: u32 = 1 << 30;

/// The host form of a guest block: a 64-bit block whose invoke is the
/// mirror trampoline and which carries its guest block. The bridge's own
/// wrapper lives for the bridge call that exported it (a stack block to the
/// host); a host that keeps the block copies it (_Block_copy), and the copy
/// helper takes a guest reference that the dispose helper gives back.
#[repr(C)]
struct HostBlock {
    isa: usize,
    flags: i32,
    reserved: i32,
    invoke: usize,
    descriptor: *const WrapperDescriptor,
    guest: usize,
}

#[repr(C)]
struct WrapperDescriptor {
    reserved: usize,
    size: usize,
    copy: extern "C" fn(*mut HostBlock, *const HostBlock),
    dispose: extern "C" fn(*mut HostBlock),
}

static WRAPPER_DESCRIPTOR: WrapperDescriptor =
    WrapperDescriptor { reserved: 0, size: std::mem::size_of::<HostBlock>(), copy: wrapper_copy, dispose: wrapper_dispose };

/// Host blocks (the bridge's wrappers and the host's copies of them) that
/// run guest blocks.
static LIVE_WRAPPERS: Mutex<Option<HashSet<usize>>> = Mutex::new(None);

/// Guest memory for the copy/dispose helpers (they run from host code).
static BLOCK_OWNER: std::sync::RwLock<Option<&'static dyn GuestSide>> = std::sync::RwLock::new(None);

thread_local! {
    /// This thread's wrappers made by the bridge, dropped when the bridge
    /// call that made them ends (see end_wrapper_scope).
    static SCOPED_WRAPPERS: std::cell::RefCell<Vec<Box<HostBlock>>> = const { std::cell::RefCell::new(Vec::new()) };
    /// Game objects whose guest form was just made on this thread and whose
    /// C++ ivars still need constructing (see construct_pending).
    static UNCONSTRUCTED: std::cell::RefCell<Vec<u32>> = const { std::cell::RefCell::new(Vec::new()) };
    /// An exception game code raised (-raise, +raise:format:) for the
    /// runtime function handler to throw in the guest (it has the engine).
    static PENDING_THROW: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

/// The exception a message just raised on this thread, to be thrown.
pub fn take_pending_throw() -> Option<u32> {
    PENDING_THROW.with(|p| p.take())
}

/// Whether framework class `name` is NSException or a subclass.
fn is_exception_class(name: &str) -> bool {
    let Some(class) = runtime::existing_class(name, false) else { return false };
    let exception = runtime::class_named("NSException");
    runtime::lock().is_subclass(class, exception)
}

const CXX_CONSTRUCT: &str = ".cxx_construct";
const CXX_DESTRUCT: &str = ".cxx_destruct";

/// Apple's object_cxxConstructFromClass for the game objects made since the
/// last call: each game class's own .cxx_construct, superclass first. Runs
/// before guest code next sees them (the bridge lock is not held then).
pub(super) fn construct_pending(bridge: &Mutex<Bridge>, g: &dyn GuestSide) -> Result<(), String> {
    loop {
        let Some(object) = UNCONSTRUCTED.with(|u| u.borrow_mut().pop()) else { return Ok(()) };
        let (sel, constructors) = {
            let mut b = lock(bridge);
            (b.guest_selector(g, CXX_CONSTRUCT), b.own_methods_up(u32_at(g, object), CXX_CONSTRUCT))
        };
        for imp in constructors.into_iter().rev() {
            if g.call(imp, &[object, sel])? as u32 == 0 {
                crate::log!("32-bit Objective-C: a .cxx_construct of object {object:#x} failed");
                break;
            }
        }
    }
}

/// Apple's object_cxxDestructFromClass: each game class's own
/// .cxx_destruct, subclass first (at the framework's -dealloc).
fn destruct(bridge: &Mutex<Bridge>, g: &dyn GuestSide, object: u32) -> Result<(), String> {
    let (sel, destructors) = {
        let mut b = lock(bridge);
        (b.guest_selector(g, CXX_DESTRUCT), b.own_methods_up(u32_at(g, object), CXX_DESTRUCT))
    };
    for imp in destructors {
        g.call(imp, &[object, sel])?;
    }
    Ok(())
}

fn live_wrappers<R>(f: impl FnOnce(&mut HashSet<usize>) -> R) -> R {
    f(LIVE_WRAPPERS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashSet::new))
}

/// The guest block a host block runs, if it is one of the bridge's.
fn wrapper_guest(host: usize) -> Option<u32> {
    live_wrappers(|live| live.contains(&host)).then(|| unsafe { (*(host as *const HostBlock)).guest as u32 })
}

/// Takes a reference to a guest heap block (a no-op for global blocks).
fn retain_guest_block(g: &dyn GuestSide, block: u32) {
    if super::functions::load_flags(g, block + 4) & BLOCK_NEEDS_FREE != 0 {
        super::functions::count_retain(g, block + 4);
    }
}

extern "C" fn wrapper_copy(destination: *mut HostBlock, source: *const HostBlock) {
    let guest = unsafe { (*source).guest as u32 };
    if let Some(g) = *BLOCK_OWNER.read().unwrap_or_else(|e| e.into_inner()) {
        retain_guest_block(g, guest);
    }
    live_wrappers(|live| live.insert(destination as usize));
}

extern "C" fn wrapper_dispose(block: *mut HostBlock) {
    live_wrappers(|live| live.remove(&(block as usize)));
    let guest = unsafe { (*block).guest as u32 };
    if let Some(g) = *BLOCK_OWNER.read().unwrap_or_else(|e| e.into_inner()) {
        if let Err(error) = super::functions::release_guest_block(g, guest) {
            crate::log!("32-bit Objective-C: releasing block {guest:#x}: {error}");
        }
    }
}

/// Where this thread's wrapper list stands (the start of a bridge call).
fn wrapper_mark() -> usize {
    SCOPED_WRAPPERS.with(|w| w.borrow().len())
}

/// Ends a bridge call: its wrappers go, with their guest references (the
/// host's copies keep their own). Call without the bridge lock held.
fn end_wrapper_scope(g: &dyn GuestSide, mark: usize) -> Result<(), String> {
    let ended: Vec<Box<HostBlock>> = SCOPED_WRAPPERS.with(|w| {
        let mut w = w.borrow_mut();
        let at = mark.min(w.len());
        w.split_off(at)
    });
    for wrapper in ended {
        live_wrappers(|live| live.remove(&(&*wrapper as *const HostBlock as usize)));
        super::functions::release_guest_block(g, wrapper.guest as u32)?;
    }
    Ok(())
}

/// Runs `work` as one bridge call: the block wrappers it makes go (with
/// their guest references) when it ends. Call without the bridge lock held.
pub fn in_wrapper_scope<R>(g: &dyn GuestSide, work: impl FnOnce() -> Result<R, String>) -> Result<R, String> {
    let mark = wrapper_mark();
    let result = work();
    end_wrapper_scope(g, mark)?;
    result
}

pub struct Bridge {
    pub classes: GuestClasses,
    to_host: HashMap<u32, usize>,
    to_guest: HashMap<usize, u32>,
    /// Game instances whose guest memory the bridge allocated.
    game_instances: HashMap<u32, ()>,
    framework: HashMap<String, (u32, u32)>,
    framework_by_guest: HashMap<u32, (String, bool)>,
    /// Game class (not metaclass) -> host mirror class.
    mirrors: HashMap<u32, usize>,
    /// Host protocol -> the game's protocol_t (see protocols.rs).
    protocols: HashMap<usize, u32>,
    sel_names: HashMap<u32, String>,
    sel_guest: HashMap<String, u32>,
    specials: HashMap<u32, Special>,
    /// Host object -> guest copy of its -bytes (address, capacity).
    buffers: HashMap<usize, (u32, u32)>,
    /// Host object -> guest copies of its `host-bytes` results.
    kept_copies: HashMap<usize, Vec<u32>>,
    /// Classes whose +initialize is running (on which thread) or done.
    init_state: HashMap<u32, InitState>,
    /// The game has a category +initialize on a framework class, so
    /// framework classes (their guest copies) are initialized too.
    framework_initializers: bool,
    /// Bytes the next game instance gets past its ivars (NSAllocateObject).
    extra_bytes: u32,
}

/// Where a message goes.
enum Target {
    Guest { imp: u32, types: String },
    Host { class: String, meta: bool, via_super: bool },
    /// No method in the game's class chain (the error if not forwarded).
    Missing(String),
}

impl Bridge {
    pub fn new(md: &ObjcMetadata32) -> Result<Bridge, String> {
        let classes = GuestClasses::from_metadata(md)?;
        let framework_initializers = classes.framework_category_entries().iter().any(|(_, meta, sel)| *meta && sel == "initialize");
        Ok(Bridge {
            framework_initializers,
            classes,
            to_host: HashMap::new(),
            to_guest: HashMap::new(),
            game_instances: HashMap::new(),
            framework: HashMap::new(),
            framework_by_guest: HashMap::new(),
            mirrors: HashMap::new(),
            protocols: HashMap::new(),
            sel_names: HashMap::new(),
            sel_guest: HashMap::new(),
            specials: HashMap::new(),
            buffers: HashMap::new(),
            kept_copies: HashMap::new(),
            init_state: HashMap::new(),
            extra_bytes: 0,
        })
    }

    /// Marks guest objects whose isa is `isa` (e.g. `__CFConstantStringClassReference`).
    pub fn set_special(&mut self, isa: u32, kind: Special) {
        self.specials.insert(isa, kind);
    }

    pub fn special(&self, isa: u32) -> Option<Special> {
        self.specials.get(&isa).copied()
    }

    /// The isa registered for a special kind.
    pub fn special_isa(&self, kind: Special) -> Option<u32> {
        self.specials.iter().find(|(_, k)| **k == kind).map(|(isa, _)| *isa)
    }

    /// The host mirror class of a game class.
    pub fn mirror_of(&self, guest_class: u32) -> Option<usize> {
        self.mirrors.get(&guest_class).copied()
    }

    /// A guest buffer of at least `length` bytes kept for host object `host`
    /// (the copy of its -bytes), grown when needed.
    fn guest_buffer(&mut self, g: &dyn GuestSide, host: usize, length: u32) -> u32 {
        match self.buffers.get(&host) {
            Some(&(address, capacity)) if capacity >= length.max(1) => address,
            old => {
                if let Some(&(address, _)) = old {
                    g.free(address);
                }
                let address = g.alloc(length.max(1));
                self.buffers.insert(host, (address, length.max(1)));
                address
            }
        }
    }

    /// Frees the guest copies of `host-bytes` results kept for host object
    /// `host` (it is finished with).
    pub fn release_host_copies(&mut self, g: &dyn GuestSide, host: usize) {
        for copy in self.kept_copies.remove(&host).unwrap_or_default() {
            g.free(copy);
        }
    }

    /// Text of a guest string object.
    pub fn object_text(&mut self, g: &dyn GuestSide, guest: u32) -> Option<String> {
        let host = self.host_object_for(g, guest).ok()?;
        (host != 0).then(|| crate::fw::string::get(host))
    }

    /// A host block that runs guest block `block` (which must already be a
    /// heap or global block; see functions::block_copy). It holds a
    /// reference to the guest block until the bridge call that made it ends;
    /// a host that keeps it copies it and holds its own.
    fn host_block(&mut self, g: &dyn GuestSide, block: u32) -> usize {
        retain_guest_block(g, block);
        let isa = crate::symbols::lookup("__NSConcreteStackBlock").map(|s| s.0).unwrap_or(0);
        let wrapper = Box::new(HostBlock {
            isa,
            flags: BLOCK_HAS_COPY_DISPOSE as i32,
            reserved: 0,
            invoke: mirror::trampoline_address(),
            descriptor: &WRAPPER_DESCRIPTOR,
            guest: block as usize,
        });
        let host = &*wrapper as *const HostBlock as usize;
        live_wrappers(|live| live.insert(host));
        SCOPED_WRAPPERS.with(|w| w.borrow_mut().push(wrapper));
        host
    }

    pub fn selector_name(&mut self, g: &dyn GuestSide, sel: u32) -> String {
        self.selector_str(g, sel).to_string()
    }

    /// The selector's name without a copy.
    pub fn selector_str(&mut self, g: &dyn GuestSide, sel: u32) -> &str {
        if !self.sel_names.contains_key(&sel) {
            let name = guest_cstr(g, sel);
            self.sel_guest.entry(name.clone()).or_insert(sel);
            self.sel_names.insert(sel, name);
        }
        &self.sel_names[&sel]
    }

    /// The one guest selector for `name` (the game's own string if it has one).
    pub fn guest_selector(&mut self, g: &dyn GuestSide, name: &str) -> u32 {
        if let Some(&sel) = self.sel_guest.get(name) {
            return sel;
        }
        let sel = g.alloc_permanent(name.len() as u32 + 1);
        g.write(sel, &[name.as_bytes(), &[0]].concat());
        self.sel_guest.insert(name.to_string(), sel);
        self.sel_names.insert(sel, name.to_string());
        sel
    }

    pub fn host_object(&self, guest: u32) -> Option<usize> {
        self.to_host.get(&guest).copied()
    }

    fn pair(&mut self, guest: u32, host: usize) {
        self.to_host.insert(guest, host);
        self.to_guest.insert(host, guest);
    }

    /// The imps of the game classes' own `sel` methods from `class` up
    /// through its game superclasses, nearest first (not inherited ones).
    fn own_methods_up(&self, mut class: u32, sel: &str) -> Vec<u32> {
        let mut found = Vec::new();
        while let Some(c) = self.classes.class(class) {
            found.extend(c.methods.get(sel).map(|m| m.imp));
            match c.superclass {
                super::classes::Super::Game(parent) => class = parent,
                _ => break,
            }
        }
        found
    }

    /// Forgets a game object whose host form was deallocated; frees its guest memory.
    pub fn forget(&mut self, g: &dyn GuestSide, guest: u32) {
        if let Some(host) = self.to_host.remove(&guest) {
            self.to_guest.remove(&host);
        }
        if self.game_instances.remove(&guest).is_some() {
            g.free(guest);
        }
    }

    pub(super) fn host_class_named(name: &str, meta: bool) -> usize {
        runtime::existing_class(name, meta).unwrap_or_else(|| runtime::placeholder_class(name, meta))
    }

    /// The guest copy of framework class `name`: (class, metaclass).
    pub fn framework_class(&mut self, g: &dyn GuestSide, name: &str) -> (u32, u32) {
        if let Some(&pair) = self.framework.get(name) {
            return pair;
        }
        let host = Self::host_class_named(name, false);
        let super_name = {
            let rt = runtime::lock();
            let superclass = rt.classes.get(&host).map(|r| r.superclass).unwrap_or(0);
            (superclass != 0).then(|| rt.class_name(superclass))
        };
        let parent = super_name.map(|s| self.framework_class(g, &s));
        let name_c = g.alloc_permanent(name.len() as u32 + 1);
        g.write(name_c, &[name.as_bytes(), &[0]].concat());
        let (class, meta) = (g.alloc_permanent(20), g.alloc_permanent(20));
        // A root metaclass is its own isa; others use their hierarchy's root metaclass.
        let root_meta = match parent {
            Some((_, parent_meta)) => u32_at(g, parent_meta),
            None => meta,
        };
        let (super_class, super_meta) = match parent {
            Some((pc, pm)) => (pc, pm),
            None => (0, class),
        };
        let root_flag = if parent.is_none() { guest32_image::RO_ROOT } else { 0 };
        let ro = |meta_flag: u32, size: u32| -> Vec<u8> {
            [meta_flag | root_flag, size, size, 0, name_c, 0, 0, 0, 0, 0].iter().flat_map(|w| w.to_le_bytes()).collect()
        };
        let (class_ro, meta_ro) = (g.alloc_permanent(40), g.alloc_permanent(40));
        g.write(class_ro, &ro(0, 4));
        g.write(meta_ro, &ro(guest32_image::RO_META, 20));
        let words = |w: [u32; 5]| -> Vec<u8> { w.iter().flat_map(|x| x.to_le_bytes()).collect() };
        g.write(class, &words([meta, super_class, 0, 0, class_ro]));
        g.write(meta, &words([root_meta, super_meta, 0, 0, meta_ro]));
        self.framework.insert(name.to_string(), (class, meta));
        self.framework_by_guest.insert(class, (name.to_string(), false));
        self.framework_by_guest.insert(meta, (name.to_string(), true));
        (class, meta)
    }

    /// The guest object for a host object (made on first sight).
    pub fn guest_object(&mut self, g: &dyn GuestSide, host: usize) -> Result<u32, String> {
        if host == 0 {
            return Ok(0);
        }
        if let Some(block) = wrapper_guest(host) {
            return Ok(block);
        }
        // A protocol (no class to read) goes back as the game's own.
        if let Some(&protocol) = self.protocols.get(&host) {
            return Ok(protocol);
        }
        let (class, is_class, name, meta, partner) = {
            let rt = runtime::lock();
            let is_class = rt.classes.contains_key(&host);
            let class = if is_class { host } else { rt.class_of(host) };
            let rec = rt.classes.get(&class).ok_or_else(|| format!("host object {host:#x} has unknown class {class:#x}"))?;
            (class, is_class, rec.name.clone(), rec.meta, rec.partner)
        };
        if let Some(&guest) = self.to_guest.get(&host) {
            // A host address can be reused after its object is freed: keep the
            // proxy only while its guest isa still names the same class.
            if is_class || self.guest_isa_matches(g, guest, class) {
                return Ok(guest);
            }
            self.to_guest.remove(&host);
            self.to_host.remove(&guest);
        }
        if is_class {
            let mirror_of = mirror::mirror_guest_class(if meta { partner } else { class });
            return Ok(match mirror_of {
                Some(guest_class) if meta => self.classes.class(guest_class).map(|c| c.partner).unwrap_or(0),
                Some(guest_class) => guest_class,
                None => {
                    let (c, m) = self.framework_class(g, &name);
                    if meta {
                        m
                    } else {
                        c
                    }
                }
            });
        }
        let guest = match mirror::mirror_guest_class(class) {
            Some(guest_class) => {
                let size = self.classes.class(guest_class).map(|c| c.instance_size).unwrap_or(4).max(4) + std::mem::take(&mut self.extra_bytes);
                let object = g.alloc(size);
                g.write(object, &vec![0u8; size as usize]);
                g.write(object, &guest_class.to_le_bytes());
                self.game_instances.insert(object, ());
                if !self.own_methods_up(guest_class, CXX_CONSTRUCT).is_empty() {
                    UNCONSTRUCTED.with(|u| u.borrow_mut().push(object));
                }
                object
            }
            None => {
                let (guest_class, _) = self.framework_class(g, &name);
                let proxy = g.alloc_permanent(8);
                g.write(proxy, &[guest_class.to_le_bytes(), [0; 4]].concat());
                proxy
            }
        };
        self.pair(guest, host);
        Ok(guest)
    }

    fn guest_isa_matches(&mut self, g: &dyn GuestSide, guest: u32, host_class: usize) -> bool {
        let isa = u32_at(g, guest);
        match mirror::mirror_guest_class(host_class) {
            Some(guest_class) => isa == guest_class,
            None => {
                let name = runtime::lock().class_name(host_class);
                self.framework.get(&name).map(|p| p.0) == Some(isa)
            }
        }
    }

    /// NSAllocateObject: a new zeroed instance of guest class `class`, the
    /// root allocator's way (no +alloc runs), with `extra` more bytes after a
    /// game class's ivars (a framework object is its usual proxy). Its C++
    /// constructors wait for construct_pending.
    pub fn allocate(&mut self, g: &dyn GuestSide, class: u32, extra: u32) -> Result<u32, String> {
        let host = runtime::alloc_instance(self.host_object_for(g, class)?);
        self.extra_bytes = extra;
        let object = self.guest_object(g, host);
        self.extra_bytes = 0;
        object
    }

    /// The host object for a guest object (a host form is made for game
    /// objects, constant strings and classes on first sight).
    pub fn host_object_for(&mut self, g: &dyn GuestSide, guest: u32) -> Result<usize, String> {
        if guest == 0 {
            return Ok(0);
        }
        if let Some(&host) = self.to_host.get(&guest) {
            return Ok(host);
        }
        if let Some(c) = self.classes.class(guest) {
            let (class, meta) = if c.meta { (c.partner, true) } else { (c.address, false) };
            let mirror = *self.mirrors.get(&class).ok_or_else(|| format!("game class {} has no host mirror", c.name))?;
            return Ok(if meta { runtime::lock().classes[&mirror].partner } else { mirror });
        }
        if let Some((name, meta)) = self.framework_by_guest.get(&guest) {
            return Ok(Self::host_class_named(name, *meta));
        }
        let isa = u32_at(g, guest);
        if let Some(c) = self.classes.class(isa) {
            if !c.meta {
                let mirror = *self.mirrors.get(&isa).ok_or_else(|| format!("game class {} has no host mirror", c.name))?;
                let host = runtime::alloc_instance(mirror);
                self.pair(guest, host);
                return Ok(host);
            }
        }
        match self.specials.get(&isa) {
            Some(Special::ConstantString) => {
                let words: Vec<u32> = (0..4).map(|i| u32_at(g, guest + 4 * i)).collect();
                let (flags, chars, length) = (words[1], words[2], words[3]);
                let text = if flags & 0x10 != 0 {
                    let raw = g.read(chars, length * 2);
                    String::from_utf16_lossy(&raw.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>())
                } else {
                    String::from_utf8_lossy(&g.read(chars, length)).into_owned()
                };
                let host = crate::fw::string::make(&text);
                self.pair(guest, host);
                Ok(host)
            }
            // Stack blocks are copied before they reach here (send_from_guest,
            // functions::block_to_host); a stray one is wrapped as it is.
            Some(Special::StackBlock | Special::MallocBlock | Special::GlobalBlock) => Ok(self.host_block(g, guest)),
            None => Err(format!("guest object {guest:#x} (isa {isa:#x}) is not known to the bridge")),
        }
    }

    /// Where a message to an object whose isa is `isa` goes.
    fn resolve(&self, isa: u32, sel: &str) -> Result<Target, String> {
        if self.classes.class(isa).is_some() {
            return match self.classes.lookup(isa, sel) {
                Lookup::Guest(m) => Ok(Target::Guest { imp: m.imp, types: m.types.clone() }),
                Lookup::Framework { class, meta } => self.framework_target(&class, meta, sel, false),
                Lookup::Missing => Ok(Target::Missing(format!("{MISSING_METHOD}{}{sel}]: no method in the game's class chain", self.describe(isa)))),
            };
        }
        match self.framework_by_guest.get(&isa) {
            Some((class, meta)) => self.framework_target(class, *meta, sel, false),
            None => Err(format!("message {sel} to an object with unknown isa {isa:#x}")),
        }
    }

    /// A framework class answers, unless the game added a category method
    /// to it or to one of its host superclasses.
    fn framework_target(&self, class: &str, meta: bool, sel: &str, via_super: bool) -> Result<Target, String> {
        if let Some((imp, types)) = self.framework_category(class, meta, sel) {
            return Ok(Target::Guest { imp, types });
        }
        Ok(Target::Host { class: class.to_string(), meta, via_super })
    }

    /// The game's category method `sel` on framework class `class` or a host
    /// superclass, unless a class nearer the receiver has its own (native)
    /// method: categories and native methods are searched together, the
    /// nearest wins, like the host's own lookup. (imp, types)
    fn framework_category(&self, class: &str, meta: bool, sel: &str) -> Option<(u32, String)> {
        let host_sel = runtime::sel(sel);
        let trampoline = mirror::trampoline_address();
        let mut host = runtime::existing_class(class, meta)?;
        loop {
            let (name, is_meta, native, superclass) = {
                let rt = runtime::lock();
                let rec = rt.classes.get(&host)?;
                (rec.name.clone(), rec.meta, rec.methods.get(&host_sel).copied(), rec.superclass)
            };
            if let Some(m) = self.classes.framework_category_method(&name, is_meta, sel) {
                return Some((m.imp, m.types.clone()));
            }
            // A category's own trampoline is not a native method.
            if native.is_some_and(|imp| imp != trampoline) {
                return None;
            }
            if superclass == 0 {
                return None;
            }
            host = superclass;
        }
    }

    /// Whether a receiver whose isa is `isa` answers `sel`: a game method, a
    /// game category on a framework class, or the host class's own method.
    pub(super) fn answers(&self, isa: u32, sel: &str) -> bool {
        let host_sel = runtime::sel(sel);
        let framework = |class: &str, meta: bool| {
            self.framework_category(class, meta, sel).is_some()
                || runtime::existing_class(class, meta).is_some_and(|host| runtime::lock().lookup(host, host_sel).is_some())
        };
        if self.classes.class(isa).is_some() {
            return match self.classes.lookup(isa, sel) {
                Lookup::Guest(_) => true,
                Lookup::Framework { class, meta } => framework(&class, meta),
                Lookup::Missing => false,
            };
        }
        self.framework_by_guest.get(&isa).is_some_and(|(class, meta)| framework(class, *meta))
    }

    /// Whether the game itself answers `sel` for a receiver whose isa is
    /// `isa` (its own method or a category method, not the host's).
    pub(super) fn game_answers(&self, isa: u32, sel: &str) -> bool {
        if self.classes.class(isa).is_some() {
            return match self.classes.lookup(isa, sel) {
                Lookup::Guest(_) => true,
                Lookup::Framework { class, meta } => self.framework_category(&class, meta, sel).is_some(),
                Lookup::Missing => false,
            };
        }
        self.framework_by_guest.get(&isa).is_some_and(|(class, meta)| self.framework_category(class, *meta, sel).is_some())
    }

    /// The armv7 type encoding of the method a receiver whose isa is `isa`
    /// answers `sel` with (the game's own, else the framework table's for a
    /// method the host has); None when it has no such method.
    pub(super) fn armv7_method_types(&self, isa: u32, sel: &str) -> Option<String> {
        let framework = |class: &str, meta: bool| -> Option<String> {
            if let Some((_, types)) = self.framework_category(class, meta, sel) {
                return Some(types);
            }
            let (host, host_sel) = (runtime::existing_class(class, meta)?, runtime::sel(sel));
            runtime::lock().lookup(host, host_sel)?;
            framework_row(class, sel).map(|row| row.encoding.to_string())
        };
        if self.classes.class(isa).is_some() {
            return match self.classes.lookup(isa, sel) {
                Lookup::Guest(m) => Some(m.types.clone()),
                Lookup::Framework { class, meta } => framework(&class, meta),
                Lookup::Missing => None,
            };
        }
        let (class, meta) = self.framework_by_guest.get(&isa)?;
        framework(class, *meta)
    }

    /// A guest class (or framework class's guest copy): whether it is a
    /// metaclass and its partner (the class for a metaclass, the metaclass
    /// for a class).
    pub(super) fn class_pair(&self, class: u32) -> Option<(bool, u32)> {
        if let Some(c) = self.classes.class(class) {
            return Some((c.meta, c.partner));
        }
        let (name, meta) = self.framework_by_guest.get(&class)?;
        self.framework.get(name).map(|&(c, m)| (*meta, if *meta { c } else { m }))
    }

    /// The name of the class the guest sees for a receiver's isa, and whether
    /// that isa is a metaclass (the receiver is the class itself).
    pub(super) fn class_display(&self, isa: u32) -> (String, bool) {
        if let Some(c) = self.classes.class(isa) {
            return (c.name.clone(), c.meta);
        }
        match self.framework_by_guest.get(&isa) {
            Some((name, meta)) => (name.clone(), *meta),
            None => (format!("isa {isa:#x}"), false),
        }
    }

    fn describe(&self, isa: u32) -> String {
        match self.classes.class(isa) {
            Some(c) => format!("{}[{} ", if c.meta { "+" } else { "-" }, c.name),
            None => format!("[{isa:#x} "),
        }
    }
}

/// Converts with the bridge state and guest memory (see convert.rs).
struct Session<'a> {
    bridge: &'a mut Bridge,
    g: &'a dyn GuestSide,
}

impl Boundary for Session<'_> {
    fn read(&self, addr: u32, len: u32) -> Vec<u8> {
        self.g.read(addr, len)
    }
    fn write(&mut self, addr: u32, data: &[u8]) {
        self.g.write(addr, data)
    }
    fn alloc(&mut self, size: u32) -> u32 {
        self.g.alloc(size)
    }
    fn free(&mut self, addr: u32) {
        self.g.free(addr)
    }
    fn host_address(&self, addr: u32) -> usize {
        self.g.host_address(addr)
    }
    fn guest_address(&self, host: usize) -> Option<u32> {
        self.g.guest_address(host)
    }
    fn id_to_host(&mut self, guest: u32) -> Result<usize, String> {
        self.bridge.host_object_for(self.g, guest)
    }
    fn id_to_guest(&mut self, host: usize) -> Result<u32, String> {
        self.bridge.guest_object(self.g, host)
    }
    fn sel_to_host(&mut self, guest: u32) -> Result<usize, String> {
        let name = self.bridge.selector_name(self.g, guest);
        Ok(runtime::sel(&name))
    }
    fn sel_to_guest(&mut self, host: usize) -> Result<u32, String> {
        let name = unsafe { runtime::cstr(host) };
        Ok(self.bridge.guest_selector(self.g, &name))
    }
    fn object_text(&mut self, guest: u32) -> Option<String> {
        let host = self.bridge.host_object_for(self.g, guest).ok()?;
        Some(crate::fw::string::get(host))
    }    /// Host objects (CG objects are objects in this layer) get their proxy,
    /// so C functions (guest32::fw, through the same pair table) and messages
    /// agree; other host pointers get a handle from the C layer's table.
    fn opaque_to_guest(&mut self, host: usize) -> Result<u32, String> {
        if is_host_object(host) {
            self.bridge.guest_object(self.g, host)
        } else {
            Ok(super::super::fw::handle_to_guest(host))
        }
    }
    fn opaque_to_host(&mut self, guest: u32) -> Option<usize> {
        self.bridge.host_object(guest).or_else(|| Some(super::super::fw::handle_to_host(guest)).filter(|h| *h != 0))
    }
}

/// Whether host pointer `value` is an object of the host runtime (checked
/// as a plausible pointer before its class is read).
pub fn is_host_object(value: usize) -> bool {
    if value < 0x10000 || value >= 1 << 48 || value % 8 != 0 {
        return false;
    }
    // Data that only looks like a pointer (NDR_record, handles) may point at
    // nothing, so the class word is read without faulting.
    let Some(isa) = crate::report::read_word(value) else { return false };
    runtime::lock().classes.contains_key(&isa)
}

fn lock(bridge: &Mutex<Bridge>) -> std::sync::MutexGuard<'_, Bridge> {
    bridge.lock().unwrap_or_else(|e| e.into_inner())
}

/// Slides game ivars past grown superclasses, defines the host mirror of
/// every game class (parents first) and makes every selector reference
/// point at one string per selector name.
pub fn install(bridge: &Mutex<Bridge>, g: &dyn GuestSide, md: &ObjcMetadata32) -> Result<(), String> {
    let mut b = lock(bridge);
    // The game's code reads the offset variables: they move with the ivars.
    for (variable, diff) in b.classes.slide_ivars() {
        g.write(variable, &(u32_at(g, variable) + diff).to_le_bytes());
    }
    let order: Vec<(u32, String, super::classes::Super, u32)> = b
        .classes
        .classes_parents_first()
        .into_iter()
        .map(|c| (c.address, c.name.clone(), c.superclass.clone(), c.partner))
        .collect();
    for (address, name, superclass, meta) in order {
        let host_super = match superclass {
            super::classes::Super::Game(parent) => *b.mirrors.get(&parent).ok_or("superclass mirror missing")?,
            super::classes::Super::Framework(name) => Bridge::host_class_named(&name, false),
            super::classes::Super::None => 0,
        };
        let instance: Vec<String> = b.classes.selectors_of(address).into_iter().map(str::to_string).collect();
        // +load and +initialize are the bridge's to run (run_loads, initialize):
        // the host runtime's own +initialize must not reach the game's again.
        let class: Vec<String> =
            b.classes.selectors_of(meta).into_iter().filter(|s| !matches!(*s, "load" | "initialize")).map(str::to_string).collect();
        let instance: Vec<&str> = instance.iter().map(String::as_str).collect();
        let class: Vec<&str> = class.iter().map(String::as_str).collect();
        let mirror = mirror::define_mirror_class(&name, host_super, address, &instance, &class)?;
        b.mirrors.insert(address, mirror);
    }
    // Game protocols: a host form each way, and the classes adopt theirs
    // (categories' too, on game and framework classes).
    let protocols = super::protocols::host_protocols(md);
    for (&guest, &host) in &protocols {
        b.pair(guest, host);
        b.protocols.insert(host, guest);
    }
    let hosts_of = |list: &[u32]| -> Vec<usize> { list.iter().filter_map(|p| protocols.get(p).copied()).collect() };
    for c in &md.classes {
        if let Some(&mirror) = b.mirrors.get(&c.address) {
            runtime::add_protocols(mirror, &hosts_of(&c.instance.protocols));
        }
    }
    for category in &md.categories {
        let class = match &category.class {
            guest32_image::ClassTarget::Local(address) => b.mirrors.get(address).copied().unwrap_or(0),
            guest32_image::ClassTarget::External(name) => Bridge::host_class_named(name, false),
            _ => 0,
        };
        runtime::add_protocols(class, &hosts_of(&category.protocols));
    }
    // Game category methods on framework classes answer host messages too.
    for (class, meta, sel) in b.classes.framework_category_entries() {
        // +initialize too: the host runtime's own +initialize of a host class
        // then reaches the bridge, which runs it once (dispatch_from_host).
        if sel != "load" {
            mirror::add_mirror_method(Bridge::host_class_named(&class, meta), &sel);
        }
    }
    for r in &md.selector_refs {
        let current = u32_at(g, r.slot);
        let canonical = *b.sel_guest.entry(r.name.clone()).or_insert(current);
        b.sel_names.insert(canonical, r.name.clone());
        if canonical != current {
            g.write(r.slot, &canonical.to_le_bytes());
        }
    }
    Ok(())
}

/// Runs +load of every game class and category that defines one, in
/// Apple's order: superclasses before subclasses, classes before
/// categories, otherwise image order. Before the image's C++ initializers.
pub fn run_loads(bridge: &Mutex<Bridge>, g: &dyn GuestSide, md: &ObjcMetadata32) -> Result<(), String> {
    fn visit<'a>(
        class: &'a guest32_image::ObjcClass32,
        by_address: &HashMap<u32, &'a guest32_image::ObjcClass32>,
        done: &mut HashSet<u32>,
        order: &mut Vec<(u32, u32)>,
    ) {
        if !done.insert(class.address) {
            return;
        }
        if let guest32_image::ClassTarget::Local(parent) = &class.superclass {
            if let Some(parent) = by_address.get(parent) {
                visit(parent, by_address, done, order);
            }
        }
        if let Some(load) = class.meta.methods.iter().find(|m| m.name == "load") {
            order.push((load.imp, class.address));
        }
    }
    let by_address: HashMap<u32, &guest32_image::ObjcClass32> = md.classes.iter().map(|c| (c.address, c)).collect();
    let (mut done, mut order) = (HashSet::new(), Vec::new());
    for class in &md.classes {
        visit(class, &by_address, &mut done, &mut order);
    }
    for category in &md.categories {
        let Some(load) = category.class_methods.iter().find(|m| m.name == "load") else { continue };
        let class = match &category.class {
            guest32_image::ClassTarget::Local(class) => *class,
            guest32_image::ClassTarget::External(name) => lock(bridge).framework_class(g, name).0,
            _ => continue,
        };
        order.push((load.imp, class));
    }
    let sel = lock(bridge).guest_selector(g, "load");
    for (imp, class) in order {
        g.call(imp, &[class, sel])?;
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InitState {
    Running(std::thread::ThreadId),
    Done,
}

/// Wakes threads waiting for some class's +initialize to finish.
static INIT_DONE: (Mutex<()>, std::sync::Condvar) = (Mutex::new(()), std::sync::Condvar::new());

/// The class (a game class, or a framework class's guest copy when the game
/// has framework category initializers) whose +initialize must be done
/// before a message to an object whose isa is `isa`.
fn class_to_initialize(b: &Bridge, isa: u32) -> Option<u32> {
    let class = match b.classes.class(isa) {
        Some(c) if c.meta => c.partner,
        Some(_) => isa,
        None if b.framework_initializers => b.framework.get(&b.framework_by_guest.get(&isa)?.0)?.0,
        None => return None,
    };
    (b.init_state.get(&class) != Some(&InitState::Done)).then_some(class)
}

/// What +initialize of a class resolves through.
enum Initializer {
    /// A game class: its metaclass's chain, then framework categories.
    Game(u32),
    /// A framework class: game categories on it or its host superclasses.
    Framework(String),
}

/// Sends +initialize to a class once, its superclasses first. Like Apple's
/// runtime it is a message: a class without its own +initialize runs its
/// superclass's (a game category's on a framework class included). The
/// thread running it may message the class again; other threads wait until
/// it is done (holding neither the bridge lock nor the guest lock).
fn initialize(bridge: &Mutex<Bridge>, g: &dyn GuestSide, class: u32) -> Result<(), String> {
    let me = std::thread::current().id();
    let state = |b: &Bridge| b.init_state.get(&class).copied();
    let (parent, initializer, sel) = loop {
        let mut b = lock(bridge);
        match state(&b) {
            Some(InitState::Done) => return Ok(()),
            Some(InitState::Running(owner)) if owner == me => return Ok(()),
            Some(InitState::Running(_)) => {
                drop(b);
                g.without_guest_lock(&mut || {
                    let mut waiting = INIT_DONE.0.lock().unwrap_or_else(|e| e.into_inner());
                    while matches!(state(&lock(bridge)), Some(InitState::Running(_))) {
                        waiting = INIT_DONE.1.wait(waiting).unwrap_or_else(|e| e.into_inner());
                    }
                });
            }
            None => {
                let framework_parent = |b: &mut Bridge, name: &str| b.framework_initializers.then(|| b.framework_class(g, name).0);
                let (parent, initializer) = if let Some(c) = b.classes.class(class) {
                    let (superclass, meta) = (c.superclass.clone(), c.partner);
                    let parent = match superclass {
                        super::classes::Super::Game(parent) => Some(parent),
                        super::classes::Super::Framework(name) => framework_parent(&mut b, &name),
                        super::classes::Super::None => None,
                    };
                    (parent, Initializer::Game(meta))
                } else if let Some((name, _)) = b.framework_by_guest.get(&class).cloned() {
                    let host = runtime::existing_class(&name, false).unwrap_or(0);
                    let superclass = {
                        let rt = runtime::lock();
                        rt.classes.get(&host).map(|r| r.superclass).filter(|s| *s != 0).map(|s| rt.class_name(s))
                    };
                    let parent = superclass.and_then(|s| framework_parent(&mut b, &s));
                    (parent, Initializer::Framework(name))
                } else {
                    return Err(format!("+initialize of unknown class {class:#x}"));
                };
                b.init_state.insert(class, InitState::Running(me));
                break (parent, initializer, b.guest_selector(g, "initialize"));
            }
        }
    };
    let result = (|| {
        if let Some(parent) = parent {
            initialize(bridge, g, parent)?;
        }
        let imp = {
            let b = lock(bridge);
            match &initializer {
                Initializer::Game(meta) => match b.classes.lookup(*meta, "initialize") {
                    Lookup::Guest(m) => Some(m.imp),
                    Lookup::Framework { class, meta } => b.framework_category(&class, meta, "initialize").map(|m| m.0),
                    Lookup::Missing => None,
                },
                Initializer::Framework(name) => b.framework_category(name, true, "initialize").map(|m| m.0),
            }
        };
        if let Some(imp) = imp {
            g.call(imp, &[class, sel])?;
        }
        Ok(())
    })();
    // Done even after a failure: waiting threads must not hang.
    lock(bridge).init_state.insert(class, InitState::Done);
    let _wake = INIT_DONE.0.lock().unwrap_or_else(|e| e.into_inner());
    INIT_DONE.1.notify_all();
    result
}

/// The guest memory the host-side block helpers use (see HostBlock).
pub fn set_block_owner(g: &'static dyn GuestSide) {
    *BLOCK_OWNER.write().unwrap_or_else(|e| e.into_inner()) = Some(g);
}

/// How every "the game asked for a method nobody has" error starts, so a
/// run that keeps going can skip just that message (`functions::skip_missing`).
pub const MISSING_METHOD: &str = "missing method ";

/// The framework table's row for a message to an instance of the host class
/// `class`: that class's own row (`class:Name`), else the general one.
pub fn framework_row(class: &str, sel: &str) -> Option<&'static FrameworkSelector> {
    let is_kind = |name: &str| {
        let (receiver, named) = (runtime::class_named(class), runtime::class_named(name));
        let rt = runtime::lock();
        receiver != 0 && named != 0 && rt.is_subclass(receiver, named)
    };
    FrameworkSelectors::builtin().get_for(sel, is_kind)
}

/// Throws `exception` (a host NSException) in the guest, as -raise does.
pub(super) fn throw_in_guest(b: &mut Bridge, g: &dyn GuestSide, exception: usize) -> Result<GuestRet, String> {
    let exception = b.guest_object(g, exception)?;
    PENDING_THROW.with(|p| p.set(Some(exception)));
    Ok(GuestRet { r0: 0, r1: 0 })
}

/// Type encoding for a message the host answers: the framework table, else
/// the game's own method of the same name (an override has the same types).
fn host_signature(b: &Bridge, class: &str, meta: bool, sel: &str) -> Result<(Signature, HostCallOptions), String> {
    if let Some(entry) = framework_row(class, sel) {
        let options =
            HostCallOptions {
                real_float: entry.real_float,
                varargs: entry.varargs,
                arrays: entry.arrays,
                host_bytes: entry.host_bytes,
                not_found: entry.not_found,
            };
        return Ok((entry.signature.clone(), options));
    }
    if let Some(types) = b.classes.types_for_selector(sel) {
        return Ok((Signature::parse_method(types)?, HostCallOptions::default()));
    }
    Err(format!(
        "{MISSING_METHOD}{}[{class} {sel}]: no type encoding for this framework selector (add it to framework_selectors.tsv)",
        if meta { "+" } else { "-" }
    ))
}

/// objc_msgSend and friends. `words`: the call's argument words by index
/// (r0-r3, then the guest stack).
pub fn send_from_guest(
    bridge: &Mutex<Bridge>,
    g: &dyn GuestSide,
    kind: SendKind,
    words: &dyn Fn(u32) -> u32,
) -> Result<GuestRet, String> {
    // A setter of a key something observes on the receiver: announced
    // around the call, as Apple's KVO does (fw/object.rs). Not for super
    // calls: the setter the game sent already announces it.
    if kind == SendKind::Normal && crate::fw::object::kvo_active() {
        if let Some((object, key)) = observed_setter(bridge, g, words) {
            crate::fw::object::will_change(object, &key);
            let result = send_unobserved(bridge, g, kind, words);
            crate::fw::object::did_change(object, &key);
            return result;
        }
    }
    send_unobserved(bridge, g, kind, words)
}

/// The receiver (as the host knows it) and key, when this message is the
/// setter of an observed key.
fn observed_setter(bridge: &Mutex<Bridge>, g: &dyn GuestSide, words: &dyn Fn(u32) -> u32) -> Option<(runtime::Id, String)> {
    let (object, name) = {
        let mut b = lock(bridge);
        let receiver = words(0);
        if receiver == 0 {
            return None;
        }
        let name = b.selector_str(g, words(1));
        if !(name.starts_with("set") || name.starts_with("_set")) {
            return None;
        }
        let name = name.to_string();
        (b.host_object(receiver)?, name)
    };
    let key = crate::fw::object::observed_setter_key(object, &name)?;
    Some((object, key))
}

fn send_unobserved(
    bridge: &Mutex<Bridge>,
    g: &dyn GuestSide,
    kind: SendKind,
    words: &dyn Fn(u32) -> u32,
) -> Result<GuestRet, String> {
    let stret = matches!(kind, SendKind::Stret | SendKind::Super2Stret);
    let first = u32::from(stret);
    let mut b = lock(bridge);
    let sel = words(first + 1);
    // A message a game method answered before: straight to it, without the
    // lookups by name below (most of a busy game's message time).
    if matches!(kind, SendKind::Normal | SendKind::Stret) && !runtime::message_trace_on() {
        let receiver = words(first);
        if receiver != 0 {
            if let Some(send) = b.classes.cached_send(u32_at(g, receiver), sel) {
                drop(b);
                return call_game_method(g, send, words);
            }
        }
    }
    let name = b.selector_name(g, sel);
    // The class the lookup starts at (a super call's starting superclass).
    let start;
    // (class, selector) when the answer may be remembered: a game method
    // found in the game's own class chain, for a class whose +initialize is done.
    let mut cacheable = None;
    let (receiver, target) = match kind {
        SendKind::Normal | SendKind::Stret => {
            let receiver = words(first);
            if receiver == 0 {
                return Ok(GuestRet { r0: if stret { words(0) } else { 0 }, r1: 0 });
            }
            let isa = u32_at(g, receiver);
            start = isa;
            if b.special(isa).is_some_and(Special::is_block) {
                drop(b);
                let ctx = super::functions::Ctx { bridge, g };
                let r0 = match name.as_str() {
                    "copy" | "retain" => super::functions::block_copy(&ctx, receiver)?,
                    "release" => {
                        super::functions::block_release(&ctx, receiver)?;
                        0
                    }
                    "autorelease" | "self" => receiver,
                    other => return Err(format!("message {other} to a block")),
                };
                return Ok(GuestRet { r0, r1: 0 });
            }
            if let Some(class) = class_to_initialize(&b, isa) {
                drop(b);
                initialize(bridge, g, class)?;
                b = lock(bridge);
            }
            let target = b.resolve(isa, &name)?;
            if matches!(target, Target::Guest { .. }) && matches!(b.classes.lookup(isa, &name), Lookup::Guest(_)) && class_to_initialize(&b, isa).is_none() {
                cacheable = Some(isa);
            }
            (receiver, target)
        }
        SendKind::Super2 | SendKind::Super2Stret => {
            let pair = words(first);
            let (receiver, current) = (u32_at(g, pair), u32_at(g, pair + 4));
            let c = b.classes.class(current).ok_or_else(|| format!("super call {name} from unknown class {current:#x}"))?;
            let (meta, superclass) = (c.meta, c.superclass.clone());
            start = match &superclass {
                super::classes::Super::Game(parent) => *parent,
                super::classes::Super::Framework(class) => {
                    let (class, metaclass) = b.framework_class(g, class);
                    if meta { metaclass } else { class }
                }
                super::classes::Super::None => return Err(format!("super call {name} from a root class")),
            };
            let target = match superclass {
                super::classes::Super::Game(parent) => b.resolve_super(parent, &name)?,
                super::classes::Super::Framework(class) => b.framework_target(&class, meta, &name, true)?,
                super::classes::Super::None => return Err(format!("super call {name} from a root class")),
            };
            (receiver, target)
        }
    };
    // Test switch IOS_TRACE_MESSAGES: the same "-[Class selector]" lines the
    // host's messages get, with the guest's class names.
    if runtime::message_trace_on() {
        let (class_name, class_is_meta) = b.class_display(u32_at(g, receiver));
        if let Some(text) = runtime::traced_message_text(&class_name, class_is_meta, &name) {
            crate::log!("32-bit message {text}");
        }
    }
    let receiver_word = first;
    let get = |i: u32| if i == receiver_word { receiver } else { words(i) };
    match target {
        Target::Missing(error) => {
            drop(b);
            super::forward::forward_message(bridge, g, kind, words, receiver, start, &error)
        }
        Target::Guest { imp, types } => {
            let send = CachedSend { imp, words: Signature::parse_method(&types)?.guest_total_words() };
            // A game method a fix follows (game_fixes.rs) is not remembered,
            // so each call comes back here.
            let fixed = super::game_fixes::follows(&name, || b.class_display(u32_at(g, receiver)).0);
            if let Some(isa) = cacheable.filter(|_| !fixed) {
                b.classes.remember_send(isa, sel, send);
            }
            drop(b);
            let result = call_game_method(g, send, &get)?;
            if fixed {
                super::game_fixes::after_message(bridge, g, receiver, get(first + 2))?;
            }
            Ok(result)
        }
        Target::Host { class, meta, via_super } => {
            // +instanceMethodForSelector: / -methodForSelector:: the game calls
            // the answer itself, so it is guest code: a game method's own
            // function, else objc_msgSend's stub (stret's for struct results),
            // which reaches the method alike. (The host's answer is host code.)
            if matches!(name.as_str(), "instanceMethodForSelector:" | "methodForSelector:") {
                let owner = if name == "instanceMethodForSelector:" { receiver } else { u32_at(g, receiver) };
                let asked = b.selector_name(g, get(first + 2));
                let imp = match b.classes.lookup(owner, &asked) {
                    Lookup::Guest(m) => m.imp,
                    Lookup::Framework { class, .. } => message_stub(g, framework_row(&class, &asked).map(|e| e.encoding)),
                    Lookup::Missing => message_stub(g, FrameworkSelectors::builtin().get(&asked).map(|e| e.encoding)),
                };
                return Ok(GuestRet { r0: imp, r1: 0 });
            }
            // NSException's -raise and +raise:format: from game code: thrown in
            // the guest, as objc_exception_throw throws (the host's own raise
            // would unwind host code).
            let raises = (name == "raise" && !meta) || (name == "raise:format:" && meta);
            if raises && is_exception_class(&class) {
                drop(b);
                let exception = if meta {
                    let (string_class, with_format) = {
                        let mut b = lock(bridge);
                        (b.framework_class(g, "NSString").0, b.guest_selector(g, "stringWithFormat:"))
                    };
                    // [NSString stringWithFormat:format, ...] with the same arguments.
                    let format_args = |i: u32| match i {
                        0 => string_class,
                        1 => with_format,
                        i => get(i + 1),
                    };
                    let reason = send_from_guest(bridge, g, SendKind::Normal, &format_args)?.r0;
                    let ctx = super::functions::Ctx { bridge, g };
                    ctx.send(receiver, "exceptionWithName:reason:userInfo:", &[get(first + 2), reason, 0])?
                } else {
                    receiver
                };
                PENDING_THROW.with(|p| p.set(Some(exception)));
                return Ok(GuestRet { r0: 0, r1: 0 });
            }
            // -doesNotRecognizeSelector: from game code: Apple's exception,
            // thrown in the guest like -raise.
            if name == "doesNotRecognizeSelector:" && !meta {
                let asked = b.selector_name(g, get(first + 2));
                let host = b.host_object_for(g, receiver)?;
                drop(b);
                let exception = crate::fw::object::unrecognized_selector_exception(host, &asked);
                let exception = lock(bridge).guest_object(g, exception)?;
                PENDING_THROW.with(|p| p.set(Some(exception)));
                return Ok(GuestRet { r0: 0, r1: 0 });
            }
            // Method signatures for game code have the armv7 layout, and the
            // root classes' -forwardInvocation: (see forward.rs).
            match name.as_str() {
                "signatureWithObjCTypes:" if meta => {
                    let types = guest_cstr(g, get(first + 2));
                    let signature = crate::fw::invocation::make_signature_in(&types, crate::fw::invocation::Layout::Armv7);
                    return Ok(GuestRet { r0: b.guest_object(g, signature)?, r1: 0 });
                }
                "methodSignatureForSelector:" | "instanceMethodSignatureForSelector:" => {
                    let asked = b.selector_name(g, get(first + 2));
                    let host_receiver = b.host_object_for(g, receiver)?;
                    if name == "methodSignatureForSelector:" && !meta && crate::objc::forward::is_proxy_object(host_receiver) {
                        drop(b);
                        let exception = crate::fw::object::proxy_abstract_exception("methodSignatureForSelector:");
                        return throw_in_guest(&mut lock(bridge), g, exception);
                    }
                    // Of the receiver's own class, also from a super call (as Apple's).
                    let owner = if name == "instanceMethodSignatureForSelector:" { receiver } else { u32_at(g, receiver) };
                    let signature = match b.armv7_method_types(owner, &asked) {
                        Some(types) => crate::fw::invocation::make_signature_in(&types, crate::fw::invocation::Layout::Armv7),
                        None => 0,
                    };
                    return Ok(GuestRet { r0: b.guest_object(g, signature)?, r1: 0 });
                }
                "forwardInvocation:" if !meta => {
                    drop(b);
                    return super::forward::root_forward_invocation(bridge, g, receiver, get(first + 2));
                }
                "sortUsingFunction:context:" | "sortedArrayUsingFunction:context:" if !meta => {
                    drop(b);
                    let in_place = name == "sortUsingFunction:context:";
                    return sort_with_game_function(bridge, g, receiver, get(first + 2), get(first + 3), in_place);
                }
                _ => {}
            }
            // No host method: Apple's forwarding (forward.rs).
            let host_receiver = b.host_object_for(g, receiver)?;
            let host_sel = runtime::sel(&name);
            drop(b);
            let imp = if via_super {
                let host_class = Bridge::host_class_named(&class, meta);
                runtime::lock().lookup(host_class, host_sel)
            } else {
                runtime::imp_for(host_receiver, host_sel)
            };
            let Some(imp) = imp else {
                let error = format!("{MISSING_METHOD}{}[{class} {name}]: not implemented by the host layer", if meta { "+" } else { "-" });
                return super::forward::forward_message(bridge, g, kind, words, receiver, start, &error);
            };
            b = lock(bridge);
            // The framework's -dealloc of a game object: its C++ ivars go first.
            let game_dealloc = name == "dealloc" && b.classes.class(u32_at(g, receiver)).is_some_and(|c| !c.meta);
            if game_dealloc {
                drop(b);
                destruct(bridge, g, receiver)?;
                super::functions::object_dying(&super::functions::Ctx { bridge, g }, receiver)?;
                b = lock(bridge);
            }
            let mark = wrapper_mark();
            let (sig, options) = host_signature(&b, &class, meta, &name)?;
            // The host may keep a block argument: stack blocks move to the heap first.
            let mut replaced: Vec<(u32, u32)> = Vec::new();
            let block_words: Vec<u32> = sig
                .args
                .iter()
                .zip(sig.guest_arg_words())
                .filter(|(ty, _)| **ty == super::encoding::Ty::Block)
                .map(|(_, at)| at)
                .collect();
            if !block_words.is_empty() {
                let stack_isa = b.special_isa(Special::StackBlock);
                drop(b);
                let ctx = super::functions::Ctx { bridge, g };
                for at in block_words {
                    let block = get(at);
                    if block != 0 && stack_isa.is_some_and(|isa| u32_at(g, block) == isa) {
                        replaced.push((at, super::functions::block_copy(&ctx, block)?));
                    }
                }
                b = lock(bridge);
            }
            let get = |i: u32| replaced.iter().find(|r| r.0 == i).map(|r| r.1).unwrap_or_else(|| get(i));
            let call = convert::prepare_host_call(&sig, options, &get, &mut Session { bridge: &mut b, g })?;
            let host_receiver = call.args.x[0] as usize;
            drop(b);
            // An exception the host method raises is thrown in the game once it returns.
            let (mut ret, raised) = crate::fw::exception::catching(|| mirror::call_host(imp, &call.args));
            // -bytes / -mutableBytes: data bytes live in guest memory (fw::data)
            // and pass as they are; bytes in host memory get a guest copy
            // (writes through a mutableBytes copy would not reach the host object).
            // -length first, unlocked: a game subclass may answer it in guest code.
            let pointer = ret.x[0] as usize;
            let in_host = pointer != 0 && g.guest_address(pointer).is_none();
            let host_bytes = matches!(name.as_str(), "bytes" | "mutableBytes") && in_host;
            let length = if host_bytes { runtime::send(host_receiver, "length", &[]) } else { 0 };
            // A `host-bytes` result (-decodeBytesForKey:returnedLength:): the
            // count is in the out-parameter; each call gets its own copy, kept
            // until the receiver is finished with (see release_host_copies).
            let counted_length = (options.host_bytes && in_host).then(|| call.first_out_value().unwrap_or(0) as usize);
            let mut b = lock(bridge);
            if host_bytes {
                let copy = b.guest_buffer(g, host_receiver, length as u32);
                g.write(copy, unsafe { std::slice::from_raw_parts(pointer as *const u8, length) });
                ret.x[0] = g.host_address(copy) as u64;
            }
            if let Some(length) = counted_length {
                let copy = g.alloc(length.max(1) as u32);
                g.write(copy, unsafe { std::slice::from_raw_parts(pointer as *const u8, length) });
                b.kept_copies.entry(host_receiver).or_default().push(copy);
                ret.x[0] = g.host_address(copy) as u64;
            }
            let result = convert::finish_host_call(call, &ret, &mut Session { bridge: &mut b, g })?;
            if game_dealloc {
                b.forget(g, receiver);
            }
            if let Some(exception) = raised {
                let exception = b.guest_object(g, exception)?;
                PENDING_THROW.with(|p| p.set(Some(exception)));
            }
            drop(b);
            construct_pending(bridge, g)?;
            // The heap copies made above: a host wrapper holds its own reference.
            let ctx = super::functions::Ctx { bridge, g };
            for (_, copy) in replaced {
                super::functions::block_release(&ctx, copy)?;
            }
            end_wrapper_scope(g, mark)?;
            Ok(result)
        }
    }
}

/// Runs a game method with the message's argument words.
fn call_game_method(g: &dyn GuestSide, send: CachedSend, words: &dyn Fn(u32) -> u32) -> Result<GuestRet, String> {
    let call_words: Vec<u32> = (0..send.words).map(words).collect();
    let r = g.call(send.imp, &call_words)?;
    Ok(GuestRet { r0: r as u32, r1: (r >> 32) as u32 })
}

/// -sortUsingFunction:context: and -sortedArrayUsingFunction:context: on a
/// framework array: the comparator is game code, so the sort runs here and
/// calls it with the elements' guest ids (a stable merge sort, which any
/// comparator answer leaves whole).
fn sort_with_game_function(
    bridge: &Mutex<Bridge>,
    g: &dyn GuestSide,
    receiver: u32,
    function: u32,
    context: u32,
    in_place: bool,
) -> Result<GuestRet, String> {
    let array = lock(bridge).host_object_for(g, receiver)?;
    let count = runtime::send(array, "count", &[]);
    let hosts: Vec<usize> = (0..count).map(|i| runtime::send(array, "objectAtIndex:", &[i])).collect();
    let guests = {
        let mut b = lock(bridge);
        hosts.iter().map(|&host| b.guest_object(g, host)).collect::<Result<Vec<u32>, String>>()?
    };
    // NSComparisonResult: NSOrderedDescending (1) puts the first after the second.
    let after = |x: usize, y: usize| -> Result<bool, String> { Ok(g.call(function, &[guests[x], guests[y], context])? as u32 as i32 > 0) };
    let mut order: Vec<usize> = (0..hosts.len()).collect();
    let mut width = 1;
    while width < order.len() {
        let mut merged = Vec::with_capacity(order.len());
        for run in order.chunks(width * 2) {
            let (left, right) = run.split_at(width.min(run.len()));
            let (mut i, mut j) = (0, 0);
            while i < left.len() && j < right.len() {
                if after(left[i], right[j])? {
                    merged.push(right[j]);
                    j += 1;
                } else {
                    merged.push(left[i]);
                    i += 1;
                }
            }
            merged.extend_from_slice(&left[i..]);
            merged.extend_from_slice(&right[j..]);
        }
        order = merged;
        width *= 2;
    }
    let sorted = crate::fw::collections::array_from(&order.iter().map(|&i| hosts[i]).collect::<Vec<_>>());
    if in_place {
        runtime::send(array, "setArray:", &[sorted]);
        runtime::send(sorted, "release", &[]);
        return Ok(GuestRet { r0: 0, r1: 0 });
    }
    Ok(GuestRet { r0: lock(bridge).guest_object(g, sorted)?, r1: 0 })
}

/// A guest call to a C function the host layer implements (`address`),
/// converted by `sig` (its arguments only, no self/_cmd).
pub fn call_host_function(
    bridge: &Mutex<Bridge>,
    g: &dyn GuestSide,
    address: usize,
    sig: &Signature,
    options: HostCallOptions,
    get: &dyn Fn(u32) -> u32,
) -> Result<GuestRet, String> {
    let mark = wrapper_mark();
    let call = convert::prepare_host_call(sig, options, get, &mut Session { bridge: &mut lock(bridge), g })?;
    // Unlocked: the host may message game objects (NSLog's %@ asks for -description).
    let ret = mirror::call_host(address, &call.args);
    let result = convert::finish_host_call(call, &ret, &mut Session { bridge: &mut lock(bridge), g });
    end_wrapper_scope(g, mark)?;
    construct_pending(bridge, g)?;
    result
}

impl Bridge {
    /// A super call's target, searched from game class `parent` up: a
    /// framework class at the end answers with its own host method (the
    /// receiver's host class would find the game's override again).
    fn resolve_super(&self, parent: u32, sel: &str) -> Result<Target, String> {
        match self.classes.lookup(parent, sel) {
            Lookup::Guest(m) => Ok(Target::Guest { imp: m.imp, types: m.types.clone() }),
            Lookup::Framework { class, meta } => self.framework_target(&class, meta, sel, true),
            Lookup::Missing => Ok(Target::Missing(format!("{MISSING_METHOD}{}{sel}]: super call finds no method", self.describe(parent)))),
        }
    }
}

/// A host message the game object `frame.x[0]` has no method for and
/// forwards by `signature` (an armv7 NSMethodSignature): the registers as
/// guest words, the invocation's result back in the host's registers.
pub fn forward_host_message(bridge: &Mutex<Bridge>, g: &dyn GuestSide, frame: &mut HostFrame, signature: usize) -> Result<(), String> {
    let types = crate::fw::invocation::signature_of(signature).ok_or("forwarding without a method signature")?;
    let sig = Signature::parse_method(&types.text)?;
    let mut b = lock(bridge);
    let receiver = b.guest_object(g, frame.x[0] as usize)?;
    let call = convert::prepare_guest_call(&sig, frame, &mut Session { bridge: &mut b, g })?;
    drop(b);
    let words = call.words.clone();
    let get = |i: u32| words.get(i as usize).copied().unwrap_or(0);
    let ret = super::forward::forward_invocation(bridge, g, receiver, signature, sig.ret.guest_returns_in_memory(), &get)?;
    let mut b = lock(bridge);
    convert::finish_guest_call(call, ret.r0, ret.r1, frame, &mut Session { bridge: &mut b, g })
}

/// The mirror trampoline's hook: a host message to a game object runs the
/// game's method by its own type encoding.
pub fn dispatch_from_host(bridge: &Mutex<Bridge>, g: &dyn GuestSide, frame: &mut HostFrame) -> Result<(), String> {
    let mut b = lock(bridge);
    let host_self = frame.x[0] as usize;
    if let Some(block) = wrapper_guest(host_self) {
        // A host call of a guest block: the block's own signature says how.
        let flags = super::functions::load_flags(g, block + 4);
        let descriptor = u32_at(g, block + 16);
        if flags & BLOCK_HAS_SIGNATURE == 0 {
            return Err(format!("the host called guest block {block:#x}, which has no type signature"));
        }
        let at = descriptor + 8 + if flags & BLOCK_HAS_COPY_DISPOSE != 0 { 8 } else { 0 };
        let sig = Signature::parse(&guest_cstr(g, u32_at(g, at)))?;
        let invoke = u32_at(g, block + 12);
        let call = convert::prepare_guest_call(&sig, frame, &mut Session { bridge: &mut b, g })?;
        drop(b);
        construct_pending(bridge, g)?;
        let r = g.call(invoke, &call.words)?;
        let mut b = lock(bridge);
        return convert::finish_guest_call(call, r as u32, (r >> 32) as u32, frame, &mut Session { bridge: &mut b, g });
    }
    let name = unsafe { runtime::cstr(frame.x[1] as usize) };
    let guest_self = b.guest_object(g, host_self)?;
    let isa = u32_at(g, guest_self);
    // IOS_TRACE_MESSAGES also names the host's calls into game methods
    // (control actions, timers, delegate calls).
    if runtime::message_trace_on() {
        let (class_name, class_is_meta) = b.class_display(isa);
        if let Some(text) = runtime::traced_message_text(&class_name, class_is_meta, &name) {
            crate::log!("32-bit message from the host {text}");
        }
    }
    if let Some(class) = class_to_initialize(&b, isa) {
        drop(b);
        initialize(bridge, g, class)?;
        b = lock(bridge);
    }
    // The host runtime's own +initialize: run once through the state above.
    if name == "initialize" {
        return Ok(());
    }
    // A game method, or a game category method on a framework class (the
    // receiver's own framework class or one its game class inherits from).
    let found = match b.classes.lookup(isa, &name) {
        Lookup::Guest(m) => Some((m.imp, m.types.clone())),
        Lookup::Framework { class, meta } => b.framework_category(&class, meta, &name),
        Lookup::Missing => {
            b.framework_by_guest.get(&isa).cloned().and_then(|(class, meta)| b.framework_category(&class, meta, &name))
        }
    };
    let Some((imp, types)) = found else {
        return Err(format!("host sent {name} to guest object {guest_self:#x}, which has no such method"));
    };
    let sig = Signature::parse_method(&types)?;
    let call = convert::prepare_guest_call(&sig, frame, &mut Session { bridge: &mut b, g })?;
    drop(b);
    // Constructed before its first method runs (-init sent by the host's +new).
    construct_pending(bridge, g)?;
    let r = g.call(imp, &call.words)?;
    let mut b = lock(bridge);
    convert::finish_guest_call(call, r as u32, (r >> 32) as u32, frame, &mut Session { bridge: &mut b, g })
}
