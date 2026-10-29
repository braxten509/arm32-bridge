//! Android app side: this library is packed into a converted game's APK once,
//! as libarm32_bridge.so. Each of the game's 32-bit libraries is replaced by
//! the small stand-in (tools/standin), with the original next to it as
//! lib<name>-arm32.so. When Java loads a stand-in, it calls
//! `arm32_bridge_load`, which loads that original into the one shared guest
//! and connects the game's Java native methods to it (natives.rs), so the
//! same prebuilt bridge works for every game.
//!
//! It also asks Android for 120 Hz on the game's surface and logs a frame
//! counter (tag "arm32bridge") once a second.

use std::ffi::{c_char, c_void, CStr, CString};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use jni_sys::*;

use crate::imports::{jni, jni_host};
use crate::runtime::{call_guest, call_guest_with, engine, hold_gil, shared, Arg};

use crate::imports::threads::TARGET_REFRESH_RATE;

pub enum HostArg {
    Object(jobject),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
}

pub struct HostValue(u64);

impl HostValue {
    pub fn bits(self) -> u64 {
        self.0
    }
    pub fn int(self) -> i32 {
        self.0 as u32 as i32
    }
    pub fn long(self) -> i64 {
        self.0 as i64
    }
    pub fn float(self) -> f32 {
        f32::from_bits(self.0 as u32)
    }
    pub fn double(self) -> f64 {
        f64::from_bits(self.0)
    }
    pub fn object(self) -> jobject {
        self.0 as usize as jobject
    }
}

fn fail(message: &str) -> ! {
    crate::log::write(6, "arm32bridge", message.as_bytes());
    let recent = crate::runtime::recent_calls();
    crate::log::write(6, "arm32bridge", format!("last phone functions: {}", recent.join(", ")).as_bytes());
    std::process::abort();
}

/// Path this library was loaded from (libarm32_bridge.so in the app's native library dir).
fn library_path() -> &'static PathBuf {
    static PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    PATH.get_or_init(|| {
        let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
        unsafe { libc::dladdr(arm32_bridge_load as *const c_void, &mut info) };
        PathBuf::from(unsafe { CStr::from_ptr(info.dli_fname) }.to_string_lossy().into_owned())
    })
}

/// Directory this library was loaded from (the app's native library dir).
pub(crate) fn library_directory() -> &'static PathBuf {
    static DIRECTORY: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIRECTORY.get_or_init(|| library_path().parent().map(PathBuf::from).unwrap_or_default())
}

/// The settings file the converter puts in a known game (converter/src/fixes.rs):
/// `name=value` lines named like the phone settings, e.g.
/// `debug.arm32bridge.unlock.<package>=60`. Named lib*.so so Android unpacks it
/// next to the bridge.
pub const SETTINGS_FILE: &str = "libarm32_bridge_settings.so";

/// A setting from the game's built-in settings file, if it has one.
pub fn game_setting(name: &str) -> Option<String> {
    static SETTINGS: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();
    SETTINGS
        .get_or_init(|| {
            let text = std::fs::read_to_string(library_directory().join(SETTINGS_FILE)).unwrap_or_default();
            let settings: Vec<(String, String)> = text
                .lines()
                .filter_map(|line| line.split_once('='))
                .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
                .filter(|(name, value)| !name.is_empty() && !value.is_empty())
                .collect();
            if !settings.is_empty() {
                crate::log::line(&format!("[bridge] game settings: {settings:?}"));
            }
            settings
        })
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
}

/// Whether a native method named *_nativeInit has returned (see `forward`).
static NATIVE_INIT_DONE: AtomicBool = AtomicBool::new(false);

/// The guest's JavaVM, made on the first load.
static GUEST_VM: std::sync::OnceLock<u32> = std::sync::OnceLock::new();

pub fn guest_vm() -> u32 {
    GUEST_VM.get().copied().unwrap_or(0)
}

/// Originals loaded so far: a NativeActivity game's library can arrive twice
/// (Java's System.loadLibrary, then Android starting the activity).
static LOADED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Loads `original` like a stand-in's JNI_OnLoad does, unless it is loaded already.
pub fn load_original(vm: *mut JavaVM, env: *mut JNIEnv, original: &std::path::Path) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;
    if LOADED.lock().unwrap().iter().any(|loaded| loaded == original) {
        return Ok(());
    }
    let path = CString::new(original.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    load(vm, env, path.as_ptr())
}

