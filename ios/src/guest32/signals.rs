//! Signals for 32-bit games, in Darwin's numbers and layouts: sigaction,
//! signal, sigprocmask / pthread_sigmask, raise, sigaltstack, strsignal.
//!
//! Darwin numbers signals differently from Linux from 7 on (SIGBUS 10,
//! SIGUSR1 30, ...), and its struct sigaction { handler, mask, flags },
//! SA_* flags, SIG_BLOCK = 1 ... and siginfo_t/ucontext_t are its own, so
//! the guest's signals live here, not in the translator's Android table.
//!
//! Only signals the game raises itself are delivered: raise() runs the
//! handler before it returns (or when sigprocmask unblocks it), on the
//! calling thread and its normal stack. Real faults (SIGSEGV, SIGBUS, ... in
//! translated code) never reach a guest handler: the bridge's own fault
//! handling stops the game and reports where, so a crash reporter's
//! handlers are kept (sigaction answers them back) but stay unused.

use std::cell::Cell;
use std::sync::Mutex;

use arm32_bridge::handler;
use arm32_bridge::runtime::{call_guest, mem, shared, stop_with_fault, ArgSource, GuestCpu, Ret, Uc};

use super::function;

const SIG_DFL: u32 = 0;
const SIG_IGN: u32 = 1;
const SIG_ERR: u32 = u32::MAX;
const SIGKILL: u32 = 9;
const SIGSTOP: u32 = 17;
/// Darwin has signals 1-31 (NSIG 32).
const LAST: u32 = 31;

const SA_RESTART: u32 = 0x2;
const SA_RESETHAND: u32 = 0x4;
const SA_NODEFER: u32 = 0x10;
const SA_SIGINFO: u32 = 0x40;

const SIG_BLOCK: u32 = 1;
const SIG_UNBLOCK: u32 = 2;
const SIG_SETMASK: u32 = 3;

const SS_DISABLE: u32 = 4;
const MINSIGSTKSZ: u32 = 32768;

const EINVAL: u32 = 22;
const ENOMEM: u32 = 12;

/// Darwin's siginfo_t (64 bytes) and ucontext_t (32) with its mcontext
/// (exception state 12, r0-r12 sp lr pc cpsr 68, VFP 260).
const SIGINFO_SIZE: u32 = 64;
const UCONTEXT_SIZE: u32 = 32;
const MCONTEXT_SIZE: u32 = 340;
const SI_USER: u32 = 0x10001;

#[derive(Clone, Copy, Default, PartialEq, Debug)]
struct Action {
    handler: u32,
    mask: u32,
    flags: u32,
}

/// The process's actions, by Darwin signal number.
static ACTIONS: Mutex<[Action; 32]> = Mutex::new([Action { handler: 0, mask: 0, flags: 0 }; 32]);

thread_local! {
    /// This thread's blocked and raised-while-blocked sets (bit n-1 = signal n).
    static BLOCKED: Cell<u32> = const { Cell::new(0) };
    static PENDING: Cell<u32> = const { Cell::new(0) };
    /// sigaltstack's stack_t { sp, size, flags }.
    static ALT_STACK: Cell<[u32; 3]> = const { Cell::new([0, 0, SS_DISABLE]) };
}

fn bit(signal: u32) -> u32 {
    1 << (signal - 1)
}

/// SIGKILL and SIGSTOP can't be blocked.
fn blockable(mask: u32) -> u32 {
    mask & !(bit(SIGKILL) | bit(SIGSTOP))
}

fn actions() -> std::sync::MutexGuard<'static, [Action; 32]> {
    ACTIONS.lock().unwrap_or_else(|e| e.into_inner())
}

fn fail(uc: &Uc, error: u32) -> Ret {
    mem().write_u32(uc.get_data().errno_slot, error);
    Ret::U32(u32::MAX)
}

/// Darwin's default action is to ignore these (SIGURG, SIGCONT, SIGCHLD, SIGIO, SIGWINCH, SIGINFO).
fn ignored_by_default(signal: u32) -> bool {
    matches!(signal, 16 | 19 | 20 | 23 | 28 | 29)
}

