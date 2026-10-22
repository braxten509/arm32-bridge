//! Dispatch timer sources, dispatch_suspend/resume and Mach semaphores.
//!
//! A timer source has its own thread that waits for the next firing and
//! sends the event handler to the source's queue. Like Apple's, a source
//! starts suspended, its handler never runs twice at once, and firings that
//! come while the handler is still waiting or running merge into one.

use crate::libsystem::mach::{self, BlockHeader};
use crate::symbols::{leak, Registry};
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};

const KERN_SUCCESS: i32 = 0;
const KERN_INVALID_ARGUMENT: i32 = 4;
const KERN_TERMINATED: i32 = 37;
const BLOCK_IS_GLOBAL: i32 = 1 << 28;

/// What the game passes as DISPATCH_SOURCE_TYPE_TIMER (&_dispatch_source_type_timer).
static TIMER_TYPE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

fn timer_type() -> usize {
    *TIMER_TYPE.get_or_init(|| leak([0usize; 8]))
}

struct Timer {
    queue: usize,
    state: Mutex<TimerState>,
    wake: Condvar,
    /// Our own block that runs the game's handler on the queue.
    runner: usize,
}

struct TimerState {
    handler: usize,
    suspended: u32,
    /// Next firing (monotonic nanoseconds); None: not set or one-shot done.
    next: Option<u64>,
    /// None: fires once.
    interval: Option<u64>,
    /// The handler was sent to the queue and hasn't finished.
    pending: bool,
    thread_started: bool,
}

/// The block sent to the queue: a global block (never copied) naming its timer.
#[repr(C)]
struct RunnerBlock {
    header: BlockHeader,
    timer: usize,
}

static TIMERS: Mutex<Option<HashMap<usize, &'static Timer>>> = Mutex::new(None);

fn timer_of(source: usize) -> Option<&'static Timer> {
    TIMERS.lock().unwrap().as_ref().and_then(|t| t.get(&source).copied())
}

extern "C" fn run_handler(block: usize) {
    let timer = unsafe { &*((*(block as *const RunnerBlock)).timer as *const Timer) };
    let handler = timer.state.lock().unwrap().handler;
    if handler != 0 {
        unsafe { mach::call_block(handler) };
    }
    timer.state.lock().unwrap().pending = false;
    timer.wake.notify_all();
}

fn timer_thread(timer: &'static Timer) {
    let mut state = timer.state.lock().unwrap();
    loop {
        if state.suspended > 0 || state.pending || state.next.is_none() {
            state = timer.wake.wait(state).unwrap();
            continue;
        }
        let at = state.next.unwrap();
        let now = mach::now_nanos();
        if now < at {
            state = timer.wake.wait_timeout(state, std::time::Duration::from_nanos(at - now)).unwrap().0;
            continue;
        }
        // Missed firings are skipped: the next one is the first still ahead.
        state.next = state.interval.map(|every| at + every * ((now - at) / every + 1));
        if state.handler != 0 {
            state.pending = true;
            drop(state);
            mach::dispatch_async(timer.queue, timer.runner);
            state = timer.state.lock().unwrap();
        }
    }
}

extern "C" fn dispatch_source_create(kind: usize, _handle: usize, _mask: usize, queue: usize) -> usize {
    if kind != timer_type() {
        crate::log!("dispatch_source_create: only timer sources are here (answered none)");
        return 0;
    }
    let queue = if queue == 0 { mach::dispatch_get_global_queue(0, 0) } else { queue };
    let source = leak([0usize; 16]);
    let isa = crate::symbols::lookup("__NSConcreteGlobalBlock").map_or(0, |(address, _)| address);
    let descriptor = leak([0usize, std::mem::size_of::<RunnerBlock>(), 0, 0]) as *const [usize; 4];
    let runner = leak(RunnerBlock {
        header: BlockHeader { isa, flags: BLOCK_IS_GLOBAL, reserved: 0, invoke: run_handler as usize, descriptor },
        timer: 0,
    });
    let timer: &'static Timer = Box::leak(Box::new(Timer {
        queue,
        state: Mutex::new(TimerState { handler: 0, suspended: 1, next: None, interval: None, pending: false, thread_started: false }),
        wake: Condvar::new(),
        runner,
    }));
    unsafe { (*(runner as *mut RunnerBlock)).timer = timer as *const Timer as usize };
    TIMERS.lock().unwrap().get_or_insert_with(Default::default).insert(source, timer);
    source
}

