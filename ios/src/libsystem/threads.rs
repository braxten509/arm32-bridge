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
        libc::pthread_cond_wait(c, m)
    }
}

extern "C" fn cond_timedwait(c: *mut libc::pthread_cond_t, m: *mut libc::pthread_mutex_t, t: *const libc::timespec) -> i32 {
    unsafe {
        fix_cond(c);
        fix_mutex(m);
        let r = libc::pthread_cond_timedwait(c, m, t);
        if r == libc::ETIMEDOUT { 60 } else { r }
    }
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

extern "C" fn pthread_mach_thread_np(_thread: libc::pthread_t) -> u32 {
    unsafe { libc::gettid() as u32 }
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

extern "C" fn ios_pthread_create(thread: *mut libc::pthread_t, attr: *const libc::pthread_attr_t, start: usize, arg: usize) -> i32 {
    let start: extern "C" fn(*mut libc::c_void) -> *mut libc::c_void = unsafe { std::mem::transmute(start) };
    let r = unsafe { libc::pthread_create(thread, attr, start, arg as *mut _) };
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
        pthread_kill, pthread_exit, pthread_attr_setschedparam, pthread_attr_getschedparam,
        pthread_setschedparam, pthread_getschedparam, sched_get_priority_max, sched_get_priority_min,
        nanosleep, pause
    );
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
