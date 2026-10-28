//! Threads, timers, the display link and the main thread's run loop.

use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::{collections, string};
use crate::objc::runtime::{self, lock, Class, Id, Sel};
use crate::symbols::Registry;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

/// Runs a wait without the guest lock: a 32-bit game's thread waiting here
/// must let the threads it waits for run game code (GIL mode: Unicorn, the
/// checker, ARM32_PARALLEL=0). With threads running in parallel, just waits.
fn waiting<R>(wait: impl FnOnce() -> R) -> R {
    use arm32_bridge::runtime::{gil_held, parallel, without_gil};
    if !parallel() && gil_held() {
        without_gil(wait)
    } else {
        wait()
    }
}

fn now() -> f64 {
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) };
    t.tv_sec as f64 + t.tv_nsec as f64 / 1e9
}

// ----- run loops: one per thread, the main one also takes posted work -----

pub enum Work {
    /// A block for the main queue (called; the poster keeps its copy).
    Block(usize),
    /// The dispatch layer's work for the main queue (dispatch_async/sync).
    Queue(Box<dyn FnOnce() + Send>),
    /// Our own work for the main thread.
    Native(Box<dyn FnOnce() + Send>),
    /// `modes`: the run loop modes it may run in (None: the common modes).
    Message { target: Id, selector: Sel, argument: Id, done: Option<Arc<(Mutex<bool>, Condvar)>>, modes: Option<Vec<String>> },
}

const DEFAULT_MODE: &str = "kCFRunLoopDefaultMode";
const COMMON_MODES: &str = "kCFRunLoopCommonModes";

/// The modes kCFRunLoopCommonModes stands for (UIKit adds event tracking).
fn is_common_mode(mode: &str) -> bool {
    matches!(mode, DEFAULT_MODE | "UITrackingRunLoopMode" | COMMON_MODES)
}

/// Whether a loop running in `mode` runs `work`: messages wait for one of
/// their modes, main-queue blocks run in the common modes (CFRunLoop serves
/// the dispatch port only there), everything else runs in any.
fn runs_in(work: &Work, mode: &str) -> bool {
    match work {
        Work::Message { modes: Some(modes), .. } => modes.iter().any(|m| m == mode || (m == COMMON_MODES && is_common_mode(mode))),
        Work::Message { modes: None, .. } | Work::Block(_) | Work::Queue(_) => is_common_mode(mode),
        Work::Native(_) => true,
    }
}

