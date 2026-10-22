//! Dispatch timer sources, dispatch_suspend/resume and Mach semaphores.
//!
//! A timer source has its own thread that waits for the next firing and
//! sends the event handler to the source's target queue. As in libdispatch,
//! a source starts suspended; its handler never runs twice at once; firings
//! that come while the handler waits or runs merge into one (the count is
//! dispatch_source_get_data); a handler that was sent but hasn't started
//! when the source is suspended waits for dispatch_resume; and after
//! dispatch_source_cancel the event handler never starts again and the
//! cancel handler is sent once, when no event handler is running and the
//! source isn't suspended.

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
    state: Mutex<TimerState>,
    wake: Condvar,
    /// Our own blocks that run the game's event and cancel handlers on the queue.
    runner: usize,
    cancel_runner: usize,
}

struct TimerState {
    /// Where handlers go (dispatch_set_target_queue on the source changes it).
    queue: usize,
    handler: usize,
    cancel_handler: usize,
    suspended: u32,
    /// Next firing (monotonic nanoseconds); None: not set or one-shot done.
    next: Option<u64>,
    /// None: fires once.
    interval: Option<u64>,
    /// Firings not yet handed to a handler (dispatch_source_get_data).
    fired: u64,
    /// What the running handler sees from dispatch_source_get_data.
    data: u64,
    /// The runner was sent to the queue and hasn't started.
    queued: bool,
    /// The event handler is running.
    running: bool,
    cancelled: bool,
    cancel_sent: bool,
    cancel_queued: bool,
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

/// Sends the event handler if firings wait and nothing stops it. Lock held.
fn send_handler(timer: &'static Timer, state: &mut TimerState) {
    if state.fired > 0 && state.handler != 0 && !state.queued && !state.running && !state.cancelled && state.suspended == 0 {
        state.queued = true;
        mach::dispatch_async(state.queue, timer.runner);
    }
}

/// Sends the cancel handler once, when it may run. Lock held. Like the event
/// handler, a cancel handler sent but not started waits while the source is
/// suspended.
fn send_cancel_handler(timer: &'static Timer, state: &mut TimerState) {
    if state.cancelled && !state.cancel_sent && !state.cancel_queued && !state.running && !state.queued && state.suspended == 0 {
        state.cancel_queued = true;
        mach::dispatch_async(state.queue, timer.cancel_runner);
    }
}

extern "C" fn run_cancel_handler(block: usize) {
    let timer = unsafe { &*((*(block as *const RunnerBlock)).timer as *const Timer) };
    let handler = {
        let mut state = timer.state.lock().unwrap();
        state.cancel_queued = false;
        if state.suspended > 0 || state.cancel_sent {
            return;
        }
        state.cancel_sent = true;
        state.cancel_handler
    };
    if handler != 0 {
        unsafe { mach::call_block(handler) };
    }
}

extern "C" fn run_handler(block: usize) {
    let timer = unsafe { &*((*(block as *const RunnerBlock)).timer as *const Timer) };
    let handler = {
        let mut state = timer.state.lock().unwrap();
        state.queued = false;
        if state.cancelled || state.suspended > 0 || state.handler == 0 {
            // Suspended: the firings stay counted until dispatch_resume sends it again.
            send_cancel_handler(timer, &mut state);
            return;
        }
        state.running = true;
        state.data = std::mem::take(&mut state.fired);
        state.handler
    };
    unsafe { mach::call_block(handler) };
    let mut state = timer.state.lock().unwrap();
    state.running = false;
    send_handler(timer, &mut state);
    send_cancel_handler(timer, &mut state);
    drop(state);
    timer.wake.notify_all();
}

fn timer_thread(timer: &'static Timer) {
    let mut state = timer.state.lock().unwrap();
    loop {
        if state.cancelled {
            return;
        }
        if state.suspended > 0 || state.next.is_none() {
            state = timer.wake.wait(state).unwrap();
            continue;
        }
        let at = state.next.unwrap();
        let now = mach::now_nanos();
        if now < at {
            state = timer.wake.wait_timeout(state, std::time::Duration::from_nanos(at - now)).unwrap().0;
            continue;
        }
        // Firings missed meanwhile are counted, and the next one is the first still ahead.
        match state.interval {
            Some(every) => {
                let missed = (now - at) / every;
                state.fired += missed + 1;
                state.next = Some(at + every * (missed + 1));
            }
            None => {
                state.fired += 1;
                state.next = None;
            }
        }
        send_handler(timer, &mut state);
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
    let cancel_runner = leak(RunnerBlock {
        header: BlockHeader { isa, flags: BLOCK_IS_GLOBAL, reserved: 0, invoke: run_cancel_handler as usize, descriptor },
        timer: 0,
    });
    let timer: &'static Timer = Box::leak(Box::new(Timer {
        state: Mutex::new(TimerState {
            queue,
            handler: 0,
            cancel_handler: 0,
            suspended: 1,
            next: None,
            interval: None,
            fired: 0,
            data: 0,
            queued: false,
            running: false,
            cancelled: false,
            cancel_sent: false,
            cancel_queued: false,
            thread_started: false,
        }),
        wake: Condvar::new(),
        runner,
        cancel_runner,
    }));
    unsafe {
        (*(runner as *mut RunnerBlock)).timer = timer as *const Timer as usize;
        (*(cancel_runner as *mut RunnerBlock)).timer = timer as *const Timer as usize;
    }
    TIMERS.lock().unwrap().get_or_insert_with(Default::default).insert(source, timer);
    source
}

