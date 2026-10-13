//! Signals that game threads send each other. Mono's garbage collector pauses
//! every thread with pthread_kill (SIGPWR on Android), waits for each to
//! answer from its handler, and resumes them with another signal (SIGXCPU)
//! that the paused handler waits for in sigsuspend.
//!
//! Host signals never reach guest code; only signals sent by guest code are
//! delivered. Only one thread runs guest code at a time and switching happens
//! inside phone functions, so a signalled thread is always parked in one. It
//! runs the handler when it next holds the guest lock (after a wait, or before
//! a phone function returns), like the kernel delivering on the way out of a
//! system call. Waits re-check their conditions afterwards (SA_RESTART).
//!
//! Guest layouts (bionic ARM32): sigset_t is one word (bit n-1 = signal n);
//! struct sigaction is { handler, mask, flags, restorer }.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::handler;
use crate::imports::threads::current_handle;
use crate::runtime::{engine, gil_held, mem, shared, wait_on, ArgSource, GuestCpu, Handler, Ret, Uc};

const SIG_DFL: u32 = 0;
const SIG_IGN: u32 = 1;
const SIG_BLOCK: u32 = 0;
const SIG_UNBLOCK: u32 = 1;
const SIG_SETMASK: u32 = 2;
const SA_SIGINFO: u32 = 0x4;
const SA_RESTART: u32 = 0x1000_0000;
const SA_NODEFER: u32 = 0x4000_0000;
const SA_RESETHAND: u32 = 0x8000_0000;
const SIGKILL: u32 = 9;
const SIGSTOP: u32 = 19;
const SI_TKILL: i32 = -6;
const EINVAL: u32 = 22;
const EINTR: u32 = 4;
const ESRCH: u32 = 3;
const LAST_SIGNAL: u32 = 64;

/// siginfo_t is 128 bytes; ucontext_t (with VFP register space) 744.
const SIGINFO_SIZE: u32 = 128;
const UCONTEXT_SIZE: u32 = 744;
/// Offsets in ucontext_t: uc_mcontext (struct sigcontext) and uc_sigmask.
const MCONTEXT: u32 = 20;
const MCONTEXT_R0: u32 = MCONTEXT + 12;
const MCONTEXT_PC: u32 = MCONTEXT_R0 + 15 * 4;
const MCONTEXT_CPSR: u32 = MCONTEXT_PC + 4;
const SIGMASK: u32 = MCONTEXT + 84;

#[derive(Clone, Copy, Default)]
struct Action {
    handler: u32,
    mask: u32,
    flags: u32,
}

#[derive(Default)]
struct ThreadSignals {
    pending: u64,
    blocked: u64,
}

#[derive(Default)]
pub struct Signals {
    actions: HashMap<u32, Action>,
    /// By pthread_t.
    threads: HashMap<u32, ThreadSignals>,
}

/// Signals sent and not yet delivered, over all threads (a quick check).
static PENDING: AtomicU32 = AtomicU32::new(0);

pub fn any_pending() -> bool {
    PENDING.load(Ordering::Relaxed) != 0
}

thread_local! {
    /// Handlers this thread has run (sigsuspend waits for this to change).
    static DELIVERED: Cell<u64> = const { Cell::new(0) };
}

fn bit(signal: u32) -> u64 {
    1u64 << (signal - 1)
}

/// SIGKILL and SIGSTOP can't be blocked.
fn blockable(mask: u64) -> u64 {
    mask & !(bit(SIGKILL) | bit(SIGSTOP))
}

fn blocked_now() -> u64 {
    let me = current_handle();
    shared().state.signals.threads.get(&me).map(|t| t.blocked).unwrap_or(0)
}

fn set_blocked(mask: u64) {
    let me = current_handle();
    shared().state.signals.threads.entry(me).or_default().blocked = blockable(mask);
}

/// A thread finished: drop what it had pending.
pub fn forget_thread(handle: u32) {
    if let Some(gone) = shared().state.signals.threads.remove(&handle) {
        PENDING.fetch_sub(gone.pending.count_ones(), Ordering::Relaxed);
    }
}

/// Sends `signal` to guest thread `target`; returns an errno value (0 = sent).
fn send(target: u32, signal: u32) -> u32 {
    if signal > LAST_SIGNAL {
        return EINVAL;
    }
    if !crate::imports::threads::is_live(target) {
        return ESRCH;
    }
    if signal == 0 {
        return 0;
    }
    {
        let mut shared = shared();
        let signals = &mut shared.state.signals;
        let action = signals.actions.get(&signal).copied().unwrap_or_default();
        match action.handler {
            SIG_IGN => return 0,
            SIG_DFL => {
                drop(shared);
                crate::log::line(&format!("[bridge] signal {signal} sent with no handler set; ignored"));
                return 0;
            }
            _ => {}
        }
        let thread = signals.threads.entry(target).or_default();
        if thread.pending & bit(signal) == 0 {
            thread.pending |= bit(signal);
            PENDING.fetch_add(1, Ordering::Relaxed);
        }
    }
    crate::runtime::wake_thread(target);
    if target == current_handle() {
        deliver_pending();
    }
    0
}

