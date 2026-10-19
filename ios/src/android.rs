//! The Game Bridge phone app's way in: it hands over its window, the game's
//! folder and a writable home folder, then passes touches along.
//! (dev.braxten.gamebridge.IosGameActivity)

use jni_sys::*;
use std::ffi::{c_char, CStr};
use std::sync::atomic::{AtomicU32, Ordering};

/// The Java side, for calls back into the game screen: (JavaVM, activity).
/// (JavaVM, game screen); the screen is replaced when a new one opens.
static JAVA: std::sync::Mutex<Option<(usize, usize)>> = std::sync::Mutex::new(None);

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
    this: jobject,
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
        // Each start writes a new log; the one before is kept beside it.
        let _ = std::fs::rename(format!("{home}/ios-run.log"), format!("{home}/ios-run.prev.log"));
        std::env::set_var("IOS_LOG_FILE", format!("{home}/ios-run.log"));
    }
    // An Apple feature we don't have yet answers "nothing" (0), as asking a
    // missing object does on iOS, and the game carries on; the log notes it.
    crate::trap::KEEP_GOING.store(true, Ordering::Relaxed);
    // Test switches for the game screen (it has no command line; they win
    // over the settings above, IOS_HOME included):
    // setprop debug.gamebridge.ios.env "IOS_STACKS_AT=40;IOS_TAPS=20:550,300"
    let mut extra = [0u8; 128];
    if libc::__system_property_get(c"debug.gamebridge.ios.env".as_ptr(), extra.as_mut_ptr() as *mut c_char) > 0 {
        let extra = CStr::from_ptr(extra.as_ptr() as *const c_char).to_string_lossy().into_owned();
        for pair in extra.split(';') {
            if let Some((key, value)) = pair.split_once('=') {
                std::env::set_var(key.trim(), value.trim());
            }
        }
    }
    // The game's screen is sized from the real display (see screen_points).
    if std::env::var_os("IOS_DISPLAY").is_none() && width > 0 && height > 0 {
        std::env::set_var("IOS_DISPLAY", format!("{width}x{height}"));
    }
    WINDOW_WIDTH.store(width.max(1) as u32, Ordering::Relaxed);
    WINDOW_HEIGHT.store(height.max(1) as u32, Ordering::Relaxed);
    let window = native_window(env, surface);
    if window == 0 {
        crate::log!("phone app: no window to draw in");
        return JNI_FALSE;
    }
    crate::fw::eagl::set_native_window(window);
    NATIVE_WINDOW.store(window, Ordering::SeqCst);
    let functions = &**env;
    let mut vm: *mut JavaVM = std::ptr::null_mut();
    if (functions.GetJavaVM.unwrap())(env, &mut vm) == 0 {
        let activity = (functions.NewGlobalRef.unwrap())(env, this);
        *JAVA.lock().unwrap() = Some((vm as usize, activity as usize));
        crate::fw::media::set_video_screen(crate::fw::media::VideoScreen { show: show_video, pause: pause_video, resume: resume_video, stop: stop_video });
    }
    crate::log!("phone app: starting {app} ({width}x{height} window)");
    let started = std::thread::Builder::new().name("ios-main".into()).stack_size(16 << 20).spawn(move || {
        if let Err(error) = crate::run::run(&app) {
            crate::log!("STOP: can't start {app}: {error}");
            crate::report::finish(1);
        }
    });
    if started.is_ok() { JNI_TRUE } else { JNI_FALSE }
}

/// nativeSetReport(game name, folder): where a report goes when the game
/// stops by mistake (IOS_GAME_NAME, IOS_REPORT_DIR). Set here, not from
/// Java: under the emulator's ARM translation the game's C library has its
/// own copy of the environment.
#[no_mangle]
pub unsafe extern "C" fn Java_dev_braxten_gamebridge_IosGameActivity_nativeSetReport(env: *mut JNIEnv, _this: jobject, name: jstring, folder: jstring) {
    std::env::set_var("IOS_GAME_NAME", text(env, name));
    std::env::set_var("IOS_REPORT_DIR", text(env, folder));
}

/// nativeSetScreenRate(hz): the screen's top refresh rate (IOS_SCREEN_HZ),
/// so games with their own frame limit can be let up to it (fw::paths).
#[no_mangle]
pub unsafe extern "C" fn Java_dev_braxten_gamebridge_IosGameActivity_nativeSetScreenRate(_env: *mut JNIEnv, _this: jobject, hz: jint) {
    if hz > 0 {
        std::env::set_var("IOS_SCREEN_HZ", hz.to_string());
    }
}

/// nativeNoticeFd(): the end of the "game stopped" pipe the screen watches
/// (see report::notice_pipe), or -1.
#[no_mangle]
pub unsafe extern "C" fn Java_dev_braxten_gamebridge_IosGameActivity_nativeNoticeFd(_env: *mut JNIEnv, _this: jobject) -> jint {
    crate::report::notice_pipe()
}

/// The Android window the game's frames go to (to hand back when it goes).
static NATIVE_WINDOW: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// ANativeWindow_release, from Android's own library.
unsafe fn release_native_window(window: usize) {
    let library = libc::dlopen(c"libandroid.so".as_ptr(), libc::RTLD_NOW);
    let function = libc::dlsym(library, c"ANativeWindow_release".as_ptr());
    if window != 0 && !function.is_null() {
        let release: unsafe extern "C" fn(usize) = std::mem::transmute(function);
        release(window);
    }
}

