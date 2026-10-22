//! Apple threads, locks, atomics, jumps and signals.

use crate::symbols::Registry;
use std::sync::atomic::{fence, AtomicI32, AtomicI64, AtomicU64, AtomicUsize, Ordering};

const APPLE_MUTEX_INIT: u64 = 0x32AA_ABA7;
const APPLE_RECURSIVE_MUTEX_INIT: u64 = 0x32AA_ABA2;
const APPLE_ERRORCHECK_MUTEX_INIT: u64 = 0x32AA_ABA1;
const APPLE_COND_INIT: u64 = 0x3CB0_B1BB;

/// Turns Apple's ready-made static lock values into host locks the first
/// time. The host lock is built aside; its first word (where Apple's marker
/// sits) is written last, so no thread sees a half-made lock.
unsafe fn fix_mutex(mutex: *mut libc::pthread_mutex_t) {
    let signature = &*(mutex as *const AtomicU64);
    if !is_apple_mutex_marker(signature.load(Ordering::Acquire)) {
        return;
    }
    static FIXING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = FIXING.lock().unwrap_or_else(|e| e.into_inner());
    let value = signature.load(Ordering::Acquire);
    let kind = match value {
        APPLE_MUTEX_INIT => libc::PTHREAD_MUTEX_NORMAL,
        APPLE_RECURSIVE_MUTEX_INIT => libc::PTHREAD_MUTEX_RECURSIVE,
        APPLE_ERRORCHECK_MUTEX_INIT => libc::PTHREAD_MUTEX_ERRORCHECK,
        _ => return,
    };
    let mut fresh: libc::pthread_mutex_t = std::mem::zeroed();
    let mut attr: libc::pthread_mutexattr_t = std::mem::zeroed();
    libc::pthread_mutexattr_init(&mut attr);
    libc::pthread_mutexattr_settype(&mut attr, kind);
    libc::pthread_mutex_init(&mut fresh, &attr);
    libc::pthread_mutexattr_destroy(&mut attr);
    let size = std::mem::size_of::<libc::pthread_mutex_t>();
    let source = &fresh as *const libc::pthread_mutex_t as *const u8;
    std::ptr::copy_nonoverlapping(source.add(8), (mutex as *mut u8).add(8), size - 8);
    signature.store(std::ptr::read_unaligned(source as *const u64), Ordering::Release);
}

fn is_apple_mutex_marker(value: u64) -> bool {
    matches!(value, APPLE_MUTEX_INIT | APPLE_RECURSIVE_MUTEX_INIT | APPLE_ERRORCHECK_MUTEX_INIT)
}

unsafe fn fix_cond(cond: *mut libc::pthread_cond_t) {
    let signature = &*(cond as *const AtomicU64);
    if signature.load(Ordering::Acquire) == APPLE_COND_INIT {
        let _ = signature.compare_exchange(APPLE_COND_INIT, 0, Ordering::AcqRel, Ordering::Acquire);
    }
}

extern "C" fn mutex_lock(m: *mut libc::pthread_mutex_t) -> i32 {
    unsafe {
        fix_mutex(m);
        libc::pthread_mutex_lock(m)
    }
}

extern "C" fn mutex_trylock(m: *mut libc::pthread_mutex_t) -> i32 {
    unsafe {
        fix_mutex(m);
        let r = libc::pthread_mutex_trylock(m);
        if r == libc::EBUSY { 16 } else { r }
    }
}

extern "C" fn mutex_unlock(m: *mut libc::pthread_mutex_t) -> i32 {
    unsafe {
        fix_mutex(m);
        libc::pthread_mutex_unlock(m)
    }
}

extern "C" fn mutex_destroy(m: *mut libc::pthread_mutex_t) -> i32 {
    unsafe {
        fix_mutex(m);
        libc::pthread_mutex_destroy(m)
    }
}

extern "C" fn mutexattr_settype(attr: *mut libc::pthread_mutexattr_t, apple_kind: i32) -> i32 {
    let kind = match apple_kind {
        1 => libc::PTHREAD_MUTEX_ERRORCHECK,
        2 => libc::PTHREAD_MUTEX_RECURSIVE,
        _ => libc::PTHREAD_MUTEX_NORMAL,
    };
    unsafe { libc::pthread_mutexattr_settype(attr, kind) }
}

extern "C" fn cond_wait(c: *mut libc::pthread_cond_t, m: *mut libc::pthread_mutex_t) -> i32 {
    unsafe {
        fix_cond(c);
        fix_mutex(m);
        waiting(c as usize, || libc::pthread_cond_wait(c, m))
    }
}

extern "C" fn cond_timedwait(c: *mut libc::pthread_cond_t, m: *mut libc::pthread_mutex_t, t: *const libc::timespec) -> i32 {
    unsafe {
        fix_cond(c);
        fix_mutex(m);
        let r = waiting(c as usize, || libc::pthread_cond_timedwait(c, m, t));
        if r == libc::ETIMEDOUT { 60 } else { r }
    }
}

