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
// the next cancellation point this layer runs for it: condition waits,
// sleeps, socket waits and pthread_testcancel. A thread blocked in one of
// those is woken (its condition is signalled, or a no-op signal ends its
// system call), again every 10 ms for 3 s, in case the wake came just
// before it went to sleep.

const APPLE_PTHREAD_CANCELED: usize = 1;

/// What a thread is blocked in: 0 nothing, 1 a system call, else a condition.
struct Slot(AtomicUsize);

#[derive(Default)]
struct CancelState {
    /// Threads that ran a cancellation point (and still live): their slot.
    slots: std::collections::HashMap<usize, &'static Slot>,
    pending: std::collections::HashSet<usize>,
}

static CANCEL: std::sync::Mutex<Option<CancelState>> = std::sync::Mutex::new(None);
static ANY_PENDING: AtomicUsize = AtomicUsize::new(0);

fn cancel_state() -> std::sync::MutexGuard<'static, Option<CancelState>> {
    CANCEL.lock().unwrap_or_else(|e| e.into_inner())
}

/// The calling thread's slot, registered on first use and dropped at thread end.
struct Registered(&'static Slot, usize);

impl Drop for Registered {
    fn drop(&mut self) {
        if let Some(state) = cancel_state().as_mut() {
            state.slots.remove(&self.1);
            if state.pending.remove(&self.1) {
                ANY_PENDING.fetch_sub(1, Ordering::SeqCst);
            }
        }
    }
}

thread_local! {
    static SLOT: Registered = {
        let slot: &'static Slot = Box::leak(Box::new(Slot(AtomicUsize::new(0))));
        let me = unsafe { libc::pthread_self() } as usize;
        cancel_state().get_or_insert_with(Default::default).slots.insert(me, slot);
        Registered(slot, me)
    };
    static CANCEL_ENABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
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
    if ANY_PENDING.load(Ordering::SeqCst) == 0 || !CANCEL_ENABLED.with(|e| e.get()) {
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

/// Runs a blocking call as a cancellation point (`on`: 1, or the condition waited on).
pub fn waiting<T>(on: usize, work: impl FnOnce() -> T) -> T {
    let slot = SLOT.try_with(|s| s.0).ok();
    if let Some(slot) = slot {
        slot.0.store(on, Ordering::SeqCst);
    }
    cancel_point();
    let result = work();
    if let Some(slot) = slot {
        slot.0.store(0, Ordering::SeqCst);
    }
    cancel_point();
    result
}

pub fn blocking<T>(work: impl FnOnce() -> T) -> T {
    waiting(1, work)
}

fn wake_signal() -> i32 {
    libc::SIGRTMIN() + 7
}

extern "C" fn ignore_signal(_signal: i32) {}

fn install_wake_signal() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = ignore_signal as usize;
        // No SA_RESTART: a blocked system call returns EINTR.
        action.sa_flags = 0;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(wake_signal(), &action, std::ptr::null_mut());
    });
}

extern "C" fn ios_pthread_cancel(thread: libc::pthread_t) -> i32 {
    let target = thread as usize;
    install_wake_signal();
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
        for _ in 0..300 {
            {
                let state = cancel_state();
                let Some(state) = state.as_ref() else { return };
                if !state.pending.contains(&target) {
                    return;
                }
                // Threads in `slots` are alive: they leave it before they end.
                if let Some(slot) = state.slots.get(&target) {
                    match slot.0.load(Ordering::SeqCst) {
                        0 => {}
                        1 => unsafe {
                            libc::pthread_kill(target as libc::pthread_t, wake_signal());
                        },
                        cond => unsafe {
                            libc::pthread_cond_broadcast(cond as *mut libc::pthread_cond_t);
                        },
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    });
    0
}

/// Apple: PTHREAD_CANCEL_ENABLE 1, PTHREAD_CANCEL_DISABLE 0.
extern "C" fn ios_pthread_setcancelstate(state: i32, old: *mut i32) -> i32 {
    if state != 0 && state != 1 {
        return libc::EINVAL;
    }
    let before = CANCEL_ENABLED.with(|e| e.replace(state == 1));
    if !old.is_null() {
        unsafe { *old = before as i32 };
    }
    0
}

extern "C" fn ios_pthread_testcancel() {
    cancel_point();
}

extern "C" fn ios_usleep(micros: u32) -> i32 {
    blocking(|| unsafe { libc::usleep(micros) })
}

extern "C" fn ios_nanosleep(span: *const libc::timespec, left: *mut libc::timespec) -> i32 {
    blocking(|| unsafe { libc::nanosleep(span, left) })
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
        pthread_join, pthread_detach, pthread_self, pthread_equal, pthread_key_create, pthread_key_delete,
        pthread_getspecific, pthread_setspecific, pthread_mutex_init, pthread_mutexattr_init,
        pthread_mutexattr_destroy, pthread_cond_init, pthread_condattr_init, pthread_condattr_destroy,
        pthread_kill, pthread_exit,
        pthread_setschedparam, pthread_getschedparam, sched_get_priority_max, sched_get_priority_min,
        pause
    );
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
