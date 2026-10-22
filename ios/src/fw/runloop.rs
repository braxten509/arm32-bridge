//! Threads, timers, the display link and the main thread's run loop.

use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::{collections, string};
use crate::objc::runtime::{self, lock, Class, Id, Sel};
use crate::symbols::Registry;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

fn now() -> f64 {
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) };
    t.tv_sec as f64 + t.tv_nsec as f64 / 1e9
}

// ----- run loops: one per thread, the main one also takes posted work -----

pub enum Work {
    Block(usize),
    /// Our own work for the main thread.
    Native(Box<dyn FnOnce() + Send>),
    Message { target: Id, selector: Sel, argument: Id, done: Option<Arc<(Mutex<bool>, Condvar)>> },
}

pub struct Loop {
    queue: Mutex<VecDeque<Work>>,
    timers: Mutex<Vec<Id>>,
    wake: Condvar,
}

impl Loop {
    const fn new() -> Self {
        Loop { queue: Mutex::new(VecDeque::new()), timers: Mutex::new(Vec::new()), wake: Condvar::new() }
    }

    fn notify(&self) {
        // Taking the queue lock first means a waiting loop can't miss the wake-up.
        drop(self.queue.lock().unwrap());
        self.wake.notify_all();
    }
}

static MAIN_LOOP: Loop = Loop::new();
static MAIN_THREAD: AtomicU64 = AtomicU64::new(0);

pub fn mark_main_thread() {
    MAIN_THREAD.store(unsafe { libc::pthread_self() } as u64, Ordering::Relaxed);
}

pub fn is_main_thread() -> bool {
    let main = MAIN_THREAD.load(Ordering::Relaxed);
    main == 0 || main == unsafe { libc::pthread_self() } as u64
}

pub fn is_main_thread_id(thread: u64) -> bool {
    MAIN_THREAD.load(Ordering::Relaxed) == thread
}

/// Queues work for the main thread's run loop.
pub fn post(work: Work) {
    MAIN_LOOP.queue.lock().unwrap().push_back(work);
    MAIN_LOOP.wake.notify_all();
}

fn run_work(work: Work) {
    match work {
        Work::Block(block) => unsafe { crate::libsystem::mach::call_block(block) },
        Work::Native(work) => work(),
        Work::Message { target, selector, argument, done } => {
            let name = lock().sel_name(selector);
            runtime::send(target, &name, &[argument]);
            // Let go of the argument kept while the message waited (see perform_on_main).
            crate::fw::counted::release(argument);
            if let Some(done) = done {
                *done.0.lock().unwrap() = true;
                done.1.notify_all();
            }
        }
    }
}

// ----- timers (NSTimer and CADisplayLink): { isa, *mut Timer } -----

pub struct TimerState {
    pub fire_at: f64,
    pub interval: f64,
    pub repeats: bool,
    pub target: Id,
    pub selector: Sel,
    pub user_info: Id,
    pub valid: bool,
    /// The run loop that fires this timer, once scheduled.
    pub owner: Option<&'static Loop>,
    pub paused: bool,
    /// performSelector:afterDelay: passes its argument, not the timer.
    pub pass_argument: bool,
}

pub struct Timer(Mutex<TimerState>);

static TIMERS_FIRED: AtomicUsize = AtomicUsize::new(0);

fn timer(object: Id) -> std::sync::MutexGuard<'static, TimerState> {
    unsafe { (**((object + 8) as *const *const Timer)).0.lock().unwrap() }
}

fn make_timer(class: Class, interval: f64, target: Id, selector: Sel, user_info: Id, repeats: bool) -> Id {
    let object = runtime::alloc_instance(class);
    // The timer holds on to its user info (performSelector:afterDelay:'s argument).
    crate::fw::counted::keep(user_info);
    let state = TimerState {
        fire_at: now() + interval.max(0.0),
        interval: interval.max(0.0001),
        repeats,
        target,
        selector,
        user_info,
        valid: true,
        owner: None,
        paused: false,
        pass_argument: false,
    };
    unsafe { *((object + 8) as *mut *const Timer) = Box::into_raw(Box::new(Timer(Mutex::new(state)))) };
    object
}

fn schedule(object: Id, run_loop: &'static Loop) {
    {
        let mut state = timer(object);
        if state.owner.is_some() || !state.valid {
            return;
        }
        state.owner = Some(run_loop);
    }
    run_loop.timers.lock().unwrap().push(object);
    run_loop.notify();
}

