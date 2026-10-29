//! Host mirror classes for the game's classes, the trampoline their methods
//! point at, and a helper that calls any host function by registers.
//!
//! Each game class gets a host class of the same name (a subclass of the
//! host's framework superclass) so the 64-bit layer can hold and message the
//! game's objects. Every method of a mirror class is the one generic
//! trampoline: it saves x0-x8, d0-d7 and the caller's stack-argument address
//! into a [`HostFrame`], calls the dispatch hook (which converts and runs
//! the guest method; see [`set_dispatch`]), and returns whatever the hook
//! left in the frame's x0/x1/d0-d3. AAPCS64 returns every result kind in
//! those registers (or through x8, which the frame keeps), so one
//! trampoline serves void, integer, double and CGPoint/CGSize/CGRect
//! results alike.
//!
//! Only new classes are added through the host runtime's public calls; the
//! 64-bit games' classes and lookups are untouched.

use super::convert::{HostArgs, HostFrame, HostRet};
use crate::objc::runtime;
use std::sync::RwLock;

std::arch::global_asm!(
    // HostFrame: x[9] @0, d[8] @72, stack @136, ret.x[2] @144, ret.d[4] @160.
    ".globl ios_g32_trampoline",
    ".p2align 2",
    "ios_g32_trampoline:",
    "stp x29, x30, [sp, #-208]!",
    "mov x29, sp",
    "add x9, sp, #16",
    "stp x0, x1, [x9, #0]",
    "stp x2, x3, [x9, #16]",
    "stp x4, x5, [x9, #32]",
    "stp x6, x7, [x9, #48]",
    "str x8, [x9, #64]",
    "stp d0, d1, [x9, #72]",
    "stp d2, d3, [x9, #88]",
    "stp d4, d5, [x9, #104]",
    "stp d6, d7, [x9, #120]",
    "add x10, sp, #208",
    "str x10, [x9, #136]",
    "stp xzr, xzr, [x9, #144]",
    "stp xzr, xzr, [x9, #160]",
    "stp xzr, xzr, [x9, #176]",
    "mov x0, x9",
    "bl ios_g32_dispatch",
    "add x9, sp, #16",
    "ldr x8, [x9, #64]",
    "ldp x0, x1, [x9, #144]",
    "ldp d0, d1, [x9, #160]",
    "ldp d2, d3, [x9, #176]",
    "ldp x29, x30, [sp], #208",
    "ret",
    // HostCall: imp @0, x[9] @8, d[8] @80, stack @144, stack_words @152,
    // ret.x[2] @160, ret.d[4] @176.
    ".globl ios_g32_call_host",
    ".p2align 2",
    "ios_g32_call_host:",
    "stp x29, x30, [sp, #-32]!",
    "mov x29, sp",
    "str x19, [sp, #16]",
    "mov x19, x0",
    "ldr x10, [x19, #152]",
    "add x11, x10, #1",
    "and x11, x11, #0xfffffffffffffffe",
    "lsl x11, x11, #3",
    "sub sp, sp, x11",
    "ldr x12, [x19, #144]",
    "mov x13, #0",
    "1:",
    "cmp x13, x10",
    "b.ge 2f",
    "ldr x14, [x12, x13, lsl #3]",
    "str x14, [sp, x13, lsl #3]",
    "add x13, x13, #1",
    "b 1b",
    "2:",
    "ldp d0, d1, [x19, #80]",
    "ldp d2, d3, [x19, #96]",
    "ldp d4, d5, [x19, #112]",
    "ldp d6, d7, [x19, #128]",
    "ldp x0, x1, [x19, #8]",
    "ldp x2, x3, [x19, #24]",
    "ldp x4, x5, [x19, #40]",
    "ldp x6, x7, [x19, #56]",
    "ldr x8, [x19, #72]",
    "ldr x16, [x19, #0]",
    "blr x16",
    "stp x0, x1, [x19, #160]",
    "stp d0, d1, [x19, #176]",
    "stp d2, d3, [x19, #192]",
    "mov sp, x29",
    "ldr x19, [sp, #16]",
    "ldp x29, x30, [sp], #32",
    "ret",
);

extern "C" {
    fn ios_g32_trampoline();
    fn ios_g32_call_host(call: *mut HostCall);
}

#[repr(C)]
struct HostCall {
    imp: u64,
    x: [u64; 9],
    d: [u64; 8],
    stack: *const u64,
    stack_words: u64,
    ret: HostRet,
}

/// The address every mirror method points at.
pub fn trampoline_address() -> usize {
    ios_g32_trampoline as usize
}

type Dispatch = Box<dyn Fn(&mut HostFrame) + Send + Sync>;
/// Tests that install a dispatch hook hold this, so they do not swap each other's hook.
#[cfg(test)]
pub(crate) static DISPATCH_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static DISPATCH: RwLock<Option<Dispatch>> = RwLock::new(None);

/// Installs what the trampoline runs: it gets the saved registers and fills
/// in `frame.ret` (or the x8 buffer).
pub fn set_dispatch(hook: Dispatch) {
    *DISPATCH.write().unwrap_or_else(|e| e.into_inner()) = Some(hook);
}

