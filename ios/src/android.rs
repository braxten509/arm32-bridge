//! The Game Bridge phone app's way in: it hands over its window, the game's
//! folder and a writable home folder, then passes touches along.
//! (dev.braxten.gamebridge.IosGameActivity)

use jni_sys::*;
use std::ffi::{c_char, CStr};
use std::sync::atomic::{AtomicU32, Ordering};

static WINDOW_WIDTH: AtomicU32 = AtomicU32::new(1);
static WINDOW_HEIGHT: AtomicU32 = AtomicU32::new(1);

unsafe fn text(env: *mut JNIEnv, value: jstring) -> String {
    let functions = &**env;
    let chars = (functions.GetStringUTFChars.unwrap())(env, value, std::ptr::null_mut());
    let out = CStr::from_ptr(chars as *const c_char).to_string_lossy().into_owned();
    (functions.ReleaseStringUTFChars.unwrap())(env, value, chars);
    out
}

/// ANativeWindow_fromSurface, from Android's own library.
unsafe fn native_window(env: *mut JNIEnv, surface: jobject) -> usize {
    let library = libc::dlopen(c"libandroid.so".as_ptr(), libc::RTLD_NOW);
    let function = libc::dlsym(library, c"ANativeWindow_fromSurface".as_ptr());
    if function.is_null() {
        return 0;
    }
    let from_surface: unsafe extern "C" fn(*mut JNIEnv, jobject) -> usize = std::mem::transmute(function);
    from_surface(env, surface)
}

/// nativeStart(surface, app folder, home folder, window width, height).
#[no_mangle]
pub unsafe extern "C" fn Java_dev_braxten_gamebridge_IosGameActivity_nativeStart(
    env: *mut JNIEnv,
    _this: jobject,
    surface: jobject,
    app: jstring,
    home: jstring,
    width: jint,
    height: jint,
) -> jboolean {
    let app = text(env, app);
    let home = text(env, home);
    let _ = std::fs::create_dir_all(&home);
    std::env::set_var("IOS_HOME", &home);
    if std::env::var_os("IOS_LOG_FILE").is_none() {
        std::env::set_var("IOS_LOG_FILE", format!("{home}/ios-run.log"));
    }
    WINDOW_WIDTH.store(width.max(1) as u32, Ordering::Relaxed);
    WINDOW_HEIGHT.store(height.max(1) as u32, Ordering::Relaxed);
    let window = native_window(env, surface);
    if window == 0 {
        crate::log!("phone app: no window to draw in");
        return JNI_FALSE;
    }
    crate::fw::eagl::set_native_window(window);
    crate::log!("phone app: starting {app} ({width}x{height} window)");
    let started = std::thread::Builder::new().name("ios-main".into()).stack_size(16 << 20).spawn(move || {
        if let Err(error) = crate::run::run(&app) {
            crate::log!("STOP: can't start {app}: {error}");
            crate::report::finish(1);
        }
    });
    if started.is_ok() { JNI_TRUE } else { JNI_FALSE }
}

/// nativeResize(width, height): the window changed size (touches map through it).
#[no_mangle]
pub extern "C" fn Java_dev_braxten_gamebridge_IosGameActivity_nativeResize(_env: *mut JNIEnv, _this: jobject, width: jint, height: jint) {
    WINDOW_WIDTH.store(width.max(1) as u32, Ordering::Relaxed);
    WINDOW_HEIGHT.store(height.max(1) as u32, Ordering::Relaxed);
}

/// nativeTouch(finger, phase 0 began / 1 moved / 3 ended / 4 cancelled, x, y) in window pixels.
#[no_mangle]
pub extern "C" fn Java_dev_braxten_gamebridge_IosGameActivity_nativeTouch(
    _env: *mut JNIEnv,
    _this: jobject,
    finger: jint,
    phase: jint,
    x: jfloat,
    y: jfloat,
) {
    use crate::fw::touch::Phase;
    let phase = match phase {
        0 => Phase::Began,
        1 => Phase::Moved,
        3 => Phase::Ended,
        _ => Phase::Cancelled,
    };
    let (window_width, window_height) = (WINDOW_WIDTH.load(Ordering::Relaxed) as f32, WINDOW_HEIGHT.load(Ordering::Relaxed) as f32);
    crate::fw::touch::deliver(finger, phase, x, y, window_width, window_height);
}
