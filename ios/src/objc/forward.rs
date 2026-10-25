//! Message forwarding: what a message that finds no method does, in Apple's
//! order. [`steps`] is shared by both kinds of game; this file drives it for
//! 64-bit senders (objc_msgSend's lookup and `runtime::send`), and
//! guest32/objc/forward.rs for 32-bit ones.
//!
//! 1. +resolveInstanceMethod: / +resolveClassMethod: may add the method;
//! 2. -forwardingTargetForSelector: may name another receiver for the same
//!    message (same registers: objc_msgSend's saved receiver is replaced);
//! 3. -methodSignatureForSelector: gives the types: the message becomes an
//!    NSInvocation for -forwardInvocation:, whose result goes back;
//! 4. otherwise -doesNotRecognizeSelector:.
//!
//! Steps 2-4 are only taken for classes that take part (a forwarding method
//! of their own, or an NSProxy). A plain class missing a method keeps the
//! missing-method report: usually our layer lacks an Apple method there.

use crate::objc::runtime::{self, lock, Class, Id, Imp, Sel};
use std::cell::Cell;

/// What a sender gives the shared steps: messages to the receiver.
pub trait Forwarding {
    type Value: Copy + PartialEq + Default;
    fn receiver(&self) -> Self::Value;
    /// Step 1: whether the class added the method (it can be found now).
    fn resolve(&self) -> Result<bool, String>;
    /// Whether steps 2-4 apply (see the file's notes).
    fn takes_part(&self) -> Result<bool, String>;
    /// Whether the receiver answers `selector` itself (forwarding methods
    /// are only sent when it does, as Apple's runtime checks).
    fn responds(&self, selector: &str) -> Result<bool, String>;
    /// [receiver selector:_cmd] for the forwarding methods.
    fn ask(&self, selector: &str) -> Result<Self::Value, String>;
}

/// Where a message that found no method goes.
#[derive(Debug, PartialEq)]
pub enum Outcome<V> {
    /// The method exists now: send it again.
    Resolved,
    /// The same message to another receiver.
    Target(V),
    /// An NSInvocation by this NSMethodSignature for -forwardInvocation:.
    Signature(V),
    /// -doesNotRecognizeSelector:.
    Unrecognized,
    /// Not forwarded: the missing-method report.
    Missing,
}

/// Apple's forwarding steps, in order.
pub fn steps<F: Forwarding>(f: &F) -> Result<Outcome<F::Value>, String> {
    if f.resolve()? {
        return Ok(Outcome::Resolved);
    }
    if !f.takes_part()? {
        return Ok(Outcome::Missing);
    }
    let nil = F::Value::default();
    if f.responds("forwardingTargetForSelector:")? {
        let target = f.ask("forwardingTargetForSelector:")?;
        if target != nil && target != f.receiver() {
            return Ok(Outcome::Target(target));
        }
    }
    if f.responds("methodSignatureForSelector:")? {
        let signature = f.ask("methodSignatureForSelector:")?;
        if signature != nil {
            return Ok(Outcome::Signature(signature));
        }
    }
    Ok(Outcome::Unrecognized)
}

/// Whether a host class (or metaclass) takes part in forwarding: one of the
/// forwarding methods is not a root class's default, or its root is NSProxy.
pub fn host_class_forwards(class: Class) -> bool {
    let rt = lock();
    let is_default = |owner: Class| rt.classes.get(&owner).is_some_and(|r| !r.from_game && matches!(r.name.as_str(), "NSObject" | "NSProxy"));
    let overrides = ["forwardingTargetForSelector:", "methodSignatureForSelector:", "forwardInvocation:"].iter().any(|name| {
        let Some(sel) = rt.selector(name) else { return false };
        rt.owner_of(class, sel).is_some_and(|owner| !is_default(owner))
    });
    drop(rt);
    overrides || is_proxy_class(class)
}