extern "C" fn timer_scheduled(class: Class, _: Sel, interval: f64, target: Id, selector: Sel, info: Id, repeats: u8) -> Id {
    let object = make_timer(class, interval, target, selector, info, repeats != 0);
    schedule(object, current_loop());
    object
}

extern "C" fn timer_unscheduled(class: Class, _: Sel, interval: f64, target: Id, selector: Sel, info: Id, repeats: u8) -> Id {
    make_timer(class, interval, target, selector, info, repeats != 0)
}

extern "C" fn timer_invalidate(this: Id, _: Sel) {
    timer(this).valid = false;
}

extern "C" fn timer_is_valid(this: Id, _: Sel) -> bool {
    timer(this).valid
}

extern "C" fn timer_user_info(this: Id, _: Sel) -> Id {
    timer(this).user_info
}

extern "C" fn timer_interval(this: Id, _: Sel) -> f64 {
    timer(this).interval
}

extern "C" fn timer_fire(this: Id, _: Sel) {
    let (target, selector) = {
        let mut state = timer(this);
        if !state.valid {
            return;
        }
        if !state.repeats {
            state.valid = false;
        }
        (state.target, state.selector)
    };
    let name = lock().sel_name(selector);
    runtime::send(target, &name, &[this]);
}

// CADisplayLink: a timer at the screen's refresh rate (60 Hz, like the
// iPhones these games were made for; IOS_REFRESH_HZ=120 to try others).
fn refresh_seconds() -> f64 {
    static SECONDS: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *SECONDS.get_or_init(|| {
        let hz = std::env::var("IOS_REFRESH_HZ").ok().and_then(|t| t.parse::<f64>().ok()).filter(|&hz| hz >= 1.0);
        1.0 / hz.unwrap_or(60.0)
    })
}

extern "C" fn display_link_with_target(class: Class, _: Sel, target: Id, selector: Sel) -> Id {
    make_timer(class, refresh_seconds(), target, selector, 0, true)
}

extern "C" fn display_link_add_to_run_loop(this: Id, _: Sel, run_loop: Id, _mode: Id) {
    schedule(this, loop_of(run_loop));
}

extern "C" fn display_link_set_frame_interval(this: Id, _: Sel, frames: isize) {
    timer(this).interval = refresh_seconds() * frames.max(1) as f64;
}

extern "C" fn display_link_frame_interval(this: Id, _: Sel) -> isize {
    (timer(this).interval / refresh_seconds()).round() as isize
}

extern "C" fn display_link_set_paused(this: Id, _: Sel, paused: u8) {
    timer(this).paused = paused != 0;
}

extern "C" fn display_link_paused(this: Id, _: Sel) -> bool {
    timer(this).paused
}

extern "C" fn display_link_timestamp(_this: Id, _: Sel) -> f64 {
    now()
}

extern "C" fn display_link_duration(_this: Id, _: Sel) -> f64 {
    refresh_seconds()
}

/// Fires this loop's due timers. Returns how many fired and when the next one is due.
fn fire_due_timers(run_loop: &'static Loop) -> (usize, Option<f64>) {
    let due: Vec<Id> = {
        let mut list = run_loop.timers.lock().unwrap();
        list.retain(|&t| timer(t).valid);
        let current = now();
        list.iter().copied().filter(|&t| {
            let state = timer(t);
            !state.paused && state.fire_at <= current
        }).collect()
    };
    let mut fired = 0;
    for object in due {
        // An earlier timer's callback may have stopped or paused this one.
        let (target, selector, argument) = {
            let mut state = timer(object);
            let current = now();
            if !state.valid || state.paused || state.fire_at > current {
                continue;
            }
            if state.repeats {
                state.fire_at += state.interval;
                if state.fire_at < current {
                    state.fire_at = current + state.interval;
                }
            } else {
                state.valid = false;
            }
            let argument = if state.pass_argument { state.user_info } else { object };
            (state.target, state.selector, argument)
        };
        fired += 1;
        TIMERS_FIRED.fetch_add(1, Ordering::Relaxed);
        let name = lock().sel_name(selector);
        runtime::send(target, &name, &[argument]);
    }
    let list = run_loop.timers.lock().unwrap();
    let next = list
        .iter()
        .filter_map(|&t| {
            let state = timer(t);
            (state.valid && !state.paused).then_some(state.fire_at)
        })
        .reduce(f64::min);
    (fired, next)
}

