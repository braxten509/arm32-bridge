//! pthreads for guest code. Each guest thread is a real host thread with its
//! own CPU engine. The words below are changed with atomic operations (guest
//! threads may run at the same time); a thread that has to wait sleeps in
//! `runtime::wait_until`, which re-checks the word under the waiters' lock.
//!
//! Guest object layouts (bionic ARM32, all 4 bytes):
//! - pthread_mutex_t: bits 14-15 type (as in bionic's static initializers),
//!   bits 16-31 owner thread, bit 13 "someone may be waiting", bits 0-11 lock count.
//! - pthread_cond_t: a counter bumped by every signal/broadcast.
//! - pthread_once_t: 0 not run, 1 running, 2 done.
//! - pthread_rwlock_t (40 bytes, first word used): bits 16-31 writer thread,
//!   bits 0-15 reader count.
//! - pthread_condattr_t: the clock (CLOCK_REALTIME 0 / CLOCK_MONOTONIC 1).
//! - pthread_attr_t (24 bytes): flags (1 = detached), stack base, stack size,
//!   guard size, policy, priority.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::handler;
use crate::runtime::{
    call_guest, drop_engine, engine, hold_gil, mem, shared, stop_with_fault, wait_until, wake_on, wake_on_count, without_gil,
    ArgSource, Handler, Ret, Uc, Wait,
};

const TYPE_MASK: u32 = 0xC000;
const RECURSIVE: u32 = 0x4000;
const COUNT_MASK: u32 = 0x0FFF;
/// Set while a thread may be asleep waiting for the mutex: unlock then wakes one.
const WAITER_BIT: u32 = 0x2000;

const EBUSY: u32 = 16;
const EDEADLK: u32 = 35;
const EPERM: u32 = 1;
const EINVAL: u32 = 22;
const ESRCH: u32 = 3;
const ETIMEDOUT: u32 = 110;
const EAGAIN: u32 = 11;
const EINTR: u32 = 4;

const FIRST_HANDLE: u32 = 0x1000;

#[derive(Default)]
pub struct Threads {
    /// pthread_t → (finished, return value, detached)
    records: HashMap<u32, (bool, u32, bool)>,
    /// Condition variables whose timed waits use CLOCK_MONOTONIC deadlines.
    monotonic_conditions: std::collections::HashSet<u32>,
}

/// Most thread-specific data keys a program can make (bionic allows 128).
const KEYS: usize = 1024;

/// One guest thread (a pthread_t): its signals and thread-specific data. Made
/// when the handle is handed out, so a signal can be sent before the thread runs.
pub struct Identity {
    pub handle: u32,
    /// Signals sent and not delivered yet / blocked (bit n-1 = signal n).
    pub pending: AtomicU64,
    pub blocked: AtomicU64,
    /// Handlers run so far (sigsuspend waits for this to change).
    pub delivered: AtomicU64,
    /// Set once the thread has finished; signals to it then fail with ESRCH.
    pub dead: AtomicBool,
    /// Set once the thread is finishing: no new helper may stand in for it.
    pub exiting: AtomicBool,
    /// pthread_getspecific / pthread_setspecific values by key.
    values: Box<[AtomicU32]>,
    /// The host thread running this guest thread's code, told when a signal comes.
    pub runner: Mutex<Option<Arc<crate::runtime::Runner>>>,
    /// The signal helper standing in for this thread while it is stuck in a
    /// phone function (its number, 0 = none), and that helper's runner.
    pub borrower: AtomicU32,
    pub helper_runner: Mutex<Option<Arc<crate::runtime::Runner>>>,
}

impl Identity {
    pub(crate) fn new(handle: u32) -> Identity {
        Identity {
            handle,
            pending: AtomicU64::new(0),
            blocked: AtomicU64::new(0),
            delivered: AtomicU64::new(0),
            dead: AtomicBool::new(false),
            exiting: AtomicBool::new(false),
            values: (0..KEYS).map(|_| AtomicU32::new(0)).collect(),
            runner: Mutex::new(None),
            borrower: AtomicU32::new(0),
            helper_runner: Mutex::new(None),
        }
    }
}

/// Every guest thread by pthread_t (finished ones stay, marked dead: handles are never reused).
static IDENTITIES: Mutex<Option<HashMap<u32, Arc<Identity>>>> = Mutex::new(None);
static NEXT_HANDLE: AtomicU32 = AtomicU32::new(FIRST_HANDLE);

/// Thread-specific data keys: bit 63 = in use, low 32 bits = destructor.
static KEY_SLOTS: [AtomicU64; KEYS] = [const { AtomicU64::new(0) }; KEYS];
static NEXT_KEY: AtomicU32 = AtomicU32::new(0);
const KEY_LIVE: u64 = 1 << 63;