extern "C" fn dispatch_source_set_event_handler(source: usize, handler: usize) {
    let Some(timer) = timer_of(source) else {
        crate::log!("dispatch_source_set_event_handler: not one of our sources");
        return;
    };
    let copy = if handler == 0 { 0 } else { mach::block_copy(handler as *mut BlockHeader) as usize };
    timer.state.lock().unwrap().handler = copy;
}

/// interval ~0: fires once; 0 counts as 1 nanosecond (Apple's rule).
extern "C" fn dispatch_source_set_timer(source: usize, start: u64, interval: u64, _leeway: u64) {
    let Some(timer) = timer_of(source) else {
        crate::log!("dispatch_source_set_timer: not one of our sources");
        return;
    };
    let mut state = timer.state.lock().unwrap();
    state.next = mach::deadline_nanos(start);
    state.interval = match interval {
        u64::MAX => None,
        0 => Some(1),
        every => Some(every.min(i64::MAX as u64)),
    };
    drop(state);
    timer.wake.notify_all();
}

extern "C" fn dispatch_suspend(object: usize) {
    if let Some(timer) = timer_of(object) {
        timer.state.lock().unwrap().suspended += 1;
    } else if !mach::suspend_queue(object) {
        crate::log!("dispatch_suspend: the main or a global queue (ignored, as on Apple)");
    }
}

extern "C" fn dispatch_resume(object: usize) {
    let Some(timer) = timer_of(object) else {
        if !mach::resume_queue(object) {
            crate::log!("dispatch_resume: the main or a global queue (ignored, as on Apple)");
        }
        return;
    };
    let mut state = timer.state.lock().unwrap();
    if state.suspended == 0 {
        crate::log!("dispatch_resume on a source that wasn't suspended; ignored");
        return;
    }
    state.suspended -= 1;
    if state.suspended == 0 && !state.thread_started {
        state.thread_started = true;
        std::thread::spawn(move || timer_thread(timer));
    }
    drop(state);
    timer.wake.notify_all();
}

/// dispatch_walltime: a timespec (or now) plus nanoseconds, as a wall-clock time.
extern "C" fn dispatch_walltime(when: *const libc::timespec, delta: i64) -> u64 {
    let nanos = match unsafe { when.as_ref() } {
        Some(t) => t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64,
        None => mach::wall_nanos(),
    };
    mach::wall_time(nanos, delta)
}

// ----- Mach semaphores (semaphore_t is a port number) -----

struct Semaphore {
    count: Mutex<(i64, bool)>,
    wake: Condvar,
}

static SEMAPHORES: Mutex<Option<(u32, HashMap<u32, Arc<Semaphore>>)>> = Mutex::new(None);

fn semaphore(port: u32) -> Option<Arc<Semaphore>> {
    SEMAPHORES.lock().unwrap().as_ref().and_then(|(_, table)| table.get(&port).cloned())
}

extern "C" fn semaphore_create(_task: u32, out: *mut u32, _policy: i32, value: i32) -> i32 {
    if out.is_null() || value < 0 {
        return KERN_INVALID_ARGUMENT;
    }
    let mut table = SEMAPHORES.lock().unwrap();
    let (next, table) = table.get_or_insert_with(|| (0x1_0003, HashMap::new()));
    let port = *next;
    *next += 0x100;
    table.insert(port, Arc::new(Semaphore { count: Mutex::new((i64::from(value), false)), wake: Condvar::new() }));
    unsafe { *out = port };
    KERN_SUCCESS
}