/// Runs a loop's queued work and due timers until `until` (or forever).
/// With `once`, returns as soon as something was handled. Returns true if anything was.
fn run_loop_until(run_loop: &'static Loop, until: Option<f64>, once: bool) -> bool {
    let mut handled = false;
    // Each turn of the loop has its own autorelease pool, as on Apple.
    let pool = crate::fw::counted::pool_mark();
    loop {
        loop {
            let work = run_loop.queue.lock().unwrap().pop_front();
            match work {
                Some(work) => {
                    run_work(work);
                    handled = true;
                }
                None => break,
            }
        }
        let (fired, next_timer) = fire_due_timers(run_loop);
        handled |= fired > 0;
        crate::fw::counted::pool_drain(pool);
        if once && handled {
            return true;
        }
        // The game's UIKit pictures go to the phone screen from the main thread.
        let pictures = std::ptr::eq(run_loop, &MAIN_LOOP) && crate::fw::pictures::active();
        if pictures {
            crate::fw::pictures::tick();
        }
        let current = now();
        if let Some(end) = until {
            if current >= end {
                return handled;
            }
        }
        let mut wait_until = next_timer.unwrap_or(current + 0.25);
        if pictures {
            wait_until = wait_until.min(current + 0.05);
        }
        if let Some(end) = until {
            wait_until = wait_until.min(end);
        }
        let guard = run_loop.queue.lock().unwrap();
        if guard.is_empty() && wait_until > current {
            let _ = run_loop.wake.wait_timeout(guard, std::time::Duration::from_secs_f64((wait_until - current).min(0.25)));
        }
    }
}

/// Runs the main thread's loop until `until` (or forever).
pub fn run_main_loop(until: Option<f64>) {
    run_loop_until(&MAIN_LOOP, until, false);
}

pub fn timers_fired() -> usize {
    TIMERS_FIRED.load(Ordering::Relaxed)
}

// ----- NSRunLoop / CFRunLoop: { isa, &'static Loop } -----

static MAIN_RUN_LOOP: std::sync::OnceLock<Id> = std::sync::OnceLock::new();

thread_local! {
    static THREAD_RUN_LOOP: std::cell::Cell<Id> = const { std::cell::Cell::new(0) };
}

fn run_loop_object(run_loop: &'static Loop) -> Id {
    let object = runtime::alloc_instance(runtime::class_named("NSRunLoop"));
    unsafe { *((object + 8) as *mut *const Loop) = run_loop };
    object
}

fn main_run_loop() -> Id {
    *MAIN_RUN_LOOP.get_or_init(|| run_loop_object(&MAIN_LOOP))
}

fn current_run_loop() -> Id {
    if is_main_thread() {
        return main_run_loop();
    }
    THREAD_RUN_LOOP.with(|cell| {
        if cell.get() == 0 {
            // Lives as long as the program: the game may keep the object.
            let made: &'static Loop = Box::leak(Box::new(Loop::new()));
            cell.set(run_loop_object(made));
        }
        cell.get()
    })
}

fn loop_of(object: Id) -> &'static Loop {
    if object == 0 {
        return current_loop();
    }
    unsafe { &**((object + 8) as *const *const Loop) }
}

fn current_loop() -> &'static Loop {
    loop_of(current_run_loop())
}

extern "C" fn run_loop_current(_: Class, _: Sel) -> Id {
    current_run_loop()
}

extern "C" fn run_loop_main(_: Class, _: Sel) -> Id {
    main_run_loop()
}

extern "C" fn run_loop_add_timer(this: Id, _: Sel, timer_object: Id, _mode: Id) {
    schedule(timer_object, loop_of(this));
}

fn sleep_seconds(seconds: f64) {
    if seconds > 0.0 {
        std::thread::sleep(std::time::Duration::from_secs_f64(seconds.min(3600.0)));
    }
}

fn date_deadline(date: Id) -> f64 {
    let seconds = if date == 0 { 0.0 } else { runtime_send_f64(date, "timeIntervalSinceNow") };
    now() + seconds.clamp(0.0, 1e9)
}

extern "C" fn run_loop_run_until(this: Id, _: Sel, date: Id) {
    run_loop_until(loop_of(this), Some(date_deadline(date)), false);
}

