//! Grand Central Dispatch for 32-bit games: the 64-bit layer's queues
//! (libsystem/mach.rs: serial queues on a worker thread, global queues,
//! the main queue on the run loop) run the game's blocks. A guest block
//! crosses as the bridge's host block, whose invoke runs it in the guest
//! (taking the guest lock on whatever thread the queue runs it); queues
//! cross as handles, and `_dispatch_main_q` is the main queue's handle.
//! Groups, timer sources (`_dispatch_source_type_timer` is the host timer
//! type's handle), dispatch_after and suspend/resume are the host's too;
//! dispatch times are 64-bit values in word pairs.
//!
//! dispatch_once runs its block in place on Apple's 32-bit predicate (a
//! `long`: one word, where the 64-bit layer's is eight bytes). Waits for
//! another thread (dispatch_sync, a dispatch_once already running) let go
//! of the guest lock, which the other thread needs to run guest code.

use super::bridge::in_wrapper_scope;
use super::functions::{block_to_host, Ctx};
use crate::guest32::fw::{handle_to_guest, handle_to_host};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Condvar, Mutex};

type Words<'a> = &'a dyn Fn(u32) -> u32;

fn host_function(name: &str) -> Result<usize, String> {
    crate::symbols::lookup(&format!("_{name}")).map(|s| s.0).ok_or_else(|| format!("the host layer has no {name}"))
}

fn queue_to_host(guest: u32) -> Result<usize, String> {
    match handle_to_host(guest) {
        0 => Err(format!("{guest:#x} is not a dispatch queue")),
        queue => Ok(queue),
    }
}

/// `_dispatch_main_q`: the guest handle of the host's main queue (the data
/// symbol's address is the queue, as `dispatch_get_main_queue()` takes it).
pub fn main_queue() -> u32 {
    let host = crate::symbols::lookup("__dispatch_main_q").map(|s| s.0).unwrap_or(0);
    handle_to_guest(host)
}

pub fn global_queue(_: &Ctx, w: Words) -> Result<u64, String> {
    let get: extern "C" fn(i64, u64) -> usize = unsafe { std::mem::transmute(host_function("dispatch_get_global_queue")?) };
    Ok(u64::from(handle_to_guest(get(w(0) as i32 as i64, u64::from(w(1))))))
}

/// dispatch_queue_create(label, attr): no attribute is serial, any is concurrent.
pub fn queue_create(_: &Ctx, w: Words) -> Result<u64, String> {
    let create: extern "C" fn(usize, usize) -> usize = unsafe { std::mem::transmute(host_function("dispatch_queue_create")?) };
    Ok(u64::from(handle_to_guest(create(0, w(1) as usize))))
}

/// dispatch_async(queue, block): the host's queue copies the host block,
/// which keeps a reference to the guest block until it has run.
pub fn run_async(c: &Ctx, w: Words) -> Result<u64, String> {
    let queue = queue_to_host(w(0))?;
    let send: extern "C" fn(usize, usize) = unsafe { std::mem::transmute(host_function("dispatch_async")?) };
    in_wrapper_scope(c.g, || {
        send(queue, block_to_host(c, w(1))?);
        Ok(0)
    })
}

/// dispatch_sync(queue, block): runs on the queue's turn; the wait lets go
/// of the guest lock.
pub fn run_sync(c: &Ctx, w: Words) -> Result<u64, String> {
    let queue = queue_to_host(w(0))?;
    let send: extern "C" fn(usize, usize) = unsafe { std::mem::transmute(host_function("dispatch_sync")?) };
    in_wrapper_scope(c.g, || {
        let block = block_to_host(c, w(1))?;
        c.g.without_guest_lock(&mut || send(queue, block));
        Ok(0)
    })
}

/// A 64-bit argument from two words, low first (Apple's packed rule).
fn pair(w: Words, first: u32) -> u64 {
    u64::from(w(first)) | u64::from(w(first + 1)) << 32
}

/// A queue argument where 0 means the default (a global queue).
fn queue_or_default(guest: u32) -> Result<usize, String> {
    if guest == 0 {
        Ok(0)
    } else {
        queue_to_host(guest)
    }
}

/// The host object (queue, source, group) behind a guest handle.
fn object_to_host(guest: u32, what: &str) -> Result<usize, String> {
    match handle_to_host(guest) {
        0 => Err(format!("{guest:#x} is not a dispatch {what}")),
        object => Ok(object),
    }
}

/// `_dispatch_source_type_timer`: the guest handle of the host's timer type
/// (DISPATCH_SOURCE_TYPE_TIMER is that data symbol's address).
pub fn timer_type() -> u32 {
    let host = crate::symbols::lookup("__dispatch_source_type_timer").map(|s| s.0).unwrap_or(0);
    handle_to_guest(host)
}

/// dispatch_get_current_queue(): the queue running the current block.
pub fn current_queue(_: &Ctx, _: Words) -> Result<u64, String> {
    let get: extern "C" fn() -> usize = unsafe { std::mem::transmute(host_function("dispatch_get_current_queue")?) };
    Ok(u64::from(handle_to_guest(get())))
}

/// dispatch_time(when, delta): both 64-bit, the result too.
pub fn time(_: &Ctx, w: Words) -> Result<u64, String> {
    let make: extern "C" fn(u64, i64) -> u64 = unsafe { std::mem::transmute(host_function("dispatch_time")?) };
    Ok(make(pair(w, 0), pair(w, 2) as i64))
}

/// dispatch_after(when, queue, block)
pub fn after(c: &Ctx, w: Words) -> Result<u64, String> {
    let queue = queue_to_host(w(2))?;
    let send: extern "C" fn(u64, usize, usize) = unsafe { std::mem::transmute(host_function("dispatch_after")?) };
    in_wrapper_scope(c.g, || {
        send(pair(w, 0), queue, block_to_host(c, w(3))?);
        Ok(0)
    })
}

