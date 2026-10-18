//! NSLock, NSRecursiveLock, NSCondition and NSConditionLock: { isa, *mut Lock }.

use crate::fw::object::{define_class, methods};
use crate::objc::runtime::{self, Id, Sel};
use crate::symbols::Registry;
use std::sync::{Condvar, Mutex};

struct State {
    owner: u64,
    depth: u32,
    condition: isize,
}

pub struct Lock {
    state: Mutex<State>,
    wake: Condvar,
    recursive: bool,
}

fn me() -> u64 {
    unsafe { libc::pthread_self() as u64 }
}

fn lock_of(object: Id) -> &'static Lock {
    use std::sync::atomic::{AtomicPtr, Ordering};
    unsafe {
        let slot = &*((object + 8) as *const AtomicPtr<Lock>);
        let found = slot.load(Ordering::Acquire);
        if !found.is_null() {
            return &*found;
        }
        let class = runtime::lock().class_of(object);
        let recursive = runtime::lock().class_name(class) == "NSRecursiveLock";
        let made = Box::into_raw(Box::new(Lock {
            state: Mutex::new(State { owner: 0, depth: 0, condition: 0 }),
            wake: Condvar::new(),
            recursive,
        }));
        // Two threads may race here on a brand-new lock; keep the first.
        match slot.compare_exchange(std::ptr::null_mut(), made, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => &*made,
            Err(winner) => {
                drop(Box::from_raw(made));
                &*winner
            }
        }
    }
}

/// Seconds from now until an NSDate (0 = no date given).
fn seconds_until(date: Id) -> Option<f64> {
    if date == 0 {
        return None;
    }
    let s = runtime::sel("timeIntervalSinceNow");
    let imp = runtime::imp_for(date, s)?;
    let f: extern "C" fn(Id, Sel) -> f64 = unsafe { std::mem::transmute(imp) };
    Some(f(date, s))
}

/// Takes the lock, optionally only when `condition` matches, waiting at most `limit` seconds.
fn acquire(lock: &Lock, condition: Option<isize>, limit: Option<f64>) -> bool {
    let me = me();
    let deadline = limit.map(|s| std::time::Instant::now() + std::time::Duration::from_secs_f64(s.max(0.0)));
    let mut state = lock.state.lock().unwrap();
    loop {
        let free = state.depth == 0 || (lock.recursive && state.owner == me);
        let condition_ok = condition.map(|c| c == state.condition).unwrap_or(true);
        if free && condition_ok {
            state.owner = me;
            state.depth += 1;
            return true;
        }
        match deadline {
            None => state = lock.wake.wait(state).unwrap(),
            Some(at) => {
                let now = std::time::Instant::now();
                if now >= at {
                    return false;
                }
                state = lock.wake.wait_timeout(state, at - now).unwrap().0;
            }
        }
    }
}

fn release(lock: &Lock, condition: Option<isize>) {
    let mut state = lock.state.lock().unwrap();
    if state.depth > 0 {
        state.depth -= 1;
        if state.depth == 0 {
            state.owner = 0;
        }
    }
    if let Some(c) = condition {
        state.condition = c;
    }
    lock.wake.notify_all();
}

extern "C" fn do_lock(this: Id, _: Sel) {
    acquire(lock_of(this), None, None);
}

extern "C" fn do_unlock(this: Id, _: Sel) {
    release(lock_of(this), None);
}

extern "C" fn try_lock(this: Id, _: Sel) -> bool {
    acquire(lock_of(this), None, Some(0.0))
}

extern "C" fn lock_before(this: Id, _: Sel, date: Id) -> bool {
    acquire(lock_of(this), None, seconds_until(date))
}

/// NSCondition wait: give the lock up, sleep until signalled, take it back.
fn wait(this: Id, limit: Option<f64>) -> bool {
    let lock = lock_of(this);
    let deadline = limit.map(|s| std::time::Instant::now() + std::time::Duration::from_secs_f64(s.max(0.0)));
    let mut state = lock.state.lock().unwrap();
    let depth = state.depth;
    state.depth = 0;
    state.owner = 0;
    let generation = state.condition;
    lock.wake.notify_all();
    let mut signalled = false;
    loop {
        if state.condition != generation {
            signalled = true;
        }
        if signalled && state.depth == 0 {
            break;
        }
        match deadline {
            None => state = lock.wake.wait(state).unwrap(),
            Some(at) => {
                let now = std::time::Instant::now();
                if now >= at && state.depth == 0 {
                    break;
                }
                let left = if at > now { at - now } else { std::time::Duration::from_millis(1) };
                state = lock.wake.wait_timeout(state, left).unwrap().0;
            }
        }
    }
    state.depth = depth;
    state.owner = me();
    signalled
}

extern "C" fn condition_wait(this: Id, _: Sel) {
    wait(this, None);
}

extern "C" fn condition_wait_until(this: Id, _: Sel, date: Id) -> bool {
    wait(this, seconds_until(date))
}

/// signal/broadcast: waiting threads notice the counter change.
extern "C" fn condition_signal(this: Id, _: Sel) {
    let lock = lock_of(this);
    let mut state = lock.state.lock().unwrap();
    state.condition = state.condition.wrapping_add(1);
    lock.wake.notify_all();
}

extern "C" fn condition_lock_init(this: Id, _: Sel, condition: isize) -> Id {
    lock_of(this).state.lock().unwrap().condition = condition;
    this
}

extern "C" fn condition_lock_condition(this: Id, _: Sel) -> isize {
    lock_of(this).state.lock().unwrap().condition
}

extern "C" fn lock_when(this: Id, _: Sel, condition: isize) {
    acquire(lock_of(this), Some(condition), None);
}

extern "C" fn lock_when_before(this: Id, _: Sel, condition: isize, date: Id) -> bool {
    acquire(lock_of(this), Some(condition), seconds_until(date))
}

extern "C" fn try_lock_when(this: Id, _: Sel, condition: isize) -> bool {
    acquire(lock_of(this), Some(condition), Some(0.0))
}

extern "C" fn unlock_with(this: Id, _: Sel, condition: isize) {
    release(lock_of(this), Some(condition));
}

extern "C" fn ignore_id(_this: Id, _: Sel, _value: Id) {}

pub fn register(_: &mut Registry) {
    let common: &[(&str, usize)] = &[
        ("lock", do_lock as usize),
        ("unlock", do_unlock as usize),
        ("tryLock", try_lock as usize),
        ("lockBeforeDate:", lock_before as usize),
        ("setName:", ignore_id as usize),
    ];
    let lock = define_class("NSLock", "NSObject", 16);
    methods(lock, common);
    let recursive = define_class("NSRecursiveLock", "NSObject", 16);
    methods(recursive, common);
    let condition = define_class("NSCondition", "NSObject", 16);
    methods(condition, common);
    methods(
        condition,
        &[
            ("wait", condition_wait as usize),
            ("waitUntilDate:", condition_wait_until as usize),
            ("signal", condition_signal as usize),
            ("broadcast", condition_signal as usize),
        ],
    );
    let condition_lock = define_class("NSConditionLock", "NSObject", 16);
    methods(condition_lock, common);
    methods(
        condition_lock,
        &[
            ("initWithCondition:", condition_lock_init as usize),
            ("condition", condition_lock_condition as usize),
            ("lockWhenCondition:", lock_when as usize),
            ("lockWhenCondition:beforeDate:", lock_when_before as usize),
            ("tryLockWhenCondition:", try_lock_when as usize),
            ("unlockWithCondition:", unlock_with as usize),
        ],
    );
}