#[no_mangle]
extern "C" fn ios_g32_dispatch(frame: *mut HostFrame) {
    let frame = unsafe { &mut *frame };
    let guard = DISPATCH.read().unwrap_or_else(|e| e.into_inner());
    match guard.as_ref() {
        Some(hook) => hook(frame),
        None => {
            let sel = unsafe { runtime::cstr(frame.x[1] as usize) };
            eprintln!("guest32: host sent {sel} to a game object before the bridge was set up; returning 0");
        }
    }
}

/// Calls a host function with the given registers and stack slots.
pub fn call_host(imp: usize, args: &HostArgs) -> HostRet {
    let mut call = HostCall {
        imp: imp as u64,
        x: args.x,
        d: args.d,
        stack: args.stack.as_ptr(),
        stack_words: args.stack.len() as u64,
        ret: HostRet::default(),
    };
    unsafe { ios_g32_call_host(&mut call) };
    call.ret
}

/// host class or metaclass -> (guest class, is metaclass)
static MIRRORS: RwLock<Option<rustc_hash::FxHashMap<usize, (u32, bool)>>> = RwLock::new(None);

/// Defines the host mirror of a game class: `name` under `host_superclass`
/// (0 for a root), with the trampoline for each selector the game class
/// (or its categories) implements. Refuses a name the host already has.
pub fn define_mirror_class(
    name: &str,
    host_superclass: usize,
    guest_class: u32,
    instance_selectors: &[&str],
    class_selectors: &[&str],
) -> Result<usize, String> {
    let (class, meta) = {
        let mut rt = runtime::lock();
        if rt.by_name.contains_key(name) {
            return Err(format!("game class {name} has the name of a host class"));
        }
        let size = rt.classes.get(&host_superclass).map(|r| r.instance_size).unwrap_or(8);
        let class = rt.define_class(name, host_superclass, size);
        let meta = rt.classes[&class].partner;
        for selector in instance_selectors {
            let sel = rt.sel(selector);
            rt.add_method(class, sel, trampoline_address());
        }
        for selector in class_selectors {
            let sel = rt.sel(selector);
            rt.add_method(meta, sel, trampoline_address());
        }
        (class, meta)
    };
    let mut mirrors = MIRRORS.write().unwrap_or_else(|e| e.into_inner());
    let map = mirrors.get_or_insert_with(Default::default);
    map.insert(class, (guest_class, false));
    map.insert(meta, (guest_class, true));
    // Its objects' retains and releases are counted (counted.rs, "would dealloc").
    crate::fw::counted::count_game_class(class);
    Ok(class)
}

/// The guest class a mirror class (or its metaclass) stands for.
pub fn mirror_guest_class(host_class: usize) -> Option<u32> {
    MIRRORS.read().unwrap_or_else(|e| e.into_inner()).as_ref()?.get(&host_class).map(|m| m.0)
}

/// Adds the trampoline for a selector the game added later (class_addMethod).
pub fn add_mirror_method(host_class: usize, selector: &str) {
    let mut rt = runtime::lock();
    let sel = rt.sel(selector);
    rt.add_method(host_class, sel, trampoline_address());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirror_methods_reach_the_dispatch_hook_with_every_register() {
        let class = define_mirror_class("G32MirrorTest", 0, 0x4321, &["touch:at:scale:"], &["make"]).unwrap();
        assert_eq!(mirror_guest_class(class), Some(0x4321));
        let meta = runtime::lock().classes[&class].partner;
        assert_eq!(mirror_guest_class(meta), Some(0x4321));
        let sel = runtime::sel("touch:at:scale:");
        let imp = runtime::lock().lookup(class, sel).unwrap();
        assert_eq!(imp, trampoline_address());
        let make = runtime::sel("make");
        assert_eq!(runtime::lock().lookup(meta, make), Some(trampoline_address()));
        // A second class of the same name is refused, so a host class is never replaced.
        assert!(define_mirror_class("G32MirrorTest", 0, 1, &[], &[]).is_err());

        let _hook = DISPATCH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_dispatch(Box::new(|frame: &mut HostFrame| {
            // Ten integer arguments: x0-x7, then two stack slots.
            let stack = unsafe { std::slice::from_raw_parts(frame.stack as *const u64, 2) };
            let sum: u64 = frame.x[2..8].iter().sum::<u64>() + stack.iter().sum::<u64>();
            frame.ret.x[0] = sum;
            frame.ret.d[0] = (f64::from_bits(frame.d[0]) * 2.0).to_bits();
            frame.ret.d[3] = frame.x[1];
        }));
        let mut args = HostArgs::default();
        args.x[..8].copy_from_slice(&[0x1000, sel as u64, 1, 2, 3, 4, 5, 6]);
        args.stack = vec![7, 8];
        args.d[0] = 1.25f64.to_bits();
        let ret = call_host(imp, &args);
        assert_eq!(ret.x[0], 36);
        assert_eq!(f64::from_bits(ret.d[0]), 2.5);
        assert_eq!(ret.d[3], sel as u64);
    }

    #[test]
    fn frame_layout_matches_the_assembly() {
        assert_eq!(std::mem::offset_of!(HostFrame, d), 72);
        assert_eq!(std::mem::offset_of!(HostFrame, stack), 136);
        assert_eq!(std::mem::offset_of!(HostFrame, ret), 144);
        assert_eq!(std::mem::size_of::<HostFrame>(), 192);
        assert_eq!(std::mem::offset_of!(HostCall, stack), 144);
        assert_eq!(std::mem::offset_of!(HostCall, ret), 160);
    }
}
