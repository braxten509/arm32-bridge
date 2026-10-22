//! Mach thread control: thread_suspend, thread_resume and thread_get_state.
//! A thread port is the thread's system id (see mach_thread_self). Like a
//! Linux garbage collector would, a private signal stops the thread inside a
//! handler that saves its registers and waits there until resumed.

use crate::symbols::Registry;
use std::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;

const KERN_SUCCESS: i32 = 0;
const KERN_INVALID_ARGUMENT: i32 = 4;
const KERN_FAILURE: i32 = 5;
const KERN_RESOURCE_SHORTAGE: i32 = 6;

const ARM_THREAD_STATE: i32 = 1;
const ARM_THREAD_STATE64: i32 = 6;
/// x0-x28, fp, lr, sp, pc, then cpsr and padding (in 32-bit words).
const ARM_THREAD_STATE64_COUNT: u32 = 68;
const REGISTERS: usize = 34;

const FUTEX_WAIT_PRIVATE: i32 = 128;
const FUTEX_WAKE_PRIVATE: i32 = 129;

struct Slot {
    /// The thread this slot is for while it is suspended (0: free).
    tid: AtomicI32,
    count: AtomicU32,
    /// 1 while the thread sits in the handler with its registers saved.
    stopped: AtomicU32,
    /// Set to 1 to let the thread go.
    go: AtomicU32,
    registers: [AtomicU64; REGISTERS],
}

const SLOTS: usize = 512;
static TABLE: [Slot; SLOTS] = [const {
    Slot {
        tid: AtomicI32::new(0),
        count: AtomicU32::new(0),
        stopped: AtomicU32::new(0),
        go: AtomicU32::new(0),
        registers: [const { AtomicU64::new(0) }; REGISTERS],
    }
}; SLOTS];
/// One suspend or resume at a time (the handler itself takes no locks).
static LOCK: Mutex<()> = Mutex::new(());

fn find(tid: i32) -> Option<&'static Slot> {
    TABLE.iter().find(|slot| slot.tid.load(Ordering::Acquire) == tid)
}

fn suspend_signal() -> i32 {
    libc::SIGRTMIN() + 9
}

fn wake(word: &AtomicU32) {
    unsafe { libc::syscall(libc::SYS_futex, word.as_ptr(), FUTEX_WAKE_PRIVATE, i32::MAX) };
}

fn wait_while(word: &AtomicU32, value: u32, timeout: Option<&libc::timespec>) {
    let timeout = timeout.map_or(std::ptr::null(), |t| t as *const libc::timespec);
    unsafe { libc::syscall(libc::SYS_futex, word.as_ptr(), FUTEX_WAIT_PRIVATE, value, timeout) };
}

fn alive(tid: i32) -> bool {
    unsafe { libc::syscall(libc::SYS_tgkill, libc::getpid(), tid, 0) == 0 }
}

/// Waits until `word` stops being `value`, or the thread has ended.
fn wait_for_thread(word: &AtomicU32, value: u32, tid: i32) -> bool {
    let poll = libc::timespec { tv_sec: 0, tv_nsec: 50_000_000 };
    while word.load(Ordering::Acquire) == value {
        if !alive(tid) {
            return false;
        }
        wait_while(word, value, Some(&poll));
    }
    true
}

extern "C" fn on_suspend(_signal: i32, _info: *mut libc::siginfo_t, context: *mut libc::c_void) {
    let errno = unsafe { *crate::libsystem::host_errno() };
    let tid = unsafe { libc::gettid() };
    if let Some(slot) = find(tid) {
        let m = unsafe { &(*(context as *const libc::ucontext_t)).uc_mcontext };
        for (index, value) in m.regs.iter().chain([&m.sp, &m.pc, &m.pstate]).enumerate() {
            slot.registers[index].store(*value, Ordering::Relaxed);
        }
        slot.stopped.store(1, Ordering::Release);
        wake(&slot.stopped);
        while slot.go.load(Ordering::Acquire) == 0 {
            wait_while(&slot.go, 0, None);
        }
        slot.stopped.store(0, Ordering::Release);
        wake(&slot.stopped);
    }
    unsafe { *crate::libsystem::host_errno() = errno };
}

