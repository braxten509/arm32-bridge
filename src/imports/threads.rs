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

use std::cell::Cell;
use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::handler;
use crate::runtime::{
    call_guest, drop_engine, engine, hold_gil, mem, shared, stop_with_fault, wait_for_guests, wake_guests, without_gil,
    ArgSource, Handler, Ret, Uc,
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
    values: HashMap<(u32, u32), u32>,
}

thread_local! {
    static SELF_HANDLE: Cell<u32> = const { Cell::new(0) };
}

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

fn new_handle() -> u32 {
    let mut shared = shared();
    let threads = &mut shared.state.threads;
    if threads.next_handle == 0 {
        threads.next_handle = FIRST_HANDLE;
    }
    threads.next_handle += 4;
    threads.next_handle
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
        wait_for_guests(None);
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
        wake_guests();
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
        wait_for_guests(timeout);
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
        wait_for_guests(timeout);
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

/// Body of a guest thread started with pthread_create.
fn run_thread(handle: u32, start: u32, argument: u32) {
    let _lock = hold_gil();
    SELF_HANDLE.with(|h| h.set(handle));
    let uc = engine();
    let result = match call_guest(uc, start, &[argument]) {
        Ok(value) => value as u32,
        Err(error) => {
            crate::log::line(&format!("[bridge] guest thread {handle:#x} stopped: {error}"));
            0
        }
    };
    run_key_destructors(uc, handle);
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
    wake_guests();
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
            mem().write_u32(a.u32(), 0);
            Ret::U32(0)
        }),
        "pthread_cond_destroy" => handler!(|uc, a| Ret::U32(0)),
        "pthread_cond_signal" | "pthread_cond_broadcast" => handler!(|uc, a| {
            let condition = a.u32();
            mem().write_u32(condition, mem().read_u32(condition).wrapping_add(1));
            wake_guests();
            Ret::U32(0)
        }),
        "pthread_cond_wait" => handler!(|uc, a| {
            let (condition, mutex) = (a.u32(), a.u32());
            Ret::U32(condition_wait(uc, condition, mutex, None))
        }),
        "pthread_cond_timedwait" => handler!(|uc, a| {
            let (condition, mutex, deadline) = (a.u32(), a.u32(), a.u32());
            Ret::U32(condition_wait(uc, condition, mutex, Some(guest_time(deadline))))
        }),
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
            wake_guests();
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
        "pthread_equal" => handler!(|uc, a| {
            let (x, y) = (a.u32(), a.u32());
            Ret::U32((x == y) as u32)
        }),
        "pthread_create" => handler!(|uc, a| {
            let (out, _attributes, start, argument) = (a.u32(), a.u32(), a.u32(), a.u32());
            let handle = new_handle();
            shared().state.threads.records.insert(handle, (false, 0, false));
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
                    Some(_) => wait_for_guests(None),
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
                        wake_guests();
                        return Ret::U32(0);
                    }
                    1 => wait_for_guests(Some(Duration::from_millis(5))),
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
            without_gil(|| std::thread::sleep(wait));
            Ret::U32(0)
        }),
        "usleep" => handler!(|uc, a| {
            let microseconds = a.u32();
            without_gil(|| std::thread::sleep(Duration::from_micros(microseconds as u64)));
            Ret::U32(0)
        }),
        "sleep" => handler!(|uc, a| {
            let seconds = a.u32();
            without_gil(|| std::thread::sleep(Duration::from_secs(seconds as u64)));
            Ret::U32(0)
        }),
        _ => None,
    }
}