thread_local! {
    /// A main-queue block is running on this thread (libdispatch's
    /// main_q_is_draining): a run loop nested in it runs no other.
    static MAIN_QUEUE_DRAINING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether a turn whose drain began at `marker` runs work number `number`
/// now. As Apple's _dispatch_main_queue_callback_4CF, a turn runs only the
/// main-queue blocks queued before its drain began (ones they queue wait for
/// the next turn, after the commit), and none while one is running.
fn runs_now(number: u64, work: &Work, mode: &str, marker: u64) -> bool {
    let main_queue = matches!(work, Work::Block(_) | Work::Queue(_));
    runs_in(work, mode) && !(main_queue && (number > marker || MAIN_QUEUE_DRAINING.with(|d| d.get())))
}

/// A run loop mode argument (nil: the default mode).
fn mode_name(mode: Id) -> String {
    if mode == 0 { DEFAULT_MODE.to_string() } else { string::get(mode) }
}

pub struct Loop {
    /// Work with its number, in the order it was queued.
    queue: Mutex<VecDeque<(u64, Work)>>,
    /// The number of the work queued last (counted under the queue lock).
    queued: AtomicU64,
    timers: Mutex<Vec<Id>>,
    wake: Condvar,
    /// Set once its thread has run it (the main loop always counts).
    ran: std::sync::atomic::AtomicBool,
    /// Set when its thread ended: it takes no more work.
    ended: std::sync::atomic::AtomicBool,
    /// The mode it is running in (-currentMode), None while it isn't running.
    mode: Mutex<Option<String>>,
    /// For tests: counts as the main loop (the scroll tracking switch applies).
    #[cfg(test)]
    test_main: std::sync::atomic::AtomicBool,
}

impl Loop {
    const fn new() -> Self {
        Loop {
            queue: Mutex::new(VecDeque::new()),
            queued: AtomicU64::new(0),
            timers: Mutex::new(Vec::new()),
            wake: Condvar::new(),
            ran: std::sync::atomic::AtomicBool::new(false),
            ended: std::sync::atomic::AtomicBool::new(false),
            mode: Mutex::new(None),
            #[cfg(test)]
            test_main: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// For tests: counts as the main loop, so the scroll tracking switch
    /// applies to it (a loop of its own, away from other tests' turns).
    #[cfg(test)]
    fn mark_main_for_test(&'static self) {
        self.test_main.store(true, Ordering::Relaxed);
    }

    /// Queues work, unless its thread ended (then the work is let go of).
    fn push(&self, work: Work) {
        let mut queue = self.queue.lock().unwrap();
        if self.ended.load(Ordering::Relaxed) {
            drop(queue);
            return discard(work);
        }
        let number = self.queued.fetch_add(1, Ordering::SeqCst) + 1;
        queue.push_back((number, work));
        drop(queue);
        self.wake.notify_all();
    }

    /// Whether work posted here will be run: the main loop, or a loop its
    /// thread has run at least once.
    pub fn runs(&self) -> bool {
        std::ptr::eq(self, &MAIN_LOOP) || self.ran.load(Ordering::Relaxed)
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
    MAIN_LOOP.push(work);
}

/// Our own work for the main thread, each at its time (CLOCK_MONOTONIC
/// seconds, the clock of CACurrentMediaTime).
static MAIN_LATER: Mutex<Vec<(f64, Box<dyn FnOnce() + Send>)>> = Mutex::new(Vec::new());

/// Queues work for the main thread's run loop to run `seconds` from now
/// (Core Animation's timing: animations ending, transaction completions).
pub fn post_after(seconds: f64, work: Box<dyn FnOnce() + Send>) {
    MAIN_LATER.lock().unwrap().push((now() + seconds.max(0.0), work));
    MAIN_LOOP.notify();
}

/// Runs the main loop's later work that is due; when the next is due.
fn run_due_later() -> (bool, Option<f64>) {
    let current = now();
    let due: Vec<Box<dyn FnOnce() + Send>> = {
        let mut list = MAIN_LATER.lock().unwrap();
        list.sort_by(|a, b| a.0.total_cmp(&b.0));
        let count = list.iter().take_while(|(at, _)| *at <= current).count();
        list.drain(..count).map(|(_, work)| work).collect()
    };
    let ran = !due.is_empty();
    for work in due {
        work();
    }
    (ran, MAIN_LATER.lock().unwrap().iter().map(|(at, _)| *at).reduce(f64::min))
}

/// The calling thread's run loop, for frameworks that answer on the run
/// loop of the thread that made an object (Core Location does).
pub fn this_threads_loop() -> &'static Loop {
    current_loop()
}

/// Queues work for a given run loop; it runs when that thread runs its loop.
pub fn post_to(run_loop: &'static Loop, work: Work) {
    run_loop.push(work);
}

/// Queues work for a run loop object (CFRunLoopRef) the game handed over.
pub fn post_to_object(run_loop: Id, work: Work) {
    if run_loop == 0 || run_loop == main_run_loop() {
        return post(work);
    }
    let target = unsafe { &**((run_loop + 8) as *const *const Loop) };
    target.push(work);
}

/// A message for a run loop. As on Apple, its receiver and argument are
/// retained until it has run (through their own retain: a game's objects
/// count it in the game's runtime).
fn message(target: Id, selector: Sel, argument: Id, done: Option<Arc<(Mutex<bool>, Condvar)>>, modes: Option<Vec<String>>) -> Work {
    for object in [target, argument] {
        if object != 0 {
            runtime::send(object, "retain", &[]);
        }
    }
    Work::Message { target, selector, argument, done, modes }
}

fn run_work(work: Work) {
    match work {
        Work::Block(block) => as_main_queue(|| unsafe { crate::libsystem::mach::call_block(block) }),
        Work::Queue(work) => as_main_queue(work),
        Work::Native(work) => work(),
        Work::Message { target, selector, argument, done, .. } => {
            let name = lock().sel_name(selector);
            runtime::send(target, &name, &[argument]);
            message_over(target, argument, done);
        }
    }
}

/// Runs a main-queue block, marked as draining the main queue meanwhile.
fn as_main_queue(work: impl FnOnce()) {
    /// Puts the flag back when the block ends, even by unwinding.
    struct Draining(bool);
    impl Drop for Draining {
        fn drop(&mut self) {
            MAIN_QUEUE_DRAINING.with(|d| d.set(self.0));
        }
    }
    let _draining = Draining(MAIN_QUEUE_DRAINING.with(|d| d.replace(true)));
    work();
}

/// A message ran or never will: let go of what it held while it waited (see
/// `message`), and wake the caller waiting for it.
fn message_over(target: Id, argument: Id, done: Option<Arc<(Mutex<bool>, Condvar)>>) {
    for object in [target, argument] {
        if object != 0 {
            runtime::send(object, "release", &[]);
        }
    }
    if let Some(done) = done {
        *done.0.lock().unwrap() = true;
        done.1.notify_all();
    }
}

/// Work for a thread that ended. Blocks and our own work are dropped.
fn discard(work: Work) {
    if let Work::Message { target, argument, done, .. } = work {
        message_over(target, argument, done);
    }
}

/// When a started thread ends: its run loop takes no more work, and what was
/// still queued is let go of; its timers end too, letting go of their
/// targets (Apple's run loop goes with its thread). A thread that never had
/// one gets one marked ended (under the lock loops are made with), so work
/// sent to it later is let go of at once too.
fn end_thread_loop(thread: Id) {
    let run_loop = loop_of(run_loop_of_thread(thread));
    let left: Vec<Work> = {
        let mut queue = run_loop.queue.lock().unwrap();
        run_loop.ended.store(true, Ordering::Relaxed);
        queue.drain(..).map(|(_, work)| work).collect()
    };
    left.into_iter().for_each(discard);
    let timers = std::mem::take(&mut *run_loop.timers.lock().unwrap());
    for object in timers {
        let held = let_go(&mut timer(object));
        release_held(held);
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
    /// CADisplayLink: the frame interval the game asked for.
    pub frames: isize,
    /// A CADisplayLink: fires on the screen's refreshes (vsync.rs).
    pub display_link: bool,
    /// The run loop modes it fires in (given when it is scheduled).
    pub modes: Vec<String>,
    /// It still holds its target and user info (retained when made, as
    /// Apple's; let go of when it is done: see `let_go`).
    pub holds: bool,
    /// Its call is running (a loop run inside it doesn't fire it again).
    pub firing: bool,
}

/// A timer is done (invalidated, cancelled, or fired for the last time):
/// it fires no more, and what it held is handed back to be released once.
fn let_go(state: &mut TimerState) -> Option<(Id, Id)> {
    state.valid = false;
    std::mem::take(&mut state.holds).then_some((state.target, state.user_info))
}

/// Releases what a done timer held (outside its lock: a release may run
/// the game's code).
fn release_held(held: Option<(Id, Id)>) {
    for object in held.map(|(target, info)| [target, info]).into_iter().flatten() {
        if object != 0 {
            runtime::send(object, "release", &[]);
        }
    }
}

/// How many valid scheduled timers (delayed performs, display links) hold
/// `object` as target or user info (for the "would dealloc" log, counted.rs).
pub fn timers_holding(object: Id) -> usize {
    let mut loops: Vec<&'static Loop> = vec![&MAIN_LOOP];
    loops.extend(THREAD_RUN_LOOPS.lock().unwrap().iter().map(|&(_, run_loop)| loop_of(run_loop)));
    loops
        .into_iter()
        .map(|run_loop| {
            let list = run_loop.timers.lock().unwrap().clone();
            list.into_iter()
                .filter(|&t| {
                    let state = timer(t);
                    state.valid && (state.target == object || state.user_info == object)
                })
                .count()
        })
        .sum()
}

/// Whether a timer in `modes` fires while its loop runs in `mode`.
fn timer_runs_in(modes: &[String], mode: &str) -> bool {
    modes.iter().any(|m| m == mode || (m == COMMON_MODES && is_common_mode(mode)))
}

const TRACKING_MODE: &str = "UITrackingRunLoopMode";

/// Whether a loop's turn in `mode` fires a timer in `modes`. Apple switches
/// the main loop into UITrackingRunLoopMode while a scroll view tracks (see
/// `run_loop_until`), so a default-mode timer waits during a drag, as on
/// Apple.
fn timer_fires_in_turn(modes: &[String], mode: &str) -> bool {
    timer_runs_in(modes, mode)
}

/// What a new timer holds: its target and user info, retained (through
/// their own retain: a game's objects count it in the game's runtime).
fn hold(target: Id, user_info: Id) {
    for object in [target, user_info] {
        if object != 0 {
            runtime::send(object, "retain", &[]);
        }
    }
}

pub struct Timer(Mutex<TimerState>);

static TIMERS_FIRED: AtomicUsize = AtomicUsize::new(0);

fn timer(object: Id) -> std::sync::MutexGuard<'static, TimerState> {
    let slot = unsafe { &*((object + 8) as *const std::sync::atomic::AtomicPtr<Timer>) };
    let mut state = slot.load(Ordering::Acquire);
    if state.is_null() {
        // Made with plain alloc/init, not one of the timer makers: a timer
        // that never fires (Apple's answers to it likewise do nothing).
        let blank = TimerState {
            fire_at: f64::INFINITY,
            interval: 0.0,
            repeats: false,
            target: 0,
            selector: 0,
            user_info: 0,
            valid: false,
            owner: None,
            paused: false,
            pass_argument: false,
            frames: 1,
            display_link: false,
            modes: Vec::new(),
            holds: false,
            firing: false,
        };
        let made = Box::into_raw(Box::new(Timer(Mutex::new(blank))));
        state = match slot.compare_exchange(std::ptr::null_mut(), made, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => made,
            Err(other) => {
                drop(unsafe { Box::from_raw(made) });
                other
            }
        };
    }
    unsafe { (*state).0.lock().unwrap() }
}

/// How many calls the main thread has asked for at once (timers and delayed
/// performs for 0 s), so a game's frame can tell its step asked for one.
static AT_ONCE: AtomicU64 = AtomicU64::new(0);

pub fn asked_at_once() -> u64 {
    AT_ONCE.load(Ordering::Relaxed)
}

fn make_timer(class: Class, interval: f64, target: Id, selector: Sel, user_info: Id, repeats: bool) -> Id {
    let object = runtime::alloc_instance(class);
    // The timer holds on to its target and user info (performSelector:afterDelay:'s argument).
    hold(target, user_info);
    // As NSTimer's: 0 s or less is 0.1 ms.
    if interval <= 0.0 && is_main_thread() {
        AT_ONCE.fetch_add(1, Ordering::Relaxed);
    }
    let interval = if interval <= 0.0 { 0.0001 } else { interval };
    let state = TimerState {
        fire_at: now() + interval,
        interval,
        repeats,
        target,
        selector,
        user_info,
        valid: true,
        owner: None,
        paused: false,
        pass_argument: false,
        frames: 1,
        display_link: false,
        modes: Vec::new(),
        holds: true,
        firing: false,
    };
    unsafe { *((object + 8) as *mut *const Timer) = Box::into_raw(Box::new(Timer(Mutex::new(state)))) };
    object
}

/// Adds a timer to a run loop in `mode` (once on a loop; another mode on the
/// same loop is added to its modes). As CFRunLoop's, a timer is on one run
/// loop only: adding it to another while it is on one does nothing.
fn schedule(object: Id, run_loop: &'static Loop, mode: &str) {
    {
        let mut state = timer(object);
        if !state.valid || state.owner.is_some_and(|owner| !std::ptr::eq(owner, run_loop)) {
            return;
        }
        if !state.modes.iter().any(|m| m == mode) {
            state.modes.push(mode.to_string());
        }
        if state.owner.is_some() {
            drop(state);
            run_loop.notify();
            return;
        }
        state.owner = Some(run_loop);
    }
    run_loop.timers.lock().unwrap().push(object);
    run_loop.notify();
}

extern "C" fn timer_scheduled(class: Class, _: Sel, interval: f64, target: Id, selector: Sel, info: Id, repeats: u8) -> Id {
    let object = make_timer(class, interval, target, selector, info, repeats != 0);
    schedule(object, current_loop(), DEFAULT_MODE);
    object
}

extern "C" fn timer_unscheduled(class: Class, _: Sel, interval: f64, target: Id, selector: Sel, info: Id, repeats: u8) -> Id {
    make_timer(class, interval, target, selector, info, repeats != 0)
}

/// A timer made with alloc: first fires at `date` (not scheduled until added to a run loop).
extern "C" fn timer_init_with_fire_date(this: Id, _: Sel, date: Id, interval: f64, target: Id, selector: Sel, info: Id, repeats: u8) -> Id {
    hold(target, info);
    let wait = if date == 0 { 0.0 } else { runtime_send_f64(date, "timeIntervalSinceNow") };
    let state = TimerState {
        fire_at: now() + wait.max(0.0),
        interval: if interval <= 0.0 { 0.0001 } else { interval },
        repeats: repeats != 0,
        target,
        selector,
        user_info: info,
        valid: true,
        owner: None,
        paused: false,
        pass_argument: false,
        frames: 1,
        display_link: false,
        modes: Vec::new(),
        holds: true,
        firing: false,
    };
    let made = Box::into_raw(Box::new(Timer(Mutex::new(state))));
    let slot = unsafe { &*((this + 8) as *const std::sync::atomic::AtomicPtr<Timer>) };
    let old = slot.swap(made, Ordering::AcqRel);
    if !old.is_null() {
        let old = unsafe { Box::from_raw(old) };
        let held = {
            let mut state = old.0.lock().unwrap();
            let_go(&mut state)
        };
        release_held(held);
    }
    this
}

extern "C" fn timer_fire_date(this: Id, _: Sel) -> Id {
    let at = timer(this).fire_at;
    crate::fw::misc::date(crate::fw::misc::now_since_2001() + (at - now()))
}

/// Moves the next firing (a scheduled timer is woken to notice).
extern "C" fn timer_set_fire_date(this: Id, _: Sel, date: Id) {
    let wait = if date == 0 { 0.0 } else { runtime_send_f64(date, "timeIntervalSinceNow") };
    let owner = {
        let mut state = timer(this);
        state.fire_at = now() + wait.max(0.0);
        state.owner
    };
    if let Some(run_loop) = owner {
        run_loop.notify();
    }
}

extern "C" fn timer_invalidate(this: Id, _: Sel) {
    let held = let_go(&mut timer(this));
    release_held(held);
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
    let (target, selector, last) = {
        let mut state = timer(this);
        if !state.valid {
            return;
        }
        if !state.repeats {
            state.valid = false;
        }
        (state.target, state.selector, !state.repeats)
    };
    let name = lock().sel_name(selector);
    // Held through the call (see fire_due_timers).
    hold(target, this);
    runtime::send(target, &name, &[this]);
    release_held(Some((target, this)));
    // A one-shot timer lets go of its target after its call.
    if last {
        let held = let_go(&mut timer(this));
        release_held(held);
    }
}

// CADisplayLink: a timer at the screen's top refresh rate (IOS_SCREEN_HZ,
// 120 on the Pixel; 60 when unknown), so no game is held to the 60 Hz of the
// iPhones it was made for. IOS_REFRESH_HZ=<n> tries another rate.
pub(crate) fn refresh_seconds() -> f64 {
    static SECONDS: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *SECONDS.get_or_init(|| {
        let hz = refresh_hz(std::env::var("IOS_REFRESH_HZ").ok().as_deref(), std::env::var("IOS_SCREEN_HZ").ok().as_deref());
        crate::log!("graphics: display link at {hz} Hz");
        1.0 / hz
    })
}

/// The display link's rate: the test switch, else the screen's, else 60.
fn refresh_hz(test_switch: Option<&str>, screen: Option<&str>) -> f64 {
    let valid = |text: Option<&str>| text.and_then(|t| t.trim().parse::<f64>().ok()).filter(|&hz| hz >= 1.0);
    valid(test_switch).or(valid(screen)).unwrap_or(60.0)
}

/// Seconds between display link calls when the game asks for every
/// `frames`-th screen refresh. Games asked for 2 to get 30 fps (Unity's
/// default); frame rates are unlocked, so every refresh calls the game
/// unless IOS_FRAME_INTERVAL=1 keeps its choice.
fn link_interval(refresh: f64, frames: isize, keep_game_choice: bool) -> f64 {
    if keep_game_choice { refresh * frames.max(1) as f64 } else { refresh }
}

fn keep_frame_interval() -> bool {
    static KEEP: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *KEEP.get_or_init(|| std::env::var("IOS_FRAME_INTERVAL").is_ok_and(|v| v == "1"))
}

extern "C" fn display_link_with_target(class: Class, _: Sel, target: Id, selector: Sel) -> Id {
    let link = make_timer(class, refresh_seconds(), target, selector, 0, true);
    let mut state = timer(link);
    state.display_link = true;
    let (origin, step) = crate::fw::vsync::grid();
    state.fire_at = crate::fw::vsync::next_on_grid(origin, step, now());
    drop(state);
    link
}

extern "C" fn display_link_add_to_run_loop(this: Id, _: Sel, run_loop: Id, mode: Id) {
    schedule(this, loop_of(run_loop), &mode_name(mode));
}

extern "C" fn display_link_set_frame_interval(this: Id, _: Sel, frames: isize) {
    let mut state = timer(this);
    state.frames = frames.max(1);
    state.interval = link_interval(refresh_seconds(), frames, keep_frame_interval());
}

/// What the game asked for (it may base its own timing on it).
extern "C" fn display_link_frame_interval(this: Id, _: Sel) -> isize {
    timer(this).frames
}

/// iOS 10's way to ask for a lower rate; unlocked like frameInterval.
extern "C" fn display_link_set_preferred_fps(this: Id, _: Sel, fps: isize) {
    if fps > 0 && keep_frame_interval() {
        timer(this).interval = (1.0 / fps as f64).max(refresh_seconds());
    }
}

extern "C" fn display_link_set_paused(this: Id, _: Sel, paused: u8) {
    timer(this).paused = paused != 0;
}

extern "C" fn display_link_paused(this: Id, _: Sel) -> bool {
    timer(this).paused
}

/// The refresh this call is for (the last one), as an iPhone gives it.
extern "C" fn display_link_timestamp(_this: Id, _: Sel) -> f64 {
    let (origin, step) = crate::fw::vsync::grid();
    crate::fw::vsync::last_on_grid(origin, step, now())
}

/// The refresh the frame drawn now is shown at (the next one).
extern "C" fn display_link_target_timestamp(this: Id, sel: Sel) -> f64 {
    display_link_timestamp(this, sel) + crate::fw::vsync::grid().1
}

extern "C" fn display_link_duration(_this: Id, _: Sel) -> f64 {
    crate::fw::vsync::grid().1
}

/// Fires this loop's due timers for `mode`, earliest fire date first (as
/// CFRunLoop, which keeps them sorted). Returns how many fired and when the
/// next one is due.
fn fire_due_timers(run_loop: &'static Loop, mode: &str) -> (usize, Option<f64>) {
    let due: Vec<Id> = {
        let mut list = run_loop.timers.lock().unwrap();
        list.retain(|&t| timer(t).valid);
        let current = now();
        let mut due: Vec<(f64, Id)> = list
            .iter()
            .filter_map(|&t| {
                let state = timer(t);
                (!state.paused && !state.firing && state.fire_at <= current && timer_fires_in_turn(&state.modes, mode)).then_some((state.fire_at, t))
            })
            .collect();
        due.sort_by(|a, b| a.0.total_cmp(&b.0));
        due.into_iter().map(|(_, t)| t).collect()
    };
    let mut fired = 0;
    for object in due {
        // An earlier timer's callback may have stopped or paused this one.
        let (target, selector, argument, last, was_due) = {
            let mut state = timer(object);
            let current = now();
            if !state.valid || state.paused || state.firing || state.fire_at > current {
                continue;
            }
            let was_due = state.fire_at;
            if state.repeats {
                if state.display_link {
                    // The refresh its frame interval's worth of refreshes on,
                    // however late this call came.
                    let (origin, step) = crate::fw::vsync::grid();
                    state.fire_at = crate::fw::vsync::next_on_grid(origin, step, current + (state.interval - step).max(0.0));
                } else {
                    // Its next time is worked out once the call returns.
                    state.firing = true;
                }
            } else {
                state.valid = false;
            }
            let argument = if state.pass_argument { state.user_info } else { object };
            (state.target, state.selector, argument, !state.repeats, was_due)
        };
        fired += 1;
        TIMERS_FIRED.fetch_add(1, Ordering::Relaxed);
        let name = lock().sel_name(selector);
        // Held through the call, as __CFRunLoopDoTimer retains the timer's
        // context: a callback that invalidates its timer can't free its target.
        hold(target, argument);
        runtime::send(target, &name, &[argument]);
        {
            // As __CFRunLoopDoTimer: a repeating timer's next time is the
            // first on its schedule (from the time it was due) after the call
            // returned, unless the call set a later one, which is kept. So
            // what the call asked for at once comes before its next call.
            let mut state = timer(object);
            if state.firing {
                state.firing = false;
                if state.fire_at <= was_due {
                    let current = now();
                    state.fire_at = if current < was_due { was_due } else { crate::fw::vsync::next_on_grid(was_due, state.interval, current) };
                }
            }
        }
        release_held(Some((target, argument)));
        // A one-shot timer (a delayed perform) lets go of its target after its call.
        if last {
            let held = let_go(&mut timer(object));
            release_held(held);
        }
    }
    let list = run_loop.timers.lock().unwrap();
    let next = list
        .iter()
        .filter_map(|&t| {
            let state = timer(t);
            (state.valid && !state.paused && !state.firing && timer_fires_in_turn(&state.modes, mode)).then_some(state.fire_at)
        })
        .reduce(f64::min);
    (fired, next)
}

/// Runs `f` as the main thread would: in tests, where several test threads
/// run the main loop, one piece of its work at a time (a nested loop on the
/// same thread goes on); elsewhere only the main thread runs it anyway.
fn main_work<R>(main: bool, f: impl FnOnce() -> R) -> R {
    #[cfg(test)]
    if main {
        static TURN: Mutex<(u64, usize)> = Mutex::new((0, 0));
        static FREE: std::sync::Condvar = std::sync::Condvar::new();
        let me = unsafe { libc::pthread_self() } as u64;
        {
            let mut turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
            while turn.1 > 0 && turn.0 != me {
                turn = FREE.wait(turn).unwrap_or_else(|e| e.into_inner());
            }
            *turn = (me, turn.1 + 1);
        }
        struct Leave;
        impl Drop for Leave {
            fn drop(&mut self) {
                let mut turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
                turn.1 -= 1;
                if turn.1 == 0 {
                    FREE.notify_all();
                }
            }
        }
        let _leave = Leave;
        return f();
    }
    let _ = main;
    f()
}

/// For tests: runs `f` as main-loop work (taking turns with other test
/// threads running the main loop), as the main thread would.
#[cfg(test)]
pub fn as_main<R>(f: impl FnOnce() -> R) -> R {
    main_work(true, f)
}

#[cfg(test)]
thread_local! {
    /// For tests: runs on this thread where its loop's turn commits.
    static AT_COMMIT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

/// Runs a loop's queued work and due timers until `until` (or forever), in
/// run loop mode `mode`. With `once`, returns as soon as something was
/// handled. Returns true if anything was.
fn run_loop_until(run_loop: &'static Loop, until: Option<f64>, once: bool, mode: &str) -> bool {
    run_loop.ran.store(true, Ordering::Relaxed);
    /// Puts the mode the loop ran in before back when this run ends (runs nest).
    struct Running(&'static Loop, Option<String>);
    impl Drop for Running {
        fn drop(&mut self) {
            *self.0.mode.lock().unwrap_or_else(|e| e.into_inner()) = self.1.take();
        }
    }
    let _running = Running(run_loop, run_loop.mode.lock().unwrap_or_else(|e| e.into_inner()).replace(mode.to_string()));
    let mut handled = false;
    // Each turn of the loop has its own autorelease pool, as on Apple.
    let pool = crate::fw::counted::pool_mark();
    let main = std::ptr::eq(run_loop, &MAIN_LOOP);
    #[cfg(test)]
    let main = main || run_loop.test_main.load(Ordering::Relaxed);
    let mut running_mode = mode;
    loop {
        // UIKit runs the main run loop in UITrackingRunLoopMode while a
        // scroll view tracks a finger (or its coast/settle): then only
        // timers in that mode or the common modes fire, and -currentMode
        // answers it. A loop a game asked for another mode keeps that mode.
        let turn_mode = if main && mode == DEFAULT_MODE && crate::fw::controls::tracking() { TRACKING_MODE } else { mode };
        if turn_mode != running_mode {
            *run_loop.mode.lock().unwrap_or_else(|e| e.into_inner()) = Some(turn_mode.to_string());
            running_mode = turn_mode;
        }
        // A game frame that waited for a newer one reaches the screen (eagl.rs).
        if main {
            main_work(main, crate::fw::eagl::show_waiting_frame);
        }
        // The main-queue blocks queued from here on wait for the next turn.
        let marker = run_loop.queued.load(Ordering::SeqCst);
        loop {
            let ran = main_work(main, || {
                let work = {
                    let mut queue = run_loop.queue.lock().unwrap();
                    queue.iter().position(|(number, work)| runs_now(*number, work, turn_mode, marker)).and_then(|at| queue.remove(at))
                };
                work.map(|(_, work)| run_work(work)).is_some()
            });
            if !ran {
                break;
            }
            handled = true;
        }
        let (fired, mut next_timer) = main_work(main, || fire_due_timers(run_loop, turn_mode));
        handled |= fired > 0;
        if main {
            let (ran, next_later) = main_work(main, run_due_later);
            handled |= ran;
            next_timer = match (next_timer, next_later) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            // UIKit lays out marked views once a turn, before anything is
            // drawn: in Core Animation's commit (layout_and_display_if_needed,
            // at BeforeWaiting), so what this turn's work, timers, display
            // link and completion blocks marked is laid out together.
            main_work(main, crate::fw::uikit::layout_pass);
        }
        if once && handled {
            crate::fw::counted::pool_drain(pool);
            if main {
                main_work(main, crate::fw::counted::dealloc_pending);
            }
            return true;
        }
        // The game's UIKit pictures go to the phone screen from the main thread.
        let pictures = std::ptr::eq(run_loop, &MAIN_LOOP) && crate::fw::pictures::active();
        // The turn is done: what it animated begins now (Apple's commit;
        // with pictures, once their new pictures are made, in their tick).
        if main && !pictures {
            main_work(main, crate::fw::animation::commit);
        }
        if pictures {
            crate::fw::pictures::tick();
        }
        #[cfg(test)]
        if let Some(check) = AT_COMMIT.with(|c| c.borrow_mut().take()) {
            check();
        }
        // The turn's pool goes after the commit (UIKit's pool observer comes
        // after Core Animation's), so what only the pool holds lives through it.
        crate::fw::counted::pool_drain(pool);
        if main {
            main_work(main, crate::fw::counted::dealloc_pending);
        }
        let current = now();
        if let Some(end) = until {
            if current >= end {
                return handled;
            }
        }
        let mut wait_until = next_timer.unwrap_or(current + 0.25);
        if pictures {
            wait_until = wait_until.min(current + crate::fw::pictures::gap());
        }
        if main {
            if let Some(due) = main_work(main, crate::fw::eagl::show_waiting_frame) {
                wait_until = wait_until.min(due);
            }
        }
        if let Some(end) = until {
            wait_until = wait_until.min(end);
        }
        let guard = run_loop.queue.lock().unwrap();
        if !guard.iter().any(|(number, work)| runs_now(*number, work, turn_mode, u64::MAX)) && wait_until > current {
            let seconds = (wait_until - current).min(0.25);
            waiting(|| drop(run_loop.wake.wait_timeout(guard, std::time::Duration::from_secs_f64(seconds))));
        }
    }
}

/// Runs the main thread's loop until `until` (or forever).
pub fn run_main_loop(until: Option<f64>) {
    run_loop_until(&MAIN_LOOP, until, false, DEFAULT_MODE);
}

/// For tests: runs the main loop's queued work (one round, at most a moment).
#[cfg(test)]
pub fn run_main_loop_briefly() {
    run_loop_until(&MAIN_LOOP, Some(now() + 0.05), true, DEFAULT_MODE);
}

/// For tests: runs the calling thread's loop once (at most a moment).
#[cfg(test)]
pub fn run_this_threads_loop_briefly() {
    run_loop_until(current_loop(), Some(now() + 0.02), true, DEFAULT_MODE);
}

pub fn timers_fired() -> usize {
    TIMERS_FIRED.load(Ordering::Relaxed)
}

// ----- NSRunLoop / CFRunLoop: { isa, &'static Loop } -----

static MAIN_RUN_LOOP: std::sync::OnceLock<Id> = std::sync::OnceLock::new();

/// Each thread object's run loop object (made when first needed, by the
/// thread itself or by work sent to it); the main thread's is MAIN_LOOP's.
static THREAD_RUN_LOOPS: Mutex<Vec<(Id, Id)>> = Mutex::new(Vec::new());

fn run_loop_of_thread(thread: Id) -> Id {
    if thread == 0 || thread == thread_main(0, 0) {
        return main_run_loop();
    }
    let mut loops = THREAD_RUN_LOOPS.lock().unwrap();
    if let Some(&(_, object)) = loops.iter().find(|(t, _)| *t == thread) {
        return object;
    }
    // Lives as long as the program: the game may keep the object.
    let made: &'static Loop = Box::leak(Box::new(Loop::new()));
    let object = run_loop_object(made);
    loops.push((thread, object));
    object
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
    run_loop_of_thread(thread_current(0, 0))
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

/// The objects given out for modes Foundation doesn't export, one per name.
static MODE_OBJECTS: Mutex<Vec<(String, Id)>> = Mutex::new(Vec::new());

/// The object for a mode name: Foundation's own constant when it exports
/// one, else one cached string (games compare modes by pointer too).
fn mode_object(name: &str) -> Id {
    let symbol = if name == DEFAULT_MODE { "NSDefaultRunLoopMode" } else { name };
    if let Some((address, _)) = crate::symbols::lookup(symbol) {
        return unsafe { *(address as *const Id) };
    }
    let mut objects = MODE_OBJECTS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&(_, object)) = objects.iter().find(|(text, _)| text == name) {
        return object;
    }
    let object = string::make(name);
    objects.push((name.to_string(), object));
    object
}

/// -currentMode: the same object every time for a mode, and nil while the
/// loop isn't running.
extern "C" fn run_loop_current_mode(this: Id, _: Sel) -> Id {
    let mode = loop_of(this).mode.lock().unwrap_or_else(|e| e.into_inner()).clone();
    mode.as_deref().map_or(0, mode_object)
}

extern "C" fn run_loop_add_timer(this: Id, _: Sel, timer_object: Id, mode: Id) {
    schedule(timer_object, loop_of(this), &mode_name(mode));
}

fn sleep_seconds(seconds: f64) {
    if seconds > 0.0 {
        waiting(|| std::thread::sleep(std::time::Duration::from_secs_f64(seconds.min(3600.0))));
    }
}

fn date_deadline(date: Id) -> f64 {
    let seconds = if date == 0 { 0.0 } else { runtime_send_f64(date, "timeIntervalSinceNow") };
    now() + seconds.clamp(0.0, 1e9)
}

extern "C" fn run_loop_run_until(this: Id, _: Sel, date: Id) {
    run_loop_until(loop_of(this), Some(date_deadline(date)), false, DEFAULT_MODE);
}

/// runMode:beforeDate: handles at most one round of work, waiting until the date.
extern "C" fn run_loop_run_mode(this: Id, _: Sel, mode: Id, date: Id) -> bool {
    run_loop_until(loop_of(this), Some(date_deadline(date)), true, &mode_name(mode));
    true
}

extern "C" fn run_loop_run(this: Id, _: Sel) {
    run_loop_until(loop_of(this), None, false, DEFAULT_MODE);
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
extern "C" fn cf_run_loop_run_in_mode(mode: Id, seconds: f64, return_after: u8) -> i32 {
    const TIMED_OUT: i32 = 3;
    const HANDLED_SOURCE: i32 = 4;
    let until = now() + seconds.clamp(0.0, 1e9);
    if run_loop_until(current_loop(), Some(until), return_after != 0, &mode_name(mode)) && return_after != 0 {
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
    perform_on_main_in(this, selector, argument, wait, None);
}

fn perform_on_main_in(this: Id, selector: Sel, argument: Id, wait: u8, modes: Option<Vec<String>>) {
    if is_main_thread() && wait != 0 {
        let name = lock().sel_name(selector);
        runtime::send(this, &name, &[argument]);
        return;
    }
    if wait != 0 {
        let done = Arc::new((Mutex::new(false), Condvar::new()));
        post(message(this, selector, argument, Some(done.clone()), modes));
        wait_until_done(&done);
    } else {
        post(message(this, selector, argument, None, modes));
    }
}

/// -performSelector:onThread:withObject:waitUntilDone:: on the calling
/// thread and waiting, done at once; otherwise queued on that thread's run
/// loop (run when the thread runs it), the caller waiting for it if asked.
extern "C" fn perform_on_thread(this: Id, _: Sel, selector: Sel, thread: Id, argument: Id, wait: u8) {
    perform_on_thread_in(this, selector, thread, argument, wait, None);
}

/// The same, run only in `modes` (None: the common modes).
fn perform_on_thread_in(this: Id, selector: Sel, thread: Id, argument: Id, wait: u8, modes: Option<Vec<String>>) {
    let here = thread_current(0, 0);
    let thread = if thread == 0 { thread_main(0, 0) } else { thread };
    if thread == here && wait != 0 {
        let name = lock().sel_name(selector);
        runtime::send(this, &name, &[argument]);
        return;
    }
    let run_loop = loop_of(run_loop_of_thread(thread));
    let done = (wait != 0).then(|| Arc::new((Mutex::new(false), Condvar::new())));
    post_to(run_loop, message(this, selector, argument, done.clone(), modes));
    if let Some(done) = done {
        wait_until_done(&done);
    }
}

/// The caller's side of waitUntilDone:YES (without the guest lock, which the
/// thread doing the work may need).
fn wait_until_done(done: &(Mutex<bool>, Condvar)) {
    waiting(|| {
        let mut finished = done.0.lock().unwrap();
        while !*finished {
            finished = done.1.wait(finished).unwrap();
        }
    });
}

/// The modes: variants' list of mode names; None for nil or an empty
/// array, which Apple's methods return on without performing anything.
fn named_modes(modes: Id) -> Option<Vec<String>> {
    let names: Vec<String> = if modes == 0 { Vec::new() } else { collections::array_items(modes).into_iter().map(string::get).collect() };
    (!names.is_empty()).then_some(names)
}

extern "C" fn perform_on_thread_modes(this: Id, _: Sel, selector: Sel, thread: Id, argument: Id, wait: u8, modes: Id) {
    if let Some(modes) = named_modes(modes) {
        perform_on_thread_in(this, selector, thread, argument, wait, Some(modes));
    }
}

extern "C" fn perform_on_main_modes(this: Id, _: Sel, selector: Sel, argument: Id, wait: u8, modes: Id) {
    if let Some(modes) = named_modes(modes) {
        perform_on_main_in(this, selector, argument, wait, Some(modes));
    }
}

static DELAYED: Mutex<Vec<Id>> = Mutex::new(Vec::new());

extern "C" fn perform_after_delay(this: Id, _: Sel, selector: Sel, argument: Id, delay: f64) {
    perform_after_delay_in(this, selector, argument, delay, &[DEFAULT_MODE.to_string()]);
}

/// A delayed perform: a one-shot timer in `modes` of this thread's loop.
fn perform_after_delay_in(this: Id, selector: Sel, argument: Id, delay: f64, modes: &[String]) {
    let object = make_timer(runtime::class_named("NSTimer"), delay, this, selector, argument, false);
    timer(object).pass_argument = true;
    {
        let mut delayed = DELAYED.lock().unwrap();
        delayed.retain(|&t| timer(t).valid);
        delayed.push(object);
    }
    for mode in modes {
        schedule(object, current_loop(), mode);
    }
}

/// Whether a -performSelector:withObject:afterDelay: of `selector` to
/// `target` is still waiting to run.
pub fn perform_pending(target: Id, selector: Sel) -> bool {
    DELAYED.lock().unwrap().iter().any(|&t| {
        let state = timer(t);
        state.valid && state.target == target && state.selector == selector
    })
}

/// -performSelector:withObject:afterDelay:inModes:: nil or no modes performs
/// nothing, as Apple's.
extern "C" fn perform_after_delay_modes(this: Id, _: Sel, selector: Sel, argument: Id, delay: f64, modes: Id) {
    if let Some(modes) = named_modes(modes) {
        perform_after_delay_in(this, selector, argument, delay, &modes);
    }
}

extern "C" fn perform_in_background(this: Id, _: Sel, selector: Sel, argument: Id) {
    spawn(new_thread(this, selector, argument), this, selector, argument, "performSelectorInBackground");
}

/// +cancelPreviousPerformRequestsWithTarget: (the receiver is the class).
extern "C" fn cancel_previous_requests(_class: Class, _: Sel, target: Id) {
    cancel_delayed(|t, _, _| t == target);
}

/// +cancelPreviousPerformRequestsWithTarget:selector:object:
extern "C" fn cancel_previous_matching(_class: Class, _: Sel, target: Id, selector: Sel, argument: Id) {
    cancel_delayed(|t, s, info| t == target && s == selector && objects_equal(info, argument));
}

/// Cancels the waiting delayed performs `matches` picks (given each one's
/// target, selector and argument). `matches` may run game code (-isEqual:),
/// so it is asked with no lock held; a perform it picked is cancelled only
/// if it is still the same waiting perform. What they held is released last.
fn cancel_delayed(matches: impl Fn(Id, Sel, Id) -> bool) {
    let delayed = DELAYED.lock().unwrap().clone();
    let waiting: Vec<(Id, Id, Sel, Id)> = delayed
        .into_iter()
        .filter_map(|object| {
            let state = timer(object);
            state.valid.then_some((object, state.target, state.selector, state.user_info))
        })
        .collect();
    let held: Vec<Option<(Id, Id)>> = waiting
        .into_iter()
        .filter(|&(_, target, selector, info)| matches(target, selector, info))
        .filter_map(|(object, target, selector, info)| {
            let mut state = timer(object);
            (state.valid && state.target == target && state.selector == selector && state.user_info == info).then(|| let_go(&mut state))
        })
        .collect();
    held.into_iter().for_each(release_held);
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
    // Kept while the thread runs, as Apple's NSThread retains its target and argument.
    crate::fw::counted::keep(target);
    crate::fw::counted::keep(argument);
    crate::log!("thread started: -[{} {name}] ({why})", string::describe_class_of(target));
    std::thread::Builder::new()
        .stack_size(THREAD_STACK)
        .spawn(move || {
            crate::libsystem::threads::note_this_thread();
            CURRENT_THREAD.with(|cell| cell.set(thread));
            runtime::send(target, &name, &[argument]);
            crate::fw::counted::release(argument);
            crate::fw::counted::release(target);
            end_thread_loop(thread);
            crate::fw::uikit::property_set(thread, "finished", 1);
            // The thread's last safe point: what it let go of deallocs here.
            crate::fw::counted::dealloc_pending();
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
    let started = CURRENT_THREAD.with(|cell| cell.get());
    if started != 0 {
        return started;
    }
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

extern "C" fn thread_is_finished(this: Id, _: Sel) -> bool {
    crate::fw::uikit::property_get(this, "finished").is_some()
}

/// CACurrentMediaTime: seconds on the clock display link timestamps use.
extern "C" fn current_media_time() -> f64 {
    now()
}

/// The same clock for our own code (Core Animation's layer times).
pub fn media_time() -> f64 {
    now()
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
            ("initWithFireDate:interval:target:selector:userInfo:repeats:", timer_init_with_fire_date as usize),
            ("fireDate", timer_fire_date as usize),
            ("setFireDate:", timer_set_fire_date as usize),
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
            ("setPreferredFramesPerSecond:", display_link_set_preferred_fps as usize),
            ("setPaused:", display_link_set_paused as usize),
            ("isPaused", display_link_paused as usize),
            ("timestamp", display_link_timestamp as usize),
            ("targetTimestamp", display_link_target_timestamp as usize),
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
            ("currentMode", run_loop_current_mode as usize),
            ("run", run_loop_run as usize),
            ("getCFRunLoop", run_loop_get_cf as usize),
        ],
    );
    r.function("CFRunLoopGetCurrent", cf_run_loop_get_current as usize);
    r.function("CFRunLoopGetMain", cf_run_loop_get_main as usize);
    r.function("CFRunLoopRunInMode", cf_run_loop_run_in_mode as usize);
    r.function("CACurrentMediaTime", current_media_time as usize);

    let object = runtime::class_named("NSObject");
    methods(
        object,
        &[
            ("performSelectorOnMainThread:withObject:waitUntilDone:", perform_on_main as usize),
            ("performSelectorOnMainThread:withObject:waitUntilDone:modes:", perform_on_main_modes as usize),
            ("performSelector:onThread:withObject:waitUntilDone:", perform_on_thread as usize),
            ("performSelector:onThread:withObject:waitUntilDone:modes:", perform_on_thread_modes as usize),
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
            ("isFinished", thread_is_finished as usize),
        ],
    );
}

extern "C" fn yes_class(_: Class, _: Sel) -> bool {
    true
}

/// Takes the link out of a mode; out of its last one, off its run loop (it
/// stays valid, holding its target, until -invalidate, as Apple's). A run
/// loop it isn't on changes nothing.
extern "C" fn display_link_remove(this: Id, _: Sel, run_loop: Id, mode: Id) {
    let mode = mode_name(mode);
    let from = loop_of(run_loop);
    let owner = {
        let mut state = timer(this);
        if !state.owner.is_some_and(|owner| std::ptr::eq(owner, from)) {
            return;
        }
        state.modes.retain(|m| *m != mode);
        if state.modes.is_empty() { state.owner.take() } else { None }
    };
    if let Some(run_loop) = owner {
        run_loop.timers.lock().unwrap().retain(|&t| t != this);
    }
}

#[cfg(test)]
mod frame_rate_tests {
    use super::*;

    #[test]
    fn display_link_runs_at_the_screens_top_rate() {
        assert_eq!(refresh_hz(None, Some("120")), 120.0);
        assert_eq!(refresh_hz(Some("90"), Some("120")), 90.0, "the test switch wins");
        assert_eq!(refresh_hz(None, None), 60.0);
        assert_eq!(refresh_hz(Some("x"), Some("0")), 60.0);
    }

    #[test]
    fn asking_for_every_second_frame_does_not_slow_the_game() {
        let refresh = 1.0 / 120.0;
        assert_eq!(link_interval(refresh, 2, false), refresh);
        assert_eq!(link_interval(refresh, 2, true), 2.0 * refresh, "IOS_FRAME_INTERVAL=1 keeps the game's choice");
    }

    /// Work posted for later runs on the main loop once its time has come,
    /// not before, earliest first.
    #[test]
    fn work_posted_for_later_waits_for_its_time() {
        static ORDER: Mutex<Vec<u8>> = Mutex::new(Vec::new());
        let posted = now();
        post_after(0.15, Box::new(|| ORDER.lock().unwrap().push(2)));
        post_after(0.05, Box::new(|| ORDER.lock().unwrap().push(1)));
        run_main_loop(Some(now() + 0.02));
        // (Only while the first is still early: a busy machine can take
        // longer than 0.05 s to get here, and then it is due.)
        if now() - posted < 0.05 {
            assert!(ORDER.lock().unwrap().is_empty(), "nothing runs before its time");
        }
        let end = now() + 0.5;
        while ORDER.lock().unwrap().len() < 2 && now() < end {
            run_main_loop(Some(now() + 0.05));
        }
        assert_eq!(*ORDER.lock().unwrap(), [1, 2]);
    }

    /// A timer made with plain alloc/init (a placeholder some games keep
    /// before their first real one) answers like an invalidated timer.
    #[test]
    fn a_plain_alloc_init_timer_can_be_invalidated() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let class = runtime::class_named("NSTimer");
        let timer = runtime::send(runtime::send(class, "alloc", &[]), "init", &[]);
        runtime::send(timer, "invalidate", &[]);
        assert_eq!(runtime::send(timer, "isValid", &[]) as u8, 0);
        assert_eq!(runtime::send(timer, "userInfo", &[]), 0);
    }
}

#[cfg(test)]
mod main_queue_tests {
    use super::*;
    use crate::libsystem::mach::test_blocks::running;

    static RAN: Mutex<Vec<&str>> = Mutex::new(Vec::new());

    fn note(what: &'static str) {
        RAN.lock().unwrap().push(what);
    }

    /// Apple's main queue (_dispatch_main_queue_callback_4CF) runs only the
    /// blocks queued before its drain began: one a block queues runs on the
    /// loop's next turn, after the commit. A run loop nested in a main-queue
    /// block runs none (the queue is draining), and private modes run none.
    #[test]
    fn a_turn_runs_only_the_main_queue_blocks_queued_before_it() {
        let run_loop: &'static Loop = Box::leak(Box::new(Loop::new()));
        let turn = |mode: &str| run_loop_until(run_loop, Some(now() + 0.02), true, mode);
        let b = running(|| note("b"));
        run_loop.push(Work::Block(running(move || {
            note("a");
            run_loop.push(Work::Block(b));
        })));
        turn("custom");
        assert!(RAN.lock().unwrap().is_empty(), "a main-queue block ran in a private mode");
        turn(DEFAULT_MODE);
        assert_eq!(*RAN.lock().unwrap(), ["a"], "the block queued during the drain waits for the next turn");
        turn(DEFAULT_MODE);
        assert_eq!(*RAN.lock().unwrap(), ["a", "b"]);
        RAN.lock().unwrap().clear();
        run_loop.push(Work::Block(running(move || {
            note("c");
            run_loop.push(Work::Native(Box::new(|| note("other work"))));
            run_loop_until(run_loop, Some(now() + 0.02), false, DEFAULT_MODE);
            note("c ends");
        })));
        run_loop.push(Work::Block(running(|| note("d"))));
        turn(DEFAULT_MODE);
        assert_eq!(*RAN.lock().unwrap(), ["c", "other work", "c ends", "d"]);
    }

    /// A main-queue block that unwinds doesn't leave the main queue marked as
    /// draining: later blocks still run.
    #[test]
    fn a_main_queue_block_that_unwinds_leaves_the_queue_running() {
        static RAN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let run_loop: &'static Loop = Box::leak(Box::new(Loop::new()));
        run_loop.push(Work::Queue(Box::new(|| panic!("a block that unwinds"))));
        let turn = || run_loop_until(run_loop, Some(now() + 0.02), true, DEFAULT_MODE);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(turn)).is_err());
        run_loop.push(Work::Queue(Box::new(|| RAN.store(true, Ordering::SeqCst))));
        turn();
        assert!(RAN.load(Ordering::SeqCst), "the main queue stayed marked as draining");
    }

    /// The turn's autorelease pool is emptied after the commit (UIKit's pool
    /// observer comes after Core Animation's), so what only the pool holds is
    /// alive for the commit.
    #[test]
    fn what_a_turn_autoreleased_lives_through_its_commit() {
        const ISA: usize = 0x7000_0000;
        crate::fw::counted::count_class(ISA);
        let object = crate::symbols::leak([ISA, 0]);
        static ALIVE_AT_COMMIT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let run_loop: &'static Loop = Box::leak(Box::new(Loop::new()));
        run_loop.push(Work::Native(Box::new(move || {
            crate::fw::counted::track(object, |_| {});
            crate::fw::counted::autorelease(object);
            AT_COMMIT.with(|c| {
                *c.borrow_mut() =
                    Some(Box::new(move || ALIVE_AT_COMMIT.store(crate::fw::counted::count(object).is_some(), Ordering::SeqCst)))
            });
        })));
        run_loop_until(run_loop, Some(now() + 0.02), false, DEFAULT_MODE);
        assert!(ALIVE_AT_COMMIT.load(Ordering::SeqCst), "the pool let go before the commit");
        assert_eq!(crate::fw::counted::count(object), None, "the pool let go after it");
    }
}

#[cfg(test)]
mod perform_tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    static HEARD: Mutex<Vec<(String, Id)>> = Mutex::new(Vec::new());
    static STOP: AtomicBool = AtomicBool::new(false);

    extern "C" fn note(_this: Id, _: Sel, text: Id) {
        HEARD.lock().unwrap().push((string::get(text), runtime::send(runtime::class_named("NSThread"), "currentThread", &[])));
    }

    /// A worker thread's body: runs its own run loop until told to stop.
    extern "C" fn serve(_this: Id, _: Sel, _: Id) {
        while !STOP.load(Ordering::Relaxed) {
            run_loop_until(current_loop(), Some(now() + 0.02), true, DEFAULT_MODE);
        }
    }

    fn heard() -> Vec<String> {
        std::mem::take(&mut *HEARD.lock().unwrap()).into_iter().map(|(text, _)| text).collect()
    }

    /// performSelector:onThread:withObject:waitUntilDone: as iOS does it:
    /// on the calling thread and waiting, at once; not waiting, when that
    /// thread's run loop next runs; on another thread, by that thread's run
    /// loop, the caller waiting until it was done.
    #[test]
    fn messages_performed_on_a_thread_run_on_its_run_loop() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let class = define_class("PerformTestTarget", "NSObject", 8);
        methods(class, &[("note:", note as usize), ("serve:", serve as usize)]);
        let target = runtime::send(runtime::send(class, "alloc", &[]), "init", &[]);
        let thread_class = runtime::class_named("NSThread");
        let here = runtime::send(thread_class, "currentThread", &[]);
        let perform = |thread: Id, text: &str, wait: usize| {
            let s = runtime::sel("performSelector:onThread:withObject:waitUntilDone:");
            let imp = runtime::imp_for(target, s).expect("-[NSObject performSelector:onThread:withObject:waitUntilDone:] is missing");
            let f: extern "C" fn(Id, Sel, Sel, Id, Id, u8) = unsafe { std::mem::transmute(imp) };
            f(target, s, runtime::sel("note:"), thread, string::make(text), wait as u8);
        };
        perform(here, "now", 1);
        assert_eq!(heard(), ["now"], "this thread, waiting: done at once");

        let worker = runtime::send(runtime::send(thread_class, "alloc", &[]), "initWithTarget:selector:object:", &[target, runtime::sel("serve:"), 0]);
        runtime::send(worker, "start", &[]);
        perform(worker, "there", 1);
        let done = std::mem::take(&mut *HEARD.lock().unwrap());
        assert_eq!(done.iter().map(|(text, thread)| (text.as_str(), *thread)).collect::<Vec<_>>(), [("there", worker)], "run by the worker's loop before the call returned");
        // Not waiting: the worker's loop runs it soon after.
        perform(worker, "soon", 0);
        let deadline = now() + 5.0;
        while HEARD.lock().unwrap().is_empty() && now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(heard(), ["soon"]);
        STOP.store(true, Ordering::Relaxed);
    }

    static MODE_SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());

    extern "C" fn record_current_mode(_this: Id, _: Sel, _: Id) {
        let run_loop = runtime::send(runtime::class_named("NSRunLoop"), "currentRunLoop", &[]);
        let mode = runtime::send(run_loop, "currentMode", &[]);
        MODE_SEEN.lock().unwrap().push(if mode == 0 { "nil".into() } else { string::get(mode) });
    }

    /// -currentMode: the mode the run loop is running in, nil while it isn't
    /// running (on a thread of its own here, whose loop runs only when asked).
    #[test]
    fn a_run_loop_tells_the_mode_it_runs_in() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let class = define_class("ModeTestTarget", "NSObject", 8);
        methods(class, &[("noteMode:", record_current_mode as usize)]);
        std::thread::spawn(move || {
            // A thread of its own even when no test marked a main thread
            // (then every thread counts as main and shares its loop).
            CURRENT_THREAD.with(|cell| cell.set(new_thread(0, 0, 0)));
            let target = runtime::send(runtime::send(class, "alloc", &[]), "init", &[]);
            record_current_mode(target, 0, 0);
            let s = runtime::sel("performSelector:withObject:afterDelay:");
            let imp = runtime::imp_for(target, s).expect("performSelector:withObject:afterDelay:");
            let f: extern "C" fn(Id, Sel, Sel, Id, f64) = unsafe { std::mem::transmute(imp) };
            f(target, s, runtime::sel("noteMode:"), 0, 0.0);
            run_this_threads_loop_briefly();
            record_current_mode(target, 0, 0);
        })
        .join()
        .unwrap();
        assert_eq!(*MODE_SEEN.lock().unwrap(), ["nil", "kCFRunLoopDefaultMode", "nil"]);
    }

    /// Whether a delayed perform is still waiting: yes once asked, no after a
    /// cancel (a game fix checks it for SpellCraft's tutorial lock).
    #[test]
    fn a_delayed_perform_is_known_to_be_waiting_until_cancelled() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let class = define_class("PendingTestTarget", "NSObject", 8);
        methods(class, &[("noteMode:", record_current_mode as usize)]);
        let target = runtime::send(runtime::send(class, "alloc", &[]), "init", &[]);
        let note = runtime::sel("noteMode:");
        assert!(!perform_pending(target, note));
        let s = runtime::sel("performSelector:withObject:afterDelay:");
        let f: extern "C" fn(Id, Sel, Sel, Id, f64) = unsafe { std::mem::transmute(runtime::imp_for(target, s).unwrap()) };
        f(target, s, note, 0, 30.0);
        assert!(perform_pending(target, note), "waiting");
        assert!(!perform_pending(target, runtime::sel("other:")), "only that selector");
        runtime::send(runtime::class_named("NSObject"), "cancelPreviousPerformRequestsWithTarget:", &[target]);
        assert!(!perform_pending(target, note), "cancelled");
    }

    /// A loop mode answers with the same object every time it is asked for,
    /// so a game can compare it with its mode constant by pointer.
    #[test]
    fn a_run_loop_mode_is_the_same_object_every_time() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        static MODE_LOOP: Loop = Loop::new();
        let object = run_loop_object(&MODE_LOOP);
        *MODE_LOOP.mode.lock().unwrap() = Some("UITrackingRunLoopMode".into());
        // A mode with an exported constant answers with it (other tests running
        // setup at the same time make new constants, so only the text is checked).
        assert_eq!(string::get(run_loop_current_mode(object, 0)), "UITrackingRunLoopMode");
        *MODE_LOOP.mode.lock().unwrap() = Some("GameMode".into());
        let custom = run_loop_current_mode(object, 0);
        assert_eq!(string::get(custom), "GameMode");
        assert_eq!(run_loop_current_mode(object, 0), custom, "a mode of the game's own keeps its object");
        *MODE_LOOP.mode.lock().unwrap() = None;
        assert_eq!(run_loop_current_mode(object, 0), 0, "nil while it isn't running");
    }

    /// Reference counts of the objects below (counted through their own
    /// retain and release, as a game's objects are).
    static COUNTS: Mutex<Vec<(Id, isize)>> = Mutex::new(Vec::new());
    static COUNTS_WHEN_RUN: Mutex<Vec<(isize, isize)>> = Mutex::new(Vec::new());
    static GO: AtomicBool = AtomicBool::new(false);

    fn count_of(object: Id) -> isize {
        COUNTS.lock().unwrap().iter().find(|(o, _)| *o == object).map_or(0, |e| e.1)
    }

    fn change(object: Id, by: isize) {
        let mut counts = COUNTS.lock().unwrap();
        match counts.iter_mut().find(|(o, _)| *o == object) {
            Some(entry) => entry.1 += by,
            None => counts.push((object, by)),
        }
    }

    extern "C" fn counted_retain(this: Id, _: Sel) -> Id {
        change(this, 1);
        this
    }

    extern "C" fn counted_release(this: Id, _: Sel) {
        change(this, -1);
    }

    extern "C" fn check(this: Id, _: Sel, argument: Id) {
        COUNTS_WHEN_RUN.lock().unwrap().push((count_of(this), count_of(argument)));
    }

    /// A worker that starts running its run loop once told to.
    extern "C" fn serve_later(_this: Id, _: Sel, _: Id) {
        while !GO.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        loop {
            run_loop_until(current_loop(), Some(now() + 0.02), true, DEFAULT_MODE);
        }
    }

    fn wait_for(what: impl Fn() -> bool) -> bool {
        let deadline = now() + 5.0;
        while !what() && now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        what()
    }

    /// The receiver and the argument are retained (through their own retain)
    /// until the message has run on the other thread, then released: the
    /// caller may let go of both right after asking.
    #[test]
    fn a_performed_message_holds_its_receiver_and_argument_until_it_ran() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let class = define_class("PerformCountedTarget", "NSObject", 8);
        methods(
            class,
            &[("retain", counted_retain as usize), ("release", counted_release as usize), ("check:", check as usize), ("serveLater:", serve_later as usize)],
        );
        let make = || {
            let object = runtime::send(runtime::send(class, "alloc", &[]), "init", &[]);
            change(object, 1);
            object
        };
        let (target, argument) = (make(), make());
        let thread_class = runtime::class_named("NSThread");
        let worker = runtime::send(runtime::send(thread_class, "alloc", &[]), "initWithTarget:selector:object:", &[target, runtime::sel("serveLater:"), 0]);
        runtime::send(worker, "start", &[]);
        let s = runtime::sel("performSelector:onThread:withObject:waitUntilDone:");
        let perform: extern "C" fn(Id, Sel, Sel, Id, Id, u8) = unsafe { std::mem::transmute(runtime::imp_for(target, s).unwrap()) };
        // What holds the target besides the caller (the worker's NSThread may).
        let held_by_thread = count_of(target) - 1;
        perform(target, s, runtime::sel("check:"), worker, argument, 0);
        runtime::send(target, "release", &[]);
        runtime::send(argument, "release", &[]);
        GO.store(true, Ordering::Relaxed);
        assert!(wait_for(|| !COUNTS_WHEN_RUN.lock().unwrap().is_empty()), "the message never ran");
        let (target_count, argument_count) = COUNTS_WHEN_RUN.lock().unwrap()[0];
        assert!(target_count > held_by_thread && argument_count > 0, "released before it ran: counts {target_count}, {argument_count}");
        assert!(wait_for(|| count_of(target) == held_by_thread && count_of(argument) == 0), "not released after running: counts {}, {}", count_of(target), count_of(argument));
    }

    /// As Apple's: a timer, a display link and a delayed perform hold their
    /// target (and user info / argument) until they are invalidated, have
    /// fired for the last time, or are cancelled.
    #[test]
    fn timers_hold_their_target_until_invalidated() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let class = define_class("TimerCountedTarget", "NSObject", 8);
        methods(class, &[("retain", counted_retain as usize), ("release", counted_release as usize), ("check:", check as usize)]);
        let make = || {
            let object = runtime::send(runtime::send(class, "alloc", &[]), "init", &[]);
            change(object, 1);
            object
        };
        let (target, info) = (make(), make());
        let timer_class = runtime::class_named("NSTimer");
        let timer = timer_unscheduled(timer_class, 0, 30.0, target, runtime::sel("check:"), info, 0);
        assert_eq!((count_of(target), count_of(info)), (2, 2), "a timer holds its target and user info");
        runtime::send(timer, "invalidate", &[]);
        runtime::send(timer, "invalidate", &[]);
        assert_eq!((count_of(target), count_of(info)), (1, 1), "let go of once, when invalidated");
        let link = display_link_with_target(runtime::class_named("CADisplayLink"), 0, target, runtime::sel("check:"));
        assert_eq!(count_of(target), 2, "a display link holds its target");
        runtime::send(link, "invalidate", &[]);
        assert_eq!(count_of(target), 1);
        perform_after_delay(target, 0, runtime::sel("check:"), info, 30.0);
        assert_eq!((count_of(target), count_of(info)), (2, 2), "a delayed perform holds its target and argument");
        runtime::send(runtime::class_named("NSObject"), "cancelPreviousPerformRequestsWithTarget:", &[target]);
        assert_eq!((count_of(target), count_of(info)), (1, 1), "let go of when cancelled");
    }

    static COUNTS_AFTER_STOP: Mutex<Vec<isize>> = Mutex::new(Vec::new());

    extern "C" fn stop_and_check(this: Id, _: Sel, timer: Id) {
        runtime::send(timer, "invalidate", &[]);
        COUNTS_AFTER_STOP.lock().unwrap().push(count_of(this));
    }

    /// As CFRunLoop's (__CFRunLoopDoTimer retains the timer's context around
    /// the callout): a target only its timer holds outlives the call in which
    /// it invalidates that timer, both when the loop fires it and on -fire.
    #[test]
    fn a_timer_holds_its_target_through_its_call() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        static STOP_LOOP: Loop = Loop::new();
        let class = define_class("TimerStopTarget", "NSObject", 8);
        methods(class, &[("retain", counted_retain as usize), ("release", counted_release as usize), ("stop:", stop_and_check as usize)]);
        let timer_class = runtime::class_named("NSTimer");
        let make = || {
            let target = runtime::send(runtime::send(class, "alloc", &[]), "init", &[]);
            change(target, 1);
            let made = timer_unscheduled(timer_class, 0, 30.0, target, runtime::sel("stop:"), 0, 1);
            runtime::send(target, "release", &[]);
            (target, made)
        };
        let (scheduled, made) = make();
        run_loop_add_timer(run_loop_object(&STOP_LOOP), 0, made, 0);
        timer(made).fire_at = now() - 0.01;
        run_loop_until(&STOP_LOOP, Some(now() + 0.02), true, DEFAULT_MODE);
        let (fired_by_hand, made) = make();
        runtime::send(made, "fire", &[]);
        assert_eq!(*COUNTS_AFTER_STOP.lock().unwrap(), [1, 1], "the target was let go of during its own call");
        assert_eq!((count_of(scheduled), count_of(fired_by_hand)), (0, 0), "let go of after it");
    }

    static FIRED: Mutex<Vec<Id>> = Mutex::new(Vec::new());

    extern "C" fn fired(_this: Id, _: Sel, timer: Id) {
        FIRED.lock().unwrap().push(timer);
    }

    /// Due timers fire earliest fire date first (CFRunLoop keeps them
    /// sorted), not in the order they were added; a timer for 0 s first
    /// fires 0.1 ms on, as NSTimer's.
    #[test]
    fn due_timers_fire_earliest_first() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        static ORDER_LOOP: Loop = Loop::new();
        let class = define_class("TimerOrderTarget", "NSObject", 8);
        methods(class, &[("fired:", fired as usize)]);
        let target = runtime::send(runtime::send(class, "alloc", &[]), "init", &[]);
        let timer_class = runtime::class_named("NSTimer");
        let later = timer_unscheduled(timer_class, 0, 0.0, target, runtime::sel("fired:"), 0, 0);
        let sooner = timer_unscheduled(timer_class, 0, 0.0, target, runtime::sel("fired:"), 0, 0);
        timer(later).fire_at = now() - 0.01;
        timer(sooner).fire_at = now() - 0.02;
        let run_loop = run_loop_object(&ORDER_LOOP);
        for t in [later, sooner] {
            run_loop_add_timer(run_loop, 0, t, 0);
        }
        run_loop_until(&ORDER_LOOP, Some(now() + 0.02), true, DEFAULT_MODE);
        assert_eq!(*FIRED.lock().unwrap(), [sooner, later]);
        let before = now();
        let zero = timer_unscheduled(timer_class, 0, 0.0, target, runtime::sel("fired:"), 0, 0);
        assert!(timer(zero).fire_at >= before + 0.0001 - 1e-9, "a 0 s timer waits 0.1 ms");
    }

    static SLOW_LOOP: Loop = Loop::new();
    static SLOW_HEARD: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

    /// A game frame that runs longer than its timer's interval and, the
    /// first time, asks for a call at once (SpellCraft's last combat frame
    /// asks for endCombatView with performSelector:afterDelay:0).
    extern "C" fn slow_frame(this: Id, _: Sel, _timer: Id) {
        let first = SLOW_HEARD.lock().unwrap().is_empty();
        SLOW_HEARD.lock().unwrap().push("frame");
        if first {
            // A loop run inside the call doesn't fire the timer again.
            run_loop_until(&SLOW_LOOP, Some(now() + 0.005), true, DEFAULT_MODE);
            std::thread::sleep(std::time::Duration::from_millis(30));
            let at_once = timer_unscheduled(runtime::class_named("NSTimer"), 0, 0.0, this, runtime::sel("slowDone:"), 0, 0);
            run_loop_add_timer(run_loop_object(&SLOW_LOOP), 0, at_once, 0);
        }
    }

    extern "C" fn slow_done(_this: Id, _: Sel, _timer: Id) {
        SLOW_HEARD.lock().unwrap().push("done");
    }

    /// As __CFRunLoopDoTimer: a repeating timer's next firing is worked out
    /// after its call returns (the first one on its schedule after that
    /// moment), so a timer for "at once" made during a long call fires
    /// before the repeating timer's next call; while its call runs, the
    /// timer doesn't fire again.
    #[test]
    fn a_call_asked_for_at_once_in_a_long_timer_call_comes_before_its_next_call() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let class = define_class("TimerSlowFrameTarget", "NSObject", 8);
        methods(class, &[("slowFrame:", slow_frame as usize), ("slowDone:", slow_done as usize)]);
        let target = runtime::send(runtime::send(class, "alloc", &[]), "init", &[]);
        let frames = timer_unscheduled(runtime::class_named("NSTimer"), 0, 1.0 / 60.0, target, runtime::sel("slowFrame:"), 0, 1);
        run_loop_add_timer(run_loop_object(&SLOW_LOOP), 0, frames, 0);
        timer(frames).fire_at = now() - 0.001;
        let deadline = now() + 0.3;
        while !SLOW_HEARD.lock().unwrap().contains(&"done") && now() < deadline {
            run_loop_until(&SLOW_LOOP, Some(now() + 0.02), true, DEFAULT_MODE);
        }
        runtime::send(frames, "invalidate", &[]);
        let heard = SLOW_HEARD.lock().unwrap().clone();
        assert_eq!(heard.get(..2), Some(&["frame", "done"][..]), "heard {heard:?}");
    }

    /// A delayed perform for 0 s counts as asked for at once (SpellCraft's
    /// render loop then doesn't draw what its step left: game_fixes.rs);
    /// one for later doesn't.
    #[test]
    fn a_perform_for_no_delay_counts_as_asked_for_at_once() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let class = define_class("AtOnceTarget", "NSObject", 8);
        methods(class, &[("slowDone:", slow_done as usize)]);
        let target = runtime::send(runtime::send(class, "alloc", &[]), "init", &[]);
        let done = runtime::sel("slowDone:");
        let before = asked_at_once();
        perform_after_delay(target, 0, done, 0, 30.0);
        assert_eq!(asked_at_once(), before, "a perform for later");
        perform_after_delay(target, 0, done, 0, 0.0);
        assert!(asked_at_once() > before, "a perform for no delay");
        runtime::send(runtime::class_named("NSObject"), "cancelPreviousPerformRequestsWithTarget:", &[target]);
    }

    static MODE_FIRED: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn mode_fired(_this: Id, _: Sel, _timer: Id) {
        MODE_FIRED.fetch_add(1, Ordering::SeqCst);
    }

    /// A timer fires only while its run loop runs in one of the timer's
    /// modes (the common modes include the default one).
    #[test]
    fn timers_fire_only_in_their_modes() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        static MODES_LOOP: Loop = Loop::new();
        let class = define_class("TimerModeTarget", "NSObject", 8);
        methods(class, &[("modeFired:", mode_fired as usize)]);
        let target = runtime::send(runtime::send(class, "alloc", &[]), "init", &[]);
        let timer_class = runtime::class_named("NSTimer");
        let run_loop = run_loop_object(&MODES_LOOP);
        let only_mine = timer_unscheduled(timer_class, 0, 0.0, target, runtime::sel("modeFired:"), 0, 0);
        run_loop_add_timer(run_loop, 0, only_mine, string::make("TestTimerMode"));
        timer(only_mine).fire_at = now() - 0.01;
        let turn = |mode: &str| run_loop_until(&MODES_LOOP, Some(now() + 0.02), true, mode);
        turn(DEFAULT_MODE);
        assert_eq!(MODE_FIRED.load(Ordering::SeqCst), 0, "not in the default mode");
        turn("TestTimerMode");
        assert_eq!(MODE_FIRED.load(Ordering::SeqCst), 1, "in its own mode");
        let common = timer_unscheduled(timer_class, 0, 0.0, target, runtime::sel("modeFired:"), 0, 0);
        run_loop_add_timer(run_loop, 0, common, string::make(COMMON_MODES));
        timer(common).fire_at = now() - 0.01;
        turn(DEFAULT_MODE);
        assert_eq!(MODE_FIRED.load(Ordering::SeqCst), 2, "the common modes take in the default mode");
    }

    static OTHER_LOOP_FIRED: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn other_loop_fired(_this: Id, _: Sel, _timer: Id) {
        OTHER_LOOP_FIRED.fetch_add(1, Ordering::SeqCst);
    }

    /// A timer is on one run loop only, as CFRunLoop's: adding it to a second
    /// loop changes nothing (the first doesn't take on the second's mode).
    #[test]
    fn a_timer_added_to_a_second_loop_stays_on_its_first() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        static FIRST: Loop = Loop::new();
        static SECOND: Loop = Loop::new();
        let class = define_class("TimerTwoLoopsTarget", "NSObject", 8);
        methods(class, &[("otherLoopFired:", other_loop_fired as usize)]);
        let target = runtime::send(runtime::send(class, "alloc", &[]), "init", &[]);
        let t = timer_unscheduled(runtime::class_named("NSTimer"), 0, 0.0, target, runtime::sel("otherLoopFired:"), 0, 0);
        run_loop_add_timer(run_loop_object(&FIRST), 0, t, 0);
        run_loop_add_timer(run_loop_object(&SECOND), 0, t, string::make("TestSecondLoopMode"));
        timer(t).fire_at = now() - 0.01;
        run_loop_until(&FIRST, Some(now() + 0.02), true, "TestSecondLoopMode");
        assert_eq!(OTHER_LOOP_FIRED.load(Ordering::SeqCst), 0, "the first loop took on the second's mode");
        run_loop_until(&FIRST, Some(now() + 0.02), true, DEFAULT_MODE);
        assert_eq!(OTHER_LOOP_FIRED.load(Ordering::SeqCst), 1, "still fires on its first loop");
    }

    static REENTER_TARGET: AtomicUsize = AtomicUsize::new(0);

    /// An argument whose -isEqual: cancels other delayed performs.
    extern "C" fn reentrant_is_equal(_this: Id, _: Sel, _other: Id) -> bool {
        runtime::send(runtime::class_named("NSObject"), "cancelPreviousPerformRequestsWithTarget:", &[REENTER_TARGET.load(Ordering::SeqCst)]);
        false
    }

    /// cancelPreviousPerformRequestsWithTarget:selector:object: compares the
    /// argument with -isEqual: (game code) outside the timers' locks: an
    /// -isEqual: that cancels performs itself doesn't hang.
    #[test]
    fn an_is_equal_that_cancels_performs_does_not_hang_the_cancel() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let target_class = define_class("CancelReentryTarget", "NSObject", 8);
        methods(target_class, &[("fired:", fired as usize)]);
        let argument_class = define_class("CancelReentryArgument", "NSObject", 8);
        methods(argument_class, &[("isEqual:", reentrant_is_equal as usize)]);
        let new = |class: Class| runtime::send(runtime::send(class, "alloc", &[]), "init", &[]);
        let (target, other_target, argument, other) = (new(target_class), new(target_class), new(argument_class), new(target_class));
        REENTER_TARGET.store(other_target, Ordering::SeqCst);
        // On a thread loop of its own (never run): if the cancel ever hangs
        // holding this perform's lock, the main loop's timers aren't caught.
        std::thread::spawn(move || {
            CURRENT_THREAD.with(|cell| cell.set(new_thread(0, 0, 0)));
            perform_after_delay(target, 0, runtime::sel("fired:"), argument, 30.0);
        })
        .join()
        .unwrap();
        let (finished, done) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            cancel_previous_matching(0, 0, target, runtime::sel("fired:"), other);
            let _ = finished.send(());
        });
        assert!(done.recv_timeout(std::time::Duration::from_secs(5)).is_ok(), "the cancel hung");
        assert!(perform_pending(target, runtime::sel("fired:")), "not equal: still waiting");
    }

    /// -[CADisplayLink removeFromRunLoop:forMode:] for a run loop it isn't on
    /// leaves it running on its own.
    #[test]
    fn removing_a_display_link_from_another_loop_leaves_it_on_its_own() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        static OWN: Loop = Loop::new();
        static OTHER: Loop = Loop::new();
        let class = define_class("LinkRemoveTarget", "NSObject", 8);
        methods(class, &[("fired:", fired as usize)]);
        let target = runtime::send(runtime::send(class, "alloc", &[]), "init", &[]);
        let link = display_link_with_target(runtime::class_named("CADisplayLink"), 0, target, runtime::sel("fired:"));
        display_link_add_to_run_loop(link, 0, run_loop_object(&OWN), 0);
        display_link_remove(link, 0, run_loop_object(&OTHER), 0);
        assert!(OWN.timers.lock().unwrap().contains(&link), "taken off its own loop");
        assert_eq!(timer(link).modes, [DEFAULT_MODE]);
        display_link_remove(link, 0, run_loop_object(&OWN), 0);
        assert!(!OWN.timers.lock().unwrap().contains(&link), "removing from its own loop still works");
    }

    static TRACKING_FIRED: AtomicUsize = AtomicUsize::new(0);
    static DEFAULT_FIRED: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn tracking_fired(_this: Id, _: Sel, _timer: Id) {
        TRACKING_FIRED.fetch_add(1, Ordering::SeqCst);
    }

    extern "C" fn default_fired(_this: Id, _: Sel, _timer: Id) {
        DEFAULT_FIRED.fetch_add(1, Ordering::SeqCst);
    }

    /// While a scroll view tracks, UIKit runs the main loop in
    /// UITrackingRunLoopMode: default-mode timers wait (a game must add one
    /// to NSRunLoopCommonModes to keep it going), tracking-mode and common
    /// ones fire. A programmatic animated scroll is not tracking.
    #[test]
    fn a_scroll_tracking_puts_the_main_loop_in_tracking_mode() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        static SCROLL_LOOP: Loop = Loop::new();
        SCROLL_LOOP.mark_main_for_test();
        let class = define_class("TimerTrackingTarget", "NSObject", 8);
        methods(class, &[("trackingFired:", tracking_fired as usize), ("defaultFired:", default_fired as usize)]);
        let target = runtime::send(runtime::send(class, "alloc", &[]), "init", &[]);
        let timer = |selector: &str| {
            let t = timer_unscheduled(runtime::class_named("NSTimer"), 0, 0.0, target, runtime::sel(selector), 0, 0);
            timer(t).fire_at = now() - 0.01;
            t
        };
        let plain = timer("defaultFired:");
        run_loop_add_timer(run_loop_object(&SCROLL_LOOP), 0, plain, 0);
        let tracking = timer("trackingFired:");
        run_loop_add_timer(run_loop_object(&SCROLL_LOOP), 0, tracking, string::make("UITrackingRunLoopMode"));
        let turn = || run_loop_until(&SCROLL_LOOP, Some(now() + 0.05), true, DEFAULT_MODE);
        crate::fw::controls::set_test_tracking(Some(true));
        turn();
        assert_eq!(DEFAULT_FIRED.load(Ordering::SeqCst), 0, "a default-mode timer waits while the loop tracks a scroll");
        assert_eq!(TRACKING_FIRED.load(Ordering::SeqCst), 1, "a tracking-mode timer fires then");
        crate::fw::controls::set_test_tracking(Some(false));
        turn();
        assert_eq!(DEFAULT_FIRED.load(Ordering::SeqCst), 1, "and fires again when the scroll ends");
        crate::fw::controls::set_test_tracking(None);
    }

    static KEEP_ALIVE_GO: AtomicBool = AtomicBool::new(false);

    /// A worker thread's body: schedules a repeating keep-alive timer (as
    /// SpellCraft's AEBackgroundRunLoopThread does) on its own loop, runs it
    /// once and ends when told to.
    extern "C" fn keep_alive_then_end(this: Id, _: Sel, _: Id) {
        timer_scheduled(runtime::class_named("NSTimer"), 0, 30.0, this, runtime::sel("check:"), this, 1);
        run_loop_until(current_loop(), Some(now() + 0.01), true, DEFAULT_MODE);
        while !KEEP_ALIVE_GO.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// When a thread ends, its run loop's timers end with it and let go of
    /// their targets, as Apple's do when the thread's run loop goes.
    #[test]
    fn a_threads_timers_let_go_of_their_targets_when_it_ends() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let class = define_class("TimerThreadEndTarget", "NSObject", 8);
        methods(
            class,
            &[("retain", counted_retain as usize), ("release", counted_release as usize), ("check:", check as usize), ("keepAliveThenEnd:", keep_alive_then_end as usize)],
        );
        let target = runtime::send(runtime::send(class, "alloc", &[]), "init", &[]);
        change(target, 1);
        let thread_class = runtime::class_named("NSThread");
        let worker = runtime::send(runtime::send(thread_class, "alloc", &[]), "initWithTarget:selector:object:", &[target, runtime::sel("keepAliveThenEnd:"), 0]);
        runtime::send(worker, "start", &[]);
        assert!(wait_for(|| count_of(target) >= 3), "the timer holds its target and user info: {}", count_of(target));
        KEEP_ALIVE_GO.store(true, Ordering::SeqCst);
        assert!(wait_for(|| runtime::send(worker, "isFinished", &[]) & 0xFF != 0), "the thread never finished");
        assert!(wait_for(|| count_of(target) == 1), "the ended thread's timer still holds its target: {}", count_of(target));
    }

    static RAN_FOR_ENDED: AtomicBool = AtomicBool::new(false);
    static END_NOW: AtomicBool = AtomicBool::new(false);

    extern "C" fn never_run(_this: Id, _: Sel, _: Id) {
        RAN_FOR_ENDED.store(true, Ordering::SeqCst);
    }

    /// A thread that ends, when told to, without ever running its run loop.
    extern "C" fn end_later(_this: Id, _: Sel, _: Id) {
        while !END_NOW.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// A thread that ended runs no more work: what was queued for it, or
    /// comes later, lets go of its receiver and argument, and a caller
    /// waiting for it goes on.
    #[test]
    fn work_for_an_ended_thread_is_let_go_of() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let class = define_class("PerformEndedTarget", "NSObject", 8);
        methods(
            class,
            &[("retain", counted_retain as usize), ("release", counted_release as usize), ("neverRun:", never_run as usize), ("endLater:", end_later as usize)],
        );
        let make = || {
            let object = runtime::send(runtime::send(class, "alloc", &[]), "init", &[]);
            change(object, 1);
            object
        };
        let (target, argument) = (make(), make());
        let thread_class = runtime::class_named("NSThread");
        let worker = runtime::send(runtime::send(thread_class, "alloc", &[]), "initWithTarget:selector:object:", &[target, runtime::sel("endLater:"), 0]);
        runtime::send(worker, "start", &[]);
        let baseline = (count_of(target), count_of(argument));
        let s = runtime::sel("performSelector:onThread:withObject:waitUntilDone:");
        let perform: extern "C" fn(Id, Sel, Sel, Id, Id, u8) = unsafe { std::mem::transmute(runtime::imp_for(target, s).unwrap()) };
        // Queued while the thread lives; it ends without running its loop.
        perform(target, s, runtime::sel("neverRun:"), worker, argument, 0);
        END_NOW.store(true, Ordering::SeqCst);
        assert!(wait_for(|| (count_of(target), count_of(argument)) == baseline), "work queued for an ended thread still holds its objects");
        // Asked after it ended, waiting: the caller goes on.
        let (finished, done) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            perform(target, s, runtime::sel("neverRun:"), worker, argument, 1);
            let _ = finished.send(());
        });
        assert!(done.recv_timeout(std::time::Duration::from_secs(5)).is_ok(), "a caller waiting for an ended thread never went on");
        assert_eq!((count_of(target), count_of(argument)), baseline);
        assert!(!RAN_FOR_ENDED.load(Ordering::SeqCst), "an ended thread ran work");
    }

    extern "C" fn end_now(_this: Id, _: Sel, _: Id) {}

    /// The same for a thread that ended before anything was sent to it.
    #[test]
    fn work_for_a_thread_that_ended_before_any_was_sent_is_let_go_of() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let class = define_class("PerformEarlyEndTarget", "NSObject", 8);
        methods(
            class,
            &[("retain", counted_retain as usize), ("release", counted_release as usize), ("neverRun:", never_run as usize), ("endNow:", end_now as usize)],
        );
        let make = || {
            let object = runtime::send(runtime::send(class, "alloc", &[]), "init", &[]);
            change(object, 1);
            object
        };
        let (target, argument) = (make(), make());
        let thread_class = runtime::class_named("NSThread");
        let worker = runtime::send(runtime::send(thread_class, "alloc", &[]), "initWithTarget:selector:object:", &[target, runtime::sel("endNow:"), 0]);
        runtime::send(worker, "start", &[]);
        assert!(wait_for(|| runtime::send(worker, "isFinished", &[]) & 0xFF != 0), "the thread never finished");
        let baseline = (count_of(target), count_of(argument));
        let s = runtime::sel("performSelector:onThread:withObject:waitUntilDone:");
        let perform: extern "C" fn(Id, Sel, Sel, Id, Id, u8) = unsafe { std::mem::transmute(runtime::imp_for(target, s).unwrap()) };
        perform(target, s, runtime::sel("neverRun:"), worker, argument, 0);
        assert_eq!((count_of(target), count_of(argument)), baseline, "work for an ended thread holds its objects");
        let (finished, done) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            perform(target, s, runtime::sel("neverRun:"), worker, argument, 1);
            let _ = finished.send(());
        });
        assert!(done.recv_timeout(std::time::Duration::from_secs(5)).is_ok(), "a caller waiting for an ended thread never went on");
        assert_eq!((count_of(target), count_of(argument)), baseline);
        assert!(!RAN_FOR_ENDED.load(Ordering::SeqCst), "an ended thread ran work");
    }

    static MODE_HEARD: Mutex<Vec<String>> = Mutex::new(Vec::new());
    /// The modes the two workers below run their loops in.
    static WORKER_MODES: [Mutex<&str>; 2] = [Mutex::new("kCFRunLoopDefaultMode"), Mutex::new("kCFRunLoopDefaultMode")];

    extern "C" fn note_mode(_this: Id, _: Sel, text: Id) {
        MODE_HEARD.lock().unwrap().push(string::get(text));
    }

    /// Worker `which` (0 or 1, its thread's argument) runs its loop in its mode.
    extern "C" fn serve_in_mode(_this: Id, _: Sel, which: Id) {
        loop {
            let mode = *WORKER_MODES[which].lock().unwrap();
            cf_run_loop_run_in_mode(string::make(mode), 0.02, 1);
        }
    }

    /// A target and a worker thread serving in WORKER_MODES[which]; a
    /// perform of note: with modes; what was heard with `tag`.
    fn modes_setup(which: usize, tag: &'static str) -> (impl Fn(&str, Id, u8), impl Fn() -> Vec<String>) {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            crate::run::setup();
            let class = define_class("PerformModesTarget", "NSObject", 8);
            methods(class, &[("note:", note_mode as usize), ("serveInMode:", serve_in_mode as usize)]);
        });
        let class = runtime::class_named("PerformModesTarget");
        let target = runtime::send(runtime::send(class, "alloc", &[]), "init", &[]);
        let thread_class = runtime::class_named("NSThread");
        let worker = runtime::send(runtime::send(thread_class, "alloc", &[]), "initWithTarget:selector:object:", &[target, runtime::sel("serveInMode:"), which]);
        runtime::send(worker, "start", &[]);
        let perform = move |text: &str, modes: Id, wait: u8| {
            let s = runtime::sel("performSelector:onThread:withObject:waitUntilDone:modes:");
            let perform: extern "C" fn(Id, Sel, Sel, Id, Id, u8, Id) = unsafe { std::mem::transmute(runtime::imp_for(target, s).unwrap()) };
            perform(target, s, runtime::sel("note:"), worker, string::make(&format!("{tag}{text}")), wait, modes);
        };
        let heard = move || {
            let mut all = MODE_HEARD.lock().unwrap();
            let (mine, others): (Vec<String>, Vec<String>) = all.drain(..).partition(|text| text.starts_with(tag));
            *all = others;
            mine.into_iter().map(|text| text[tag.len()..].to_string()).collect()
        };
        (perform, heard)
    }

    fn modes(names: &[&str]) -> Id {
        collections::array_from(&names.iter().map(|name| string::make(name)).collect::<Vec<_>>())
    }

    /// The modes: variant as on Apple: nil or no modes performs nothing (even
    /// waiting); the common modes include the default mode.
    #[test]
    fn performed_messages_with_nil_or_no_modes_do_nothing() {
        let (perform, heard) = modes_setup(0, "a:");
        perform("nil", 0, 1);
        perform("none", modes(&[]), 1);
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(heard().is_empty(), "nil or no modes: not performed");
        perform("common", modes(&["kCFRunLoopCommonModes"]), 1);
        assert_eq!(heard(), ["common"], "the default mode is a common mode");
    }

    /// A message for named modes waits until the thread's run loop runs in one of them.
    #[test]
    fn performed_messages_with_modes_run_only_in_those_modes() {
        let (perform, heard) = modes_setup(1, "b:");
        perform("other", modes(&["TestOtherMode"]), 0);
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(heard().is_empty(), "not performed while the loop runs in the default mode");
        *WORKER_MODES[1].lock().unwrap() = "TestOtherMode";
        assert!(wait_for(|| MODE_HEARD.lock().unwrap().iter().any(|text| text.starts_with("b:"))), "never performed in its mode");
        assert_eq!(heard(), ["other"]);
    }
}