/// Called by a stand-in from its JNI_OnLoad: loads its 32-bit original at
/// `original` (and the game libraries it needs) into the shared guest,
/// connects its exported Java_* functions and runs its JNI_OnLoad. Returns
/// null on success, or an error message (never freed) when the stand-in
/// should fall back to its do-nothing methods.
#[no_mangle]
pub extern "C" fn arm32_bridge_load(vm: *mut JavaVM, env: *mut JNIEnv, original: *const c_char) -> *const c_char {
    match load(vm, env, original) {
        Ok(()) => std::ptr::null(),
        Err(error) => {
            crate::log::line(&format!("[bridge] {error}"));
            CString::new(error).unwrap_or_default().into_raw()
        }
    }
}

/// debug.arm32bridge.env="NAME=VALUE;NAME2=VALUE2" sets environment variables
/// the guest reads through getenv, once, before any game code runs. For tests.
fn set_debug_environment() {
    static DONE: std::sync::Once = std::sync::Once::new();
    DONE.call_once(|| {
        let Some(list) = crate::dynarec::property("debug.arm32bridge.env") else { return };
        for (name, value) in list.split(';').filter_map(|pair| pair.split_once('=')) {
            if !name.is_empty() {
                crate::log::line(&format!("[bridge] debug environment {name}={value}"));
                std::env::set_var(name, value);
            }
        }
    });
}

fn load(vm: *mut JavaVM, env: *mut JNIEnv, original: *const c_char) -> Result<(), String> {
    let started = Instant::now();
    let path = PathBuf::from(unsafe { CStr::from_ptr(original) }.to_string_lossy().into_owned());
    if LOADED.lock().unwrap().contains(&path) {
        return Ok(());
    }
    // debug.arm32bridge.trace=1 logs every phone function call (very slow).
    crate::runtime::init(crate::dynarec::property("debug.arm32bridge.trace").as_deref() == Some("1"));
    set_debug_environment();
    jni_host::set_vm(vm);
    let _lock = hold_gil();
    let _entry = jni_host::enter(env);
    let uc = engine();
    let guest_vm = *GUEST_VM.get_or_init(|| jni::create_vm(uc).0);
    crate::elf::set_search_dirs(vec![library_directory().clone()]);

    crate::log::line(&format!("[bridge] loading {}", path.display()));
    let index = crate::elf::open(uc, &path, false).map_err(|e| format!("could not load {}: {e}", path.display()))?;
    // Connect exported Java_* functions first, so the game's own
    // RegisterNatives calls in its JNI_OnLoad take priority, as on a real phone.
    let (exports, onload) = crate::elf::with_loader(|loader| {
        let library = &loader.libraries[index];
        let exports: Vec<(String, u32)> =
            library.exports(uc).into_iter().filter(|(name, _)| name.starts_with("Java_")).collect();
        (exports, library.export(uc, "JNI_OnLoad"))
    });
    crate::natives::register_exports(env, &exports);
    LOADED.lock().unwrap().push(path.clone());
    let Some(onload) = onload else {
        return Ok(());
    };
    let version = call_guest(uc, onload, &[guest_vm, 0]).map_err(|e| format!("game JNI_OnLoad of {} failed: {e}", path.display()))?;
    crate::log::line(&format!(
        "[bridge] {} ready in {} ms (JNI_OnLoad returned {:#x})",
        path.display(),
        started.elapsed().as_millis(),
        version as u32
    ));
    Ok(())
}

struct FrameCounter {
    window_start: Instant,
    frames: u32,
    guest_time: Duration,
    /// Part of `guest_time` spent in phone functions (GL calls, which include waiting for the screen).
    phone_time: Duration,
    slowest: Duration,
    /// Slowest frame not counting phone-function time.
    slowest_own: Duration,
    lock_wait: Duration,
}

static FRAMES: Mutex<Option<FrameCounter>> = Mutex::new(None);

