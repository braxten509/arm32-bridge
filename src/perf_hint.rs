//! Android's performance hint (ADPF) for the game's frame thread.
//!
//! The game's frames run on one thread that is busy nearly all the time, so
//! Android often moves it to a slower core or lowers its clock. When Card Wars'
//! main thread was held on the fastest core, doubled frames and long pauses
//! fell by about two thirds (DEBUGGING.md, "Core causality test").
//!
//! The hint is how Android wants games to say this: the bridge opens a hint
//! session for the thread that runs the game's frames, with the screen's frame
//! time as the target, and reports how long each frame's work took. Time the
//! frame spends waiting in eglSwapBuffers for the screen is not work, so it is
//! left out; a game that keeps up therefore reports less than the target and
//! gets no extra speed. Nothing here is specific to one game.
//!
//! debug.arm32bridge.hint=0 turns it off (testing).

use std::cell::Cell;
use std::ffi::c_void;
use std::sync::Mutex;
use std::time::Duration;

type GetManager = unsafe extern "C" fn() -> *mut c_void;
type CreateSession = unsafe extern "C" fn(*mut c_void, *const i32, usize, i64) -> *mut c_void;
type ReportDuration = unsafe extern "C" fn(*mut c_void, i64) -> i32;
type CloseSession = unsafe extern "C" fn(*mut c_void);

struct Session {
    handle: *mut c_void,
    thread: i32,
    report: ReportDuration,
    close: CloseSession,
}

// The session handle is only used under the mutex.
unsafe impl Send for Session {}

enum State {
    Unknown,
    Off,
    On(Session),
}

static STATE: Mutex<State> = Mutex::new(State::Unknown);

thread_local! {
    /// Time this thread spent in eglSwapBuffers since its last frame report.
    static SWAP_WAIT: Cell<Duration> = const { Cell::new(Duration::ZERO) };
}

/// Called by the eglSwapBuffers handler with how long the swap took.
pub fn note_swap(took: Duration) {
    SWAP_WAIT.with(|wait| wait.set(wait.get() + took));
}

/// Called after each game frame on the frame thread with the frame's whole time.
pub fn frame_done(took: Duration, target: Duration) {
    let swap = SWAP_WAIT.with(|wait| wait.replace(Duration::ZERO));
    let work = took.saturating_sub(swap);
    let thread = unsafe { libc::gettid() };
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if matches!(*state, State::Unknown) {
        *state = if crate::dynarec::property("debug.arm32bridge.hint").as_deref() == Some("0") {
            crate::log::line("[bridge] performance hint off (debug.arm32bridge.hint=0)");
            State::Off
        } else {
            open(thread, target).map_or(State::Off, State::On)
        };
    }
    // The game moved its frames to another thread (a restarted renderer): follow it.
    if let State::On(session) = &*state {
        if session.thread != thread {
            unsafe { (session.close)(session.handle) };
            *state = open(thread, target).map_or(State::Off, State::On);
        }
    }
    if let State::On(session) = &*state {
        unsafe { (session.report)(session.handle, work.as_nanos().max(1) as i64) };
    }
}

fn open(thread: i32, target: Duration) -> Option<Session> {
    let library = unsafe { libc::dlopen(c"libandroid.so".as_ptr(), libc::RTLD_NOW) };
    let symbol = |name: &std::ffi::CStr| {
        let address = if library.is_null() { std::ptr::null_mut() } else { unsafe { libc::dlsym(library, name.as_ptr()) } };
        (!address.is_null()).then_some(address)
    };
    let (Some(get_manager), Some(create), Some(report), Some(close)) = (
        symbol(c"APerformanceHint_getManager"),
        symbol(c"APerformanceHint_createSession"),
        symbol(c"APerformanceHint_reportActualWorkDuration"),
        symbol(c"APerformanceHint_closeSession"),
    ) else {
        crate::log::line("[bridge] performance hint: not available on this Android");
        return None;
    };
    let (get_manager, create, report, close): (GetManager, CreateSession, ReportDuration, CloseSession) =
        unsafe { (std::mem::transmute(get_manager), std::mem::transmute(create), std::mem::transmute(report), std::mem::transmute(close)) };
    let manager = unsafe { get_manager() };
    let handle = if manager.is_null() { std::ptr::null_mut() } else { unsafe { create(manager, &thread, 1, target.as_nanos() as i64) } };
    if handle.is_null() {
        crate::log::line("[bridge] performance hint: Android refused a session");
        return None;
    }
    crate::log::line(&format!("[bridge] performance hint on for thread {thread}, target {:.2} ms", target.as_secs_f64() * 1000.0));
    Some(Session { handle, thread, report, close })
}