fn install_handler() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_suspend as usize;
        action.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(suspend_signal(), &action, std::ptr::null_mut());
    });
}

extern "C" fn thread_suspend(port: u32) -> i32 {
    let tid = port as i32;
    if tid <= 0 || !alive(tid) {
        return KERN_INVALID_ARGUMENT;
    }
    install_handler();
    let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let slot = match find(tid) {
        Some(slot) => slot,
        None => match TABLE.iter().find(|slot| slot.tid.compare_exchange(0, tid, Ordering::AcqRel, Ordering::Relaxed).is_ok()) {
            Some(slot) => slot,
            None => return KERN_RESOURCE_SHORTAGE,
        },
    };
    if slot.count.fetch_add(1, Ordering::AcqRel) > 0 {
        return KERN_SUCCESS;
    }
    slot.go.store(0, Ordering::Release);
    slot.stopped.store(0, Ordering::Release);
    if tid == unsafe { libc::gettid() } {
        // Suspending itself: the handler runs right away and waits for a resume.
        drop(guard);
        unsafe { libc::syscall(libc::SYS_tgkill, libc::getpid(), tid, suspend_signal()) };
        return KERN_SUCCESS;
    }
    let sent = unsafe { libc::syscall(libc::SYS_tgkill, libc::getpid(), tid, suspend_signal()) } == 0;
    if !sent || !wait_for_thread(&slot.stopped, 0, tid) {
        slot.count.store(0, Ordering::Release);
        slot.tid.store(0, Ordering::Release);
        return KERN_INVALID_ARGUMENT;
    }
    KERN_SUCCESS
}

extern "C" fn thread_resume(port: u32) -> i32 {
    let tid = port as i32;
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(slot) = find(tid).filter(|slot| slot.count.load(Ordering::Acquire) > 0) else {
        return KERN_FAILURE;
    };
    if slot.count.fetch_sub(1, Ordering::AcqRel) > 1 {
        return KERN_SUCCESS;
    }
    slot.go.store(1, Ordering::Release);
    wake(&slot.go);
    wait_for_thread(&slot.stopped, 1, tid);
    slot.tid.store(0, Ordering::Release);
    KERN_SUCCESS
}

fn copy_registers(slot: &Slot, out: *mut u32) {
    let words = out as *mut u64;
    for index in 0..33 {
        unsafe { words.add(index).write_unaligned(slot.registers[index].load(Ordering::Relaxed)) };
    }
    unsafe {
        out.add(66).write(slot.registers[33].load(Ordering::Relaxed) as u32);
        out.add(67).write(0);
    }
}

/// Registers of a suspended thread (a running one is stopped for the moment).
extern "C" fn thread_get_state(port: u32, flavor: i32, state: *mut u32, count: *mut u32) -> i32 {
    let tid = port as i32;
    let (needed, out) = match flavor {
        ARM_THREAD_STATE64 => (ARM_THREAD_STATE64_COUNT, state),
        ARM_THREAD_STATE => (ARM_THREAD_STATE64_COUNT + 2, unsafe { state.add(2) }),
        _ => return KERN_INVALID_ARGUMENT,
    };
    if state.is_null() || count.is_null() || unsafe { *count } < needed || tid == unsafe { libc::gettid() } {
        return KERN_INVALID_ARGUMENT;
    }
    let suspended = find(tid).is_some_and(|slot| slot.stopped.load(Ordering::Acquire) == 1);
    if !suspended && thread_suspend(port) != KERN_SUCCESS {
        return KERN_INVALID_ARGUMENT;
    }
    match find(tid) {
        Some(slot) => copy_registers(slot, out),
        None => return KERN_INVALID_ARGUMENT,
    }
    if !suspended {
        thread_resume(port);
    }
    if flavor == ARM_THREAD_STATE {
        unsafe {
            *state = ARM_THREAD_STATE64 as u32;
            *state.add(1) = ARM_THREAD_STATE64_COUNT;
        }
    }
    unsafe { *count = needed };
    KERN_SUCCESS
}