/// dispatch_suspend / dispatch_resume on a queue or source.
pub fn suspend(_: &Ctx, w: Words) -> Result<u64, String> {
    let call: extern "C" fn(usize) = unsafe { std::mem::transmute(host_function("dispatch_suspend")?) };
    call(object_to_host(w(0), "object")?);
    Ok(0)
}

pub fn resume(_: &Ctx, w: Words) -> Result<u64, String> {
    let call: extern "C" fn(usize) = unsafe { std::mem::transmute(host_function("dispatch_resume")?) };
    call(object_to_host(w(0), "object")?);
    Ok(0)
}

/// dispatch_source_create(type, handle, mask, queue): the host's timer
/// sources (other types answer NULL, as the host layer does).
pub fn source_create(_: &Ctx, w: Words) -> Result<u64, String> {
    let create: extern "C" fn(usize, usize, usize, usize) -> usize = unsafe { std::mem::transmute(host_function("dispatch_source_create")?) };
    let source = create(handle_to_host(w(0)), w(1) as usize, w(2) as usize, queue_or_default(w(3))?);
    Ok(u64::from(handle_to_guest(source)))
}

pub fn source_set_event_handler(c: &Ctx, w: Words) -> Result<u64, String> {
    let source = object_to_host(w(0), "source")?;
    let set: extern "C" fn(usize, usize) = unsafe { std::mem::transmute(host_function("dispatch_source_set_event_handler")?) };
    in_wrapper_scope(c.g, || {
        set(source, if w(1) == 0 { 0 } else { block_to_host(c, w(1))? });
        Ok(0)
    })
}

/// dispatch_source_set_timer(source, start, interval, leeway): three 64-bit values.
pub fn source_set_timer(_: &Ctx, w: Words) -> Result<u64, String> {
    let source = object_to_host(w(0), "source")?;
    let set: extern "C" fn(usize, u64, u64, u64) = unsafe { std::mem::transmute(host_function("dispatch_source_set_timer")?) };
    set(source, pair(w, 1), pair(w, 3), pair(w, 5));
    Ok(0)
}

pub fn group_create(_: &Ctx, _: Words) -> Result<u64, String> {
    let create: extern "C" fn() -> usize = unsafe { std::mem::transmute(host_function("dispatch_group_create")?) };
    Ok(u64::from(handle_to_guest(create())))
}

pub fn group_enter(_: &Ctx, w: Words) -> Result<u64, String> {
    let enter: extern "C" fn(usize) = unsafe { std::mem::transmute(host_function("dispatch_group_enter")?) };
    enter(object_to_host(w(0), "group")?);
    Ok(0)
}

pub fn group_leave(_: &Ctx, w: Words) -> Result<u64, String> {
    let leave: extern "C" fn(usize) = unsafe { std::mem::transmute(host_function("dispatch_group_leave")?) };
    leave(object_to_host(w(0), "group")?);
    Ok(0)
}

/// dispatch_group_wait(group, timeout): the wait lets go of the guest lock,
/// which the group's blocks need. A long: 0, or non-zero when timed out.
pub fn group_wait(c: &Ctx, w: Words) -> Result<u64, String> {
    let group = object_to_host(w(0), "group")?;
    let wait: extern "C" fn(usize, u64) -> isize = unsafe { std::mem::transmute(host_function("dispatch_group_wait")?) };
    let mut result = 0;
    c.g.without_guest_lock(&mut || result = wait(group, pair(w, 1)));
    Ok(u64::from(result as i32 as u32))
}

/// dispatch_group_notify(group, queue, block) and dispatch_group_async(group, queue, block).
pub fn group_notify(c: &Ctx, w: Words) -> Result<u64, String> {
    group_send(c, w, "dispatch_group_notify")
}

pub fn group_async(c: &Ctx, w: Words) -> Result<u64, String> {
    group_send(c, w, "dispatch_group_async")
}

fn group_send(c: &Ctx, w: Words, name: &str) -> Result<u64, String> {
    let (group, queue) = (object_to_host(w(0), "group")?, queue_to_host(w(1))?);
    let send: extern "C" fn(usize, usize, usize) = unsafe { std::mem::transmute(host_function(name)?) };
    in_wrapper_scope(c.g, || {
        send(group, queue, block_to_host(c, w(2))?);
        Ok(0)
    })
}

static ONCE_WAIT: (Mutex<()>, Condvar) = (Mutex::new(()), Condvar::new());

/// dispatch_once(predicate, block), as mach.rs's run_once: 0 = not yet,
/// 1 = running, ~0 = done.
pub fn once(c: &Ctx, w: Words) -> Result<u64, String> {
    let (predicate, block) = (w(0), w(1));
    let state = unsafe { &*(c.g.host_address(predicate) as *const AtomicI32) };
    if state.load(Ordering::Acquire) == !0 {
        return Ok(0);
    }
    if state.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire).is_ok() {
        let ran = c.g.call(c.word(block + 12), &[block]);
        let _guard = ONCE_WAIT.0.lock().unwrap_or_else(|e| e.into_inner());
        state.store(!0, Ordering::Release);
        ONCE_WAIT.1.notify_all();
        return ran.map(|_| 0);
    }
    c.g.without_guest_lock(&mut || {
        let mut guard = ONCE_WAIT.0.lock().unwrap_or_else(|e| e.into_inner());
        while state.load(Ordering::Acquire) != !0 {
            guard = ONCE_WAIT.1.wait(guard).unwrap_or_else(|e| e.into_inner());
        }
    });
    Ok(0)
}
