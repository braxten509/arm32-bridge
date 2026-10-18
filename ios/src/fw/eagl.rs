//! EAGLContext on top of EGL. Each EAGLContext is an OpenGL ES 3 context.
//! iPhone games draw into a renderbuffer whose memory comes from the
//! layer (`renderbufferStorage:fromDrawable:`) and show it with
//! `presentRenderbuffer:`. Here that renderbuffer is an ordinary one of the
//! layer's pixel size; presenting copies it to the window (on the phone) or,
//! with no window (PC tests), can save it as a PNG picture.

use crate::fw::geometry::Rect;
use crate::fw::gl::{self, system, Enum};
use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::uikit::{property_get, property_set};
use crate::objc::runtime::{self, Class, Id, Sel};
use crate::symbols::{leak, Registry};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

type Display = usize;
type Config = usize;
type Context = usize;
type Surface = usize;

system! {
    eglGetDisplay(native: usize) -> Display;
    eglGetPlatformDisplayEXT(platform: Enum, native: usize, attributes: *const i32) -> Display;
    eglInitialize(display: Display, major: *mut i32, minor: *mut i32) -> u32;
    eglBindAPI(api: Enum) -> u32;
    eglChooseConfig(display: Display, attributes: *const i32, configs: *mut Config, size: i32, count: *mut i32) -> u32;
    eglCreateContext(display: Display, config: Config, share: Context, attributes: *const i32) -> Context;
    eglCreatePbufferSurface(display: Display, config: Config, attributes: *const i32) -> Surface;
    eglCreateWindowSurface(display: Display, config: Config, window: usize, attributes: *const i32) -> Surface;
    eglMakeCurrent(display: Display, draw: Surface, read: Surface, context: Context) -> u32;
    eglSwapBuffers(display: Display, surface: Surface) -> u32;
    eglQuerySurface(display: Display, surface: Surface, name: i32, value: *mut i32) -> u32;
    eglQueryString(display: Display, name: i32) -> *const libc::c_char;
    eglGetError() -> i32;
    eglSwapInterval(display: Display, interval: i32) -> u32;
}

const EGL_NONE: i32 = 0x3038;
const EGL_RED_SIZE: i32 = 0x3024;
const EGL_GREEN_SIZE: i32 = 0x3023;
const EGL_BLUE_SIZE: i32 = 0x3022;
const EGL_ALPHA_SIZE: i32 = 0x3021;
const EGL_SURFACE_TYPE: i32 = 0x3033;
const EGL_PBUFFER_BIT: i32 = 0x1;
const EGL_WINDOW_BIT: i32 = 0x4;
const EGL_RENDERABLE_TYPE: i32 = 0x3040;
const EGL_OPENGL_ES3_BIT: i32 = 0x40;
const EGL_CONTEXT_CLIENT_VERSION: i32 = 0x3098;
const EGL_WIDTH: i32 = 0x3057;
const EGL_HEIGHT: i32 = 0x3056;
const EGL_OPENGL_ES_API: Enum = 0x30A0;
const EGL_PLATFORM_SURFACELESS_MESA: Enum = 0x31DD;
const EGL_EXTENSIONS: i32 = 0x3055;

// ----- the display, chosen once -----

struct Egl {
    display: Display,
    config: Config,
    /// The window to show frames in (phone), or 0 (PC tests: no window).
    window: Surface,
    /// Contexts can be current without a surface.
    surfaceless: bool,
}

static WINDOW: AtomicUsize = AtomicUsize::new(0);

/// The Android app hands over its window (an ANativeWindow) before the game starts.
pub fn set_native_window(window: usize) {
    WINDOW.store(window, Ordering::SeqCst);
}