thread_local! {
    static SELF: RefCell<Option<Arc<Identity>>> = const { RefCell::new(None) };
    /// Set by pthread_exit: the value the thread returns once its guest calls unwind.
    static EXIT_VALUE: Cell<Option<u32>> = const { Cell::new(None) };
}

const THREAD_EXIT: &str = "guest thread called pthread_exit";

/// This thread's guest identity, handing one out on first use.
pub fn current_identity() -> Arc<Identity> {
    SELF.with(|me| Arc::clone(me.borrow_mut().get_or_insert_with(new_identity)))
}

/// A signal helper takes on (Some) or gives back (None) the identity of the
/// thread it stands in for.
pub fn adopt_identity(identity: Option<Arc<Identity>>) {
    SELF.with(|me| *me.borrow_mut() = identity);
}

/// This thread's pthread_t, handing one out on first use.
pub fn current_handle() -> u32 {
    current_identity().handle
}

/// The live thread with handle `handle`.
pub fn identity(handle: u32) -> Option<Arc<Identity>> {
    let identities = IDENTITIES.lock().unwrap_or_else(|e| e.into_inner());
    identities.as_ref()?.get(&handle).filter(|found| !found.dead.load(Ordering::Acquire)).cloned()
}

/// Whether `handle` is a thread that exists (and hasn't finished).
pub fn is_live(handle: u32) -> bool {
    identity(handle).is_some()
}

fn new_identity() -> Arc<Identity> {
    let handle = NEXT_HANDLE.fetch_add(4, Ordering::Relaxed) + 4;
    let made = Arc::new(Identity::new(handle));
    IDENTITIES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(handle, Arc::clone(&made));
    made
}

/// Marks this thread finished: from now on signals to it fail.
fn retire_identity(identity: &Identity) {
    let _identities = IDENTITIES.lock().unwrap_or_else(|e| e.into_inner());
    identity.dead.store(true, Ordering::Release);
}

fn join_key(handle: u32) -> u64 {
    1 << 32 | handle as u64
}

fn owner_of(value: u32) -> u32 {
    value >> 16
}

/// Locks a guest mutex, waiting while another thread owns it. `count` is the
/// recursion depth to restore (1 normally). A thread that had to wait takes
/// the mutex with the waiter bit set, since others may still be asleep on it.
fn lock(uc: &Uc, mutex: u32, count: u32, try_only: bool) -> u32 {
    let me = uc.get_data().id;
    let word = mem().atomic_u32(mutex);
    let mut waited = 0;
    loop {
        let value = word.load(Ordering::Relaxed);
        let kind = value & TYPE_MASK;
        let owner = owner_of(value);
        if owner == 0 {
            let taken = kind | (value & WAITER_BIT) | waited | (me << 16) | count;
            if word.compare_exchange_weak(value, taken, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                return 0;
            }
            continue;
        }
        if owner == me {
            if kind != RECURSIVE {
                return if try_only { EBUSY } else { EDEADLK };
            }
            let deeper = (value & !COUNT_MASK) | ((value & COUNT_MASK) + 1);
            if word.compare_exchange_weak(value, deeper, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
                return 0;
            }
            continue;
        }
        if try_only {
            return EBUSY;
        }
        let marked = value | WAITER_BIT;
        if value != marked && word.compare_exchange(value, marked, Ordering::Relaxed, Ordering::Relaxed).is_err() {
            continue;
        }
        waited = WAITER_BIT;
        wait_until(mutex as u64, None, || word.load(Ordering::Relaxed) == marked);
    }
}

/// Unlocks fully when `all` (for condition waits); returns the old depth.
fn unlock(uc: &Uc, mutex: u32, all: bool) -> Result<u32, u32> {
    let me = uc.get_data().id;
    let word = mem().atomic_u32(mutex);
    loop {
        let value = word.load(Ordering::Relaxed);
        if owner_of(value) != me {
            return Err(EPERM);
        }
        let depth = value & COUNT_MASK;
        if all || depth <= 1 {
            if word.compare_exchange_weak(value, value & TYPE_MASK, Ordering::Release, Ordering::Relaxed).is_ok() {
                if value & WAITER_BIT != 0 {
                    wake_on_count(mutex as u64, 1);
                }
                return Ok(depth.max(1));
            }
        } else if word.compare_exchange_weak(value, (value & !COUNT_MASK) | (depth - 1), Ordering::Relaxed, Ordering::Relaxed).is_ok() {
            return Ok(depth);
        }
    }
}

/// A wall-clock deadline as an `Instant` (None once it has passed).
fn instant(deadline: SystemTime) -> Option<std::time::Instant> {
    deadline.duration_since(SystemTime::now()).ok().map(|left| std::time::Instant::now() + left)
}