fn count_frame(spent: Duration, phone: Duration, waiting: Duration) {
    let mut counter = FRAMES.lock().unwrap_or_else(|e| e.into_inner());
    let counter = counter.get_or_insert_with(|| FrameCounter {
        window_start: Instant::now(),
        frames: 0,
        guest_time: Duration::ZERO,
        phone_time: Duration::ZERO,
        slowest: Duration::ZERO,
        slowest_own: Duration::ZERO,
        lock_wait: Duration::ZERO,
    });
    counter.frames += 1;
    counter.guest_time += spent;
    counter.phone_time += phone;
    counter.lock_wait += waiting;
    counter.slowest = counter.slowest.max(spent);
    counter.slowest_own = counter.slowest_own.max(spent.saturating_sub(phone));
    let elapsed = counter.window_start.elapsed();
    if elapsed >= Duration::from_secs(1) {
        let fps = counter.frames as f64 / elapsed.as_secs_f64();
        let per_frame = |time: Duration| time.as_secs_f64() * 1000.0 / counter.frames as f64;
        let ms = |time: Duration| time.as_secs_f64() * 1000.0;
        let parallel = if crate::runtime::parallel() {
            format!(
                "; parallel: {} stops of every thread, {} signal helpers",
                crate::runtime::STOPS.load(std::sync::atomic::Ordering::Relaxed),
                crate::imports::signals::borrows()
            )
        } else {
            String::new()
        };
        crate::log::line(&format!(
            "[fps] {fps:.1} fps, game code {:.2} ms/frame (own code {:.2} ms, phone functions {:.2} ms, waiting for the guest lock {:.2} ms; slowest {:.2} ms, own {:.2} ms){parallel}",
            per_frame(counter.guest_time),
            per_frame(counter.guest_time.saturating_sub(counter.phone_time)),
            per_frame(counter.phone_time),
            per_frame(counter.lock_wait),
            ms(counter.slowest),
            ms(counter.slowest_own)
        ));
        *counter = FrameCounter {
            window_start: Instant::now(),
            frames: 0,
            guest_time: Duration::ZERO,
            phone_time: Duration::ZERO,
            slowest: Duration::ZERO,
            slowest_own: Duration::ZERO,
            lock_wait: Duration::ZERO,
        };
    }
}

macro_rules! jcall {
    ($env:expr, $function:ident $(, $arg:expr)*) => {
        unsafe { ((**$env).$function.unwrap())($env $(, $arg)*) }
    };
}

/// Asks Android to run the game's surface at 120 Hz (Surface.setFrameRate, API 30).
/// The game's view through cocos2d-x's Cocos2dxGLSurfaceView.getInstance (newer cocos2d-x).
fn cocos_view(env: *mut JNIEnv) -> Option<jobject> {
    let view_class = jcall!(env, FindClass, c"org/cocos2dx/lib/Cocos2dxGLSurfaceView".as_ptr());
    if view_class.is_null() {
        return None;
    }
    let get_instance = jcall!(
        env,
        GetStaticMethodID,
        view_class,
        c"getInstance".as_ptr(),
        c"()Lorg/cocos2dx/lib/Cocos2dxGLSurfaceView;".as_ptr()
    );
    if get_instance.is_null() {
        return None;
    }
    let view = jcall!(env, CallStaticObjectMethodA, view_class, get_instance, std::ptr::null());
    (!view.is_null()).then_some(view)
}

/// The game's view for any game drawing with android.opengl.GLSurfaceView:
/// its drawing thread keeps a weak reference to the view.
fn gl_thread_view(env: *mut JNIEnv) -> Option<jobject> {
    let thread_class = jcall!(env, FindClass, c"java/lang/Thread".as_ptr());
    let current = jcall!(env, GetStaticMethodID, thread_class, c"currentThread".as_ptr(), c"()Ljava/lang/Thread;".as_ptr());
    let thread = jcall!(env, CallStaticObjectMethodA, thread_class, current, std::ptr::null());
    let gl_thread_class = jcall!(env, FindClass, c"android/opengl/GLSurfaceView$GLThread".as_ptr());
    if thread.is_null() || gl_thread_class.is_null() || jcall!(env, IsInstanceOf, thread, gl_thread_class) == 0 {
        return None;
    }
    let field = jcall!(env, GetFieldID, gl_thread_class, c"mGLSurfaceViewWeakRef".as_ptr(), c"Ljava/lang/ref/WeakReference;".as_ptr());
    if field.is_null() {
        return None;
    }
    let reference = jcall!(env, GetObjectField, thread, field);
    let reference_class = jcall!(env, FindClass, c"java/lang/ref/Reference".as_ptr());
    let get = jcall!(env, GetMethodID, reference_class, c"get".as_ptr(), c"()Ljava/lang/Object;".as_ptr());
    if reference.is_null() || get.is_null() {
        return None;
    }
    let view = jcall!(env, CallObjectMethodA, reference, get, std::ptr::null());
    (!view.is_null()).then_some(view)
}

