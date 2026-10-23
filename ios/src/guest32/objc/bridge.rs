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

use super::classes::{GuestClasses, Lookup};
use super::convert::{self, Boundary, GuestRet, HostCallOptions, HostFrame};
use super::encoding::Signature;
use super::mirror;
use super::selectors::FrameworkSelectors;
use crate::objc::runtime;
use guest32_image::ObjcMetadata32;
use std::collections::HashMap;
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
    /// Runs guest code at `imp` (bit 0 = Thumb) with argument words; r0 | r1 << 32.
    fn call(&self, imp: u32, words: &[u32]) -> Result<u64, String>;
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

/// The host form of a guest block: a global (never copied, never freed)
/// 64-bit block whose invoke is the mirror trampoline.
#[repr(C)]
struct HostBlock {
    isa: usize,
    flags: i32,
    reserved: i32,
    invoke: usize,
    descriptor: usize,
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
    sel_names: HashMap<u32, String>,
    sel_guest: HashMap<String, u32>,
    specials: HashMap<u32, Special>,
    /// Host block wrappers -> the guest (heap) blocks they run.
    host_blocks: HashMap<usize, u32>,
    /// Host object -> guest copy of its -bytes (address, capacity).
    buffers: HashMap<usize, (u32, u32)>,
    /// Guest address of the objc_msgSend stand-in (imp of framework Methods).
    pub msg_send_stub: u32,
}

/// Where a message goes.
enum Target {
    Guest { imp: u32, types: String },
    Host { class: String, meta: bool, via_super: bool },
}

