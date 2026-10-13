//! Android app side: this library replaces the game's 32-bit native library
//! inside its APK. Java loads it like the original; `JNI_OnLoad` loads the
//! real 32-bit library (packed next to it as lib<name>-arm32.so) into the bridge
//! and connects the game's Java native methods to it (natives.rs), so the
//! same prebuilt bridge works for every game.
//!
//! It also asks Android for 120 Hz on the game's surface and logs a frame
//! counter (tag "arm32bridge") once a second.

use std::ffi::{c_void, CStr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use jni_sys::*;

use crate::imports::{jni, jni_host};
use crate::runtime::{call_guest, call_guest_with, engine, hold_gil, shared, Arg};

const TARGET_REFRESH_RATE: f32 = 120.0;

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
    let recent: Vec<&str> = shared().recent.iter().copied().collect();
    crate::log::write(6, "arm32bridge", format!("last phone functions: {}", recent.join(", ")).as_bytes());
    std::process::abort();
}

/// Path this library was loaded from (lib<name>.so in the app's native library dir).
fn library_path() -> &'static PathBuf {
    static PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    PATH.get_or_init(|| {
        let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
        unsafe { libc::dladdr(JNI_OnLoad as *const c_void, &mut info) };
        PathBuf::from(unsafe { CStr::from_ptr(info.dli_fname) }.to_string_lossy().into_owned())
    })
}

/// Directory this library was loaded from (the app's native library dir).
fn library_directory() -> &'static PathBuf {
    static DIRECTORY: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIRECTORY.get_or_init(|| library_path().parent().map(PathBuf::from).unwrap_or_default())
}

#[no_mangle]
pub extern "system" fn JNI_OnLoad(vm: *mut JavaVM, _reserved: *mut c_void) -> jint {
    let started = Instant::now();
    crate::runtime::init(false);
    jni_host::set_vm(vm);
    let mut env: *mut c_void = std::ptr::null_mut();
    unsafe { ((**vm).GetEnv.unwrap())(vm, &mut env, JNI_VERSION_1_6) };

    let _lock = hold_gil();
    let _entry = jni_host::enter(env as *mut JNIEnv);
    let uc = engine();
    let (guest_vm, _guest_env) = jni::create_vm(uc);

    let directory = library_directory().clone();
    let own_name = library_path().file_name().unwrap_or_default().to_string_lossy();
    let path = directory.join(format!("{}{}", own_name.strip_suffix(".so").unwrap_or(&own_name), crate::elf::ORIGINAL_SUFFIX));
    crate::log::line(&format!("[bridge] loading {}", path.display()));
    let mut loader = crate::elf::Loader::new(vec![directory]);
    let index = match loader.load(uc, &path) {
        Ok(index) => index,
        Err(error) => fail(&format!("[bridge] could not load {}: {error}", path.display())),
    };
    for i in 0..loader.libraries.len() {
        if let Err(error) = loader.run_init(uc, i, false) {
            fail(&format!("[bridge] start-up code failed: {error}"));
        }
    }
    // Connect exported Java_* functions first, so the game's own
    // RegisterNatives calls in its JNI_OnLoad take priority, as on a real phone.
    let exports: Vec<(String, u32)> =
        loader.libraries[index].exports(uc).into_iter().filter(|(name, _)| name.starts_with("Java_")).collect();
    crate::natives::register_exports(env as *mut JNIEnv, &exports);
    let Some(onload) = loader.libraries[index].export(uc, "JNI_OnLoad") else {
        return JNI_VERSION_1_6;
    };
    let version = match call_guest(uc, onload, &[guest_vm, 0]) {
        Ok(version) => version as u32,
        Err(error) => fail(&format!("[bridge] game JNI_OnLoad failed: {error}")),
    };
    crate::log::line(&format!(
        "[bridge] game library ready in {} ms (JNI_OnLoad returned {version:#x})",
        started.elapsed().as_millis()
    ));
    JNI_VERSION_1_6
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
}

static FRAMES: Mutex<Option<FrameCounter>> = Mutex::new(None);