/// A Java string's text (empty for null).
fn java_text(env: *mut JNIEnv, text: jobject) -> String {
    if text.is_null() {
        return String::new();
    }
    let chars = jcall!(env, GetStringUTFChars, text as jni_sys::jstring, std::ptr::null_mut());
    if chars.is_null() {
        return String::new();
    }
    let owned = unsafe { std::ffi::CStr::from_ptr(chars) }.to_string_lossy().into_owned();
    jcall!(env, ReleaseStringUTFChars, text as jni_sys::jstring, chars);
    owned
}

/// The game's view for games that draw from their own thread (SDL and others): a
/// SurfaceView kept in a field of the class (or object) whose native method was called.
/// Static fields first, then the object's own fields. Returns the view and the field's name.
fn field_view(env: *mut JNIEnv, owner: jobject) -> Option<(jobject, String)> {
    if owner.is_null() {
        return None;
    }
    let class_class = jcall!(env, FindClass, c"java/lang/Class".as_ptr());
    let is_class = jcall!(env, IsInstanceOf, owner, class_class) != 0;
    let class = if is_class { owner as jclass } else { jcall!(env, GetObjectClass, owner) };
    let surface_view = jcall!(env, FindClass, c"android/view/SurfaceView".as_ptr());
    let get_fields = jcall!(env, GetMethodID, class_class, c"getDeclaredFields".as_ptr(), c"()[Ljava/lang/reflect/Field;".as_ptr());
    let field_class = jcall!(env, FindClass, c"java/lang/reflect/Field".as_ptr());
    let get_type = jcall!(env, GetMethodID, field_class, c"getType".as_ptr(), c"()Ljava/lang/Class;".as_ptr());
    let get_modifiers = jcall!(env, GetMethodID, field_class, c"getModifiers".as_ptr(), c"()I".as_ptr());
    let get_name = jcall!(env, GetMethodID, field_class, c"getName".as_ptr(), c"()Ljava/lang/String;".as_ptr());
    if class.is_null()
        || surface_view.is_null()
        || get_fields.is_null()
        || get_type.is_null()
        || get_modifiers.is_null()
        || get_name.is_null()
    {
        return None;
    }
    let fields = jcall!(env, CallObjectMethodA, class, get_fields, std::ptr::null());
    if fields.is_null() || jcall!(env, ExceptionCheck) != 0 {
        return None;
    }
    let mut found_static = None;
    let mut found_in_object = None;
    for i in 0..jcall!(env, GetArrayLength, fields) {
        if found_static.is_some() {
            break;
        }
        // nativeInit-style calls may never return to Java, so local references are let go here.
        let field = jcall!(env, GetObjectArrayElement, fields, i);
        let kind = jcall!(env, CallObjectMethodA, field, get_type, std::ptr::null());
        // JNI IsAssignableFrom(A, B): "can an object of class A be cast to B", so this is
        // true for SurfaceView and its subclasses (SDLSurface), false for Object or View.
        let is_view = !kind.is_null() && jcall!(env, IsAssignableFrom, kind as jclass, surface_view) != 0;
        jcall!(env, DeleteLocalRef, kind);
        if !is_view {
            jcall!(env, DeleteLocalRef, field);
            continue;
        }
        let is_static = jcall!(env, CallIntMethodA, field, get_modifiers, std::ptr::null()) & 8 != 0;
        let id = jcall!(env, FromReflectedField, field);
        let name_object = jcall!(env, CallObjectMethodA, field, get_name, std::ptr::null());
        let name = java_text(env, name_object);
        jcall!(env, DeleteLocalRef, name_object);
        jcall!(env, DeleteLocalRef, field);
        let view = if is_static {
            jcall!(env, GetStaticObjectField, class, id)
        } else if !is_class && found_in_object.is_none() {
            jcall!(env, GetObjectField, owner, id)
        } else {
            continue;
        };
        // The value itself must be a SurfaceView too.
        if view.is_null() || jcall!(env, IsInstanceOf, view, surface_view) == 0 {
            jcall!(env, DeleteLocalRef, view);
            continue;
        }
        if is_static {
            found_static = Some((view, name));
        } else {
            found_in_object = Some((view, name));
        }
    }
    jcall!(env, DeleteLocalRef, fields);
    found_static.or(found_in_object)
}