impl Bridge {
    pub fn new(md: &ObjcMetadata32) -> Result<Bridge, String> {
        Ok(Bridge {
            classes: GuestClasses::from_metadata(md)?,
            to_host: HashMap::new(),
            to_guest: HashMap::new(),
            game_instances: HashMap::new(),
            framework: HashMap::new(),
            framework_by_guest: HashMap::new(),
            mirrors: HashMap::new(),
            sel_names: HashMap::new(),
            sel_guest: HashMap::new(),
            specials: HashMap::new(),
            host_blocks: HashMap::new(),
            buffers: HashMap::new(),
            msg_send_stub: 0,
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

    /// Text of a guest string object.
    pub fn object_text(&mut self, g: &dyn GuestSide, guest: u32) -> Option<String> {
        let host = self.host_object_for(g, guest).ok()?;
        (host != 0).then(|| crate::fw::string::get(host))
    }

    /// A host block that runs guest block `block` (which must already be a
    /// heap or global block; see functions::block_copy). The wrapper holds a
    /// reference to a heap block, so the guest's own release can't free it
    /// while the host may still call it. The 64-bit layer never frees blocks
    /// (its _Block_release does nothing), so that reference is kept for good.
    fn host_block(&mut self, g: &dyn GuestSide, block: u32) -> usize {
        let flags = u32_at(g, block + 4);
        if flags & BLOCK_NEEDS_FREE != 0 {
            let count = ((flags & BLOCK_REFCOUNT_MASK) + 2).min(BLOCK_REFCOUNT_MASK);
            g.write(block + 4, &((flags & !BLOCK_REFCOUNT_MASK) | count).to_le_bytes());
        }
        static DESCRIPTOR: [usize; 2] = [0, std::mem::size_of::<HostBlock>()];
        let isa = crate::symbols::lookup("__NSConcreteGlobalBlock").map(|s| s.0).unwrap_or(0);
        let host = crate::symbols::leak(HostBlock {
            isa,
            flags: BLOCK_IS_GLOBAL as i32,
            reserved: 0,
            invoke: mirror::trampoline_address(),
            descriptor: DESCRIPTOR.as_ptr() as usize,
        });
        self.host_blocks.insert(host, block);
        self.pair(block, host);
        host
    }

    pub fn selector_name(&mut self, g: &dyn GuestSide, sel: u32) -> String {
        if let Some(name) = self.sel_names.get(&sel) {
            return name.clone();
        }
        let name = guest_cstr(g, sel);
        self.sel_names.insert(sel, name.clone());
        self.sel_guest.entry(name.clone()).or_insert(sel);
        name
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

    /// Forgets a game object whose host form was deallocated; frees its guest memory.
    pub fn forget(&mut self, g: &dyn GuestSide, guest: u32) {
        if let Some(host) = self.to_host.remove(&guest) {
            self.to_guest.remove(&host);
        }
        if self.game_instances.remove(&guest).is_some() {
            g.free(guest);
        }
    }

    fn host_class_named(name: &str, meta: bool) -> usize {
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
        if let Some(&block) = self.host_blocks.get(&host) {
            return Ok(block);
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
                let size = self.classes.class(guest_class).map(|c| c.instance_size).unwrap_or(4).max(4);
                let object = g.alloc(size);
                g.write(object, &vec![0u8; size as usize]);
                g.write(object, &guest_class.to_le_bytes());
                self.game_instances.insert(object, ());
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
                Lookup::Missing => Err(format!("{}{sel}: no method in the game's class chain", self.describe(isa))),
            };
        }
        match self.framework_by_guest.get(&isa) {
            Some((class, meta)) => self.framework_target(class, *meta, sel, false),
            None => Err(format!("message {sel} to an object with unknown isa {isa:#x}")),
        }
    }

    /// A framework class answers, unless the game added a category method to it.
    fn framework_target(&self, class: &str, meta: bool, sel: &str, via_super: bool) -> Result<Target, String> {
        if let Some(m) = self.classes.framework_category_method(class, meta, sel) {
            return Ok(Target::Guest { imp: m.imp, types: m.types.clone() });
        }
        Ok(Target::Host { class: class.to_string(), meta, via_super })
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
    let rt = runtime::lock();
    rt.classes.contains_key(&rt.class_of(value))
}

fn lock(bridge: &Mutex<Bridge>) -> std::sync::MutexGuard<'_, Bridge> {
    bridge.lock().unwrap_or_else(|e| e.into_inner())
}

/// Defines the host mirror of every game class (parents first) and makes
/// every selector reference point at one string per selector name.
pub fn install(bridge: &Mutex<Bridge>, g: &dyn GuestSide, md: &ObjcMetadata32) -> Result<(), String> {
    let mut b = lock(bridge);
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
        let class: Vec<String> = b.classes.selectors_of(meta).into_iter().map(str::to_string).collect();
        let instance: Vec<&str> = instance.iter().map(String::as_str).collect();
        let class: Vec<&str> = class.iter().map(String::as_str).collect();
        let mirror = mirror::define_mirror_class(&name, host_super, address, &instance, &class)?;
        b.mirrors.insert(address, mirror);
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

/// Type encoding for a message the host answers: the framework table, else
/// the game's own method of the same name (an override has the same types).
fn host_signature(b: &Bridge, class: &str, meta: bool, sel: &str) -> Result<(Signature, HostCallOptions), String> {
    if let Some(entry) = FrameworkSelectors::builtin().get(sel) {
        let options = HostCallOptions { real_float: entry.real_float, varargs: entry.varargs, arrays: entry.arrays };
        return Ok((entry.signature.clone(), options));
    }
    if let Some(types) = b.classes.types_for_selector(sel) {
        return Ok((Signature::parse_method(types)?, HostCallOptions::default()));
    }
    Err(format!(
        "{}[{class} {sel}]: no type encoding for this framework selector (add it to framework_selectors.tsv)",
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
    let stret = matches!(kind, SendKind::Stret | SendKind::Super2Stret);
    let first = u32::from(stret);
    let mut b = lock(bridge);
    let sel = words(first + 1);
    let name = b.selector_name(g, sel);
    let (receiver, target) = match kind {
        SendKind::Normal | SendKind::Stret => {
            let receiver = words(first);
            if receiver == 0 {
                return Ok(GuestRet { r0: if stret { words(0) } else { 0 }, r1: 0 });
            }
            let isa = u32_at(g, receiver);
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
            (receiver, b.resolve(isa, &name)?)
        }
        SendKind::Super2 | SendKind::Super2Stret => {
            let pair = words(first);
            let (receiver, current) = (u32_at(g, pair), u32_at(g, pair + 4));
            let c = b.classes.class(current).ok_or_else(|| format!("super call {name} from unknown class {current:#x}"))?;
            let meta = c.meta;
            let target = match c.superclass.clone() {
                super::classes::Super::Game(parent) => b.resolve_super(parent, &name)?,
                super::classes::Super::Framework(class) => b.framework_target(&class, meta, &name, true)?,
                super::classes::Super::None => return Err(format!("super call {name} from a root class")),
            };
            (receiver, target)
        }
    };
    let receiver_word = first;
    let get = |i: u32| if i == receiver_word { receiver } else { words(i) };
    match target {
        Target::Guest { imp, types } => {
            drop(b);
            let sig = Signature::parse_method(&types)?;
            let call_words: Vec<u32> = (0..sig.guest_total_words()).map(get).collect();
            let r = g.call(imp, &call_words)?;
            Ok(GuestRet { r0: r as u32, r1: (r >> 32) as u32 })
        }
        Target::Host { class, meta, via_super } => {
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
            let host_sel = runtime::sel(&name);
            drop(b);
            let imp = if via_super {
                let host_class = Bridge::host_class_named(&class, meta);
                runtime::lock().lookup(host_class, host_sel)
            } else {
                runtime::imp_for(host_receiver, host_sel)
            };
            let imp = imp.ok_or_else(|| format!("{}[{class} {name}] is not implemented by the host layer", if meta { "+" } else { "-" }))?;
            let mut ret = mirror::call_host(imp, &call.args);
            let mut b = lock(bridge);
            // -bytes / -mutableBytes point into host memory: hand the guest a copy
            // (writes through a mutableBytes copy do not reach the host object).
            let pointer = ret.x[0] as usize;
            if matches!(name.as_str(), "bytes" | "mutableBytes") && pointer != 0 && g.guest_address(pointer).is_none() {
                let length = runtime::send(host_receiver, "length", &[]);
                let copy = b.guest_buffer(g, host_receiver, length as u32);
                g.write(copy, unsafe { std::slice::from_raw_parts(pointer as *const u8, length) });
                ret.x[0] = g.host_address(copy) as u64;
            }
            let result = convert::finish_host_call(call, &ret, &mut Session { bridge: &mut b, g })?;
            if via_super && name == "dealloc" {
                b.forget(g, receiver);
            }
            drop(b);
            // The heap copies made above: a host wrapper holds its own reference.
            let ctx = super::functions::Ctx { bridge, g };
            for (_, copy) in replaced {
                super::functions::block_release(&ctx, copy)?;
            }
            Ok(result)
        }
    }
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
    let call = convert::prepare_host_call(sig, options, get, &mut Session { bridge: &mut lock(bridge), g })?;
    // Unlocked: the host may message game objects (NSLog's %@ asks for -description).
    let ret = mirror::call_host(address, &call.args);
    convert::finish_host_call(call, &ret, &mut Session { bridge: &mut lock(bridge), g })
}

impl Bridge {
    /// A super call's target, searched from game class `parent` up: a
    /// framework class at the end answers with its own host method (the
    /// receiver's host class would find the game's override again).
    fn resolve_super(&self, parent: u32, sel: &str) -> Result<Target, String> {
        match self.classes.lookup(parent, sel) {
            Lookup::Guest(m) => Ok(Target::Guest { imp: m.imp, types: m.types.clone() }),
            Lookup::Framework { class, meta } => self.framework_target(&class, meta, sel, true),
            Lookup::Missing => Err(format!("{}{sel}]: super call finds no method", self.describe(parent))),
        }
    }
}

/// The mirror trampoline's hook: a host message to a game object runs the
/// game's method by its own type encoding.
pub fn dispatch_from_host(bridge: &Mutex<Bridge>, g: &dyn GuestSide, frame: &mut HostFrame) -> Result<(), String> {
    let mut b = lock(bridge);
    let host_self = frame.x[0] as usize;
    if let Some(&block) = b.host_blocks.get(&host_self) {
        // A host call of a guest block: the block's own signature says how.
        let flags = u32_at(g, block + 4);
        let descriptor = u32_at(g, block + 16);
        if flags & BLOCK_HAS_SIGNATURE == 0 {
            return Err(format!("the host called guest block {block:#x}, which has no type signature"));
        }
        let at = descriptor + 8 + if flags & BLOCK_HAS_COPY_DISPOSE != 0 { 8 } else { 0 };
        let sig = Signature::parse(&guest_cstr(g, u32_at(g, at)))?;
        let invoke = u32_at(g, block + 12);
        let call = convert::prepare_guest_call(&sig, frame, &mut Session { bridge: &mut b, g })?;
        drop(b);
        let r = g.call(invoke, &call.words)?;
        let mut b = lock(bridge);
        return convert::finish_guest_call(call, r as u32, (r >> 32) as u32, frame, &mut Session { bridge: &mut b, g });
    }
    let name = unsafe { runtime::cstr(frame.x[1] as usize) };
    let guest_self = b.guest_object(g, host_self)?;
    let isa = u32_at(g, guest_self);
    let (imp, types) = match b.classes.lookup(isa, &name) {
        Lookup::Guest(m) => (m.imp, m.types.clone()),
        _ => return Err(format!("host sent {name} to game object {guest_self:#x}, which has no such method")),
    };
    let sig = Signature::parse_method(&types)?;
    let call = convert::prepare_guest_call(&sig, frame, &mut Session { bridge: &mut b, g })?;
    drop(b);
    let r = g.call(imp, &call.words)?;
    let mut b = lock(bridge);
    convert::finish_guest_call(call, r as u32, (r >> 32) as u32, frame, &mut Session { bridge: &mut b, g })
}