fn count_frame(spent: Duration, phone: Duration) {
    let mut counter = FRAMES.lock().unwrap_or_else(|e| e.into_inner());
    let counter = counter.get_or_insert_with(|| FrameCounter {
        window_start: Instant::now(),
        frames: 0,
        guest_time: Duration::ZERO,
        phone_time: Duration::ZERO,
        slowest: Duration::ZERO,
        slowest_own: Duration::ZERO,
    });
    counter.frames += 1;
    counter.guest_time += spent;
    counter.phone_time += phone;
    counter.slowest = counter.slowest.max(spent);
    counter.slowest_own = counter.slowest_own.max(spent.saturating_sub(phone));
    let elapsed = counter.window_start.elapsed();
    if elapsed >= Duration::from_secs(1) {
        let fps = counter.frames as f64 / elapsed.as_secs_f64();
        let per_frame = |time: Duration| time.as_secs_f64() * 1000.0 / counter.frames as f64;
        let ms = |time: Duration| time.as_secs_f64() * 1000.0;
        crate::log::line(&format!(
            "[fps] {fps:.1} fps, game code {:.2} ms/frame (own code {:.2} ms, phone functions {:.2} ms; slowest {:.2} ms, own {:.2} ms)",
            per_frame(counter.guest_time),
            per_frame(counter.guest_time.saturating_sub(counter.phone_time)),
            per_frame(counter.phone_time),
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

fn request_refresh_rate(env: *mut JNIEnv) {
    // A failed lookup leaves a Java exception pending; clear it before the next try.
    let view = cocos_view(env).or_else(|| {
        jcall!(env, ExceptionClear);
        gl_thread_view(env)
    });
    jcall!(env, ExceptionClear);
    let Some(view) = view else {
        crate::log::line("[bridge] could not reach the game's surface to ask for 120 Hz");
        return;
    };
    let surface_view = jcall!(env, FindClass, c"android/view/SurfaceView".as_ptr());
    let get_holder = jcall!(env, GetMethodID, surface_view, c"getHolder".as_ptr(), c"()Landroid/view/SurfaceHolder;".as_ptr());
    let holder = if view.is_null() { std::ptr::null_mut() } else { jcall!(env, CallObjectMethodA, view, get_holder, std::ptr::null()) };
    let holder_class = jcall!(env, FindClass, c"android/view/SurfaceHolder".as_ptr());
    let get_surface = jcall!(env, GetMethodID, holder_class, c"getSurface".as_ptr(), c"()Landroid/view/Surface;".as_ptr());
    let surface =
        if holder.is_null() { std::ptr::null_mut() } else { jcall!(env, CallObjectMethodA, holder, get_surface, std::ptr::null()) };
    let surface_class = jcall!(env, FindClass, c"android/view/Surface".as_ptr());
    let set_frame_rate = jcall!(env, GetMethodID, surface_class, c"setFrameRate".as_ptr(), c"(FI)V".as_ptr());
    if surface.is_null() || set_frame_rate.is_null() {
        jcall!(env, ExceptionClear);
        crate::log::line("[bridge] could not reach the game's surface to ask for 120 Hz");
        return;
    }
    // FRAME_RATE_COMPATIBILITY_FIXED_SOURCE (1): the game really renders at this rate.
    let arguments = [jvalue { f: TARGET_REFRESH_RATE }, jvalue { i: 1 }];
    jcall!(env, CallVoidMethodA, surface, set_frame_rate, arguments.as_ptr());
    if jcall!(env, ExceptionCheck) != 0 {
        jcall!(env, ExceptionDescribe);
        jcall!(env, ExceptionClear);
        crate::log::line("[bridge] Surface.setFrameRate failed");
        return;
    }
    crate::log::line(&format!("[bridge] asked for {TARGET_REFRESH_RATE} Hz on the game surface"));
}

/// Calls the 32-bit `symbol` for a Java native method and converts the result.
pub fn forward(env: *mut JNIEnv, symbol: &str, address: &AtomicU32, args: &[HostArg], returns: u8) -> HostValue {
    let is_frame = symbol.ends_with("_nativeRender");
    if symbol.ends_with("_nativeOnSurfaceChanged") || symbol.ends_with("_nativeInit") {
        request_refresh_rate(env);
    }
    if is_frame {
        crate::sound_preload::on_frame(env, library_directory());
    }
    let started = Instant::now();
    let phone_before = crate::runtime::phone_time();
    let _lock = hold_gil();
    let _entry = jni_host::enter(env);
    let uc = engine();
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
        count_frame(started.elapsed(), crate::runtime::phone_time() - phone_before);
    }
    HostValue(value)
}