/// pthread_cond_timedwait_relative_np: the wait limit is a span from now
/// (conditions here measure time on the wall clock, the default).
extern "C" fn cond_timedwait_relative(c: *mut libc::pthread_cond_t, m: *mut libc::pthread_mutex_t, span: *const libc::timespec) -> i32 {
    if span.is_null() {
        return libc::EINVAL;
    }
    let mut at = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe {
        libc::clock_gettime(libc::CLOCK_REALTIME, &mut at);
        at.tv_sec += (*span).tv_sec;
        at.tv_nsec += (*span).tv_nsec;
    }
    at.tv_sec += at.tv_nsec / 1_000_000_000;
    at.tv_nsec %= 1_000_000_000;
    cond_timedwait(c, m, &at)
}

extern "C" fn cond_signal(c: *mut libc::pthread_cond_t) -> i32 {
    unsafe {
        fix_cond(c);
        libc::pthread_cond_signal(c)
    }
}

extern "C" fn cond_broadcast(c: *mut libc::pthread_cond_t) -> i32 {
    unsafe {
        fix_cond(c);
        libc::pthread_cond_broadcast(c)
    }
}

extern "C" fn cond_destroy(c: *mut libc::pthread_cond_t) -> i32 {
    unsafe {
        fix_cond(c);
        libc::pthread_cond_destroy(c)
    }
}

extern "C" fn attr_setdetachstate(attr: *mut libc::pthread_attr_t, apple_state: i32) -> i32 {
    let state = if apple_state == 2 { libc::PTHREAD_CREATE_DETACHED } else { libc::PTHREAD_CREATE_JOINABLE };
    unsafe { libc::pthread_attr_setdetachstate(attr, state) }
}

extern "C" {
    fn pthread_attr_setschedpolicy(attr: *mut libc::pthread_attr_t, policy: i32) -> i32;
    fn pthread_atfork(prepare: usize, parent: usize, child: usize) -> i32;
    fn pthread_getattr_np(thread: libc::pthread_t, attr: *mut libc::pthread_attr_t) -> i32;
}

/// Apple's policy numbers: SCHED_OTHER 1, SCHED_RR 2, SCHED_FIFO 4.
extern "C" fn attr_setschedpolicy(attr: *mut libc::pthread_attr_t, apple_policy: i32) -> i32 {
    let policy = match apple_policy {
        1 => 0, // SCHED_OTHER (the libc crate lacks it on Android)
        2 => libc::SCHED_RR,
        4 => libc::SCHED_FIFO,
        _ => return libc::EINVAL,
    };
    unsafe { pthread_attr_setschedpolicy(attr, policy) }
}

/// Apple's pthread_setname_np names the calling thread (the system keeps 15 bytes).
extern "C" fn setname_np(name: *const libc::c_char) -> i32 {
    if name.is_null() {
        return libc::EINVAL;
    }
    let mut short = unsafe { std::ffi::CStr::from_ptr(name) }.to_bytes().to_vec();
    short.truncate(15);
    while std::str::from_utf8(&short).is_err() {
        short.pop();
    }
    let short = std::ffi::CString::new(short).unwrap_or_default();
    unsafe { libc::pthread_setname_np(libc::pthread_self(), short.as_ptr()) };
    0
}

/// pthread_get_stackaddr_np: the top (highest address) of the thread's stack.
extern "C" fn get_stackaddr_np(thread: libc::pthread_t) -> usize {
    unsafe {
        let mut attr: libc::pthread_attr_t = std::mem::zeroed();
        if pthread_getattr_np(thread, &mut attr) != 0 {
            return 0;
        }
        let (mut low, mut size) = (std::ptr::null_mut(), 0usize);
        libc::pthread_attr_getstack(&attr, &mut low, &mut size);
        libc::pthread_attr_destroy(&mut attr);
        low as usize + size
    }
}

// ----- cancellation (deferred, Apple's default) -----
// Android has no pthread_cancel, so it is done here: the request is noted,
// and the thread ends (as if it called pthread_exit(PTHREAD_CANCELED)) at
// the next cancellation point it reaches: every Apple cancellation point this
// layer exports (read, write, open, fsync, pause, sleep, usleep, nanosleep,
// pthread_join, condition waits, socket calls, poll, select) and
// pthread_testcancel. A thread blocked in one of those is woken: its
// condition is broadcast, or a no-op signal (a free real-time signal, picked
// so no one else's handler is replaced) ends its system call. While the
// cancel is pending the wake is sent again, every 10 ms at first and then
// less often (up to once a second), so a wake that came just before the
// thread went to sleep isn't lost; nothing runs while no cancel is under way.
// A system call our wake interrupted without ending the thread is made again
// with the time it had left, so the game never sees that interruption.
// Limit: a thread the game didn't start through this layer and that never
// reaches a cancellation point keeps its request (and one check a second).

const APPLE_PTHREAD_CANCELED: usize = 1;

