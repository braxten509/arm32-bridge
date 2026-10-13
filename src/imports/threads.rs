//! pthreads for guest code. Each guest thread is a real host thread with its
//! own CPU engine. Guest code only runs while holding the guest lock, so the
//! mutex and condition variable words below are read and written without
//! races; waiting gives the lock up (`wait_for_guests`).
//!
//! Guest object layouts (bionic ARM32, all 4 bytes):
//! - pthread_mutex_t: bits 14-15 type (as in bionic's static initializers),
//!   bits 16-31 owner thread, bits 0-11 lock count.
//! - pthread_cond_t: a counter bumped by every signal/broadcast.
//! - pthread_once_t: 0 not run, 1 running, 2 done.
//! - pthread_rwlock_t (40 bytes, first word used): bits 16-31 writer thread,
//!   bits 0-15 reader count.
//! - pthread_condattr_t: the clock (CLOCK_REALTIME 0 / CLOCK_MONOTONIC 1).
//! - pthread_attr_t (24 bytes): flags (1 = detached), stack base, stack size,
//!   guard size, policy, priority.

use std::cell::Cell;
use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::handler;
use crate::runtime::{
    call_guest, drop_engine, engine, hold_gil, mem, shared, stop_with_fault, wait_on, wake_on, without_gil, ArgSource,
    Handler, Ret, Uc,
};

const TYPE_MASK: u32 = 0xC000;
const RECURSIVE: u32 = 0x4000;
const COUNT_MASK: u32 = 0x0FFF;

const EBUSY: u32 = 16;
const EDEADLK: u32 = 35;
const EPERM: u32 = 1;
const EINVAL: u32 = 22;
const ESRCH: u32 = 3;
const ETIMEDOUT: u32 = 110;
const EAGAIN: u32 = 11;

const FIRST_HANDLE: u32 = 0x1000;

#[derive(Default)]
pub struct Threads {
    next_handle: u32,
    /// pthread_t → (finished, return value, detached)
    records: HashMap<u32, (bool, u32, bool)>,
    /// Thread-specific data: key → live, and (thread, key) → value.
    keys: Vec<Option<u32>>,
    /// Futex words: address -> wake count (a waiter returns once it changes).
    futex_wakes: HashMap<u32, u32>,
    /// Condition variables whose timed waits use CLOCK_MONOTONIC deadlines.
    monotonic_conditions: std::collections::HashSet<u32>,
    values: HashMap<(u32, u32), u32>,
}

thread_local! {
    static SELF_HANDLE: Cell<u32> = const { Cell::new(0) };
    /// Set by pthread_exit: the value the thread returns once its guest calls unwind.
    static EXIT_VALUE: Cell<Option<u32>> = const { Cell::new(None) };
}

const THREAD_EXIT: &str = "guest thread called pthread_exit";

/// This thread's pthread_t, handing one out on first use.
pub fn current_handle() -> u32 {
    let existing = SELF_HANDLE.with(|h| h.get());
    if existing != 0 {
        return existing;
    }
    let handle = new_handle();
    SELF_HANDLE.with(|h| h.set(handle));
    handle
}

/// Whether `handle` is a thread that exists (and hasn't finished).
pub fn is_live(handle: u32) -> bool {
    let shared = shared();
    let threads = &shared.state.threads;
    if handle <= FIRST_HANDLE || handle > threads.next_handle {
        return false;
    }
    !matches!(threads.records.get(&handle), Some((true, _, _)))
}

fn new_handle() -> u32 {
    let mut shared = shared();
    let threads = &mut shared.state.threads;
    if threads.next_handle == 0 {
        threads.next_handle = FIRST_HANDLE;
    }
    threads.next_handle += 4;
    threads.next_handle
}

fn join_key(handle: u32) -> u64 {
    1 << 32 | handle as u64
}

fn owner_of(value: u32) -> u32 {
    value >> 16
}

/// Locks a guest mutex, waiting (without the guest lock) while another
/// thread owns it. `count` is the recursion depth to restore (1 normally).
fn lock(uc: &Uc, mutex: u32, count: u32, try_only: bool) -> u32 {
    let me = uc.get_data().id;
    loop {
        let value = mem().read_u32(mutex);
        let kind = value & TYPE_MASK;
        let owner = owner_of(value);
        if owner == 0 {
            mem().write_u32(mutex, kind | (me << 16) | count);
            return 0;
        }
        if owner == me {
            if kind == RECURSIVE {
                let depth = (value & COUNT_MASK) + 1;
                mem().write_u32(mutex, kind | (me << 16) | depth);
                return 0;
            }
            return if try_only { EBUSY } else { EDEADLK };
        }
        if try_only {
            return EBUSY;
        }
        wait_on(mutex as u64, None);
    }
}