/// Destroying wakes every waiter with KERN_TERMINATED, as on Apple.
extern "C" fn semaphore_destroy(_task: u32, port: u32) -> i32 {
    let removed = SEMAPHORES.lock().unwrap().as_mut().and_then(|(_, table)| table.remove(&port));
    let Some(semaphore) = removed else { return KERN_INVALID_ARGUMENT };
    semaphore.count.lock().unwrap().1 = true;
    semaphore.wake.notify_all();
    KERN_SUCCESS
}

extern "C" fn semaphore_signal(port: u32) -> i32 {
    let Some(semaphore) = semaphore(port) else { return KERN_INVALID_ARGUMENT };
    semaphore.count.lock().unwrap().0 += 1;
    semaphore.wake.notify_one();
    KERN_SUCCESS
}

extern "C" fn semaphore_wait(port: u32) -> i32 {
    let Some(semaphore) = semaphore(port) else { return KERN_INVALID_ARGUMENT };
    let mut count = semaphore.count.lock().unwrap();
    while count.0 <= 0 && !count.1 {
        count = semaphore.wake.wait(count).unwrap();
    }
    if count.1 {
        return KERN_TERMINATED;
    }
    count.0 -= 1;
    KERN_SUCCESS
}

pub fn register(r: &mut Registry) {
    r.data("_dispatch_source_type_timer", timer_type());
    r.function("dispatch_source_create", dispatch_source_create as usize);
    r.function("dispatch_source_set_event_handler", dispatch_source_set_event_handler as usize);
    r.function("dispatch_source_set_timer", dispatch_source_set_timer as usize);
    r.function("dispatch_suspend", dispatch_suspend as usize);
    r.function("dispatch_resume", dispatch_resume as usize);
    r.function("dispatch_walltime", dispatch_walltime as usize);
    r.function("semaphore_create", semaphore_create as usize);
    r.function("semaphore_destroy", semaphore_destroy as usize);
    r.function("semaphore_signal", semaphore_signal as usize);
    r.function("semaphore_wait", semaphore_wait as usize);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static FIRED: AtomicUsize = AtomicUsize::new(0);
    static INSIDE: AtomicUsize = AtomicUsize::new(0);
    static OVERLAPPED: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn tick(_block: usize) {
        if INSIDE.fetch_add(1, Ordering::SeqCst) > 0 {
            OVERLAPPED.fetch_add(1, Ordering::SeqCst);
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
        FIRED.fetch_add(1, Ordering::SeqCst);
        INSIDE.fetch_sub(1, Ordering::SeqCst);
    }

    /// A 5 ms timer whose handler takes 15 ms: it waits for resume, never
    /// overlaps itself, merges late firings, and stops while suspended.
    #[test]
    fn timer_sources_fire_after_resume_without_overlap() {
        let header = BlockHeader { isa: 0, flags: BLOCK_IS_GLOBAL, reserved: 0, invoke: tick as usize, descriptor: std::ptr::null() };
        let handler = leak(header);
        let source = dispatch_source_create(timer_type(), 0, 0, mach::dispatch_get_global_queue(0, 0));
        dispatch_source_set_event_handler(source, handler);
        dispatch_source_set_timer(source, 0, 5_000_000, 0);
        std::thread::sleep(std::time::Duration::from_millis(40));
        assert_eq!(FIRED.load(Ordering::SeqCst), 0, "fired before dispatch_resume");
        dispatch_resume(source);
        std::thread::sleep(std::time::Duration::from_millis(200));
        dispatch_suspend(source);
        std::thread::sleep(std::time::Duration::from_millis(40));
        let fired = FIRED.load(Ordering::SeqCst);
        assert!((4..=14).contains(&fired), "fired {fired} times in 200 ms");
        assert_eq!(OVERLAPPED.load(Ordering::SeqCst), 0, "handler ran twice at once");
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert_eq!(FIRED.load(Ordering::SeqCst), fired, "fired while suspended");
    }
}
