//! Apple's C library (libSystem) for the game.

pub mod extra;
pub mod io;
pub mod libc;
pub mod mach;
pub mod net;
pub mod threads;
pub mod timers;

use crate::symbols::Registry;

pub fn register(r: &mut Registry) {
    libc::register(r);
    io::register(r);
    threads::register(r);
    mach::register(r);
    extra::register(r);
    net::register(r);
    timers::register(r);
}

/// The system's own error number for this thread (named differently on Android).
pub fn host_errno() -> *mut i32 {
    #[cfg(target_os = "android")]
    unsafe {
        ::libc::__errno()
    }
    #[cfg(not(target_os = "android"))]
    unsafe {
        ::libc::__errno_location()
    }
}

/// System functions the libc crate doesn't declare on every system.
pub mod host {
    extern "C" {
        pub fn rand() -> i32;
        pub fn srand(seed: u32);
        pub fn pthread_attr_setschedparam(attr: *mut ::libc::pthread_attr_t, param: *const ::libc::sched_param) -> i32;
        pub fn pthread_attr_getschedparam(attr: *const ::libc::pthread_attr_t, param: *mut ::libc::sched_param) -> i32;
    }
}