/// The 120 Hz request is still owed: the game's surface wasn't there (or not valid) yet.
static REFRESH_PENDING: AtomicBool = AtomicBool::new(false);
/// Later tries: at most this many, this far apart, from any native call (cheap: one atomic load).
const REFRESH_RETRIES: u32 = 20;
const REFRESH_RETRY_GAP: Duration = Duration::from_millis(500);
static REFRESH_RETRY: Mutex<(u32, Option<Instant>)> = Mutex::new((0, None));

/// Asks for 120 Hz from a native call made after an earlier request failed.
fn retry_refresh_rate(env: *mut JNIEnv, owner: jobject) {
    {
        let mut retry = REFRESH_RETRY.lock().unwrap_or_else(|e| e.into_inner());
        if retry.1.is_some_and(|last| last.elapsed() < REFRESH_RETRY_GAP) {
            return;
        }
        if retry.0 >= REFRESH_RETRIES {
            REFRESH_PENDING.store(false, Ordering::Relaxed);
            crate::log::line("[bridge] gave up asking for 120 Hz: the game's surface never became ready");
            return;
        }
        retry.0 += 1;
        retry.1 = Some(Instant::now());
    }
    request_refresh_rate(env, owner);
}

/// Looks up the game's surface and asks for 120 Hz; when it isn't reachable yet the request
/// stays pending and later native calls try again (see `retry_refresh_rate`).
fn request_refresh_rate(env: *mut JNIEnv, owner: jobject) {
    // Test switch: debug.arm32bridge.refresh_test=late holds the first request back as if the
    // surface weren't ready, so the retry path can be seen working.
    static HELD_BACK: AtomicBool = AtomicBool::new(false);
    if !HELD_BACK.swap(true, Ordering::Relaxed) && crate::dynarec::property("debug.arm32bridge.refresh_test").as_deref() == Some("late") {
        crate::log::line("[bridge] test: first 120 Hz request held back (debug.arm32bridge.refresh_test=late)");
        REFRESH_PENDING.store(true, Ordering::Relaxed);
        return;
    }
    let asked = ask_refresh_rate(env, owner);
    jcall!(env, ExceptionClear);
    REFRESH_PENDING.store(!asked, Ordering::Relaxed);
}

fn ask_refresh_rate(env: *mut JNIEnv, owner: jobject) -> bool {
    // A failed lookup leaves a Java exception pending; clear it before the next try.
    let view = cocos_view(env)
        .map(|view| (view, "cocos2d-x view".to_string()))
        .or_else(|| {
            jcall!(env, ExceptionClear);
            gl_thread_view(env).map(|view| (view, "GLSurfaceView thread".to_string()))
        })
        .or_else(|| {
            jcall!(env, ExceptionClear);
            field_view(env, owner).map(|(view, name)| (view, format!("field {name}")))
        });
    jcall!(env, ExceptionClear);
    let Some((view, found_in)) = view else {
        crate::log::line("[bridge] could not reach the game's surface to ask for 120 Hz (will try again)");
        return false;
    };
    let surface_view = jcall!(env, FindClass, c"android/view/SurfaceView".as_ptr());
    let get_holder = jcall!(env, GetMethodID, surface_view, c"getHolder".as_ptr(), c"()Landroid/view/SurfaceHolder;".as_ptr());
    let holder = jcall!(env, CallObjectMethodA, view, get_holder, std::ptr::null());
    let holder_class = jcall!(env, FindClass, c"android/view/SurfaceHolder".as_ptr());
    let get_surface = jcall!(env, GetMethodID, holder_class, c"getSurface".as_ptr(), c"()Landroid/view/Surface;".as_ptr());
    let surface =
        if holder.is_null() { std::ptr::null_mut() } else { jcall!(env, CallObjectMethodA, holder, get_surface, std::ptr::null()) };
    let surface_class = jcall!(env, FindClass, c"android/view/Surface".as_ptr());
    let is_valid = jcall!(env, GetMethodID, surface_class, c"isValid".as_ptr(), c"()Z".as_ptr());
    let set_frame_rate = jcall!(env, GetMethodID, surface_class, c"setFrameRate".as_ptr(), c"(FI)V".as_ptr());
    if set_frame_rate.is_null() {
        // Android before 11: nothing to ask, and nothing to try again.
        jcall!(env, ExceptionClear);
        crate::log::line("[bridge] this Android has no Surface.setFrameRate");
        return true;
    }
    if surface.is_null() || is_valid.is_null() || jcall!(env, CallBooleanMethodA, surface, is_valid, std::ptr::null()) == 0 {
        jcall!(env, ExceptionClear);
        crate::log::line(&format!("[bridge] the game's surface ({found_in}) isn't ready for 120 Hz yet (will try again)"));
        return false;
    }
    // FRAME_RATE_COMPATIBILITY_FIXED_SOURCE (1): the game really renders at this rate.
    let arguments = [jvalue { f: TARGET_REFRESH_RATE }, jvalue { i: 1 }];
    jcall!(env, CallVoidMethodA, surface, set_frame_rate, arguments.as_ptr());
    if jcall!(env, ExceptionCheck) != 0 {
        jcall!(env, ExceptionDescribe);
        jcall!(env, ExceptionClear);
        crate::log::line("[bridge] Surface.setFrameRate failed (will try again)");
        return false;
    }
    crate::log::line(&format!("[bridge] asked for {TARGET_REFRESH_RATE} Hz on the game surface ({found_in})"));
    true
}