/// Darwin's sys_siglist.
fn name(signal: u32) -> Option<&'static str> {
    const NAMES: [&str; 32] = [
        "Signal 0",
        "Hangup",
        "Interrupt",
        "Quit",
        "Illegal instruction",
        "Trace/BPT trap",
        "Abort trap",
        "EMT trap",
        "Floating point exception",
        "Killed",
        "Bus error",
        "Segmentation fault",
        "Bad system call",
        "Broken pipe",
        "Alarm clock",
        "Terminated",
        "Urgent I/O condition",
        "Suspended (signal)",
        "Suspended",
        "Continued",
        "Child exited",
        "Stopped (tty input)",
        "Stopped (tty output)",
        "I/O possible",
        "Cputime limit exceeded",
        "Filesize limit exceeded",
        "Virtual timer expired",
        "Profiling timer expired",
        "Window size changes",
        "Information request",
        "User defined signal 1",
        "User defined signal 2",
    ];
    NAMES.get(signal as usize).copied()
}

/// Delivers `signal` on this thread now, or keeps it for later if blocked.
fn raise(uc: &mut Uc, signal: u32) {
    if BLOCKED.get() & bit(signal) != 0 {
        PENDING.set(PENDING.get() | bit(signal));
        return;
    }
    let action = {
        let mut actions = actions();
        let action = actions[signal as usize];
        if action.handler > SIG_IGN && action.flags & SA_RESETHAND != 0 {
            actions[signal as usize] = Action::default();
        }
        action
    };
    match action.handler {
        SIG_IGN => {}
        SIG_DFL if ignored_by_default(signal) => {}
        SIG_DFL => {
            let text = name(signal).unwrap_or("unknown signal");
            stop_with_fault(uc, format!("the game raised signal {signal} ({text}) with no handler set, which ends it"));
        }
        handler => run_handler(uc, signal, handler, action),
    }
}

/// Runs a guest handler as the kernel would: the signal (unless SA_NODEFER)
/// and the action's mask blocked meanwhile; with SA_SIGINFO, a siginfo_t
/// and a ucontext_t saying where raise() was called.
fn run_handler(uc: &mut Uc, signal: u32, handler: u32, action: Action) {
    let blocked = BLOCKED.get();
    let deferred = if action.flags & SA_NODEFER == 0 { bit(signal) } else { 0 };
    BLOCKED.set(blockable(blocked | action.mask | deferred));
    let result = if action.flags & SA_SIGINFO != 0 {
        let frame = super::alloc(SIGINFO_SIZE + UCONTEXT_SIZE + MCONTEXT_SIZE);
        let m = mem();
        unsafe { std::ptr::write_bytes(m.host(frame), 0, (SIGINFO_SIZE + UCONTEXT_SIZE + MCONTEXT_SIZE) as usize) };
        let (info, context) = (frame, frame + SIGINFO_SIZE);
        let machine = context + UCONTEXT_SIZE;
        m.write_u32(info, signal);
        m.write_u32(info + 8, SI_USER);
        m.write_u32(info + 12, std::process::id());
        m.write_u32(info + 16, unsafe { libc::getuid() });
        let [stack, size, flags] = ALT_STACK.get();
        for (offset, word) in [(4, blocked), (8, stack), (12, size), (16, flags), (24, MCONTEXT_SIZE), (28, machine)] {
            m.write_u32(context + offset, word);
        }
        // r0-r12, sp, lr; pc: where raise returns to.
        let state = machine + 12;
        for register in 0..15u32 {
            m.write_u32(state + register * 4, uc.reg(register as usize));
        }
        let resume = uc.reg(14);
        m.write_u32(state + 15 * 4, resume & !1);
        m.write_u32(state + 16 * 4, 0x10 | (resume & 1) << 5);
        let result = call_guest(uc, handler, &[signal, info, context]);
        super::free(frame);
        result
    } else {
        call_guest(uc, handler, &[signal])
    };
    BLOCKED.set(blocked);
    if let Err(error) = result {
        stop_with_fault(uc, format!("handler for signal {signal}: {error}"));
    }
}

/// Runs the signals waiting for an unblock, lowest first.
fn deliver_unblocked(uc: &mut Uc) {
    loop {
        let ready = PENDING.get() & !BLOCKED.get();
        if ready == 0 || uc.get_data().fault.is_some() {
            return;
        }
        let signal = ready.trailing_zeros() + 1;
        PENDING.set(PENDING.get() & !bit(signal));
        raise(uc, signal);
    }
}