fn copied(block: usize) -> usize {
    if block == 0 { 0 } else { mach::block_copy(block as *mut BlockHeader) as usize }
}

extern "C" fn dispatch_source_set_event_handler(source: usize, handler: usize) {
    let Some(timer) = timer_of(source) else {
        crate::log!("dispatch_source_set_event_handler: not one of our sources");
        return;
    };
    timer.state.lock().unwrap().handler = copied(handler);
}

extern "C" fn dispatch_source_set_cancel_handler(source: usize, handler: usize) {
    let Some(timer) = timer_of(source) else {
        crate::log!("dispatch_source_set_cancel_handler: not one of our sources");
        return;
    };
    timer.state.lock().unwrap().cancel_handler = copied(handler);
}

/// After this the event handler never starts again; the cancel handler follows.
extern "C" fn dispatch_source_cancel(source: usize) {
    let Some(timer) = timer_of(source) else {
        crate::log!("dispatch_source_cancel: not one of our sources");
        return;
    };
    let mut state = timer.state.lock().unwrap();
    state.cancelled = true;
    send_cancel_handler(timer, &mut state);
    drop(state);
    timer.wake.notify_all();
}

extern "C" fn dispatch_source_testcancel(source: usize) -> isize {
    timer_of(source).map_or(0, |timer| timer.state.lock().unwrap().cancelled as isize)
}

/// For a timer: how many times it fired since the handler last ran.
extern "C" fn dispatch_source_get_data(source: usize) -> u64 {
    timer_of(source).map_or(0, |timer| timer.state.lock().unwrap().data)
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
    // A new timer setting drops firings not yet handled (libdispatch does the same).
    state.fired = 0;
    drop(state);
    timer.wake.notify_all();
}

