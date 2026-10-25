//! Message forwarding for 32-bit senders: Apple's steps (objc/forward.rs)
//! as guest messages, and the armv7 NSInvocations they make.
//!
//! A message no class answers goes through +resolveInstanceMethod:,
//! -forwardingTargetForSelector: (the same words to another receiver) and
//! -methodSignatureForSelector: + -forwardInvocation:. The invocation is the
//! host's NSInvocation with an armv7 signature: it holds the arguments as
//! the guest passed them (r0-r3 then the stack, 8-byte types in the next two
//! words, floats in core registers) and its result as the guest gets it
//! (r0/r1, or the bytes for the hidden struct pointer). -invoke sends them
//! from the guest again ([`Hooks`]), from game or host code alike.

use super::bridge::{forward_host_message, send_from_guest, throw_in_guest, Bridge, GuestSide, SendKind};
use super::convert::{self, GuestRet, HostFrame};
use super::encoding::Signature;
use super::functions::Ctx;
use crate::fw::invocation::{self, Layout};
use crate::objc::forward::{steps, Forwarding, Outcome};
use crate::objc::runtime;
use std::cell::RefCell;
use std::sync::{Arc, Mutex, MutexGuard};

fn lock(bridge: &Mutex<Bridge>) -> MutexGuard<'_, Bridge> {
    bridge.lock().unwrap_or_else(|e| e.into_inner())
}

fn u32_at(g: &dyn GuestSide, addr: u32) -> u32 {
    u32::from_le_bytes(g.read(addr, 4).try_into().unwrap())
}

thread_local! {
    /// (isa, selector) messages being sent again after their class said it
    /// added the method: a second miss is not resolved again.
    static RESOLVING: RefCell<Vec<(u32, u32)>> = const { RefCell::new(Vec::new()) };
}

/// A guest message that found no method.
struct GuestMessage<'a> {
    bridge: &'a Mutex<Bridge>,
    g: &'a dyn GuestSide,
    receiver: u32,
    isa: u32,
    is_class: bool,
    sel: u32,
    name: String,
}

impl Forwarding for GuestMessage<'_> {
    type Value = u32;

    fn receiver(&self) -> u32 {
        self.receiver
    }

    fn resolve(&self) -> Result<bool, String> {
        if RESOLVING.with(|r| r.borrow().contains(&(self.isa, self.sel))) {
            return Ok(false);
        }
        // Sent to the class; only the game's own resolver can add a method.
        let (class, resolver) = match self.is_class {
            true => (self.receiver, "resolveClassMethod:"),
            false => (self.isa, "resolveInstanceMethod:"),
        };
        if !lock(self.bridge).game_answers(u32_at(self.g, class), resolver) {
            return Ok(false);
        }
        let added = Ctx { bridge: self.bridge, g: self.g }.send(class, resolver, &[self.sel])? & 0xff != 0;
        Ok(added && lock(self.bridge).answers(self.isa, &self.name))
    }

    fn takes_part(&self) -> Result<bool, String> {
        let mut b = lock(self.bridge);
        if ["forwardingTargetForSelector:", "methodSignatureForSelector:", "forwardInvocation:"].iter().any(|s| b.game_answers(self.isa, s)) {
            return Ok(true);
        }
        let host = b.host_object_for(self.g, self.receiver)?;
        drop(b);
        Ok(crate::objc::forward::is_proxy_object(host))
    }

    fn responds(&self, selector: &str) -> Result<bool, String> {
        Ok(lock(self.bridge).answers(self.isa, selector))
    }

    fn ask(&self, selector: &str) -> Result<u32, String> {
        Ctx { bridge: self.bridge, g: self.g }.send(self.receiver, selector, &[self.sel])
    }
}