/// Whether a host object (or class) is an NSProxy (or a proxy class).
pub fn is_proxy_object(object: Id) -> bool {
    let class = lock().class_of(object);
    is_proxy_class(class)
}

/// Whether a host class (or metaclass) has NSProxy at its root.
pub fn is_proxy_class(class: Class) -> bool {
    let rt = lock();
    let mut root = class;
    for _ in 0..256 {
        match rt.classes.get(&root).map(|r| r.superclass) {
            Some(parent) if parent != 0 => root = parent,
            _ => break,
        }
    }
    rt.classes.get(&root).is_some_and(|r| r.name == "NSProxy" && !r.from_game)
}

/// A 64-bit message: the receiver, its class and the class the method was
/// looked up from (a super call's start).
struct HostMessage {
    receiver: Id,
    isa: Class,
    start: Class,
    is_class: bool,
    sel: Sel,
}

impl Forwarding for HostMessage {
    type Value = Id;

    fn receiver(&self) -> Id {
        self.receiver
    }

    fn resolve(&self) -> Result<bool, String> {
        // Sent to the class: +resolveClassMethod: for a class receiver.
        let (class, name) = match self.is_class {
            true => (self.receiver, "resolveClassMethod:"),
            false => (self.isa, "resolveInstanceMethod:"),
        };
        if !runtime::responds(class, name) {
            return Ok(false);
        }
        let added = runtime::send(class, name, &[self.sel]) & 0xff != 0;
        Ok(added && lock().lookup(self.start, self.sel).is_some())
    }

    fn takes_part(&self) -> Result<bool, String> {
        Ok(host_class_forwards(self.isa))
    }

    fn responds(&self, selector: &str) -> Result<bool, String> {
        Ok(runtime::responds(self.receiver, selector))
    }

    fn ask(&self, selector: &str) -> Result<Id, String> {
        Ok(runtime::send(self.receiver, selector, &[self.sel]))
    }
}

thread_local! {
    /// The signature step 3 found, for the forwarding trampoline that runs
    /// next on this thread: (receiver, selector, NSMethodSignature).
    static PENDING: Cell<Option<(Id, Sel, Id)>> = const { Cell::new(None) };
}

/// A 64-bit message found no method from `start`: the method to run
/// instead, with `*receiver` possibly replaced (a forwarding target).
/// None: report it as missing.
pub(crate) fn after_miss(receiver: &mut Id, start: Class, sel: Sel, caller: usize) -> Option<Imp> {
    let (isa, is_class) = {
        let rt = lock();
        (rt.class_of(*receiver), rt.is_class_object(*receiver))
    };
    let message = HostMessage { receiver: *receiver, isa, start, is_class, sel };
    let outcome = match steps(&message) {
        Ok(outcome) => outcome,
        Err(error) => {
            crate::log!("forwarding {}: {error}", lock().sel_name(sel));
            Outcome::Missing
        }
    };
    match outcome {
        Outcome::Resolved => lock().lookup(start, sel),
        Outcome::Target(target) => {
            *receiver = target;
            Some(runtime::find_method(receiver, sel, caller))
        }
        Outcome::Signature(signature) => {
            PENDING.with(|p| p.set(Some((*receiver, sel, signature))));
            Some(ios_objc_forward as usize)
        }
        Outcome::Unrecognized => Some(unrecognized as usize),
        Outcome::Missing => None,
    }
}

/// Step 4 as a method: the message's own registers come in.
extern "C" fn unrecognized(this: Id, sel: Sel) -> usize {
    runtime::send(this, "doesNotRecognizeSelector:", &[sel]);
    0
}

/// The registers of a forwarded call, filled by the trampoline: x0-x8,
/// d0-d7, the caller's stack arguments, and the result registers. The same
/// layout as the 32-bit bridge's HostFrame.
#[repr(C)]
#[derive(Debug, Default)]
pub struct Frame {
    pub x: [u64; 9],
    pub d: [u64; 8],
    pub stack: usize,
    pub ret_x: [u64; 2],
    pub ret_d: [u64; 4],
}