fn egl() -> &'static Egl {
    static EGL: OnceLock<Egl> = OnceLock::new();
    EGL.get_or_init(|| unsafe {
        let window = WINDOW.load(Ordering::SeqCst);
        let mut display = 0;
        if window == 0 && !cfg!(target_os = "android") {
            // PC tests: Mesa's display that needs no screen.
            display = eglGetPlatformDisplayEXT(EGL_PLATFORM_SURFACELESS_MESA, 0, std::ptr::null());
        }
        if display == 0 {
            display = eglGetDisplay(0);
        }
        let (mut major, mut minor) = (0, 0);
        if display == 0 || eglInitialize(display, &mut major, &mut minor) == 0 {
            crate::log!("STOP: graphics: EGL would not start (error {:#x})", eglGetError());
            crate::report::finish(1);
        }
        eglBindAPI(EGL_OPENGL_ES_API);
        let extensions = std::ffi::CStr::from_ptr(eglQueryString(display, EGL_EXTENSIONS)).to_string_lossy().into_owned();
        let surfaceless = extensions.split_whitespace().any(|e| e == "EGL_KHR_surfaceless_context");
        let surface_type = if window != 0 { EGL_WINDOW_BIT } else { EGL_PBUFFER_BIT };
        let attributes = [
            EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8, EGL_BLUE_SIZE, 8, EGL_ALPHA_SIZE, 8,
            EGL_SURFACE_TYPE, surface_type, EGL_RENDERABLE_TYPE, EGL_OPENGL_ES3_BIT, EGL_NONE,
        ];
        let mut config = 0;
        let mut count = 0;
        eglChooseConfig(display, attributes.as_ptr(), &mut config, 1, &mut count);
        if count == 0 && !(window == 0 && surfaceless && extensions.contains("EGL_KHR_no_config_context")) {
            crate::log!("STOP: graphics: no OpenGL ES 3 setting fits (EGL {major}.{minor})");
            crate::report::finish(1);
        }
        let mut surface = 0;
        if window != 0 {
            surface = eglCreateWindowSurface(display, config, window, [EGL_NONE].as_ptr());
            if surface == 0 {
                crate::log!("STOP: graphics: the window can't be drawn into (error {:#x})", eglGetError());
                crate::report::finish(1);
            }
        }
        crate::log!("graphics: EGL {major}.{minor} ready ({})", if window != 0 { "window" } else { "no window" });
        Egl { display, config: if count == 0 { 0 } else { config }, window: surface, surfaceless }
    })
}

// ----- contexts: per EAGLContext, its EGL context and drawable -----

#[derive(Default)]
struct State {
    egl: Context,
    /// For a context made current without a window or surfaceless support.
    pbuffer: Surface,
    /// Our own framebuffer that reads the presented renderbuffer.
    present_framebuffer: u32,
    present_source: u32,
    size: (i32, i32),
    /// The thread the context is current on (0 = none).
    owner: u64,
}

fn this_thread() -> u64 {
    unsafe { libc::pthread_self() as u64 }
}

fn state_of(context: Id) -> &'static Mutex<State> {
    if let Some(found) = property_get(context, "eaglState") {
        return unsafe { &*(found as *const Mutex<State>) };
    }
    let made = Box::leak(Box::new(Mutex::new(State::default())));
    property_set(context, "eaglState", made as *const _ as u64);
    made
}

thread_local! {
    static CURRENT: Cell<Id> = const { Cell::new(0) };
}

extern "C" fn init_with_api_sharegroup(this: Id, _: Sel, api: usize, sharegroup: Id) -> Id {
    if !(2..=3).contains(&api) {
        crate::log!("graphics: OpenGL ES {api} asked for; only 2 and 3 are offered");
        return 0;
    }
    let egl = egl();
    let share = if sharegroup != 0 { state_of(sharegroup).lock().unwrap().egl } else { 0 };
    let attributes = [EGL_CONTEXT_CLIENT_VERSION, 3, EGL_NONE];
    let context = unsafe { eglCreateContext(egl.display, egl.config, share, attributes.as_ptr()) };
    if context == 0 {
        crate::log!("graphics: OpenGL ES context not made (error {:#x})", unsafe { eglGetError() });
        return 0;
    }
    property_set(this, "api", api as u64);
    state_of(this).lock().unwrap().egl = context;
    crate::log!("graphics: OpenGL ES {api} context {this:#x} made");
    this
}

extern "C" fn init_with_api(this: Id, s: Sel, api: usize) -> Id {
    init_with_api_sharegroup(this, s, api, 0)
}

extern "C" fn api(this: Id, _: Sel) -> usize {
    property_get(this, "api").unwrap_or(2) as usize
}

/// Contexts made with the same sharegroup share textures and buffers; the
/// first context stands for its group.
extern "C" fn sharegroup(this: Id, _: Sel) -> Id {
    this
}