/// runMode:beforeDate: handles at most one round of work, waiting until the date.
extern "C" fn run_loop_run_mode(this: Id, _: Sel, _mode: Id, date: Id) -> bool {
    run_loop_until(loop_of(this), Some(date_deadline(date)), true);
    true
}

extern "C" fn run_loop_run(this: Id, _: Sel) {
    run_loop_until(loop_of(this), None, false);
}

extern "C" fn run_loop_get_cf(this: Id, _: Sel) -> Id {
    this
}

extern "C" fn cf_run_loop_get_current() -> Id {
    current_run_loop()
}

extern "C" fn cf_run_loop_get_main() -> Id {
    main_run_loop()
}

/// CFRunLoopRunInMode(mode, seconds, returnAfterSourceHandled) -> result.
extern "C" fn cf_run_loop_run_in_mode(_mode: Id, seconds: f64, return_after: u8) -> i32 {
    const TIMED_OUT: i32 = 3;
    const HANDLED_SOURCE: i32 = 4;
    let until = now() + seconds.clamp(0.0, 1e9);
    if run_loop_until(current_loop(), Some(until), return_after != 0) && return_after != 0 {
        HANDLED_SOURCE
    } else {
        TIMED_OUT
    }
}

fn runtime_send_f64(object: Id, name: &str) -> f64 {
    let s = runtime::sel(name);
    match runtime::imp_for(object, s) {
        Some(imp) => {
            let f: extern "C" fn(Id, Sel) -> f64 = unsafe { std::mem::transmute(imp) };
            f(object, s)
        }
        None => 0.0,
    }
}

// ----- performSelector variants on NSObject -----

extern "C" fn perform_on_main(this: Id, _: Sel, selector: Sel, argument: Id, wait: u8) {
    if is_main_thread() && wait != 0 {
        let name = lock().sel_name(selector);
        runtime::send(this, &name, &[argument]);
        return;
    }
    // Kept until the message has run (Apple retains it too).
    crate::fw::counted::keep(argument);
    if wait != 0 {
        let done = Arc::new((Mutex::new(false), Condvar::new()));
        post(Work::Message { target: this, selector, argument, done: Some(done.clone()) });
        let mut finished = done.0.lock().unwrap();
        while !*finished {
            finished = done.1.wait(finished).unwrap();
        }
    } else {
        post(Work::Message { target: this, selector, argument, done: None });
    }
}

extern "C" fn perform_on_main_modes(this: Id, s: Sel, selector: Sel, argument: Id, wait: u8, _modes: Id) {
    perform_on_main(this, s, selector, argument, wait);
}

static DELAYED: Mutex<Vec<Id>> = Mutex::new(Vec::new());

extern "C" fn perform_after_delay(this: Id, _: Sel, selector: Sel, argument: Id, delay: f64) {
    let object = make_timer(runtime::class_named("NSTimer"), delay, this, selector, argument, false);
    timer(object).pass_argument = true;
    {
        let mut delayed = DELAYED.lock().unwrap();
        delayed.retain(|&t| timer(t).valid);
        delayed.push(object);
    }
    schedule(object, current_loop());
}

extern "C" fn perform_after_delay_modes(this: Id, s: Sel, selector: Sel, argument: Id, delay: f64, _modes: Id) {
    perform_after_delay(this, s, selector, argument, delay);
}

extern "C" fn perform_in_background(this: Id, _: Sel, selector: Sel, argument: Id) {
    spawn(new_thread(this, selector, argument), this, selector, argument, "performSelectorInBackground");
}

/// +cancelPreviousPerformRequestsWithTarget: (the receiver is the class).
extern "C" fn cancel_previous_requests(_class: Class, _: Sel, target: Id) {
    for &object in DELAYED.lock().unwrap().iter() {
        let mut state = timer(object);
        if state.target == target {
            state.valid = false;
        }
    }
}

/// +cancelPreviousPerformRequestsWithTarget:selector:object:
extern "C" fn cancel_previous_matching(_class: Class, _: Sel, target: Id, selector: Sel, argument: Id) {
    for &object in DELAYED.lock().unwrap().iter() {
        let mut state = timer(object);
        if state.target == target && state.selector == selector && objects_equal(state.user_info, argument) {
            state.valid = false;
        }
    }
}

fn objects_equal(a: Id, b: Id) -> bool {
    a == b || (a != 0 && b != 0 && runtime::send(a, "isEqual:", &[b]) & 0xFF != 0)
}