/// dispatch_set_target_queue on a source: later handlers go to the new queue.
/// False: not one of our sources.
pub(crate) fn retarget(object: usize, queue: usize) -> bool {
    let Some(timer) = timer_of(object) else { return false };
    timer.state.lock().unwrap().queue = if queue == 0 { mach::dispatch_get_global_queue(0, 0) } else { queue };
    true
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
    if state.suspended == 0 {
        if !state.thread_started && !state.cancelled {
            state.thread_started = true;
            std::thread::spawn(move || timer_thread(timer));
        }
        send_handler(timer, &mut state);
        send_cancel_handler(timer, &mut state);
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
    r.function("dispatch_source_set_cancel_handler", dispatch_source_set_cancel_handler as usize);
    r.function("dispatch_source_cancel", dispatch_source_cancel as usize);
    r.function("dispatch_source_testcancel", dispatch_source_testcancel as usize);
    r.function("dispatch_source_get_data", dispatch_source_get_data as usize);
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
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

    static FIRED: AtomicUsize = AtomicUsize::new(0);
    static INSIDE: AtomicUsize = AtomicUsize::new(0);
    static OVERLAPPED: AtomicUsize = AtomicUsize::new(0);
    static MOST_DATA: AtomicU64 = AtomicU64::new(0);
    static SOURCE: AtomicUsize = AtomicUsize::new(0);
    static CANCELS: AtomicUsize = AtomicUsize::new(0);
    static RELEASE: AtomicBool = AtomicBool::new(false);

    extern "C" fn tick(_block: usize) {
        if INSIDE.fetch_add(1, Ordering::SeqCst) > 0 {
            OVERLAPPED.fetch_add(1, Ordering::SeqCst);
        }
        MOST_DATA.fetch_max(dispatch_source_get_data(SOURCE.load(Ordering::SeqCst)), Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(15));
        FIRED.fetch_add(1, Ordering::SeqCst);
        INSIDE.fetch_sub(1, Ordering::SeqCst);
    }

    extern "C" fn hold_queue(_block: usize) {
        while !RELEASE.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    extern "C" fn cancelled(_block: usize) {
        CANCELS.fetch_add(1, Ordering::SeqCst);
    }

    fn global_block(invoke: usize) -> usize {
        leak(BlockHeader { isa: 0, flags: BLOCK_IS_GLOBAL, reserved: 0, invoke, descriptor: std::ptr::null() })
    }

    fn sleep(ms: u64) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }

    /// A 5 ms timer whose handler takes 15 ms on a serial queue: it waits for
    /// resume; a handler already sent waits while the source is suspended;
    /// late firings merge (get_data counts them); it never overlaps itself;
    /// after cancel it stops and the cancel handler runs once.
    #[test]
    fn timer_sources_fire_after_resume_without_overlap() {
        let queue = mach::dispatch_queue_create(0, 0);
        let source = dispatch_source_create(timer_type(), 0, 0, queue);
        SOURCE.store(source, Ordering::SeqCst);
        dispatch_source_set_event_handler(source, global_block(tick as usize));
        dispatch_source_set_cancel_handler(source, global_block(cancelled as usize));
        dispatch_source_set_timer(source, 0, 5_000_000, 0);
        sleep(40);
        assert_eq!(FIRED.load(Ordering::SeqCst), 0, "fired before dispatch_resume");
        // The queue is busy, so the first handler waits in it; then the source is suspended.
        mach::dispatch_async(queue, global_block(hold_queue as usize));
        dispatch_resume(source);
        sleep(30);
        dispatch_suspend(source);
        RELEASE.store(true, Ordering::SeqCst);
        sleep(50);
        assert_eq!(FIRED.load(Ordering::SeqCst), 0, "a handler sent before dispatch_suspend ran while suspended");
        dispatch_resume(source);
        sleep(200);
        let fired = FIRED.load(Ordering::SeqCst);
        assert!((4..=14).contains(&fired), "fired {fired} times in 200 ms");
        assert!(MOST_DATA.load(Ordering::SeqCst) > 1, "late firings weren't counted together");
        assert_eq!(OVERLAPPED.load(Ordering::SeqCst), 0, "handler ran twice at once");
        dispatch_source_cancel(source);
        sleep(60);
        assert_eq!(dispatch_source_testcancel(source), 1);
        assert_eq!(CANCELS.load(Ordering::SeqCst), 1, "cancel handler didn't run once");
        let after_cancel = FIRED.load(Ordering::SeqCst);
        sleep(60);
        assert_eq!(FIRED.load(Ordering::SeqCst), after_cancel, "fired after cancel");
    }
}