extern "C" fn set_current(_: Class, _: Sel, context: Id) -> bool {
    let egl = egl();
    let me = this_thread();
    let previous = CURRENT.with(|c| c.get());
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    if CALLS.fetch_add(1, Ordering::Relaxed) < 40 {
        let holder = if context == 0 { 0 } else { state_of(context).lock().unwrap().owner };
        crate::log!("graphics: setCurrentContext {context:#x} (this thread had {previous:#x}; context held by {holder:#x}, this thread is {me:#x})");
    }
    if previous != 0 && previous != context {
        state_of(previous).lock().unwrap().owner = 0;
    }
    if context == 0 {
        unsafe { eglMakeCurrent(egl.display, 0, 0, 0) };
        CURRENT.with(|c| c.set(0));
        return true;
    }
    let holder = state_of(context).lock().unwrap().owner;
    if holder != 0 && holder != me && !take_from_main_thread(context, holder) {
        crate::log!("graphics: context {context:#x} is in use on another thread and can't be taken");
        return false;
    }
    let ok = unsafe {
        let mut state = state_of(context).lock().unwrap();
        let surface = if egl.window != 0 {
            egl.window
        } else if egl.surfaceless {
            0
        } else {
            if state.pbuffer == 0 {
                state.pbuffer = eglCreatePbufferSurface(egl.display, egl.config, [EGL_WIDTH, 1, EGL_HEIGHT, 1, EGL_NONE].as_ptr());
            }
            state.pbuffer
        };
        let ok = eglMakeCurrent(egl.display, surface, surface, state.egl);
        if ok != 0 {
            state.owner = me;
        }
        ok
    };
    if ok == 0 {
        crate::log!("graphics: context {context:#x} can't be made current on this thread (EGL error {:#x})", unsafe { eglGetError() });
        return false;
    }
    CURRENT.with(|c| c.set(context));
    true
}

/// iPhone games often set a context up on the main thread and then draw
/// with it on their engine thread without letting go of it first; Apple
/// allows that, EGL does not. The main thread lets go from its run loop.
fn take_from_main_thread(context: Id, holder: u64) -> bool {
    if !crate::fw::runloop::is_main_thread_id(holder) {
        return false;
    }
    let done = std::sync::Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let signal = done.clone();
    crate::fw::runloop::post(crate::fw::runloop::Work::Native(Box::new(move || {
        if CURRENT.with(|c| c.get()) == context {
            unsafe { eglMakeCurrent(egl().display, 0, 0, 0) };
            CURRENT.with(|c| c.set(0));
            state_of(context).lock().unwrap().owner = 0;
        }
        *signal.0.lock().unwrap() = true;
        signal.1.notify_all();
    })));
    let mut finished = done.0.lock().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !*finished {
        let now = std::time::Instant::now();
        if now >= deadline {
            crate::log!("graphics: the main thread did not let go of context {context:#x} within 10 s");
            return false;
        }
        finished = done.1.wait_timeout(finished, deadline - now).unwrap().0;
    }
    crate::log!("graphics: context {context:#x} handed from the main thread to this thread");
    true
}

extern "C" fn current(_: Class, _: Sel) -> Id {
    CURRENT.with(|c| c.get())
}

fn send_rect(object: Id, name: &str) -> Rect {
    let s = runtime::sel(name);
    match runtime::imp_for(object, s) {
        Some(imp) => {
            let f: extern "C" fn(Id, Sel) -> Rect = unsafe { std::mem::transmute(imp) };
            f(object, s)
        }
        None => Rect::default(),
    }
}

fn send_f64(object: Id, name: &str) -> f64 {
    let s = runtime::sel(name);
    match runtime::imp_for(object, s) {
        Some(imp) => {
            let f: extern "C" fn(Id, Sel) -> f64 = unsafe { std::mem::transmute(imp) };
            f(object, s)
        }
        None => 1.0,
    }
}

/// renderbufferStorage:fromDrawable: gives the bound renderbuffer the layer's size in pixels.
extern "C" fn storage_from_drawable(this: Id, _: Sel, target: Enum, layer: Id) -> bool {
    let bounds = send_rect(layer, "bounds");
    let scale = send_f64(layer, "contentsScale").max(1.0);
    let width = (bounds.size.width * scale).round() as i32;
    let height = (bounds.size.height * scale).round() as i32;
    if width <= 0 || height <= 0 {
        crate::log!("graphics: layer {layer:#x} has no size; no drawing memory given");
        return false;
    }
    let properties = runtime::send(layer, "drawableProperties", &[]);
    let color = crate::fw::collections::dictionary_get(properties, crate::fw::string::make("EAGLDrawablePropertyColorFormat"));
    let format = if color != 0 && crate::fw::string::get(color) == "EAGLColorFormatRGB565" { gl::RGB565 } else { gl::RGBA8 };
    unsafe { gl::glRenderbufferStorage(target, format, width, height) };
    state_of(this).lock().unwrap().size = (width, height);
    crate::log!("graphics: screen memory {width}x{height} ({})", if format == gl::RGB565 { "16-bit" } else { "32-bit" });
    gl::check_errors("after giving the screen memory");
    true
}