/// Unlocks fully when `all` (for condition waits); returns the old depth.
fn unlock(uc: &Uc, mutex: u32, all: bool) -> Result<u32, u32> {
    let me = uc.get_data().id;
    let value = mem().read_u32(mutex);
    let kind = value & TYPE_MASK;
    if owner_of(value) != me {
        return Err(EPERM);
    }
    let depth = value & COUNT_MASK;
    if all || depth <= 1 {
        mem().write_u32(mutex, kind);
        wake_on(mutex as u64);
    } else {
        mem().write_u32(mutex, kind | (me << 16) | (depth - 1));
    }
    Ok(depth.max(1))
}

/// Waits on a guest condition variable until signalled or `deadline` passes.
fn condition_wait(uc: &Uc, condition: u32, mutex: u32, deadline: Option<SystemTime>) -> u32 {
    let start = mem().read_u32(condition);
    let depth = match unlock(uc, mutex, true) {
        Ok(depth) => depth,
        Err(error) => return error,
    };
    let mut result = 0;
    while mem().read_u32(condition) == start {
        let timeout = match deadline {
            Some(deadline) => match deadline.duration_since(SystemTime::now()) {
                Ok(left) => Some(left),
                Err(_) => {
                    result = ETIMEDOUT;
                    break;
                }
            },
            None => None,
        };
        wait_on(condition as u64, timeout);
    }
    lock(uc, mutex, depth, false);
    result
}