std::arch::global_asm!(
    // Frame: x[9] @0, d[8] @72, stack @136, ret_x @144, ret_d @160.
    ".globl ios_objc_forward",
    ".p2align 2",
    "ios_objc_forward:",
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
    "bl ios_objc_forward_invocation",
    "add x9, sp, #16",
    "ldr x8, [x9, #64]",
    "ldp x0, x1, [x9, #144]",
    "ldp d0, d1, [x9, #160]",
    "ldp d2, d3, [x9, #176]",
    "ldp x29, x30, [sp], #208",
    "ret",
);

extern "C" {
    fn ios_objc_forward();
}

/// Step 3 with the message's registers: the NSInvocation, -forwardInvocation:,
/// and the invocation's result in the result registers.
#[no_mangle]
extern "C" fn ios_objc_forward_invocation(frame: *mut Frame) {
    let frame = unsafe { &mut *frame };
    let (receiver, sel) = (frame.x[0] as Id, frame.x[1] as Sel);
    let pending = PENDING.with(|p| p.take()).filter(|p| p.0 == receiver && p.1 == sel).map(|p| p.2);
    let signature = pending.unwrap_or_else(|| runtime::send(receiver, "methodSignatureForSelector:", &[sel]));
    let Some(types) = crate::fw::invocation::signature_of(signature) else {
        unrecognized(receiver, sel);
        return;
    };
    if types.layout == crate::fw::invocation::Layout::Armv7 {
        // A 32-bit game object forwards it: its bridge converts the registers.
        let result = crate::fw::invocation::guest_invocations().map(|g| g.forward_from_host(frame, signature));
        if let Some(Err(error)) = result {
            crate::log!("forwarding {} to a 32-bit game object: {error}", lock().sel_name(sel));
        }
        return;
    }
    let arguments: Vec<_> = types.parts[1..].iter().map(|(_, t)| t.clone()).collect();
    let x: [u64; 8] = frame.x[..8].try_into().unwrap();
    let values = crate::fw::invocation::capture(&arguments, &x, &frame.d, frame.stack);
    let invocation = crate::fw::invocation::new_invocation(signature);
    crate::fw::invocation::set_invocation_arguments(invocation, &values);
    runtime::send(receiver, "forwardInvocation:", &[invocation]);
    let result = crate::fw::invocation::invocation_result(invocation).unwrap_or_default();
    crate::fw::invocation::place_result(&types.parts[0].1, &result, frame.x[8] as usize, &mut frame.ret_x, &mut frame.ret_d);
}

