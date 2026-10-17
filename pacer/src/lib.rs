//! Game Bridge's frame helper for 64-bit games.
//!
//! 64-bit games run directly on the phone, not through the bridge, so the
//! converter puts this library inside them, loaded by a small Java content
//! provider before the game's own code starts. It lifts frame caps the way the
//! bridge does for 32-bit games: asks for 120 Hz on the game's window, ends
//! frame-limiter sleeps one refresh after the last frame, turns swap intervals
//! above 1 into 1, and raises Unity's targetFrameRate. Everything it does is
//! logged under the tag "gamebridge".

mod got;
mod hooks;
mod pacing;
mod unity;

use std::sync::Once;

/// Java's System.loadLibrary calls this once the library is loaded.
#[no_mangle]
pub extern "C" fn JNI_OnLoad(_vm: *mut std::ffi::c_void, _reserved: *mut std::ffi::c_void) -> i32 {
    start();
    // JNI_VERSION_1_6
    0x0001_0006
}

fn start() {
    static STARTED: Once = Once::new();
    STARTED.call_once(|| {
        unsafe { hooks::gamebridge_loader_start() };
        let early = hooks::loaded_before_start();
        log(&format!(
            "started in {}{}{}",
            package(),
            if unsafe { hooks::gamebridge_loader_passes_caller() } != 0 { "" } else { " (loader calls without caller address)" },
            if early.is_empty() { String::new() } else { format!("; game libraries already loaded: {}", early.join(", ")) }
        ));
        pacing::setting();
        hooks::patch_new("start");
        hooks::start_watching();
    });
}

/// The app's package name (without a ":process" suffix).
pub fn package() -> String {
    std::fs::read("/proc/self/cmdline")
        .ok()
        .and_then(|line| line.split(|&byte| byte == 0).next().map(|name| String::from_utf8_lossy(name).into_owned()))
        .map(|name| name.split(':').next().unwrap_or_default().to_string())
        .unwrap_or_default()
}

/// A phone setting (`adb shell setprop`).
#[cfg(target_os = "android")]
pub fn property(name: &str) -> Option<String> {
    extern "C" {
        fn __system_property_get(name: *const libc::c_char, value: *mut libc::c_char) -> libc::c_int;
    }
    let name = std::ffi::CString::new(name).ok()?;
    let mut value = [0 as libc::c_char; 92];
    let length = unsafe { __system_property_get(name.as_ptr(), value.as_mut_ptr()) };
    (length > 0).then(|| unsafe { std::ffi::CStr::from_ptr(value.as_ptr()) }.to_string_lossy().into_owned())
}

#[cfg(not(target_os = "android"))]
pub fn property(_name: &str) -> Option<String> {
    None
}

pub fn thread_name() -> String {
    let mut name = [0u8; 16];
    unsafe { libc::prctl(libc::PR_GET_NAME, name.as_mut_ptr()) };
    String::from_utf8_lossy(&name[..name.iter().position(|&byte| byte == 0).unwrap_or(16)]).into_owned()
}

pub fn log(text: &str) {
    #[cfg(target_os = "android")]
    {
        #[link(name = "log")]
        extern "C" {
            fn __android_log_write(priority: i32, tag: *const libc::c_char, text: *const libc::c_char) -> i32;
        }
        let body = std::ffi::CString::new(format!("[pacer] {}", text.replace('\0', ""))).unwrap_or_default();
        unsafe { __android_log_write(4, c"gamebridge".as_ptr(), body.as_ptr()) };
    }
    #[cfg(not(target_os = "android"))]
    eprintln!("[gamebridge] [pacer] {text}");
}