/// A guest message (objc_msgSend's `kind` and `words`) to `receiver` that
/// found no method: forwarded, or `missing` when the class does not take part.
pub(super) fn forward_message(
    bridge: &Mutex<Bridge>,
    g: &dyn GuestSide,
    kind: SendKind,
    words: &dyn Fn(u32) -> u32,
    receiver: u32,
    missing: &str,
) -> Result<GuestRet, String> {
    let stret = matches!(kind, SendKind::Stret | SendKind::Super2Stret);
    let first = u32::from(stret);
    let sel = words(first + 1);
    let isa = u32_at(g, receiver);
    let (name, is_class) = {
        let mut b = lock(bridge);
        (b.selector_name(g, sel), b.class_display(isa).1)
    };
    let message = GuestMessage { bridge, g, receiver, isa, is_class, sel, name };
    // The receiver's word: a super call's words hold its pair there.
    let as_sent = |to: u32| move |i: u32| if i == first { to } else { words(i) };
    let plain = if stret { SendKind::Stret } else { SendKind::Normal };
    match steps(&message)? {
        Outcome::Resolved => {
            RESOLVING.with(|r| r.borrow_mut().push((isa, sel)));
            let result = send_from_guest(bridge, g, kind, words);
            RESOLVING.with(|r| r.borrow_mut().retain(|m| *m != (isa, sel)));
            result
        }
        Outcome::Target(target) => send_from_guest(bridge, g, plain, &as_sent(target)),
        Outcome::Signature(signature) => {
            let host = lock(bridge).host_object_for(g, signature)?;
            forward_invocation(bridge, g, receiver, host, stret, &as_sent(receiver))
        }
        Outcome::Unrecognized => {
            Ctx { bridge, g }.send(receiver, "doesNotRecognizeSelector:", &[sel])?;
            Ok(GuestRet { r0: if stret { words(0) } else { 0 }, r1: 0 })
        }
        Outcome::Missing => Err(missing.to_string()),
    }
}

/// Step 3: the message in `words` (its guest words, a struct result's
/// pointer first when `stret`) as an NSInvocation by host NSMethodSignature
/// `signature`, sent to `receiver`'s -forwardInvocation:; the invocation's
/// result as the guest gets it.
pub(super) fn forward_invocation(
    bridge: &Mutex<Bridge>,
    g: &dyn GuestSide,
    receiver: u32,
    signature: usize,
    stret: bool,
    words: &dyn Fn(u32) -> u32,
) -> Result<GuestRet, String> {
    let types = invocation::signature_of(signature).ok_or("-methodSignatureForSelector: did not answer an NSMethodSignature")?;
    let sig = Signature::parse_method(&types.text)?;
    if sig.ret.guest_returns_in_memory() != stret {
        return Err(format!("forwarding by signature {:?}: the call and the signature disagree on a struct result", types.text));
    }
    // A signature made by our own code: the same types by armv7's sizes.
    let signature = match types.layout {
        Layout::Armv7 => signature,
        Layout::Arm64 => invocation::make_signature_in(&types.text, Layout::Armv7),
    };
    let arguments: Vec<Vec<u8>> = sig.args.iter().zip(sig.guest_arg_words()).map(|(ty, at)| convert::guest_bytes(words, at, ty.guest_size())).collect();
    let host_invocation = invocation::new_invocation(signature);
    invocation::set_invocation_arguments(host_invocation, &arguments);
    let guest_invocation = lock(bridge).guest_object(g, host_invocation)?;
    Ctx { bridge, g }.send(receiver, "forwardInvocation:", &[guest_invocation])?;
    let result = invocation::invocation_result(host_invocation).unwrap_or_default();
    if stret {
        let destination = words(0);
        g.write(destination, &result[..sig.ret.guest_size() as usize]);
        return Ok(GuestRet { r0: destination, r1: 0 });
    }
    Ok(convert::guest_ret_from_bytes(&sig.ret, &result))
}

/// -forwardInvocation: of the root classes from game code: NSObject's gives
/// up (-doesNotRecognizeSelector: of the invocation's selector), NSProxy's
/// raises (a subclass must override it).
pub(super) fn root_forward_invocation(bridge: &Mutex<Bridge>, g: &dyn GuestSide, receiver: u32, invocation: u32) -> Result<GuestRet, String> {
    let host_receiver = lock(bridge).host_object_for(g, receiver)?;
    if crate::objc::forward::is_proxy_object(host_receiver) {
        let exception = crate::fw::object::proxy_abstract_exception("forwardInvocation:");
        return throw_in_guest(&mut lock(bridge), g, exception);
    }
    let host_invocation = lock(bridge).host_object_for(g, invocation)?;
    let selector = runtime::send(host_invocation, "selector", &[]);
    let name = unsafe { runtime::cstr(selector) };
    let selector = lock(bridge).guest_selector(g, &name);
    Ctx { bridge, g }.send(receiver, "doesNotRecognizeSelector:", &[selector])?;
    Ok(GuestRet { r0: 0, r1: 0 })
}

/// The 32-bit game's side of NSInvocation (see invocation::GuestInvocations).
struct Hooks {
    bridge: &'static Mutex<Bridge>,
    g: &'static dyn GuestSide,
}