/// Where the picture goes in a window (OpenGL corners, bottom-left origin):
/// as large as fits, keeping the iPhone screen's shape, centred.
pub fn picture_area(window_width: i32, window_height: i32) -> (i32, i32, i32, i32) {
    let (wide, high, _) = crate::fw::uikit::screen_points();
    let scale = (window_width as f64 / wide).min(window_height as f64 / high);
    let (width, height) = ((wide * scale).round() as i32, (high * scale).round() as i32);
    let (x, y) = ((window_width - width) / 2, (window_height - height) / 2);
    (x, y, x + width, y + height)
}

static FRAMES: AtomicUsize = AtomicUsize::new(0);

pub fn frames_shown() -> usize {
    FRAMES.load(Ordering::Relaxed)
}

/// presentRenderbuffer: shows the bound renderbuffer.
extern "C" fn present(this: Id, _: Sel, _target: Enum) -> bool {
    let egl = egl();
    let frame = FRAMES.fetch_add(1, Ordering::Relaxed) + 1;
    let mut state = state_of(this).lock().unwrap();
    let (width, height) = state.size;
    if frame <= 3 {
        let status = unsafe { gl::glCheckFramebufferStatus(gl::DRAW_FRAMEBUFFER) };
        let mut bound = 0;
        unsafe { gl::glGetIntegerv(gl::DRAW_FRAMEBUFFER_BINDING, &mut bound) };
        crate::log!("graphics: frame {frame}: the game's framebuffer {bound} is {}", if status == gl::FRAMEBUFFER_COMPLETE { "complete".to_string() } else { format!("not complete ({status:#x})") });
    }
    unsafe {
        let mut renderbuffer = 0;
        gl::glGetIntegerv(gl::RENDERBUFFER_BINDING, &mut renderbuffer);
        let (mut read, mut draw) = (0, 0);
        gl::glGetIntegerv(gl::READ_FRAMEBUFFER_BINDING, &mut read);
        gl::glGetIntegerv(gl::DRAW_FRAMEBUFFER_BINDING, &mut draw);
        if state.present_framebuffer == 0 {
            gl::glGenFramebuffers(1, &mut state.present_framebuffer);
        }
        gl::glBindFramebuffer(gl::READ_FRAMEBUFFER, state.present_framebuffer);
        if state.present_source != renderbuffer as u32 {
            gl::glFramebufferRenderbuffer(gl::READ_FRAMEBUFFER, gl::COLOR_ATTACHMENT0, gl::RENDERBUFFER, renderbuffer as u32);
            state.present_source = renderbuffer as u32;
        }
        if egl.window != 0 {
            let scissor = gl::glIsEnabled(gl::SCISSOR_TEST);
            let discard = gl::glIsEnabled(gl::RASTERIZER_DISCARD);
            let mut mask = [0u8; 4];
            gl::glGetBooleanv(gl::COLOR_WRITEMASK, mask.as_mut_ptr());
            gl::glDisable(gl::SCISSOR_TEST);
            gl::glDisable(gl::RASTERIZER_DISCARD);
            gl::glColorMask(1, 1, 1, 1);
            let (mut window_width, mut window_height) = (0, 0);
            eglQuerySurface(egl.display, egl.window, EGL_WIDTH, &mut window_width);
            eglQuerySurface(egl.display, egl.window, EGL_HEIGHT, &mut window_height);
            gl::glBindFramebuffer(gl::DRAW_FRAMEBUFFER, 0);
            let mut clear = [0f32; 4];
            gl::glGetFloatv(gl::COLOR_CLEAR_VALUE, clear.as_mut_ptr());
            gl::glClearColor(0.0, 0.0, 0.0, 1.0);
            gl::glClear(gl::COLOR_BUFFER_BIT);
            let (x0, y0, x1, y1) = picture_area(window_width, window_height);
            gl::glBlitFramebuffer(0, 0, width, height, x0, y0, x1, y1, gl::COLOR_BUFFER_BIT, gl::LINEAR);
            // The iPhone screen ignores alpha and games often leave it at 0;
            // Android would show such a window as see-through.
            gl::glColorMask(0, 0, 0, 1);
            gl::glClear(gl::COLOR_BUFFER_BIT);
            gl::glClearColor(clear[0], clear[1], clear[2], clear[3]);
            gl::glColorMask(mask[0], mask[1], mask[2], mask[3]);
            if scissor != 0 {
                gl::glEnable(gl::SCISSOR_TEST);
            }
            if discard != 0 {
                gl::glEnable(gl::RASTERIZER_DISCARD);
            }
            eglSwapBuffers(egl.display, egl.window);
        } else {
            save_frame_if_asked(frame, width, height);
        }
        gl::glBindFramebuffer(gl::READ_FRAMEBUFFER, read as u32);
        gl::glBindFramebuffer(gl::DRAW_FRAMEBUFFER, draw as u32);
    }
    drop(state);
    gl::check_errors(&format!("in frame {frame}"));
    if frame <= 3 || frame % 300 == 0 {
        crate::log!("graphics: frame {frame} shown ({width}x{height}) at {:.1} s", crate::report::seconds_since_start());
    }
    if let Some(limit) = std::env::var("IOS_FRAME_LIMIT").ok().and_then(|v| v.parse::<usize>().ok()) {
        if frame >= limit {
            crate::log!("graphics: {frame} frames shown; stopping as asked (IOS_FRAME_LIMIT)");
            crate::report::finish(0);
        }
    }
    true
}