// ----- NSThread: { isa, target, selector, argument, stack size } -----

static THREAD_STACK: usize = 16 << 20;

fn new_thread(target: Id, selector: Sel, argument: Id) -> Id {
    let object = runtime::alloc_instance(runtime::class_named("NSThread"));
    unsafe { *((object + 8) as *mut [usize; 3]) = [target, selector, argument] };
    object
}

fn spawn(thread: Id, target: Id, selector: Sel, argument: Id, why: &str) {
    let name = lock().sel_name(selector);
    // Kept while the thread runs, as Apple's NSThread retains its argument.
    crate::fw::counted::keep(argument);
    crate::log!("thread started: -[{} {name}] ({why})", string::describe_class_of(target));
    std::thread::Builder::new()
        .stack_size(THREAD_STACK)
        .spawn(move || {
            crate::libsystem::threads::note_this_thread();
            CURRENT_THREAD.with(|cell| cell.set(thread));
            runtime::send(target, &name, &[argument]);
            crate::fw::counted::release(argument);
        })
        .expect("can't start a thread");
}

extern "C" fn thread_detach(_: Class, _: Sel, selector: Sel, target: Id, argument: Id) {
    spawn(new_thread(target, selector, argument), target, selector, argument, "detachNewThreadSelector");
}

extern "C" fn thread_init_with_target(this: Id, _: Sel, target: Id, selector: Sel, argument: Id) -> Id {
    unsafe {
        *((this + 8) as *mut [usize; 3]) = [target, selector, argument];
    }
    this
}

extern "C" fn thread_start(this: Id, _: Sel) {
    let [target, selector, argument] = unsafe { *((this + 8) as *const [usize; 3]) };
    if target != 0 {
        spawn(this, target, selector, argument, "NSThread start");
    } else {
        spawn(this, this, runtime::sel("main"), 0, "NSThread subclass start");
    }
}

thread_local! {
    static CURRENT_THREAD: std::cell::Cell<Id> = const { std::cell::Cell::new(0) };
}

extern "C" fn thread_current(_: Class, _: Sel) -> Id {
    if is_main_thread() {
        return thread_main(0, 0);
    }
    CURRENT_THREAD.with(|cell| {
        if cell.get() == 0 {
            // A thread the game started with pthread_create.
            cell.set(new_thread(0, 0, 0));
        }
        cell.get()
    })
}

extern "C" fn thread_main(_: Class, _: Sel) -> Id {
    static MAIN: std::sync::OnceLock<Id> = std::sync::OnceLock::new();
    *MAIN.get_or_init(|| new_thread(0, 0, 0))
}

extern "C" fn thread_is_main_class(_: Class, _: Sel) -> bool {
    is_main_thread()
}

extern "C" fn thread_is_main(this: Id, _: Sel) -> bool {
    this == thread_main(0, 0)
}

extern "C" fn thread_sleep(_: Class, _: Sel, seconds: f64) {
    sleep_seconds(seconds);
}

extern "C" fn thread_sleep_until(_: Class, _: Sel, date: Id) {
    sleep_seconds(runtime_send_f64(date, "timeIntervalSinceNow"));
}

extern "C" fn thread_set_priority(_: Class, _: Sel, _priority: f64) -> bool {
    true
}

extern "C" fn thread_priority(_: Class, _: Sel) -> f64 {
    0.5
}

extern "C" fn thread_dictionary(this: Id, _: Sel) -> Id {
    if let Some(d) = crate::fw::uikit::property_get(this, "threadDictionary") {
        return d as Id;
    }
    let dict = runtime::send(collections::dictionary_from(&[]), "mutableCopy", &[]);
    crate::fw::uikit::property_set(this, "threadDictionary", dict as u64);
    dict
}

extern "C" fn thread_set_name(this: Id, _: Sel, name: Id) {
    crate::fw::uikit::property_set(this, "name", name as u64);
}

extern "C" fn thread_name(this: Id, _: Sel) -> Id {
    crate::fw::uikit::property_get(this, "name").unwrap_or(0) as Id
}

extern "C" fn ignore_size(_this: Id, _: Sel, _size: usize) {}
extern "C" fn no(_this: Id, _: Sel) -> bool {
    false
}