/// A thread's cancellation state, readable by the thread that cancels it.
struct Slot {
    /// What the thread is blocked in: 0 nothing, 1 a system call, else a condition.
    blocked_in: AtomicUsize,
    /// pthread_setcancelstate: cancellation allowed.
    enabled: std::sync::atomic::AtomicBool,
    /// A wake signal was sent while it was blocked.
    woken: std::sync::atomic::AtomicBool,
    /// The wake signal this thread has unblocked (0: none yet).
    unblocked: std::sync::atomic::AtomicI32,
}

#[derive(Default)]
struct CancelState {
    /// Live threads that used a cancellation call: their slot.
    slots: std::collections::HashMap<usize, &'static Slot>,
    pending: std::collections::HashSet<usize>,
}

static CANCEL: std::sync::Mutex<Option<CancelState>> = std::sync::Mutex::new(None);
static ANY_PENDING: AtomicUsize = AtomicUsize::new(0);

fn cancel_state() -> std::sync::MutexGuard<'static, Option<CancelState>> {
    CANCEL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Drops a cancel request for this pthread_t: at a thread's start and end, so a
/// later thread that gets the same pthread_t never inherits it.
fn forget_cancel(thread: usize) {
    if let Some(state) = cancel_state().as_mut() {
        if state.pending.remove(&thread) {
            ANY_PENDING.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

/// The calling thread's slot, registered on first use and dropped at thread end.
struct Registered(&'static Slot, usize);

impl Drop for Registered {
    fn drop(&mut self) {
        if let Some(state) = cancel_state().as_mut() {
            state.slots.remove(&self.1);
        }
        forget_cancel(self.1);
    }
}

/// Forgets the thread's cancel request when a thread started here ends.
struct ThreadEnd(usize);

impl Drop for ThreadEnd {
    fn drop(&mut self) {
        forget_cancel(self.0);
    }
}

thread_local! {
    static SLOT: Registered = {
        let slot: &'static Slot = Box::leak(Box::new(Slot {
            blocked_in: AtomicUsize::new(0),
            enabled: std::sync::atomic::AtomicBool::new(true),
            woken: std::sync::atomic::AtomicBool::new(false),
            unblocked: std::sync::atomic::AtomicI32::new(0),
        }));
        let me = unsafe { libc::pthread_self() } as usize;
        cancel_state().get_or_insert_with(Default::default).slots.insert(me, slot);
        Registered(slot, me)
    };
    static THREAD_END: std::cell::RefCell<Option<ThreadEnd>> = const { std::cell::RefCell::new(None) };
}

fn my_slot() -> Option<&'static Slot> {
    SLOT.try_with(|s| s.0).ok()
}

std::arch::global_asm!(
    // pthread_exit called from code with no unwind tables, so on glibc the
    // thread's unwinding stops here and jumps straight to the thread's end
    // instead of running through this layer's Rust frames.
    ".globl ios_cancel_exit",
    ".p2align 2",
    "ios_cancel_exit:",
    "mov x16, x0",
    "mov x0, x1",
    "blr x16",
    "brk #0",
);

extern "C" {
    fn ios_cancel_exit(exit: usize, value: usize) -> !;
}

/// Ends the calling thread here if it was cancelled (and allows it).
pub fn cancel_point() {
    if ANY_PENDING.load(Ordering::SeqCst) == 0 {
        return;
    }
    if !my_slot().map_or(true, |slot| slot.enabled.load(Ordering::SeqCst)) {
        return;
    }
    let me = unsafe { libc::pthread_self() } as usize;
    let found = cancel_state().as_mut().is_some_and(|state| state.pending.remove(&me));
    if !found {
        return;
    }
    ANY_PENDING.fetch_sub(1, Ordering::SeqCst);
    crate::log!("a cancelled game thread ends at a cancellation point");
    unsafe { ios_cancel_exit(libc::pthread_exit as usize, APPLE_PTHREAD_CANCELED) }
}

/// A condition wait as a cancellation point (a cancel broadcasts the condition;
/// the wake looks like an ordinary spurious wake-up).
pub fn waiting<T>(cond: usize, work: impl FnOnce() -> T) -> T {
    let slot = my_slot();
    if let Some(slot) = slot {
        slot.blocked_in.store(cond, Ordering::SeqCst);
    }
    cancel_point();
    let result = work();
    if let Some(slot) = slot {
        slot.blocked_in.store(0, Ordering::SeqCst);
    }
    cancel_point();
    result
}