#[cfg(test)]
mod tests {
    use crate::objc::runtime::{self, Class, Id, Sel};
    use crate::symbols::leak_cstr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    static DICTIONARY: AtomicUsize = AtomicUsize::new(0);
    static REAL: AtomicUsize = AtomicUsize::new(0);
    static UNRECOGNIZED: Mutex<Vec<String>> = Mutex::new(Vec::new());

    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    struct Rect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }

    extern "C" fn target_for(_: Id, _: Sel, _: Sel) -> Id {
        DICTIONARY.load(Ordering::Relaxed)
    }

    extern "C" fn signature_for(_: Id, _: Sel, sel: Sel) -> Id {
        let types = match runtime::lock().sel_name(sel).as_str() {
            "label" => "@@:",
            "twice:" => "d@:d",
            "rectScaled:" => "{CGRect={CGPoint=dd}{CGSize=dd}}@:d",
            "respondsToSelector:" => "c@::",
            _ => return 0,
        };
        runtime::send(runtime::class_named("NSMethodSignature"), "signatureWithObjCTypes:", &[leak_cstr(types)])
    }

    extern "C" fn forward_invocation(_: Id, _: Sel, invocation: Id) {
        runtime::send(invocation, "invokeWithTarget:", &[REAL.load(Ordering::Relaxed)]);
    }

    extern "C" fn does_not_recognize(_: Id, _: Sel, sel: Sel) {
        let name = runtime::lock().sel_name(sel);
        UNRECOGNIZED.lock().unwrap().push(name);
    }

    extern "C" fn label(_: Id, _: Sel) -> Id {
        0x5150
    }

    extern "C" fn twice(_: Id, _: Sel, x: f64) -> f64 {
        x * 2.0
    }

    extern "C" fn rect_scaled(_: Id, _: Sel, scale: f64) -> Rect {
        Rect { x: scale, y: 2.0 * scale, w: 3.0 * scale, h: 4.0 * scale }
    }

    fn class(name: &str, parent: &str, methods: &[(&str, usize)]) -> Class {
        let class = crate::fw::object::define_class(name, parent, 16);
        crate::fw::object::methods(class, methods);
        class
    }

    /// Our objc_msgSend as game code calls it, typed for one message.
    fn message<F: Copy>() -> F {
        assert_eq!(std::mem::size_of::<F>(), std::mem::size_of::<usize>());
        unsafe { std::mem::transmute_copy(&(runtime::ios_objc_msgSend as usize)) }
    }

    #[test]
    fn host_proxies_forward_by_target_and_by_invocation() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let target = class("H64FwdTarget", "NSProxy", &[("forwardingTargetForSelector:", target_for as usize)]);
        let invoker = class(
            "H64FwdInvoker",
            "NSProxy",
            &[
                ("methodSignatureForSelector:", signature_for as usize),
                ("forwardInvocation:", forward_invocation as usize),
                ("doesNotRecognizeSelector:", does_not_recognize as usize),
            ],
        );
        let real = class("H64FwdReal", "NSObject", &[("label", label as usize), ("twice:", twice as usize), ("rectScaled:", rect_scaled as usize)]);
        REAL.store(runtime::alloc_instance(real), Ordering::Relaxed);
        let dictionary = runtime::send(runtime::class_named("NSMutableDictionary"), "dictionary", &[]);
        runtime::send(dictionary, "setObject:forKey:", &[crate::fw::string::make("value"), crate::fw::string::make("key")]);
        DICTIONARY.store(dictionary, Ordering::Relaxed);
        let (proxy, invoking) = (runtime::alloc_instance(target), runtime::alloc_instance(invoker));
        let sel = runtime::sel;

        let is_proxy: extern "C" fn(Id, Sel) -> bool = message();
        assert!(is_proxy(proxy, sel("isProxy")));
        let object_for_key: extern "C" fn(Id, Sel, Id) -> Id = message();
        let value = object_for_key(proxy, sel("objectForKey:"), crate::fw::string::make("key"));
        assert_eq!(crate::fw::string::get(value), "value");

        let get: extern "C" fn(Id, Sel) -> Id = message();
        assert_eq!(get(invoking, sel("label")), 0x5150);
        let double: extern "C" fn(Id, Sel, f64) -> f64 = message();
        assert_eq!(double(invoking, sel("twice:"), 1.25), 2.5);
        let rect: extern "C" fn(Id, Sel, f64) -> Rect = message();
        assert_eq!(rect(invoking, sel("rectScaled:"), 2.0), Rect { x: 2.0, y: 4.0, w: 6.0, h: 8.0 });
        let responds: extern "C" fn(Id, Sel, Sel) -> bool = message();
        assert!(responds(invoking, sel("respondsToSelector:"), sel("twice:")));

        // No signature: -doesNotRecognizeSelector: gets the selector.
        let unknown: extern "C" fn(Id, Sel, usize) -> usize = message();
        assert_eq!(unknown(invoking, sel("frobnicate:"), 7), 0);
        assert_eq!(*UNRECOGNIZED.lock().unwrap(), ["frobnicate:"]);
    }
}