/// Calls the 32-bit `symbol` for a Java native method and converts the result.
pub fn forward(env: *mut JNIEnv, symbol: &str, address: &AtomicU32, args: &[HostArg], returns: u8) -> HostValue {
    // Android can pause a game before its renderer has started (it restarts the
    // screen when the screen size changes at launch). Older cocos2d-x then reads
    // objects nativeInit hasn't made yet; later versions skip these calls until
    // nativeInit has finished, and so does the bridge (only for games that have one).
    let is_init = symbol.ends_with("_nativeInit");
    if (symbol.ends_with("_nativeOnPause") || symbol.ends_with("_nativeOnResume"))
        && !NATIVE_INIT_DONE.load(Ordering::Relaxed)
        && crate::natives::has_native_ending("_nativeInit")
    {
        crate::log::line(&format!("[bridge] skipped {symbol}: the game hasn't started yet"));
        return HostValue(0);
    }
    let is_frame = symbol.ends_with("_nativeRender");
    let owner = match args.first() {
        Some(HostArg::Object(owner)) => *owner,
        _ => std::ptr::null_mut(),
    };
    if symbol.ends_with("_nativeOnSurfaceChanged") || symbol.ends_with("_nativeInit") {
        request_refresh_rate(env, owner);
    } else if REFRESH_PENDING.load(Ordering::Relaxed) {
        retry_refresh_rate(env, owner);
    }
    if is_frame {
        crate::sound_preload::on_frame(env, library_directory());
    }
    let started = Instant::now();
    let waiting_before = crate::runtime::lock_wait_time();
    let _lock = hold_gil();
    let _entry = jni_host::enter(env);
    let uc = engine();
    let phone_before = crate::runtime::phone_time(uc);
    let mut guest_address = address.load(Ordering::Relaxed);
    if guest_address == 0 {
        let found = shared().guest_exports.get(symbol).copied();
        guest_address = match found {
            Some(found) => found,
            None => fail(&format!("[bridge] the game library has no {symbol}")),
        };
        address.store(guest_address, Ordering::Relaxed);
    }
    let guest_env = shared().state.jni.env;
    let mut guest_args = vec![Arg::Word(guest_env)];
    for arg in args {
        guest_args.push(match *arg {
            HostArg::Object(object) => Arg::Word(jni_host::to_guest(object)),
            HostArg::Int(value) => Arg::Word(value as u32),
            HostArg::Long(value) => Arg::Pair(value as u64),
            HostArg::Float(value) => Arg::Word(value.to_bits()),
            HostArg::Double(value) => Arg::Pair(value.to_bits()),
        });
    }
    let result = match call_guest_with(uc, guest_address, &guest_args) {
        Ok(value) => value,
        Err(error) => fail(&format!("[bridge] {symbol} failed: {error}")),
    };
    let value = match returns {
        b'L' => {
            let object = jni_host::to_host(result as u32);
            if object.is_null() {
                0
            } else {
                jcall!(env, NewLocalRef, object) as usize as u64
            }
        }
        b'J' | b'D' => result,
        _ => result & 0xFFFF_FFFF,
    };
    if is_frame {
        crate::perf_hint::frame_done(started.elapsed(), Duration::from_secs_f32(1.0 / TARGET_REFRESH_RATE));
        count_frame(
            started.elapsed(),
            crate::runtime::phone_time(uc) - phone_before,
            crate::runtime::lock_wait_time() - waiting_before,
        );
    }
    if is_init {
        NATIVE_INIT_DONE.store(true, Ordering::Relaxed);
    }
    HostValue(value)
}