/// A system call as a cancellation point. A call our wake signal ended with
/// EINTR, where the thread then wasn't cancelled, is made again (the closure
/// keeps what time is left for calls with a time limit).
pub fn blocking<T: PartialEq + From<i8>>(mut work: impl FnMut() -> T) -> T {
    let slot = my_slot();
    if let Some(slot) = slot {
        let signal = WAKE_SIGNAL.load(Ordering::Relaxed);
        if signal != 0 && slot.unblocked.load(Ordering::Relaxed) != signal {
            unblock(signal);
            slot.unblocked.store(signal, Ordering::Relaxed);
        }
    }
    loop {
        if let Some(slot) = slot {
            slot.woken.store(false, Ordering::SeqCst);
            slot.blocked_in.store(1, Ordering::SeqCst);
        }
        cancel_point();
        let result = work();
        let error = unsafe { *crate::libsystem::host_errno() };
        if let Some(slot) = slot {
            slot.blocked_in.store(0, Ordering::SeqCst);
        }
        cancel_point();
        let woken = slot.is_some_and(|slot| slot.woken.swap(false, Ordering::SeqCst));
        if woken && result == T::from(-1) && error == libc::EINTR {
            continue;
        }
        unsafe { *crate::libsystem::host_errno() = error };
        return result;
    }
}

/// The real-time signal used to wake blocked threads (0: none picked yet or none free).
static WAKE_SIGNAL: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

static SIGNAL_ARRIVED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn ignore_signal(_signal: i32) {
    SIGNAL_ARRIVED.store(true, Ordering::SeqCst);
}

/// Whether the signal really arrives (an emulator can lack some numbers).
fn signal_arrives(signal: i32) -> bool {
    SIGNAL_ARRIVED.store(false, Ordering::SeqCst);
    std::thread::spawn(move || {
        unblock(signal);
        unsafe { libc::pthread_kill(libc::pthread_self(), signal) };
    })
    .join()
    .is_ok()
        && SIGNAL_ARRIVED.load(Ordering::SeqCst)
}

fn unblock(signal: i32) {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, signal);
        libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
    }
}

/// Picks a real-time signal nobody handles, from SIGRTMIN+7 up (clear of the
/// ones the C library keeps), and installs a no-op handler there (no
/// SA_RESTART, so a blocked system call returns EINTR).
fn wake_signal() -> i32 {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        for signal in libc::SIGRTMIN() + 7..=libc::SIGRTMAX() {
            let mut current: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(signal, std::ptr::null(), &mut current) != 0 || current.sa_sigaction != libc::SIG_DFL {
                continue;
            }
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = ignore_signal as usize;
            action.sa_flags = 0;
            libc::sigemptyset(&mut action.sa_mask);
            if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                continue;
            }
            if signal_arrives(signal) {
                WAKE_SIGNAL.store(signal, Ordering::SeqCst);
                return;
            }
            libc::sigaction(signal, &current, std::ptr::null_mut());
        }
        crate::log!("pthread_cancel: no free real-time signal; blocked system calls end only at their next cancellation point");
    });
    WAKE_SIGNAL.load(Ordering::SeqCst)
}