/// Waits on a guest condition variable until signalled or `deadline` passes.
/// The counter is read before the mutex is let go, so a signal sent after
/// that is never missed.
fn condition_wait(uc: &Uc, condition: u32, mutex: u32, deadline: Option<SystemTime>) -> u32 {
    let word = mem().atomic_u32(condition);
    let start = word.load(Ordering::Acquire);
    let depth = match unlock(uc, mutex, true) {
        Ok(depth) => depth,
        Err(error) => return error,
    };
    let mut result = 0;
    loop {
        let until = match deadline {
            Some(deadline) => match instant(deadline) {
                Some(until) => Some(until),
                None => {
                    result = ETIMEDOUT;
                    break;
                }
            },
            None => None,
        };
        match wait_until(condition as u64, until, || word.load(Ordering::Acquire) == start) {
            Wait::Woken | Wait::NotWaiting => break,
            // A signal handler ran: waits may end early (the caller re-checks).
            Wait::Interrupted => break,
            Wait::TimedOut => {}
        }
    }
    lock(uc, mutex, depth, false);
    result
}

/// Takes one from a semaphore. `wait`: None = don't wait, Some(None) = wait
/// as long as needed, Some(Some(deadline)) = wait until the deadline.
fn semaphore_wait(uc: &Uc, semaphore: u32, wait: Option<Option<SystemTime>>) -> Ret {
    let word = mem().atomic_u32(semaphore);
    loop {
        let count = word.load(Ordering::Relaxed);
        if count > 0 {
            if word.compare_exchange_weak(count, count - 1, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                return Ret::U32(0);
            }
            continue;
        }
        let until = match wait {
            None => return fail_with(uc, EAGAIN),
            Some(None) => None,
            Some(Some(deadline)) => match instant(deadline) {
                Some(until) => Some(until),
                None => return fail_with(uc, ETIMEDOUT),
            },
        };
        wait_until(semaphore as u64, until, || word.load(Ordering::Relaxed) == 0);
    }
}

/// Returns -1 with the guest's errno set to `error`.
fn fail_with(uc: &Uc, error: u32) -> Ret {
    mem().write_u32(uc.get_data().errno_slot, error);
    Ret::U32(u32::MAX)
}

fn guest_time(pointer: u32) -> SystemTime {
    let seconds = mem().read_u32(pointer) as i32 as i64;
    let nanoseconds = mem().read_u32(pointer + 4) as i32 as i64;
    UNIX_EPOCH + Duration::from_secs(seconds.max(0) as u64) + Duration::from_nanos(nanoseconds.max(0) as u64)
}

/// A CLOCK_MONOTONIC deadline as a wall-clock time.
fn monotonic_deadline(pointer: u32) -> SystemTime {
    let wanted = Duration::new(mem().read_u32(pointer) as i32 as u64, (mem().read_u32(pointer + 4) as i32).max(0) as u32);
    let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    let now = Duration::new(now.tv_sec as u64, now.tv_nsec as u32);
    SystemTime::now() + wanted.saturating_sub(now)
}

const RWLOCK_READERS: u32 = 0xFFFF;

/// A Linux system call by its ARM EABI number, from libc's syscall() or a
/// raw `svc` in game code. Returns the kernel-style result (negative errno).
pub fn linux_syscall(uc: &mut Uc, number: u32, arguments: [u32; 6]) -> i64 {
    let [first, second, third, fourth, _, _] = arguments;
    match number {
        240 => futex(uc, first, second, third, fourth) as i64,
        224 => (unsafe { libc::gettid() }) as i64,
        20 => (unsafe { libc::getpid() }) as i64,
        158 => {
            without_gil(|| std::thread::yield_now());
            0
        }
        // sched_setaffinity: the guest's threads are the bridge's; leave their CPUs alone.
        241 => 0,
        // clock_gettime(clock, struct timespec *) with 32-bit fields.
        263 => {
            let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
            if unsafe { libc::clock_gettime(first as i32, &mut now) } != 0 {
                return -(EINVAL as i64);
            }
            mem().write_u32(second, now.tv_sec as u32);
            mem().write_u32(second + 4, now.tv_nsec as u32);
            0
        }
        // getrandom(buffer, length, flags)
        384 => {
            let host = crate::imports::gles::guest_pointer(first);
            let got = unsafe { libc::getrandom(host, second as usize, third) };
            if got < 0 { -(EAGAIN as i64) } else { got as i64 }
        }
        // ARM cacheflush(start, end, flags): code was written there (a JIT).
        0x000f_0002 => {
            // Unicorn engines see the change before translated code is dropped
            // (both happen before this thread or any other runs it again).
            crate::runtime::note_code_change(first, second);
            crate::dynarec::invalidate(uc, first, second);
            0
        }
        _ => {
            crate::log::line(&format!("[bridge] syscall {number} not handled; returning ENOSYS"));
            -38
        }
    }
}