/// Runs handlers for this thread's pending, unblocked signals. Only while the
/// thread is inside guest code (parked in a phone function); otherwise they
/// wait until it is. Returns whether any handler ran.
pub fn deliver_pending() -> bool {
    if PENDING.load(Ordering::Relaxed) == 0 || !gil_held() {
        return false;
    }
    let uc = engine();
    if uc.get_data().depth == 0 {
        return false;
    }
    let me = current_handle();
    let mut delivered = false;
    loop {
        let next = {
            let mut shared = shared();
            let signals = &mut shared.state.signals;
            let Some(thread) = signals.threads.get_mut(&me) else { return delivered };
            let ready = thread.pending & !thread.blocked;
            if ready == 0 {
                return delivered;
            }
            let signal = ready.trailing_zeros() + 1;
            thread.pending &= !bit(signal);
            PENDING.fetch_sub(1, Ordering::Relaxed);
            let blocked = thread.blocked;
            let action = signals.actions.get(&signal).copied().unwrap_or_default();
            if action.flags & SA_RESETHAND != 0 {
                signals.actions.remove(&signal);
            }
            (signal, action, blocked)
        };
        run_handler(uc, next.0, next.1, next.2);
        delivered = true;
    }
}

/// Calls the guest handler with a siginfo_t and a ucontext_t describing where
/// the thread is parked (the phone function's return address).
fn run_handler(uc: &mut Uc, signal: u32, action: Action, blocked: u64) {
    if action.handler <= SIG_IGN {
        return;
    }
    let mut during = blocked | action.mask as u64;
    if action.flags & SA_NODEFER == 0 {
        during |= bit(signal);
    }
    set_blocked(during);

    let frame = shared().heap.malloc(SIGINFO_SIZE + UCONTEXT_SIZE);
    let memory = mem();
    unsafe { std::ptr::write_bytes(memory.host(frame), 0, (SIGINFO_SIZE + UCONTEXT_SIZE) as usize) };
    let info = frame;
    memory.write_u32(info, signal);
    memory.write_u32(info + 8, SI_TKILL as u32);
    memory.write_u32(info + 12, std::process::id());
    let context = frame + SIGINFO_SIZE;
    for register in 0..15 {
        memory.write_u32(context + MCONTEXT_R0 + register as u32 * 4, uc.reg(register));
    }
    // Parked in a phone function: the thread carries on at its return address.
    let resume = crate::runtime::resume_point().unwrap_or_else(|| uc.reg(14));
    let thumb = resume & 1;
    memory.write_u32(context + MCONTEXT_PC, resume & !1);
    memory.write_u32(context + MCONTEXT_CPSR, 0x10 | (thumb << 5));
    memory.write_u32(context + MCONTEXT + 8, blocked as u32);
    memory.write_u32(context + SIGMASK, blocked as u32);
    memory.write_u32(context + SIGMASK + 4, (blocked >> 32) as u32);

    let before: Vec<u32> = (0..16).map(|r| uc.reg(r)).collect();
    let arguments: &[u32] =
        if action.flags & SA_SIGINFO != 0 { &[signal, info, context] } else { &[signal] };
    if let Err(error) = crate::runtime::call_guest(uc, action.handler, arguments) {
        crate::log::line(&format!("[bridge] handler for signal {signal} stopped: {error}"));
    }
    let after: Vec<u32> = (0..16).map(|r| uc.reg(r)).collect();
    let changed: Vec<String> = (0..16)
        .filter(|&r| before[r] != after[r])
        .map(|r| format!("r{r} {:#x}->{:#x}", before[r], after[r]))
        .collect();
    crate::log::line(&format!(
        "[signal] {signal} ran on thread {} (depth {}); registers changed: {}",
        current_handle(),
        uc.get_data().depth,
        if changed.is_empty() { "none".to_string() } else { changed.join(", ") }
    ));
    if memory.read_u32(context + MCONTEXT_PC) != resume & !1 {
        crate::log::line(&format!("[bridge] handler for signal {signal} moved the resume point; not supported"));
    }
    // Returning from a handler restores the mask saved in the context.
    let restored = memory.read_u32(context + SIGMASK) as u64 | (memory.read_u32(context + SIGMASK + 4) as u64) << 32;
    set_blocked(restored);
    shared().heap.free(frame);
    DELIVERED.with(|d| d.set(d.get() + 1));
}

/// Changes this thread's blocked set (pthread_sigmask / sigprocmask).
fn change_mask(how: u32, set: u32, old: u32) -> u32 {
    let current = blocked_now();
    if old != 0 {
        mem().write_u32(old, current as u32);
    }
    if set != 0 {
        let given = mem().read_u32(set) as u64;
        let next = match how {
            SIG_BLOCK => current | given,
            SIG_UNBLOCK => current & !given,
            SIG_SETMASK => (current & !0xFFFF_FFFF) | given,
            _ => return EINVAL,
        };
        set_blocked(next);
        deliver_pending();
    }
    0
}