extern "C" fn ios_pthread_cancel(thread: libc::pthread_t) -> i32 {
    let target = thread as usize;
    let signal = wake_signal();
    {
        let mut state = cancel_state();
        if state.get_or_insert_with(Default::default).pending.insert(target) {
            ANY_PENDING.fetch_add(1, Ordering::SeqCst);
        }
    }
    if unsafe { libc::pthread_equal(thread, libc::pthread_self()) } != 0 {
        return 0;
    }
    std::thread::spawn(move || {
        let mut pause_ms = 10;
        loop {
            {
                let state = cancel_state();
                let Some(state) = state.as_ref() else { return };
                if !state.pending.contains(&target) {
                    return;
                }
                // Threads in `slots` are alive: they leave it (under this lock) before they end.
                if let Some(slot) = state.slots.get(&target) {
                    if slot.enabled.load(Ordering::SeqCst) {
                        match slot.blocked_in.load(Ordering::SeqCst) {
                            0 => {}
                            1 if signal != 0 => unsafe {
                                slot.woken.store(true, Ordering::SeqCst);
                                libc::pthread_kill(target as libc::pthread_t, signal);
                            },
                            1 => {}
                            cond => unsafe {
                                libc::pthread_cond_broadcast(cond as *mut libc::pthread_cond_t);
                            },
                        }
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(pause_ms));
            pause_ms = (pause_ms * 2).min(1000);
        }
    });
    0
}

/// Apple: PTHREAD_CANCEL_ENABLE 1, PTHREAD_CANCEL_DISABLE 0.
extern "C" fn ios_pthread_setcancelstate(state: i32, old: *mut i32) -> i32 {
    if state != 0 && state != 1 {
        return libc::EINVAL;
    }
    let before = match my_slot() {
        Some(slot) => slot.enabled.swap(state == 1, Ordering::SeqCst),
        None => true,
    };
    if !old.is_null() {
        unsafe { *old = before as i32 };
    }
    0
}

extern "C" fn ios_pthread_testcancel() {
    cancel_point();
}

/// nanosleep as a cancellation point; a retry after our wake sleeps only what was left.
fn sleep_for(span: libc::timespec, left: *mut libc::timespec) -> i32 {
    let mut request = span;
    let result = blocking(|| {
        let mut rest = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        let result = unsafe { libc::nanosleep(&request, &mut rest) };
        if result != 0 && unsafe { *crate::libsystem::host_errno() } == libc::EINTR {
            request = rest;
        }
        result
    });
    if result != 0 && !left.is_null() {
        unsafe { *left = request };
    }
    result
}

extern "C" fn ios_usleep(micros: u32) -> i32 {
    let span = libc::timespec { tv_sec: (micros / 1_000_000) as _, tv_nsec: ((micros % 1_000_000) * 1000) as _ };
    sleep_for(span, std::ptr::null_mut())
}

extern "C" fn ios_nanosleep(span: *const libc::timespec, left: *mut libc::timespec) -> i32 {
    if span.is_null() {
        unsafe { *crate::libsystem::host_errno() = libc::EFAULT };
        return -1;
    }
    sleep_for(unsafe { *span }, left)
}

/// sleep: whole seconds; a cancel ends it like the other sleeps.
extern "C" fn ios_sleep(seconds: u32) -> u32 {
    let mut left = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    if sleep_for(libc::timespec { tv_sec: seconds as _, tv_nsec: 0 }, &mut left) != 0 {
        return left.tv_sec as u32 + (left.tv_nsec > 0) as u32;
    }
    0
}

extern "C" fn ios_pause() -> i32 {
    blocking(|| unsafe { libc::pause() })
}

pub extern "C" fn ios_read(fd: i32, buffer: *mut libc::c_void, count: usize) -> isize {
    blocking(|| unsafe { libc::read(fd, buffer, count) })
}

pub extern "C" fn ios_write(fd: i32, buffer: *const libc::c_void, count: usize) -> isize {
    blocking(|| unsafe { libc::write(fd, buffer, count) })
}

pub extern "C" fn ios_fsync(fd: i32) -> i32 {
    blocking(|| unsafe { libc::fsync(fd) })
}

// ----- pthread_join -----
// The system's pthread_join can't be interrupted, so a helper thread does it
// and the caller waits for the helper (cancellably, on a futex word). The
// answer therefore comes only once the thread has really ended (its key
// destructors included). If the caller is cancelled while waiting, the
// helper still finishes, and a later pthread_join of that thread gets the
// same answer.

const FUTEX_WAIT_PRIVATE: i32 = 128;
const FUTEX_WAKE_PRIVATE: i32 = 129;

#[derive(Default)]
struct Join {
    /// 1 once the helper's pthread_join returned.
    done: std::sync::atomic::AtomicU32,
    result: std::sync::atomic::AtomicI32,
    value: AtomicUsize,
}

static JOINS: std::sync::Mutex<Option<std::collections::HashMap<usize, std::sync::Arc<Join>>>> = std::sync::Mutex::new(None);

extern "C" fn ios_pthread_join(thread: libc::pthread_t, value: *mut *mut libc::c_void) -> i32 {
    if unsafe { libc::pthread_equal(thread, libc::pthread_self()) } != 0 {
        return libc::EDEADLK;
    }
    let (join, start) = {
        let mut joins = JOINS.lock().unwrap_or_else(|e| e.into_inner());
        let joins = joins.get_or_insert_with(Default::default);
        match joins.get(&(thread as usize)) {
            Some(join) => (join.clone(), false),
            None => {
                let join = std::sync::Arc::new(Join::default());
                joins.insert(thread as usize, join.clone());
                (join, true)
            }
        }
    };
    if start {
        let helper = join.clone();
        std::thread::spawn(move || {
            let mut out: *mut libc::c_void = std::ptr::null_mut();
            let result = unsafe { libc::pthread_join(thread, &mut out) };
            helper.value.store(out as usize, Ordering::SeqCst);
            helper.result.store(result, Ordering::SeqCst);
            helper.done.store(1, Ordering::SeqCst);
            unsafe { libc::syscall(libc::SYS_futex, helper.done.as_ptr(), FUTEX_WAKE_PRIVATE, i32::MAX) };
        });
    }
    while join.done.load(Ordering::SeqCst) == 0 {
        blocking(|| unsafe { libc::syscall(libc::SYS_futex, join.done.as_ptr(), FUTEX_WAIT_PRIVATE, 0, std::ptr::null::<libc::timespec>()) });
    }
    JOINS.lock().unwrap_or_else(|e| e.into_inner()).as_mut().map(|m| m.remove(&(thread as usize)));
    if !value.is_null() {
        unsafe { *value = join.value.load(Ordering::SeqCst) as *mut libc::c_void };
    }
    join.result.load(Ordering::SeqCst)
}

/// Each thread's Mach port number (here its Linux thread id), by pthread_t.
/// Games compare these to tell threads apart (Unreal: "am I the rendering
/// thread?"), so another thread's number must never be the caller's own.
static THREAD_IDS: std::sync::Mutex<Option<std::collections::HashMap<usize, u32>>> = std::sync::Mutex::new(None);

/// Records the calling thread's number (every thread start does this).
pub fn note_this_thread() -> u32 {
    let tid = unsafe { libc::gettid() as u32 };
    let me = unsafe { libc::pthread_self() } as usize;
    THREAD_IDS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Default::default).insert(me, tid);
    tid
}

extern "C" fn pthread_mach_thread_np(thread: libc::pthread_t) -> u32 {
    if unsafe { libc::pthread_equal(thread, libc::pthread_self()) } != 0 {
        return note_this_thread();
    }
    if let Some(&tid) = THREAD_IDS.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&(thread as usize))) {
        return tid;
    }
    crate::log!("pthread_mach_thread_np: thread {:#x} was not started through this layer; giving a made-up number", thread as usize);
    0x4000_0000 | (thread as usize as u32 >> 4)
}