const FUTEX_WAIT: u32 = 0;
const FUTEX_WAKE: u32 = 1;
const FUTEX_WAIT_BITSET: u32 = 9;
const FUTEX_WAKE_BITSET: u32 = 10;
const FUTEX_PRIVATE: u32 = 128;
const FUTEX_CLOCK_REALTIME: u32 = 256;

/// futex(2) on a guest word. Returns the syscall result (negative errno on failure).
fn futex(uc: &Uc, address: u32, operation: u32, value: u32, timeout: u32) -> i32 {
    let _ = uc;
    let command = operation & !(FUTEX_PRIVATE | FUTEX_CLOCK_REALTIME);
    match command {
        FUTEX_WAIT | FUTEX_WAIT_BITSET => {
            let deadline = (timeout != 0).then(|| {
                if command == FUTEX_WAIT {
                    let relative = Duration::new(
                        mem().read_u32(timeout) as i32 as u64,
                        (mem().read_u32(timeout + 4) as i32).max(0) as u32,
                    );
                    std::time::Instant::now() + relative
                } else {
                    let deadline = if operation & FUTEX_CLOCK_REALTIME != 0 { guest_time(timeout) } else { monotonic_deadline(timeout) };
                    instant(deadline).unwrap_or_else(std::time::Instant::now)
                }
            });
            let word = mem().atomic_u32(address);
            match wait_until(address as u64, deadline, || word.load(Ordering::Acquire) == value) {
                Wait::Woken => 0,
                Wait::NotWaiting => -(EAGAIN as i32),
                Wait::TimedOut => -(ETIMEDOUT as i32),
                Wait::Interrupted => -(EINTR as i32),
            }
        }
        // Wakes up to `value` waiters and says how many it woke.
        FUTEX_WAKE | FUTEX_WAKE_BITSET => wake_on_count(address as u64, value) as i32,
        _ => {
            crate::log::line(&format!("[bridge] futex operation {operation} not handled"));
            -38 // ENOSYS
        }
    }
}

/// Takes a guest read-write lock for reading or writing.
fn rwlock_lock(uc: &Uc, lock: u32, write: bool, try_only: bool) -> u32 {
    let me = uc.get_data().id;
    let word = mem().atomic_u32(lock);
    loop {
        let value = word.load(Ordering::Relaxed);
        let (writer, readers) = (value >> 16, value & RWLOCK_READERS);
        if writer == me {
            return if try_only { EBUSY } else { EDEADLK };
        }
        let taken = if write && value == 0 {
            Some(me << 16)
        } else if !write && writer == 0 && readers < RWLOCK_READERS {
            Some(readers + 1)
        } else {
            None
        };
        if let Some(taken) = taken {
            if word.compare_exchange_weak(value, taken, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                return 0;
            }
            continue;
        }
        if try_only {
            return EBUSY;
        }
        wait_until(lock as u64, None, || word.load(Ordering::Relaxed) == value);
    }
}

/// Size of the guest stack of thread `id` (see runtime::engine).
fn stack_size(id: u32) -> u32 {
    if id == 1 {
        crate::runtime::MAIN_STACK_SIZE
    } else {
        crate::runtime::THREAD_STACK_SIZE
    }
}

/// Fills a guest pthread_attr_t.
fn write_attributes(attributes: u32, flags: u32, stack_base: u32, stack_size: u32) {
    let values = [flags, stack_base, stack_size, 4096, 0, 0];
    for (i, value) in values.iter().enumerate() {
        mem().write_u32(attributes + i as u32 * 4, *value);
    }
}

/// Body of a guest thread started with pthread_create.
fn run_thread(me: Arc<Identity>, start: u32, argument: u32) {
    let handle = me.handle;
    SELF.with(|slot| *slot.borrow_mut() = Some(Arc::clone(&me)));
    let _lock = hold_gil();
    let uc = engine();
    let result = match call_guest(uc, start, &[argument]) {
        Ok(value) => value as u32,
        Err(_) if EXIT_VALUE.with(|v| v.get()).is_some() => {
            uc.get_data_mut().fault = None;
            EXIT_VALUE.with(|v| v.take()).unwrap_or(0)
        }
        Err(error) => {
            crate::log::line(&format!("[bridge] guest thread {handle:#x} stopped: {error}"));
            0
        }
    };
    run_key_destructors(uc, &me);
    me.exiting.store(true, Ordering::SeqCst);
    crate::runtime::wait_for_borrow_end(&me);
    retire_identity(&me);
    {
        let mut shared = shared();
        let records = &mut shared.state.threads.records;
        let detached = records.get(&handle).map(|r| r.2).unwrap_or(false);
        if detached {
            records.remove(&handle);
        } else {
            records.insert(handle, (true, result, false));
        }
    }
    wake_on(join_key(handle));
    crate::imports::jni_host::detach_current_thread();
    drop_engine();
}

