//! Grand Central Dispatch for 32-bit games: the 64-bit layer's queues
//! (libsystem/mach.rs: serial queues on a worker thread, global queues,
//! the main queue on the run loop) run the game's blocks. A guest block
//! crosses as the bridge's host block, whose invoke runs it in the guest
//! (taking the guest lock on whatever thread the queue runs it); queues
//! cross as handles, and `_dispatch_main_q` is the main queue's handle.
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