// ----- OSAtomic and spin locks -----

extern "C" fn os_atomic_add32(amount: i32, value: *const AtomicI32) -> i32 {
    unsafe { (*value).fetch_add(amount, Ordering::SeqCst) + amount }
}

extern "C" fn os_atomic_cas32(old: i32, new: i32, value: *const AtomicI32) -> bool {
    unsafe { (*value).compare_exchange(old, new, Ordering::SeqCst, Ordering::SeqCst).is_ok() }
}

extern "C" fn os_atomic_cas_ptr(old: usize, new: usize, value: *const AtomicUsize) -> bool {
    unsafe { (*value).compare_exchange(old, new, Ordering::SeqCst, Ordering::SeqCst).is_ok() }
}

extern "C" fn os_atomic_add64(amount: i64, value: *const AtomicI64) -> i64 {
    unsafe { (*value).fetch_add(amount, Ordering::SeqCst) + amount }
}

extern "C" fn os_memory_barrier() {
    fence(Ordering::SeqCst);
}

extern "C" fn os_spin_lock(lock: *const AtomicI32) {
    let lock = unsafe { &*lock };
    let mut spins = 0;
    while lock.compare_exchange_weak(0, 1, Ordering::Acquire, Ordering::Relaxed).is_err() {
        spins += 1;
        if spins > 64 {
            unsafe { libc::sched_yield() };
        } else {
            std::hint::spin_loop();
        }
    }
}

extern "C" fn os_spin_unlock(lock: *const AtomicI32) {
    unsafe { (*lock).store(0, Ordering::Release) };
}

extern "C" fn os_spin_try(lock: *const AtomicI32) -> bool {
    unsafe { (*lock).compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_ok() }
}

// ----- setjmp / longjmp (Apple's jump buffer is 192 bytes; we use 168) -----

std::arch::global_asm!(
    ".globl ios_setjmp",
    ".p2align 2",
    "ios_setjmp:",
    "stp x19, x20, [x0, #0]",
    "stp x21, x22, [x0, #16]",
    "stp x23, x24, [x0, #32]",
    "stp x25, x26, [x0, #48]",
    "stp x27, x28, [x0, #64]",
    "stp x29, x30, [x0, #80]",
    "mov x16, sp",
    "str x16, [x0, #96]",
    "stp d8, d9, [x0, #104]",
    "stp d10, d11, [x0, #120]",
    "stp d12, d13, [x0, #136]",
    "stp d14, d15, [x0, #152]",
    "mov w0, #0",
    "ret",
    ".globl ios_longjmp",
    ".p2align 2",
    "ios_longjmp:",
    "ldp x19, x20, [x0, #0]",
    "ldp x21, x22, [x0, #16]",
    "ldp x23, x24, [x0, #32]",
    "ldp x25, x26, [x0, #48]",
    "ldp x27, x28, [x0, #64]",
    "ldp x29, x30, [x0, #80]",
    "ldr x16, [x0, #96]",
    "mov sp, x16",
    "ldp d8, d9, [x0, #104]",
    "ldp d10, d11, [x0, #120]",
    "ldp d12, d13, [x0, #136]",
    "ldp d14, d15, [x0, #152]",
    "cmp w1, #0",
    "csinc w0, w1, wzr, ne",
    "ret",
);

extern "C" {
    fn ios_setjmp();
    fn ios_longjmp();
}

// ----- signals: the game's crash handlers are not installed -----

extern "C" fn ios_sigaction(signal: i32, _new: usize, old: *mut u8) -> i32 {
    if !old.is_null() {
        unsafe { std::ptr::write_bytes(old, 0, 16) };
    }
    crate::log!("sigaction {signal}: ignored (the game's crash handlers are not installed)");
    0
}

extern "C" fn ios_signal(signal: i32, _handler: usize) -> usize {
    crate::log!("signal {signal}: ignored");
    0
}

extern "C" fn ios_sigaltstack(_new: usize, old: *mut u8) -> i32 {
    if !old.is_null() {
        unsafe { std::ptr::write_bytes(old, 0, 24) };
    }
    0
}

extern "C" fn ios_raise(signal: i32) -> i32 {
    crate::log!("STOP: the game raised signal {signal}");
    crate::report::finish(1);
}