/// Calls the destructors of this thread's non-null thread-specific values
/// (again while a destructor sets new ones, up to 4 rounds, as POSIX allows).
fn run_key_destructors(uc: &mut Uc, me: &Identity) {
    for _ in 0..4 {
        let mut ran = false;
        for key in 0..(NEXT_KEY.load(Ordering::Acquire) as usize).min(KEYS) {
            let slot = KEY_SLOTS[key].load(Ordering::Acquire);
            let destructor = slot as u32;
            if slot & KEY_LIVE == 0 || destructor == 0 {
                continue;
            }
            let value = me.values[key].swap(0, Ordering::Relaxed);
            if value != 0 {
                let _ = call_guest(uc, destructor, &[value]);
                ran = true;
            }
        }
        if !ran {
            break;
        }
    }
}

fn guest_sleep(duration: Duration) {
    let deadline = std::time::Instant::now() + duration;
    loop {
        let finished = without_gil(|| {
            let mut slept_once = false;
            loop {
                let now = std::time::Instant::now();
                if now >= deadline {
                    return true;
                }
                if slept_once && crate::imports::signals::any_pending() {
                    return false;
                }
                std::thread::sleep((deadline - now).min(Duration::from_millis(1)));
                slept_once = true;
            }
        });
        if finished {
            return;
        }
    }
}