fn fail(uc: &Uc, error: u32) -> Ret {
    mem().write_u32(uc.get_data().errno_slot, error);
    Ret::U32(u32::MAX)
}

/// Stores a new action for `signal`, returning the old one.
fn set_action(signal: u32, action: Option<Action>) -> Action {
    let mut shared = shared();
    let actions = &mut shared.state.signals.actions;
    let old = actions.get(&signal).copied().unwrap_or_default();
    if let Some(action) = action {
        actions.insert(signal, action);
    }
    old
}

#[allow(non_snake_case)]
fn rt_signal_range() -> (u32, u32) {
    extern "C" {
        fn __libc_current_sigrtmin() -> libc::c_int;
        fn __libc_current_sigrtmax() -> libc::c_int;
    }
    unsafe { (__libc_current_sigrtmin() as u32, __libc_current_sigrtmax() as u32) }
}

pub fn handler(name: &str) -> Option<Handler> {
    match name {
        "sigaction" => handler!(|uc, a| {
            let (signal, action, old) = (a.u32(), a.u32(), a.u32());
            if signal == 0 || signal > LAST_SIGNAL || (action != 0 && (signal == SIGKILL || signal == SIGSTOP)) {
                return fail(uc, EINVAL);
            }
            let new = (action != 0).then(|| Action {
                handler: mem().read_u32(action),
                mask: mem().read_u32(action + 4),
                flags: mem().read_u32(action + 8),
            });
            let previous = set_action(signal, new);
            if old != 0 {
                mem().write_u32(old, previous.handler);
                mem().write_u32(old + 4, previous.mask);
                mem().write_u32(old + 8, previous.flags);
                mem().write_u32(old + 12, 0);
            }
            Ret::U32(0)
        }),
        "bsd_signal" | "signal" => handler!(|uc, a| {
            let (signal, function) = (a.u32(), a.u32());
            if signal == 0 || signal > LAST_SIGNAL || signal == SIGKILL || signal == SIGSTOP {
                return fail(uc, EINVAL);
            }
            let previous = set_action(signal, Some(Action { handler: function, mask: 0, flags: SA_RESTART }));
            Ret::U32(previous.handler)
        }),
        "pthread_sigmask" => handler!(|uc, a| {
            let (how, set, old) = (a.u32(), a.u32(), a.u32());
            Ret::U32(change_mask(how, set, old))
        }),
        "sigprocmask" => handler!(|uc, a| {
            let (how, set, old) = (a.u32(), a.u32(), a.u32());
            match change_mask(how, set, old) {
                0 => Ret::U32(0),
                error => fail(uc, error),
            }
        }),
        // Waits with `mask` blocked until a handler has run; always fails with EINTR.
        "sigsuspend" => handler!(|uc, a| {
            let mask = mem().read_u32(a.u32()) as u64;
            let saved = blocked_now();
            set_blocked((saved & !0xFFFF_FFFF) | mask);
            let before = DELIVERED.with(|d| d.get());
            deliver_pending();
            while DELIVERED.with(|d| d.get()) == before {
                wait_on(2 << 32 | current_handle() as u64, None);
            }
            set_blocked(saved);
            fail(uc, EINTR)
        }),
        "pthread_kill" => handler!(|uc, a| {
            let (thread, signal) = (a.u32(), a.u32());
            Ret::U32(send(thread, signal))
        }),
        "raise" => handler!(|uc, a| {
            let signal = a.u32();
            if matches!(signal, 4 | 6 | 7 | 8 | 11) {
                let sp = uc.reg(13);
                let registers: Vec<String> = (0..16).map(|r| format!("r{r}={:#x}", uc.reg(r))).collect();
                let stack: Vec<String> = (0..32).map(|i| format!("{:#x}", mem().read_u32(sp + i * 4))).collect();
                crate::log::line(&format!(
                    "[bridge] raise({signal}) from {}; stack: {}",
                    registers.join(" "),
                    stack.join(" ")
                ));
            }
            match send(current_handle(), signal) {
                0 => Ret::U32(0),
                error => fail(uc, error),
            }
        }),
        // Only this process can be signalled; SIGKILL to itself really ends it.
        "kill" => handler!(|uc, a| {
            let (pid, signal) = (a.i32(), a.u32());
            let own = pid == 0 || pid == std::process::id() as i32;
            if !own {
                crate::log::line(&format!("[bridge] kill({pid}, {signal}) of another process ignored"));
                return Ret::U32(0);
            }
            if signal == SIGKILL {
                unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
            }
            match send(current_handle(), signal) {
                0 => Ret::U32(0),
                error => fail(uc, error),
            }
        }),
        "__libc_current_sigrtmin" => handler!(|uc, a| Ret::U32(rt_signal_range().0)),
        "__libc_current_sigrtmax" => handler!(|uc, a| Ret::U32(rt_signal_range().1)),
        _ => None,
    }
}