extern "C" fn ios_backtrace(_buffer: usize, _size: i32) -> i32 {
    0
}

/// What a game thread needs to start; the new thread records its number
/// before the game's code runs, and pthread_create waits for that, so the
/// game can ask for the number as soon as it has the thread.
struct Start {
    start: usize,
    arg: usize,
    noted: std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}

extern "C" fn thread_entry(start: *mut libc::c_void) -> *mut libc::c_void {
    let start = unsafe { Box::from_raw(start as *mut Start) };
    note_this_thread();
    forget_cancel(unsafe { libc::pthread_self() } as usize);
    THREAD_END.with(|cell| *cell.borrow_mut() = Some(ThreadEnd(unsafe { libc::pthread_self() } as usize)));
    *start.noted.0.lock().unwrap_or_else(|e| e.into_inner()) = true;
    start.noted.1.notify_all();
    let game: extern "C" fn(*mut libc::c_void) -> *mut libc::c_void = unsafe { std::mem::transmute(start.start) };
    game(start.arg as *mut _)
}

extern "C" fn ios_pthread_create(thread: *mut libc::pthread_t, attr: *const libc::pthread_attr_t, start: usize, arg: usize) -> i32 {
    let noted = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let boxed = Box::into_raw(Box::new(Start { start, arg, noted: noted.clone() }));
    let r = unsafe { libc::pthread_create(thread, attr, thread_entry, boxed as *mut _) };
    if r != 0 {
        drop(unsafe { Box::from_raw(boxed) });
    } else {
        let mut done = noted.0.lock().unwrap_or_else(|e| e.into_inner());
        while !*done {
            done = noted.1.wait(done).unwrap_or_else(|e| e.into_inner());
        }
    }
    crate::log!("game started a thread (result {r})");
    r
}

pub fn register(r: &mut Registry) {
    macro_rules! same {
        ($($name:ident),* $(,)?) => { $( r.function(stringify!($name), libc::$name as usize); )* };
    }
    same!(
        pthread_attr_init, pthread_attr_destroy, pthread_attr_setstacksize, pthread_attr_getstacksize,
        pthread_detach, pthread_self, pthread_equal, pthread_key_create, pthread_key_delete,
        pthread_getspecific, pthread_setspecific, pthread_mutex_init, pthread_mutexattr_init,
        pthread_mutexattr_destroy, pthread_cond_init, pthread_condattr_init, pthread_condattr_destroy,
        pthread_kill, pthread_exit,
        pthread_setschedparam, pthread_getschedparam, sched_get_priority_max, sched_get_priority_min
    );
    r.function("pthread_join", ios_pthread_join as usize);
    r.function("pause", ios_pause as usize);
    r.function("sleep", ios_sleep as usize);
    r.function("read", ios_read as usize);
    r.function("write", ios_write as usize);
    r.function("fsync", ios_fsync as usize);
    r.function("pthread_cond_timedwait_relative_np", cond_timedwait_relative as usize);
    r.function("pthread_attr_setschedpolicy", attr_setschedpolicy as usize);
    r.function("pthread_atfork", pthread_atfork as usize);
    r.function("pthread_setname_np", setname_np as usize);
    r.function("pthread_get_stackaddr_np", get_stackaddr_np as usize);
    r.function("pthread_cancel", ios_pthread_cancel as usize);
    r.function("pthread_setcancelstate", ios_pthread_setcancelstate as usize);
    r.function("pthread_testcancel", ios_pthread_testcancel as usize);
    r.function("usleep", ios_usleep as usize);
    r.function("nanosleep", ios_nanosleep as usize);
    r.function("pthread_attr_setschedparam", crate::libsystem::host::pthread_attr_setschedparam as usize);
    r.function("pthread_attr_getschedparam", crate::libsystem::host::pthread_attr_getschedparam as usize);
    r.function("pthread_create", ios_pthread_create as usize);
    r.function("pthread_mutex_lock", mutex_lock as usize);
    r.function("pthread_mutex_trylock", mutex_trylock as usize);
    r.function("pthread_mutex_unlock", mutex_unlock as usize);
    r.function("pthread_mutex_destroy", mutex_destroy as usize);
    r.function("pthread_mutexattr_settype", mutexattr_settype as usize);
    r.function("pthread_cond_wait", cond_wait as usize);
    r.function("pthread_cond_timedwait", cond_timedwait as usize);
    r.function("pthread_cond_signal", cond_signal as usize);
    r.function("pthread_cond_broadcast", cond_broadcast as usize);
    r.function("pthread_cond_destroy", cond_destroy as usize);
    r.function("pthread_attr_setdetachstate", attr_setdetachstate as usize);
    r.function("pthread_mach_thread_np", pthread_mach_thread_np as usize);
    r.function("OSAtomicAdd32", os_atomic_add32 as usize);
    r.function("OSAtomicAdd32Barrier", os_atomic_add32 as usize);
    r.function("OSAtomicAdd64", os_atomic_add64 as usize);
    r.function("OSAtomicAdd64Barrier", os_atomic_add64 as usize);
    r.function("OSAtomicCompareAndSwap32", os_atomic_cas32 as usize);
    r.function("OSAtomicCompareAndSwap32Barrier", os_atomic_cas32 as usize);
    r.function("OSAtomicCompareAndSwapPtrBarrier", os_atomic_cas_ptr as usize);
    r.function("OSAtomicCompareAndSwapPtr", os_atomic_cas_ptr as usize);
    r.function("OSMemoryBarrier", os_memory_barrier as usize);
    r.function("OSSpinLockLock", os_spin_lock as usize);
    r.function("OSSpinLockUnlock", os_spin_unlock as usize);
    r.function("OSSpinLockTry", os_spin_try as usize);
    r.function("setjmp", ios_setjmp as usize);
    r.function("_setjmp", ios_setjmp as usize);
    r.function("longjmp", ios_longjmp as usize);
    r.function("_longjmp", ios_longjmp as usize);
    r.function("sigaction", ios_sigaction as usize);
    r.function("signal", ios_signal as usize);
    r.function("sigaltstack", ios_sigaltstack as usize);
    r.function("raise", ios_raise as usize);
    r.function("backtrace", ios_backtrace as usize);
}