pub fn handler(name: &str) -> Option<Handler> {
    match name {
        "pthread_mutex_init" => handler!(|uc, a| {
            let (mutex, attributes) = (a.u32(), a.u32());
            let kind = if attributes == 0 { 0 } else { mem().read_u32(attributes) & 3 };
            mem().write_u32(mutex, kind << 14);
            Ret::U32(0)
        }),
        "pthread_mutex_destroy" => handler!(|uc, a| Ret::U32(0)),
        "pthread_mutex_lock" => handler!(|uc, a| {
            let mutex = a.u32();
            Ret::U32(lock(uc, mutex, 1, false))
        }),
        "pthread_mutex_trylock" => handler!(|uc, a| {
            let mutex = a.u32();
            Ret::U32(lock(uc, mutex, 1, true))
        }),
        "pthread_mutex_unlock" => handler!(|uc, a| {
            let mutex = a.u32();
            match unlock(uc, mutex, false) {
                Ok(_) => Ret::U32(0),
                Err(error) => Ret::U32(error),
            }
        }),
        "pthread_mutexattr_init" => handler!(|uc, a| {
            mem().write_u32(a.u32(), 0);
            Ret::U32(0)
        }),
        "pthread_mutexattr_destroy" => handler!(|uc, a| Ret::U32(0)),
        "pthread_mutexattr_settype" => handler!(|uc, a| {
            let (attributes, kind) = (a.u32(), a.u32());
            if kind > 2 {
                return Ret::U32(EINVAL);
            }
            mem().write_u32(attributes, kind);
            Ret::U32(0)
        }),
        "pthread_cond_init" => handler!(|uc, a| {
            let (condition, attributes) = (a.u32(), a.u32());
            mem().write_u32(condition, 0);
            let monotonic = attributes != 0 && mem().read_u32(attributes) == 1;
            let conditions = &mut shared().state.threads.monotonic_conditions;
            if monotonic {
                conditions.insert(condition);
            } else {
                conditions.remove(&condition);
            }
            Ret::U32(0)
        }),
        "pthread_cond_destroy" => handler!(|uc, a| Ret::U32(0)),
        "pthread_cond_signal" => handler!(|uc, a| {
            let condition = a.u32();
            mem().atomic_u32(condition).fetch_add(1, Ordering::Release);
            wake_on_count(condition as u64, 1);
            Ret::U32(0)
        }),
        "pthread_cond_broadcast" => handler!(|uc, a| {
            let condition = a.u32();
            mem().atomic_u32(condition).fetch_add(1, Ordering::Release);
            wake_on(condition as u64);
            Ret::U32(0)
        }),
        "pthread_cond_wait" => handler!(|uc, a| {
            let (condition, mutex) = (a.u32(), a.u32());
            Ret::U32(condition_wait(uc, condition, mutex, None))
        }),
        "pthread_cond_timedwait" => handler!(|uc, a| {
            let (condition, mutex, deadline) = (a.u32(), a.u32(), a.u32());
            let monotonic = shared().state.threads.monotonic_conditions.contains(&condition);
            let deadline = if monotonic { monotonic_deadline(deadline) } else { guest_time(deadline) };
            Ret::U32(condition_wait(uc, condition, mutex, Some(deadline)))
        }),
        // Raw system calls through libc's syscall(): -1 and errno on failure.
        "syscall" => handler!(|uc, a| {
            let number = a.u32();
            let arguments = [a.u32(), a.u32(), a.u32(), a.u32(), a.u32(), a.u32()];
            let result = linux_syscall(uc, number, arguments);
            if result < 0 {
                fail_with(uc, (-result) as u32)
            } else {
                Ret::U32(result as u32)
            }
        }),
        "pthread_condattr_init" => handler!(|uc, a| {
            mem().write_u32(a.u32(), 0);
            Ret::U32(0)
        }),
        "pthread_condattr_destroy" => handler!(|uc, a| Ret::U32(0)),
        "pthread_condattr_setclock" => handler!(|uc, a| {
            let (attributes, clock) = (a.u32(), a.u32());
            if clock > 1 {
                return Ret::U32(EINVAL);
            }
            mem().write_u32(attributes, clock);
            Ret::U32(0)
        }),
        "pthread_rwlock_init" => handler!(|uc, a| {
            mem().write_u32(a.u32(), 0);
            Ret::U32(0)
        }),
        "pthread_rwlock_destroy" => handler!(|uc, a| Ret::U32(0)),
        "pthread_rwlock_rdlock" => handler!(|uc, a| Ret::U32(rwlock_lock(uc, a.u32(), false, false))),
        "pthread_rwlock_tryrdlock" => handler!(|uc, a| Ret::U32(rwlock_lock(uc, a.u32(), false, true))),
        "pthread_rwlock_wrlock" => handler!(|uc, a| Ret::U32(rwlock_lock(uc, a.u32(), true, false))),
        "pthread_rwlock_trywrlock" => handler!(|uc, a| Ret::U32(rwlock_lock(uc, a.u32(), true, true))),
        "pthread_rwlock_unlock" => handler!(|uc, a| {
            let lock = a.u32();
            let word = mem().atomic_u32(lock);
            loop {
                let value = word.load(Ordering::Relaxed);
                let next = if value >> 16 == uc.get_data().id {
                    0
                } else if value & RWLOCK_READERS != 0 && value >> 16 == 0 {
                    value - 1
                } else {
                    return Ret::U32(EPERM);
                };
                if word.compare_exchange_weak(value, next, Ordering::Release, Ordering::Relaxed).is_ok() {
                    break;
                }
            }
            wake_on(lock as u64);
            Ret::U32(0)
        }),
        "pthread_attr_init" => handler!(|uc, a| {
            write_attributes(a.u32(), 0, 0, crate::runtime::THREAD_STACK_SIZE);
            Ret::U32(0)
        }),
        "pthread_attr_destroy" => handler!(|uc, a| Ret::U32(0)),
        "pthread_attr_setdetachstate" => handler!(|uc, a| {
            let (attributes, detached) = (a.u32(), a.u32());
            if detached > 1 {
                return Ret::U32(EINVAL);
            }
            mem().write_u32(attributes, (mem().read_u32(attributes) & !1) | detached);
            Ret::U32(0)
        }),
        "pthread_attr_getdetachstate" => handler!(|uc, a| {
            let (attributes, out) = (a.u32(), a.u32());
            mem().write_u32(out, mem().read_u32(attributes) & 1);
            Ret::U32(0)
        }),
        // Guest threads get the bridge's fixed stack size whatever is asked.
        "pthread_attr_setstacksize" => handler!(|uc, a| {
            let (attributes, size) = (a.u32(), a.u32());
            mem().write_u32(attributes + 8, size);
            Ret::U32(0)
        }),
        "pthread_attr_getstacksize" => handler!(|uc, a| {
            let (attributes, out) = (a.u32(), a.u32());
            mem().write_u32(out, mem().read_u32(attributes + 8));
            Ret::U32(0)
        }),
        "pthread_attr_getstack" => handler!(|uc, a| {
            let (attributes, base_out, size_out) = (a.u32(), a.u32(), a.u32());
            mem().write_u32(base_out, mem().read_u32(attributes + 4));
            mem().write_u32(size_out, mem().read_u32(attributes + 8));
            Ret::U32(0)
        }),
        // The calling thread's real guest stack (garbage collectors scan it).
        "pthread_getattr_np" => handler!(|uc, a| {
            let (thread, attributes) = (a.u32(), a.u32());
            if thread != current_handle() {
                write_attributes(attributes, 0, 0, crate::runtime::THREAD_STACK_SIZE);
                return Ret::U32(0);
            }
            let data = uc.get_data();
            let size = stack_size(data.id);
            write_attributes(attributes, 0, data.stack_top - size, size);
            Ret::U32(0)
        }),
        "pthread_getschedparam" => handler!(|uc, a| {
            let (_thread, policy, parameters) = (a.u32(), a.u32(), a.u32());
            mem().write_u32(policy, 0);
            mem().write_u32(parameters, 0);
            Ret::U32(0)
        }),
        "pthread_setschedparam" => handler!(|uc, a| Ret::U32(0)),
        "pthread_setname_np" => handler!(|uc, a| Ret::U32(0)),
        // SCHED_OTHER (0) has only priority 0; SCHED_FIFO (1) and SCHED_RR (2) 1-99.
        "sched_get_priority_max" => handler!(|uc, a| Ret::U32(if matches!(a.u32(), 1 | 2) { 99 } else { 0 })),
        "sched_get_priority_min" => handler!(|uc, a| Ret::U32(if matches!(a.u32(), 1 | 2) { 1 } else { 0 })),
        "sched_getcpu" => handler!(|uc, a| Ret::U32(unsafe { libc::sched_getcpu() } as u32)),
        "sched_getaffinity" => handler!(|uc, a| {
            let (_pid, size, mask) = (a.u32(), a.u32(), a.u32());
            let result = unsafe { libc::sched_getaffinity(0, size as usize, crate::imports::gles::guest_pointer(mask) as *mut libc::cpu_set_t) };
            if result < 0 {
                return fail_with(uc, EINVAL);
            }
            Ret::U32(0)
        }),
        // The guest's threads are the bridge's; leave their CPUs alone.
        "sched_setaffinity" => handler!(|uc, a| Ret::U32(0)),
        // Semaphores: the first word of the guest sem_t is the count.
        "sem_init" => handler!(|uc, a| {
            let (semaphore, _shared, value) = (a.u32(), a.u32(), a.u32());
            mem().write_u32(semaphore, value);
            Ret::U32(0)
        }),
        "sem_destroy" => handler!(|uc, a| Ret::U32(0)),
        "sem_post" => handler!(|uc, a| {
            let semaphore = a.u32();
            mem().atomic_u32(semaphore).fetch_add(1, Ordering::Release);
            wake_on_count(semaphore as u64, 1);
            Ret::U32(0)
        }),
        "sem_getvalue" => handler!(|uc, a| {
            let (semaphore, out) = (a.u32(), a.u32());
            mem().write_u32(out, mem().atomic_u32(semaphore).load(Ordering::Acquire));
            Ret::U32(0)
        }),
        "sem_wait" => handler!(|uc, a| {
            let semaphore = a.u32();
            semaphore_wait(uc, semaphore, Some(None))
        }),
        "sem_trywait" => handler!(|uc, a| {
            let semaphore = a.u32();
            semaphore_wait(uc, semaphore, None)
        }),
        "sem_timedwait" => handler!(|uc, a| {
            let (semaphore, deadline) = (a.u32(), a.u32());
            semaphore_wait(uc, semaphore, Some(Some(guest_time(deadline))))
        }),
        "pthread_self" => handler!(|uc, a| Ret::U32(uc.get_data().identity.handle)),
        "pthread_exit" => handler!(|uc, a| {
            EXIT_VALUE.with(|v| v.set(Some(a.u32())));
            stop_with_fault(uc, THREAD_EXIT.into());
            Ret::Void
        }),
        "pthread_equal" => handler!(|uc, a| {
            let (x, y) = (a.u32(), a.u32());
            Ret::U32((x == y) as u32)
        }),
        "pthread_create" => handler!(|uc, a| {
            let (out, attributes, start, argument) = (a.u32(), a.u32(), a.u32(), a.u32());
            let identity = new_identity();
            let handle = identity.handle;
            let detached = attributes != 0 && mem().read_u32(attributes) & 1 != 0;
            shared().state.threads.records.insert(handle, (false, 0, detached));
            mem().write_u32(out, handle);
            let spawned = std::thread::Builder::new()
                .name(format!("arm32-guest-{handle:x}"))
                .spawn({
                    let identity = Arc::clone(&identity);
                    move || run_thread(identity, start, argument)
                });
            match spawned {
                Ok(_) => Ret::U32(0),
                Err(_) => {
                    shared().state.threads.records.remove(&handle);
                    retire_identity(&identity);
                    Ret::U32(11)
                }
            }
        }),
        "pthread_detach" => handler!(|uc, a| {
            let handle = a.u32();
            let mut shared = shared();
            let records = &mut shared.state.threads.records;
            match records.get(&handle).copied() {
                Some((true, _, _)) => {
                    records.remove(&handle);
                    Ret::U32(0)
                }
                Some((false, value, _)) => {
                    records.insert(handle, (false, value, true));
                    Ret::U32(0)
                }
                None => Ret::U32(ESRCH),
            }
        }),
        "pthread_join" => handler!(|uc, a| {
            let (handle, out) = (a.u32(), a.u32());
            loop {
                let record = shared().state.threads.records.get(&handle).copied();
                match record {
                    None => return Ret::U32(ESRCH),
                    Some((true, value, _)) => {
                        shared().state.threads.records.remove(&handle);
                        if out != 0 {
                            mem().write_u32(out, value);
                        }
                        return Ret::U32(0);
                    }
                    Some(_) => {
                        wait_until(join_key(handle), None, || {
                            !matches!(shared().state.threads.records.get(&handle), Some((true, _, _)))
                        });
                    }
                }
            }
        }),
        "pthread_once" => handler!(|uc, a| {
            let (control, function) = (a.u32(), a.u32());
            let word = mem().atomic_u32(control);
            loop {
                match word.load(Ordering::Acquire) {
                    0 => {
                        if word.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_err() {
                            continue;
                        }
                        if let Err(error) = call_guest(uc, function, &[]) {
                            stop_with_fault(uc, error);
                        }
                        word.store(2, Ordering::Release);
                        wake_on(control as u64);
                        return Ret::U32(0);
                    }
                    1 => {
                        wait_until(control as u64, None, || word.load(Ordering::Acquire) == 1);
                    }
                    _ => return Ret::U32(0),
                }
            }
        }),
        // Keys are never reused, as before; values live with each thread.
        "pthread_key_create" => handler!(|uc, a| {
            let (out, destructor) = (a.u32(), a.u32());
            let key = NEXT_KEY.fetch_add(1, Ordering::AcqRel);
            if key as usize >= KEYS {
                return Ret::U32(EAGAIN);
            }
            KEY_SLOTS[key as usize].store(KEY_LIVE | destructor as u64, Ordering::Release);
            mem().write_u32(out, key);
            Ret::U32(0)
        }),
        "pthread_key_delete" => handler!(|uc, a| {
            let key = a.u32() as usize;
            if key >= KEYS || KEY_SLOTS[key].swap(0, Ordering::AcqRel) & KEY_LIVE == 0 {
                return Ret::U32(EINVAL);
            }
            Ret::U32(0)
        }),
        "pthread_getspecific" => handler!(|uc, a| {
            let key = a.u32() as usize;
            Ret::U32(uc.get_data().identity.values.get(key).map_or(0, |value| value.load(Ordering::Relaxed)))
        }),
        "pthread_setspecific" => handler!(|uc, a| {
            let (key, value) = (a.u32() as usize, a.u32());
            if key >= KEYS || KEY_SLOTS[key].load(Ordering::Acquire) & KEY_LIVE == 0 {
                return Ret::U32(EINVAL);
            }
            uc.get_data().identity.values[key].store(value, Ordering::Relaxed);
            Ret::U32(0)
        }),
        "sched_yield" => handler!(|uc, a| {
            without_gil(std::thread::yield_now);
            Ret::U32(0)
        }),
        "nanosleep" => handler!(|uc, a| {
            let request = a.u32();
            let seconds = mem().read_u32(request) as i32 as i64;
            let nanoseconds = mem().read_u32(request + 4) as i32 as i64;
            let wait = Duration::from_secs(seconds.max(0) as u64) + Duration::from_nanos(nanoseconds.max(0) as u64);
            guest_sleep(wait);
            Ret::U32(0)
        }),
        "clock_nanosleep" => handler!(|uc, a| {
            let (clock, flags, request) = (a.i32(), a.u32(), a.u32());
            let seconds = mem().read_u32(request) as i32 as i64;
            let nanoseconds = mem().read_u32(request + 4) as i32 as i64;
            let mut wait = Duration::from_secs(seconds.max(0) as u64) + Duration::from_nanos(nanoseconds.max(0) as u64);
            if flags & 1 != 0 {
                let mut now: libc::timespec = unsafe { std::mem::zeroed() };
                unsafe { libc::clock_gettime(clock, &mut now) };
                let now = Duration::from_secs(now.tv_sec.max(0) as u64) + Duration::from_nanos(now.tv_nsec.max(0) as u64);
                wait = wait.saturating_sub(now);
            }
            guest_sleep(wait);
            Ret::U32(0)
        }),
        "usleep" => handler!(|uc, a| {
            let microseconds = a.u32();
            guest_sleep(Duration::from_micros(microseconds as u64));
            Ret::U32(0)
        }),
        "sleep" => handler!(|uc, a| {
            let seconds = a.u32();
            guest_sleep(Duration::from_secs(seconds as u64));
            Ret::U32(0)
        }),
        _ => None,
    }
}