/// IOS_FRAMES_DIR saves frames as PNG pictures: the first, and every
/// IOS_FRAME_EVERY-th after it (default 60).
fn save_frame_if_asked(frame: usize, width: i32, height: i32) {
    let Ok(folder) = std::env::var("IOS_FRAMES_DIR") else { return };
    let every = std::env::var("IOS_FRAME_EVERY").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(60).max(1);
    if frame != 1 && frame % every != 0 {
        return;
    }
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    unsafe {
        let mut pack_buffer = 0;
        gl::glGetIntegerv(gl::PIXEL_PACK_BUFFER_BINDING, &mut pack_buffer);
        let mut alignment = 0;
        gl::glGetIntegerv(gl::PACK_ALIGNMENT, &mut alignment);
        gl::glBindBuffer(gl::PIXEL_PACK_BUFFER, 0);
        gl::glPixelStorei(gl::PACK_ALIGNMENT, 4);
        gl::glReadPixels(0, 0, width, height, gl::RGBA, gl::UNSIGNED_BYTE, pixels.as_mut_ptr());
        gl::glPixelStorei(gl::PACK_ALIGNMENT, alignment);
        gl::glBindBuffer(gl::PIXEL_PACK_BUFFER, pack_buffer as u32);
    }
    // The iPhone screen ignores alpha; games often leave it at 0.
    for pixel in pixels.chunks_exact_mut(4) {
        pixel[3] = 255;
    }
    let path = format!("{folder}/frame-{frame:05}.png");
    let _ = std::fs::create_dir_all(&folder);
    match std::fs::write(&path, crate::fw::png::encode(width as u32, height as u32, &pixels, true)) {
        Ok(()) => crate::log!("graphics: frame {frame} saved to {path}"),
        Err(e) => crate::log!("graphics: frame {frame} not saved to {path}: {e}"),
    }
}

pub fn register(r: &mut Registry) {
    let context = define_class("EAGLContext", "NSObject", 8);
    class_methods(context, &[("setCurrentContext:", set_current as usize), ("currentContext", current as usize)]);
    methods(
        context,
        &[
            ("initWithAPI:", init_with_api as usize),
            ("initWithAPI:sharegroup:", init_with_api_sharegroup as usize),
            ("API", api as usize),
            ("sharegroup", sharegroup as usize),
            ("renderbufferStorage:fromDrawable:", storage_from_drawable as usize),
            ("presentRenderbuffer:", present as usize),
        ],
    );
    r.data("kEAGLDrawablePropertyRetainedBacking", leak(crate::fw::string::make("EAGLDrawablePropertyRetainedBacking")));
    r.data("kEAGLDrawablePropertyColorFormat", leak(crate::fw::string::make("EAGLDrawablePropertyColorFormat")));
    r.data("kEAGLColorFormatRGBA8", leak(crate::fw::string::make("EAGLColorFormatRGBA8")));
    r.data("kEAGLColorFormatRGB565", leak(crate::fw::string::make("EAGLColorFormatRGB565")));
}