/// nativePause(): the window is going (the player left the game). The
/// game lets go of it before this returns, then is told it is in the
/// background, like on iOS; its sound stops.
#[no_mangle]
pub unsafe extern "C" fn Java_dev_braxten_gamebridge_IosGameActivity_nativePause(_env: *mut JNIEnv, _this: jobject) {
    crate::log!("phone app: left the game");
    crate::fw::eagl::release_window();
    release_native_window(NATIVE_WINDOW.swap(0, Ordering::SeqCst));
    crate::fw::sound::set_away(true);
    crate::fw::app::change_state(true);
}

/// nativeResume(surface, width, height): the player came back with a new window.
#[no_mangle]
pub unsafe extern "C" fn Java_dev_braxten_gamebridge_IosGameActivity_nativeResume(
    env: *mut JNIEnv,
    this: jobject,
    surface: jobject,
    width: jint,
    height: jint,
) {
    crate::log!("phone app: back in the game ({width}x{height} window)");
    WINDOW_WIDTH.store(width.max(1) as u32, Ordering::Relaxed);
    WINDOW_HEIGHT.store(height.max(1) as u32, Ordering::Relaxed);
    // The game screen may be a new one (the old one was closed): calls
    // back into Java (videos) go to this one. The old reference is kept.
    let mut java = JAVA.lock().unwrap();
    if let Some((vm, _)) = *java {
        let activity = ((**env).NewGlobalRef.unwrap())(env, this);
        *java = Some((vm, activity as usize));
    }
    drop(java);
    let window = native_window(env, surface);
    NATIVE_WINDOW.store(window, Ordering::SeqCst);
    crate::fw::eagl::attach_window(window);
    crate::fw::sound::set_away(false);
    crate::fw::app::change_state(false);
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

/// Detaches a thread this bridge attached to Java, when the thread ends.
struct Attached(*mut JavaVM);

impl Drop for Attached {
    fn drop(&mut self) {
        unsafe { ((**self.0).DetachCurrentThread.unwrap())(self.0) };
    }
}

thread_local! {
    static ATTACHED: std::cell::RefCell<Option<Attached>> = const { std::cell::RefCell::new(None) };
}

/// This thread's JNI environment (attached the first time if needed, and
/// detached again when the thread ends).
unsafe fn java_env() -> Option<(*mut JNIEnv, jobject)> {
    let (vm, activity) = (*JAVA.lock().unwrap())?;
    let vm = vm as *mut JavaVM;
    let mut env: *mut JNIEnv = std::ptr::null_mut();
    let env_out = &mut env as *mut *mut JNIEnv as *mut *mut std::ffi::c_void;
    match ((**vm).GetEnv.unwrap())(vm, env_out, JNI_VERSION_1_6) {
        JNI_OK => {}
        JNI_EDETACHED => {
            if ((**vm).AttachCurrentThread.unwrap())(vm, env_out, std::ptr::null_mut()) != JNI_OK {
                return None;
            }
            ATTACHED.with(|a| *a.borrow_mut() = Some(Attached(vm)));
        }
        _ => return None,
    }
    Some((env, activity as jobject))
}

/// Calls a void method of the game screen with the given arguments.
unsafe fn call_activity(name: &CStr, signature: &CStr, arguments: &[jvalue]) -> bool {
    let Some((env, activity)) = java_env() else { return false };
    let functions = &**env;
    let class = (functions.GetObjectClass.unwrap())(env, activity);
    let method = (functions.GetMethodID.unwrap())(env, class, name.as_ptr(), signature.as_ptr());
    (functions.DeleteLocalRef.unwrap())(env, class);
    if method.is_null() {
        (functions.ExceptionClear.unwrap())(env);
        return false;
    }
    (functions.CallVoidMethodA.unwrap())(env, activity, method, arguments.as_ptr());
    if (functions.ExceptionCheck.unwrap())(env) != 0 {
        (functions.ExceptionClear.unwrap())(env);
        return false;
    }
    true
}

fn show_video(path: &str, token: u64) -> bool {
    unsafe {
        let Some((env, _)) = java_env() else { return false };
        let Ok(text) = std::ffi::CString::new(path) else { return false };
        let functions = &**env;
        let string = (functions.NewStringUTF.unwrap())(env, text.as_ptr());
        let shown = call_activity(c"playVideo", c"(Ljava/lang/String;J)V", &[jvalue { l: string }, jvalue { j: token as jlong }]);
        (functions.DeleteLocalRef.unwrap())(env, string);
        shown
    }
}

fn video_call(name: &CStr, token: u64) {
    unsafe {
        call_activity(name, c"(J)V", &[jvalue { j: token as jlong }]);
    }
}

fn pause_video(token: u64) {
    video_call(c"pauseVideo", token);
}

fn resume_video(token: u64) {
    video_call(c"resumeVideo", token);
}

fn stop_video(token: u64) {
    video_call(c"stopVideo", token);
}

/// nativeVideoEnded(token): the video played to its end, or failed.
#[no_mangle]
pub extern "C" fn Java_dev_braxten_gamebridge_IosGameActivity_nativeVideoEnded(_env: *mut JNIEnv, _this: jobject, token: jlong) {
    crate::fw::media::video_ended(token as u64);
}