/// Takes one from a semaphore. `wait`: None = don't wait, Some(None) = wait
/// as long as needed, Some(Some(deadline)) = wait until the deadline.
fn semaphore_wait(uc: &Uc, semaphore: u32, wait: Option<Option<SystemTime>>) -> Ret {
    loop {
        let count = mem().read_u32(semaphore);
        if count > 0 {
            mem().write_u32(semaphore, count - 1);
            return Ret::U32(0);
        }
        let timeout = match wait {
            None => return fail_with(uc, EAGAIN),
            Some(None) => None,
            Some(Some(deadline)) => match deadline.duration_since(SystemTime::now()) {
                Ok(left) => Some(left),
                Err(_) => return fail_with(uc, ETIMEDOUT),
            },
        };
        wait_on(semaphore as u64, timeout);
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
            crate::dynarec::invalidate(first, second);
            let _ = uc.ctl_remove_cache(first as u64, second as u64);
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

/// futex(2) on a guest word. Guest threads only run while holding the guest
/// lock, so waiting gives it up until a wake for this word (or the timeout).
/// Returns the syscall result (negative errno on failure).
fn futex(uc: &Uc, address: u32, operation: u32, value: u32, timeout: u32) -> i32 {
    let command = operation & !(FUTEX_PRIVATE | FUTEX_CLOCK_REALTIME);
    match command {
        FUTEX_WAIT | FUTEX_WAIT_BITSET => {
            if mem().read_u32(address) != value {
                return -(EAGAIN as i32);
            }
            let deadline = (timeout != 0).then(|| {
                if command == FUTEX_WAIT {
                    let relative = Duration::new(
                        mem().read_u32(timeout) as i32 as u64,
                        (mem().read_u32(timeout + 4) as i32).max(0) as u32,
                    );
                    SystemTime::now() + relative
                } else if operation & FUTEX_CLOCK_REALTIME != 0 {
                    guest_time(timeout)
                } else {
                    monotonic_deadline(timeout)
                }
            });
            let start = shared().state.threads.futex_wakes.get(&address).copied().unwrap_or(0);
            loop {
                if shared().state.threads.futex_wakes.get(&address).copied().unwrap_or(0) != start {
                    return 0;
                }
                let left = match deadline {
                    Some(deadline) => match deadline.duration_since(SystemTime::now()) {
                        Ok(left) => Some(left),
                        Err(_) => return -(ETIMEDOUT as i32),
                    },
                    None => None,
                };
                wait_on(address as u64, left);
            }
        }
        FUTEX_WAKE | FUTEX_WAKE_BITSET => {
            let _ = uc;
            let wakes = &mut shared().state.threads.futex_wakes;
            *wakes.entry(address).or_insert(0) += 1;
            wake_on(address as u64);
            // Waiters aren't counted; report as many as were allowed.
            value.min(i32::MAX as u32) as i32
        }
        _ => {
            crate::log::line(&format!("[bridge] futex operation {operation} not handled"));
            -38 // ENOSYS
        }
    }
}

/// Takes a guest read-write lock for reading or writing.
fn rwlock_lock(uc: &Uc, lock: u32, write: bool, try_only: bool) -> u32 {
    let me = uc.get_data().id;
    loop {
        let value = mem().read_u32(lock);
        let (writer, readers) = (value >> 16, value & RWLOCK_READERS);
        if writer == me {
            return if try_only { EBUSY } else { EDEADLK };
        }
        if write && value == 0 {
            mem().write_u32(lock, me << 16);
            return 0;
        }
        if !write && writer == 0 && readers < RWLOCK_READERS {
            mem().write_u32(lock, readers + 1);
            return 0;
        }
        if try_only {
            return EBUSY;
        }
        wait_on(lock as u64, None);
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
fn run_thread(handle: u32, start: u32, argument: u32) {
    let _lock = hold_gil();
    SELF_HANDLE.with(|h| h.set(handle));
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
    run_key_destructors(uc, handle);
    crate::imports::signals::forget_thread(handle);
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

fn run_key_destructors(uc: &mut Uc, handle: u32) {
    let pending: Vec<(u32, u32)> = {
        let mut shared = shared();
        let threads = &mut shared.state.threads;
        let keys: Vec<u32> = threads.values.keys().filter(|(t, _)| *t == handle).map(|(_, k)| *k).collect();
        keys.iter()
            .filter_map(|key| {
                let value = threads.values.remove(&(handle, *key)).unwrap_or(0);
                let destructor = threads.keys.get(*key as usize).copied().flatten().unwrap_or(0);
                (value != 0 && destructor != 0).then_some((destructor, value))
            })
            .collect()
    };
    for (destructor, value) in pending {
        let _ = call_guest(uc, destructor, &[value]);
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
        "pthread_cond_signal" | "pthread_cond_broadcast" => handler!(|uc, a| {
            let condition = a.u32();
            mem().write_u32(condition, mem().read_u32(condition).wrapping_add(1));
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
            let value = mem().read_u32(lock);
            if value >> 16 == uc.get_data().id {
                mem().write_u32(lock, 0);
            } else if value & RWLOCK_READERS != 0 && value >> 16 == 0 {
                mem().write_u32(lock, value - 1);
            } else {
                return Ret::U32(EPERM);
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
            mem().write_u32(semaphore, mem().read_u32(semaphore).wrapping_add(1));
            wake_on(semaphore as u64);
            Ret::U32(0)
        }),
        "sem_getvalue" => handler!(|uc, a| {
            let (semaphore, out) = (a.u32(), a.u32());
            mem().write_u32(out, mem().read_u32(semaphore));
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
        "pthread_self" => handler!(|uc, a| Ret::U32(current_handle())),
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
            let handle = new_handle();
            let detached = attributes != 0 && mem().read_u32(attributes) & 1 != 0;
            shared().state.threads.records.insert(handle, (false, 0, detached));
            mem().write_u32(out, handle);
            let spawned = std::thread::Builder::new()
                .name(format!("arm32-guest-{handle:x}"))
                .spawn(move || run_thread(handle, start, argument));
            match spawned {
                Ok(_) => Ret::U32(0),
                Err(_) => {
                    shared().state.threads.records.remove(&handle);
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
                    Some(_) => wait_on(join_key(handle), None),
                }
            }
        }),
        "pthread_once" => handler!(|uc, a| {
            let (control, function) = (a.u32(), a.u32());
            loop {
                match mem().read_u32(control) {
                    0 => {
                        mem().write_u32(control, 1);
                        if let Err(error) = call_guest(uc, function, &[]) {
                            stop_with_fault(uc, error);
                        }
                        mem().write_u32(control, 2);
                        wake_on(control as u64);
                        return Ret::U32(0);
                    }
                    1 => wait_on(control as u64, Some(Duration::from_millis(5))),
                    _ => return Ret::U32(0),
                }
            }
        }),
        "pthread_key_create" => handler!(|uc, a| {
            let (out, destructor) = (a.u32(), a.u32());
            let mut shared = shared();
            let keys = &mut shared.state.threads.keys;
            keys.push(Some(destructor));
            let key = keys.len() as u32 - 1;
            mem().write_u32(out, key);
            Ret::U32(0)
        }),
        "pthread_key_delete" => handler!(|uc, a| {
            let key = a.u32();
            let mut shared = shared();
            let threads = &mut shared.state.threads;
            if let Some(slot) = threads.keys.get_mut(key as usize) {
                *slot = None;
            }
            threads.values.retain(|(_, k), _| *k != key);
            Ret::U32(0)
        }),
        "pthread_getspecific" => handler!(|uc, a| {
            let key = a.u32();
            let me = current_handle();
            Ret::U32(shared().state.threads.values.get(&(me, key)).copied().unwrap_or(0))
        }),
        "pthread_setspecific" => handler!(|uc, a| {
            let (key, value) = (a.u32(), a.u32());
            let me = current_handle();
            let mut shared = shared();
            let threads = &mut shared.state.threads;
            if threads.keys.get(key as usize).copied().flatten().is_none() {
                return Ret::U32(EINVAL);
            }
            threads.values.insert((me, key), value);
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