#[cfg(test)]
mod tests {
    use super::*;

    static CHILD_TID: AtomicU64 = AtomicU64::new(0);

    extern "C" fn child(_: *mut libc::c_void) -> *mut libc::c_void {
        CHILD_TID.store(unsafe { libc::gettid() } as u64, Ordering::SeqCst);
        std::ptr::null_mut()
    }

    struct Waiter {
        mutex: libc::pthread_mutex_t,
        cond: libc::pthread_cond_t,
    }

    extern "C" fn wait_forever(waiter: *mut libc::c_void) -> *mut libc::c_void {
        let waiter = waiter as *mut Waiter;
        unsafe {
            mutex_lock(&mut (*waiter).mutex);
            loop {
                cond_wait(&mut (*waiter).cond, &mut (*waiter).mutex);
            }
        }
    }

    /// A thread asleep in a condition wait is woken by pthread_cancel and
    /// ends with Apple's PTHREAD_CANCELED, holding the mutex as on Apple.
    #[test]
    fn cancelled_condition_waiter_ends_cancelled() {
        let waiter = Box::leak(Box::new(Waiter { mutex: libc::PTHREAD_MUTEX_INITIALIZER, cond: libc::PTHREAD_COND_INITIALIZER }));
        let mut thread: libc::pthread_t = 0;
        let arg = waiter as *mut Waiter as *mut libc::c_void;
        assert_eq!(unsafe { libc::pthread_create(&mut thread, std::ptr::null(), wait_forever, arg) }, 0);
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(ios_pthread_cancel(thread), 0);
        let mut value: *mut libc::c_void = std::ptr::null_mut();
        assert_eq!(unsafe { libc::pthread_join(thread, &mut value) }, 0);
        assert_eq!(value as usize, APPLE_PTHREAD_CANCELED);
        assert_eq!(unsafe { libc::pthread_mutex_trylock(&mut waiter.mutex) }, libc::EBUSY, "mutex not held after the cancel");

        // A thread blocked in read on an empty pipe ends too, and pthread_join
        // (itself a cancellation point here) hands back PTHREAD_CANCELED.
        let mut pipe = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
        let mut reader: libc::pthread_t = 0;
        assert_eq!(ios_pthread_create(&mut reader, std::ptr::null(), read_forever as usize, pipe[0] as usize), 0);
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(ios_pthread_cancel(reader), 0);
        let mut value: *mut libc::c_void = std::ptr::null_mut();
        assert_eq!(ios_pthread_join(reader, &mut value), 0);
        assert_eq!(value as usize, APPLE_PTHREAD_CANCELED, "the reader wasn't cancelled");
        unsafe {
            libc::close(pipe[0]);
            libc::close(pipe[1]);
        }
    }

    extern "C" fn read_forever(fd: *mut libc::c_void) -> *mut libc::c_void {
        let mut byte = 0u8;
        ios_read(fd as usize as i32, &mut byte as *mut u8 as *mut _, 1);
        std::ptr::null_mut()
    }

    #[test]
    fn mach_thread_numbers_belong_to_the_asked_thread() {
        let mut thread: libc::pthread_t = 0;
        assert_eq!(ios_pthread_create(&mut thread, std::ptr::null(), child as usize, 0), 0);
        let asked = pthread_mach_thread_np(thread);
        unsafe { libc::pthread_join(thread, std::ptr::null_mut()) };
        assert_eq!(asked as u64, CHILD_TID.load(Ordering::SeqCst));
        assert_ne!(asked, unsafe { libc::gettid() } as u32);
        assert_eq!(pthread_mach_thread_np(unsafe { libc::pthread_self() }), unsafe { libc::gettid() } as u32);
    }
}