pub fn register(r: &mut Registry) {
    let timer_class = define_class("NSTimer", "NSObject", 16);
    class_methods(
        timer_class,
        &[
            ("scheduledTimerWithTimeInterval:target:selector:userInfo:repeats:", timer_scheduled as usize),
            ("timerWithTimeInterval:target:selector:userInfo:repeats:", timer_unscheduled as usize),
        ],
    );
    methods(
        timer_class,
        &[
            ("invalidate", timer_invalidate as usize),
            ("isValid", timer_is_valid as usize),
            ("userInfo", timer_user_info as usize),
            ("timeInterval", timer_interval as usize),
            ("fire", timer_fire as usize),
        ],
    );
    let link = define_class("CADisplayLink", "NSObject", 16);
    class_methods(link, &[("displayLinkWithTarget:selector:", display_link_with_target as usize)]);
    methods(
        link,
        &[
            ("addToRunLoop:forMode:", display_link_add_to_run_loop as usize),
            ("removeFromRunLoop:forMode:", display_link_remove as usize),
            ("invalidate", timer_invalidate as usize),
            ("setFrameInterval:", display_link_set_frame_interval as usize),
            ("frameInterval", display_link_frame_interval as usize),
            ("setPaused:", display_link_set_paused as usize),
            ("isPaused", display_link_paused as usize),
            ("timestamp", display_link_timestamp as usize),
            ("duration", display_link_duration as usize),
        ],
    );

    let run_loop = define_class("NSRunLoop", "NSObject", 16);
    class_methods(run_loop, &[("currentRunLoop", run_loop_current as usize), ("mainRunLoop", run_loop_main as usize)]);
    methods(
        run_loop,
        &[
            ("addTimer:forMode:", run_loop_add_timer as usize),
            ("runUntilDate:", run_loop_run_until as usize),
            ("runMode:beforeDate:", run_loop_run_mode as usize),
            ("run", run_loop_run as usize),
            ("getCFRunLoop", run_loop_get_cf as usize),
        ],
    );
    r.function("CFRunLoopGetCurrent", cf_run_loop_get_current as usize);
    r.function("CFRunLoopGetMain", cf_run_loop_get_main as usize);
    r.function("CFRunLoopRunInMode", cf_run_loop_run_in_mode as usize);

    let object = runtime::class_named("NSObject");
    methods(
        object,
        &[
            ("performSelectorOnMainThread:withObject:waitUntilDone:", perform_on_main as usize),
            ("performSelectorOnMainThread:withObject:waitUntilDone:modes:", perform_on_main_modes as usize),
            ("performSelector:withObject:afterDelay:", perform_after_delay as usize),
            ("performSelector:withObject:afterDelay:inModes:", perform_after_delay_modes as usize),
            ("performSelectorInBackground:withObject:", perform_in_background as usize),
        ],
    );
    class_methods(
        object,
        &[
            ("cancelPreviousPerformRequestsWithTarget:", cancel_previous_requests as usize),
            ("cancelPreviousPerformRequestsWithTarget:selector:object:", cancel_previous_matching as usize),
        ],
    );

    let thread = define_class("NSThread", "NSObject", 40);
    class_methods(
        thread,
        &[
            ("detachNewThreadSelector:toTarget:withObject:", thread_detach as usize),
            ("currentThread", thread_current as usize),
            ("mainThread", thread_main as usize),
            ("isMainThread", thread_is_main_class as usize),
            ("sleepForTimeInterval:", thread_sleep as usize),
            ("sleepUntilDate:", thread_sleep_until as usize),
            ("setThreadPriority:", thread_set_priority as usize),
            ("threadPriority", thread_priority as usize),
            ("isMultiThreaded", yes_class as usize),
        ],
    );
    methods(
        thread,
        &[
            ("initWithTarget:selector:object:", thread_init_with_target as usize),
            ("start", thread_start as usize),
            ("isMainThread", thread_is_main as usize),
            ("threadDictionary", thread_dictionary as usize),
            ("setName:", thread_set_name as usize),
            ("name", thread_name as usize),
            ("setStackSize:", ignore_size as usize),
            ("setThreadPriority:", thread_set_priority as usize),
            ("isCancelled", no as usize),
            ("isFinished", no as usize),
        ],
    );
}

extern "C" fn yes_class(_: Class, _: Sel) -> bool {
    true
}

extern "C" fn display_link_remove(this: Id, _: Sel, _loop: Id, _mode: Id) {
    timer(this).valid = false;
}