/// sigprocmask / pthread_sigmask: None for a bad `how` (EINVAL).
fn change_mask(uc: &mut Uc, how: u32, set: u32, old: u32) -> Option<()> {
    let current = BLOCKED.get();
    if set != 0 {
        let given = mem().read_u32(set);
        let next = match how {
            SIG_BLOCK => current | given,
            SIG_UNBLOCK => current & !given,
            SIG_SETMASK => given,
            _ => return None,
        };
        BLOCKED.set(blockable(next));
    }
    if old != 0 {
        mem().write_u32(old, current);
    }
    deliver_unblocked(uc);
    Some(())
}

pub fn register() {
    // sigaction(signal, const struct sigaction *new, struct sigaction *old)
    function(
        "sigaction",
        handler!(|uc, a| {
            let (signal, new, old) = (a.u32(), a.u32(), a.u32());
            if signal == 0 || signal > LAST || (new != 0 && (signal == SIGKILL || signal == SIGSTOP)) {
                return fail(uc, EINVAL);
            }
            let m = mem();
            let previous = {
                let mut actions = actions();
                let previous = actions[signal as usize];
                if new != 0 {
                    actions[signal as usize] = Action { handler: m.read_u32(new), mask: m.read_u32(new + 4), flags: m.read_u32(new + 8) };
                }
                previous
            };
            if old != 0 {
                m.write_u32(old, previous.handler);
                m.write_u32(old + 4, previous.mask);
                m.write_u32(old + 8, previous.flags);
            }
            Ret::U32(0)
        })
        .unwrap(),
    );
    // signal(signal, handler) -> the old handler, or SIG_ERR. BSD semantics: SA_RESTART.
    function(
        "signal",
        handler!(|uc, a| {
            let (signal, handler) = (a.u32(), a.u32());
            if signal == 0 || signal > LAST || signal == SIGKILL || signal == SIGSTOP {
                fail(uc, EINVAL);
                return Ret::U32(SIG_ERR);
            }
            let new = Action { handler, mask: 0, flags: SA_RESTART };
            Ret::U32(std::mem::replace(&mut actions()[signal as usize], new).handler)
        })
        .unwrap(),
    );
    function(
        "sigprocmask",
        handler!(|uc, a| {
            let (how, set, old) = (a.u32(), a.u32(), a.u32());
            match change_mask(uc, how, set, old) {
                Some(()) => Ret::U32(0),
                None => fail(uc, EINVAL),
            }
        })
        .unwrap(),
    );
    function(
        "pthread_sigmask",
        handler!(|uc, a| {
            let (how, set, old) = (a.u32(), a.u32(), a.u32());
            Ret::U32(if change_mask(uc, how, set, old).is_some() { 0 } else { EINVAL })
        })
        .unwrap(),
    );
    function(
        "raise",
        handler!(|uc, a| {
            let signal = a.u32();
            if signal == 0 || signal > LAST {
                return fail(uc, EINVAL);
            }
            raise(uc, signal);
            Ret::U32(0)
        })
        .unwrap(),
    );
    // sigaltstack(const stack_t *new, stack_t *old): kept for the answer;
    // handlers run on the stack raise() was called on (see the notes above).
    function(
        "sigaltstack",
        handler!(|uc, a| {
            let (new, old) = (a.u32(), a.u32());
            let m = mem();
            let current = ALT_STACK.get();
            if new != 0 {
                let given = [m.read_u32(new), m.read_u32(new + 4), m.read_u32(new + 8)];
                if given[2] & !SS_DISABLE != 0 {
                    return fail(uc, EINVAL);
                }
                if given[2] & SS_DISABLE == 0 && given[1] < MINSIGSTKSZ {
                    return fail(uc, ENOMEM);
                }
                ALT_STACK.set(if given[2] & SS_DISABLE != 0 { [0, 0, SS_DISABLE] } else { given });
            }
            if old != 0 {
                for (i, word) in current.iter().enumerate() {
                    m.write_u32(old + i as u32 * 4, *word);
                }
            }
            Ret::U32(0)
        })
        .unwrap(),
    );
    function(
        "strsignal",
        handler!(|uc, a| {
            let signal = a.u32();
            let text = match name(signal) {
                Some(text) if signal <= LAST => text.to_string(),
                _ => format!("Unknown signal: {}", signal as i32),
            };
            Ret::U32(shared().cached_cstr(format!("darwin-strsignal:{signal}"), &text))
        })
        .unwrap(),
    );
}