impl invocation::GuestInvocations for Hooks {
    fn invoke(&self, types: &str, arguments: &[Vec<u8>]) -> Result<Vec<u8>, String> {
        let (bridge, g) = (self.bridge, self.g);
        let sig = Signature::parse_method(types)?;
        let size = sig.ret.guest_size();
        let stret = sig.ret.guest_returns_in_memory();
        let mut words = vec![0u32; sig.guest_total_words() as usize];
        let buffer = stret.then(|| g.alloc(size));
        if let Some(buffer) = buffer {
            g.write(buffer, &vec![0; size as usize]);
            words[0] = buffer;
        }
        for ((ty, at), bytes) in sig.args.iter().zip(sig.guest_arg_words()).zip(arguments) {
            let mut bytes = bytes.clone();
            bytes.resize((ty.guest_size().div_ceil(4).max(1) * 4) as usize, 0);
            for (k, chunk) in bytes.chunks(4).enumerate() {
                words[at as usize + k] = u32::from_le_bytes(chunk.try_into().unwrap());
            }
        }
        let get = |i: u32| words.get(i as usize).copied().unwrap_or(0);
        let kind = if stret { SendKind::Stret } else { SendKind::Normal };
        let keep_going = crate::trap::KEEP_GOING.load(std::sync::atomic::Ordering::Relaxed);
        let ret = super::functions::skip_missing(send_from_guest(bridge, g, kind, &get), keep_going);
        let result = match (buffer, &ret) {
            (Some(buffer), Ok(_)) => g.read(buffer, size),
            _ => ret.as_ref().map(|r| [r.r0.to_le_bytes(), r.r1.to_le_bytes()].concat()).unwrap_or_default(),
        };
        if let Some(buffer) = buffer {
            g.free(buffer);
        }
        ret.map(|_| result)
    }

    fn object_to_host(&self, guest: u32) -> usize {
        lock(self.bridge).host_object_for(self.g, guest).unwrap_or(0)
    }

    fn object_to_guest(&self, host: usize) -> u32 {
        lock(self.bridge).guest_object(self.g, host).unwrap_or(0)
    }

    fn selector_to_host(&self, guest: u32) -> usize {
        if guest == 0 {
            return 0;
        }
        let name = lock(self.bridge).selector_name(self.g, guest);
        runtime::sel(&name)
    }

    fn selector_to_guest(&self, host: usize) -> u32 {
        if host == 0 {
            return 0;
        }
        let name = unsafe { runtime::cstr(host) };
        lock(self.bridge).guest_selector(self.g, &name)
    }

    fn keep(&self, kind: &str, word: u32) -> u32 {
        let ctx = Ctx { bridge: self.bridge, g: self.g };
        let kept = match kind {
            "@?" => super::functions::block_copy(&ctx, word),
            "@" => ctx.send(word, "retain", &[]).map(|_| word),
            _ => {
                // A C string: the invocation's own copy in guest memory.
                let text = unsafe { std::ffi::CStr::from_ptr(self.g.host_address(word) as *const libc::c_char) }.to_bytes_with_nul().to_vec();
                let copy = self.g.alloc(text.len() as u32);
                self.g.write(copy, &text);
                Ok(copy)
            }
        };
        kept.unwrap_or_else(|error| {
            crate::log!("32-bit Objective-C: -retainArguments: {error}");
            word
        })
    }

    fn forward_from_host(&self, frame: &mut crate::objc::forward::Frame, signature: usize) -> Result<(), String> {
        // The same registers, by the bridge's own name for them (layouts checked in the tests).
        let frame = unsafe { &mut *(frame as *mut crate::objc::forward::Frame as *mut HostFrame) };
        forward_host_message(self.bridge, self.g, frame, signature)
    }
}

/// Lets NSInvocations with armv7 signatures reach this game's bridge.
pub fn set_invocation_hooks(bridge: &'static Mutex<Bridge>, g: &'static dyn GuestSide) {
    invocation::set_guest_invocations(Arc::new(Hooks { bridge, g }));
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_forwarding_frame_is_the_bridges_host_frame() {
        use crate::objc::forward::Frame;
        use std::mem::{offset_of, size_of};
        assert_eq!(size_of::<Frame>(), size_of::<super::HostFrame>());
        assert_eq!(offset_of!(Frame, d), offset_of!(super::HostFrame, d));
        assert_eq!(offset_of!(Frame, stack), offset_of!(super::HostFrame, stack));
        assert_eq!(offset_of!(Frame, ret_x), offset_of!(super::HostFrame, ret));
        assert_eq!(offset_of!(Frame, ret_d), offset_of!(super::HostFrame, ret) + 16);
    }
}
