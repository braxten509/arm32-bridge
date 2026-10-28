//! The screen's refreshes, as times on the run loop's clock. An iPhone's
//! CADisplayLink calls the game once at each refresh and gives it that
//! refresh's time; a run loop timer of our own, counting on from when it
//! last fired, slowly slides against the real refreshes and now and then
//! lets two game frames meet one refresh, or none. On the phone a helper
//! thread hears Android's refresh signal (its Choreographer) and keeps the
//! newest refresh time and the time between refreshes here; elsewhere (the
//! PC, tests) an even grid at the display link's rate stands in.

use std::sync::atomic::{AtomicU64, Ordering};

/// The newest refresh heard and the time between refreshes (seconds, as
/// f64 bits; 0: not heard).
static NEWEST: AtomicU64 = AtomicU64::new(0);
static PERIOD: AtomicU64 = AtomicU64::new(0);

/// The refreshes: the time of one and the time between them.
pub fn grid() -> (f64, f64) {
    #[cfg(target_os = "android")]
    listen();
    let period = f64::from_bits(PERIOD.load(Ordering::Relaxed));
    let period = if period > 0.0 { period } else { crate::fw::runloop::refresh_seconds() };
    (f64::from_bits(NEWEST.load(Ordering::Relaxed)), period)
}

/// Starts hearing the screen's refreshes, once.
#[cfg(target_os = "android")]
fn listen() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        if let Err(error) = std::thread::Builder::new().name("ios-refreshes".into()).spawn(|| unsafe { choreographer::hear() }) {
            crate::log!("graphics: can't hear the screen's refreshes ({error}); an even grid stands in");
        }
    });
}

/// Android's Choreographer, from its own library: on a thread with a looper
/// of its own, a call at each refresh with the refresh's time (the run
/// loop's clock, CLOCK_MONOTONIC), and one when the time between them
/// changes.
#[cfg(target_os = "android")]
mod choreographer {
    use super::{NEWEST, PERIOD};
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicUsize, Ordering};

    type Callback = extern "C" fn(i64, *mut c_void);
    type Post = unsafe extern "C" fn(*mut c_void, Callback, *mut c_void);

    static CHOREOGRAPHER: AtomicUsize = AtomicUsize::new(0);
    static POST: AtomicUsize = AtomicUsize::new(0);

    pub unsafe fn hear() {
        let library = libc::dlopen(c"libandroid.so".as_ptr(), libc::RTLD_NOW);
        let find = |name: &std::ffi::CStr| if library.is_null() { std::ptr::null_mut() } else { libc::dlsym(library, name.as_ptr()) };
        let (prepare, poll, instance, post) =
            (find(c"ALooper_prepare"), find(c"ALooper_pollOnce"), find(c"AChoreographer_getInstance"), find(c"AChoreographer_postFrameCallback64"));
        if [prepare, poll, instance, post].iter().any(|f| f.is_null()) {
            crate::log!("graphics: no Choreographer; an even grid stands in for the screen's refreshes");
            return;
        }
        let prepare: unsafe extern "C" fn(i32) -> *mut c_void = std::mem::transmute(prepare);
        let poll: unsafe extern "C" fn(i32, *mut i32, *mut i32, *mut *mut c_void) -> i32 = std::mem::transmute(poll);
        let instance: unsafe extern "C" fn() -> *mut c_void = std::mem::transmute(instance);
        prepare(0);
        let choreographer = instance();
        if choreographer.is_null() {
            crate::log!("graphics: no Choreographer for this thread; an even grid stands in for the screen's refreshes");
            return;
        }
        CHOREOGRAPHER.store(choreographer as usize, Ordering::Relaxed);
        POST.store(post as usize, Ordering::Relaxed);
        let rate = find(c"AChoreographer_registerRefreshRateCallback");
        if !rate.is_null() {
            let rate: unsafe extern "C" fn(*mut c_void, Callback, *mut c_void) = std::mem::transmute(rate);
            rate(choreographer, on_rate, std::ptr::null_mut());
        }
        on_frame_post();
        crate::log!("graphics: display links follow the screen's refreshes (Choreographer)");
        loop {
            poll(-1, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
        }
    }

    fn on_frame_post() {
        let post: Post = unsafe { std::mem::transmute(POST.load(Ordering::Relaxed)) };
        unsafe { post(CHOREOGRAPHER.load(Ordering::Relaxed) as *mut c_void, on_frame, std::ptr::null_mut()) };
    }

    extern "C" fn on_frame(nanos: i64, _: *mut c_void) {
        NEWEST.store((nanos as f64 / 1e9).to_bits(), Ordering::Relaxed);
        on_frame_post();
    }

    extern "C" fn on_rate(nanos: i64, _: *mut c_void) {
        if nanos > 0 {
            PERIOD.store((nanos as f64 / 1e9).to_bits(), Ordering::Relaxed);
            crate::log!("graphics: the screen refreshes every {:.2} ms", nanos as f64 / 1e6);
        }
    }
}

/// The first time on the grid through `origin`, `step` apart, after `time`.
pub fn next_on_grid(origin: f64, step: f64, time: f64) -> f64 {
    last_on_grid(origin, step, time) + step
}

/// The last time on the grid through `origin`, `step` apart, at or before `time`.
pub fn last_on_grid(origin: f64, step: f64, time: f64) -> f64 {
    // A hair of give, so a time a rounding error short of a grid time counts as on it.
    origin + ((time - origin) / step + 1e-9).floor() * step
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A call late by a refresh and a half still lands on the next refresh
    /// (none lost to the lateness, none slid off the refreshes), and a
    /// refresh's time is the refresh, not the moment the game asks.
    #[test]
    fn late_frames_stay_on_the_refreshes() {
        let (origin, step) = (10.0, 0.01);
        for (time, next, last) in [(10.0, 10.01, 10.0), (10.0012, 10.01, 10.0), (10.0251, 10.03, 10.02), (9.995, 10.0, 9.99)] {
            assert!((next_on_grid(origin, step, time) - next).abs() < 1e-9, "next after {time}");
            assert!((last_on_grid(origin, step, time) - last).abs() < 1e-9, "last at {time}");
        }
    }
}