pub fn register(r: &mut Registry) {
    r.function("thread_suspend", thread_suspend as usize);
    r.function("thread_resume", thread_resume as usize);
    r.function("thread_get_state", thread_get_state as usize);
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicI32, AtomicU64, AtomicUsize, Ordering};
    use std::time::Duration;

    fn function(name: &str) -> usize {
        let mut r = crate::symbols::Registry::default();
        crate::libsystem::register(&mut r);
        r.get(name).unwrap().0
    }

    /// Garbage collectors (Mono's Boehm GC) stop each thread with thread_suspend,
    /// read its registers with thread_get_state, then let it go with thread_resume.
    #[test]
    fn suspended_threads_stop_and_show_their_registers() {
        let suspend: extern "C" fn(u32) -> i32 = unsafe { std::mem::transmute(function("thread_suspend")) };
        let resume: extern "C" fn(u32) -> i32 = unsafe { std::mem::transmute(function("thread_resume")) };
        let get_state: extern "C" fn(u32, i32, *mut u64, *mut u32) -> i32 = unsafe { std::mem::transmute(function("thread_get_state")) };
        let mach_thread_self: extern "C" fn() -> u32 = unsafe { std::mem::transmute(function("mach_thread_self")) };

        static COUNTER: AtomicU64 = AtomicU64::new(0);
        static PORT: AtomicI32 = AtomicI32::new(0);
        static STACK: AtomicUsize = AtomicUsize::new(0);
        static STOP: AtomicI32 = AtomicI32::new(0);
        let worker = std::thread::spawn(move || {
            let local = 0u64;
            STACK.store(&local as *const u64 as usize, Ordering::SeqCst);
            PORT.store(mach_thread_self() as i32, Ordering::SeqCst);
            while STOP.load(Ordering::Relaxed) == 0 {
                COUNTER.fetch_add(1, Ordering::Relaxed);
            }
        });
        while PORT.load(Ordering::SeqCst) == 0 {
            std::thread::yield_now();
        }
        let port = PORT.load(Ordering::SeqCst) as u32;
        assert_eq!(resume(port), 5, "resuming a running thread worked");
        assert_eq!(suspend(port), 0, "thread_suspend failed");
        assert_eq!(suspend(port), 0, "a second thread_suspend failed");
        let stopped_at = COUNTER.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(COUNTER.load(Ordering::SeqCst), stopped_at, "the thread kept running while suspended");

        let mut state = [0u64; 34];
        let mut count = 68u32;
        assert_eq!(get_state(port, 6, state.as_mut_ptr(), &mut count), 0, "thread_get_state failed");
        assert_eq!(count, 68);
        let (sp, pc) = (state[31] as usize, state[32]);
        let stack = STACK.load(Ordering::SeqCst);
        assert!(sp <= stack && stack - sp < 64 * 1024, "sp {sp:#x} is not near the thread's stack {stack:#x}");
        assert_ne!(pc, 0);
        let mut small = 10u32;
        assert_eq!(get_state(port, 6, state.as_mut_ptr(), &mut small), 4, "a too-small state buffer was accepted");

        assert_eq!(resume(port), 0);
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(COUNTER.load(Ordering::SeqCst), stopped_at, "one resume undid two suspends");
        assert_eq!(resume(port), 0);
        while COUNTER.load(Ordering::SeqCst) == stopped_at {
            std::thread::yield_now();
        }
        STOP.store(1, Ordering::SeqCst);
        worker.join().unwrap();
    }
}
