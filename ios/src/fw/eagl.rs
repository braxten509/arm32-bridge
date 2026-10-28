//! EAGLContext on top of EGL. An EAGLContext asked for OpenGL ES 2 or 3 is
//! an OpenGL ES 3 context; one asked for OpenGL ES 1 is an OpenGL ES 1.1
//! context (fixed-function drawing, as the oldest games use).
//! iPhone games draw into a renderbuffer whose memory comes from the
//! layer (`renderbufferStorage:fromDrawable:`) and show it with
//! `presentRenderbuffer:`. Here that renderbuffer is an ordinary one of the
//! layer's pixel size; presenting copies it to the window (on the phone) or,
//! with no window (PC tests), can save it as a PNG picture.

use crate::fw::counted;
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
    eglGetCurrentSurface(which: i32) -> Surface;
    eglGetCurrentContext() -> Context;
    eglSwapInterval(display: Display, interval: i32) -> u32;
    eglDestroySurface(display: Display, surface: Surface) -> u32;
    eglDestroyContext(display: Display, context: Context) -> u32;
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
const EGL_OPENGL_ES_BIT: i32 = 0x1;
const EGL_CONTEXT_CLIENT_VERSION: i32 = 0x3098;
const EGL_WIDTH: i32 = 0x3057;
const EGL_HEIGHT: i32 = 0x3056;
const EGL_OPENGL_ES_API: Enum = 0x30A0;
const EGL_PLATFORM_SURFACELESS_MESA: Enum = 0x31DD;
const EGL_EXTENSIONS: i32 = 0x3055;
const EGL_DRAW: i32 = 0x3059;

// ----- the display, chosen once -----

struct Egl {
    display: Display,
    config: Config,
    /// The setting OpenGL ES 1 contexts use: the same one when it serves
    /// both (so they can share the window); None when the system has no ES 1.
    es1_config: Option<Config>,
    /// Started with a window (phone) rather than none (PC and hidden tests).
    has_window: bool,
    /// Contexts can be current without a surface.
    surfaceless: bool,
}

static WINDOW: AtomicUsize = AtomicUsize::new(0);

/// Told when a context is made, with its share context (the 32-bit layer's
/// GLES sharegroup tracking).
static CONTEXT_CREATED: OnceLock<fn(usize, usize)> = OnceLock::new();

/// The 32-bit layer's observer, told of every context and its share context.
pub fn set_context_created_observer(observer: fn(usize, usize)) {
    let _ = CONTEXT_CREATED.set(observer);
}

/// Told when a context's EGL context is destroyed (its EAGLContext's last
/// reference went).
static CONTEXT_DESTROYED: OnceLock<fn(usize)> = OnceLock::new();

pub fn set_context_destroyed_observer(observer: fn(usize)) {
    let _ = CONTEXT_DESTROYED.set(observer);
}

/// The host EGL context current on this thread (0 = none).
pub fn current_egl_context() -> usize {
    unsafe { eglGetCurrentContext() }
}

/// The window surface frames are shown in; 0 while the app is away.
static WINDOW_SURFACE: AtomicUsize = AtomicUsize::new(0);
/// The surface the window's context last drew with (to know when it let go).
static SHOWN_SURFACE: AtomicUsize = AtomicUsize::new(0);

/// Each picture buffer's layer (context, renderbuffer -> CAEAGLLayer), from
/// renderbufferStorage:fromDrawable:.
static BUFFER_LAYERS: Mutex<Vec<((usize, u32), Id)>> = Mutex::new(Vec::new());

/// What a frame from `buffer` comes from, for FrameSources: its layer when
/// known (two buffers a game takes turns with on one layer are one
/// source), and whether that layer is on the screen (no known layer or view
/// counts as shown; see `frames_show`).
fn frame_source(buffer: (usize, u32)) -> ((usize, u32), bool) {
    let layer = BUFFER_LAYERS.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(b, _)| *b == buffer).map(|&(_, layer)| layer);
    let view = layer.map_or(0, crate::fw::layers::view_of);
    let shown = view == 0 || frames_show(crate::fw::uikit::window_of(view) != 0, layer.and_then(crate::fw::pictures::gl_committed));
    (layer.map_or(buffer, |layer| (layer, 0)), shown)
}

/// Whether a GL view's frames reach the screen: an iPhone shows a layer's
/// frames from the commit that adds it until the one that takes it away,
/// so a new screen's GL view waits for the picture list that puts it up
/// (`committed`: whether the last list that went holds it) and an old one
/// keeps showing until the list without it goes. Before any list, it shows
/// while it is in a window.
fn frames_show(in_window: bool, committed: Option<bool>) -> bool {
    committed.unwrap_or(in_window)
}

/// Which picture buffer's frames reach the window (frame_slots.rs).
static SOURCES: Mutex<crate::fw::frame_slots::FrameSources> = Mutex::new(crate::fw::frame_slots::FrameSources::new());

/// The view-backed CAEAGLLayer whose frame the window shows last (None:
/// no frame yet, or one not known by a view's layer). On an iPhone that
/// frame is the layer's contents: it shows only while the layer does
/// (pictures.rs covers it otherwise).
pub fn shown_frame_layer() -> Option<Id> {
    let (layer, renderbuffer) = SOURCES.lock().unwrap_or_else(|e| e.into_inner()).front()?;
    (renderbuffer == 0 && crate::fw::layers::view_of(layer) != 0).then_some(layer)
}

/// When frames shown from the main thread reach the screen (frame_slots.rs).
static WAITING: Mutex<Option<crate::fw::frame_slots::FrameSlots>> = Mutex::new(None);

fn frame_wait<R>(f: impl FnOnce(&mut crate::fw::frame_slots::FrameSlots) -> R) -> R {
    let mut slots = WAITING.lock().unwrap_or_else(|e| e.into_inner());
    f(slots.get_or_insert_with(|| crate::fw::frame_slots::FrameSlots::new(crate::fw::runloop::refresh_seconds())))
}

/// From the main thread's run loop: a frame that has waited its time
/// reaches the screen. Returns when the next waiting one is due.
pub fn show_waiting_frame() -> Option<f64> {
    if WAITING.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|slots| slots.due_at()).is_none() {
        return None;
    }
    let window = WINDOW_SURFACE.load(Ordering::SeqCst);
    if window == 0 || unsafe { eglGetCurrentSurface(EGL_DRAW) } != window {
        // No window, or it moved to another context: the next frame takes its place.
        frame_wait(|slots| slots.forget());
        return None;
    }
    if frame_wait(|slots| slots.due(crate::fw::runloop::media_time())) {
        unsafe { eglSwapBuffers(egl().display, window) };
    }
    frame_wait(|slots| slots.due_at())
}

/// The frame just drawn into the window goes to the screen now, unless two
/// main-thread drawers share the window: then it waits (`show_waiting_frame`).
unsafe fn show_frame(egl: &Egl, window: Surface) {
    let main = crate::fw::runloop::is_main_thread();
    if frame_wait(|slots| slots.after_draw(crate::fw::runloop::media_time(), main)) {
        unsafe { eglSwapBuffers(egl.display, window) };
    }
}

/// The app is going away: the game's frames stop going to the window, and
/// its context moves to a hidden surface at its next frame (waited for up
/// to half a second, the window must be free when this returns).
pub fn release_window() {
    let old = WINDOW_SURFACE.swap(0, Ordering::SeqCst);
    frame_wait(|slots| slots.forget());
    if old == 0 {
        return;
    }
    for _ in 0..50 {
        if SHOWN_SURFACE.load(Ordering::SeqCst) != old {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // Freed now, or (if still current on the drawing thread) once it moves.
    unsafe { eglDestroySurface(egl().display, old) };
    crate::log!("graphics: window let go");
}

/// The app is back with a (new) window: frames go there from the next one.
pub fn attach_window(window: usize) {
    let egl = egl();
    if !egl.has_window || window == 0 {
        return;
    }
    let surface = unsafe { eglCreateWindowSurface(egl.display, egl.config, window, [EGL_NONE].as_ptr()) };
    if surface == 0 {
        crate::log!("graphics: the new window can't be drawn into (error {:#x})", unsafe { eglGetError() });
        return;
    }
    *LAST_FRAME_AT.lock().unwrap() = None;
    WINDOW_SURFACE.store(surface, Ordering::SeqCst);
    crate::log!("graphics: window back");
}

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
        // With a window, contexts that don't show anything (loading threads)
        // get a small private surface, so the same setting must allow both.
        let choices: &[i32] = if window != 0 { &[EGL_WINDOW_BIT | EGL_PBUFFER_BIT, EGL_WINDOW_BIT] } else { &[EGL_PBUFFER_BIT] };
        let choose = |renderable: i32| -> Option<Config> {
            for &surface_type in choices {
                let attributes = [
                    EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8, EGL_BLUE_SIZE, 8, EGL_ALPHA_SIZE, 8,
                    EGL_SURFACE_TYPE, surface_type, EGL_RENDERABLE_TYPE, renderable, EGL_NONE,
                ];
                let (mut config, mut count) = (0, 0);
                eglChooseConfig(display, attributes.as_ptr(), &mut config, 1, &mut count);
                if count > 0 {
                    return Some(config);
                }
            }
            None
        };
        // One setting for OpenGL ES 3 and 1 where there is one: an ES 1 game's
        // context then draws into the same window.
        let both = choose(EGL_OPENGL_ES3_BIT | EGL_OPENGL_ES_BIT);
        let chosen = both.or_else(|| choose(EGL_OPENGL_ES3_BIT));
        let no_config = window == 0 && surfaceless && extensions.contains("EGL_KHR_no_config_context");
        if chosen.is_none() && !no_config {
            crate::log!("STOP: graphics: no OpenGL ES 3 setting fits (EGL {major}.{minor})");
            crate::report::finish(1);
        }
        let config = chosen.unwrap_or(0);
        let es1_config = both.or_else(|| choose(EGL_OPENGL_ES_BIT)).or(no_config.then_some(0));
        let mut surface = 0;
        if window != 0 {
            surface = eglCreateWindowSurface(display, config, window, [EGL_NONE].as_ptr());
            if surface == 0 {
                crate::log!("STOP: graphics: the window can't be drawn into (error {:#x})", eglGetError());
                crate::report::finish(1);
            }
        }
        crate::log!("graphics: EGL {major}.{minor} ready ({})", if window != 0 { "window" } else { "no window" });
        WINDOW_SURFACE.store(surface, Ordering::SeqCst);
        Egl { display, config, es1_config, has_window: window != 0, surfaceless }
    })
}

// ----- contexts: per EAGLContext, its EGL context and drawable -----

#[derive(Default)]
struct State {
    egl: Context,
    /// The EGL setting it was made with (its surfaces must match it).
    config: Config,
    /// An OpenGL ES 1 context.
    es1: bool,
    /// For a context made current without a window or surfaceless support.
    pbuffer: Surface,
    /// Our own framebuffer that reads the presented renderbuffer.
    present_framebuffer: u32,
    present_source: u32,
    /// OpenGL ES 1: the texture a frame is copied into to be drawn to the
    /// window, its size, and what the context offers (asked once).
    present_texture: u32,
    present_texture_size: (i32, i32),
    es1_features: Option<Es1Features>,
    size: (i32, i32),
    /// The thread the context is current on (0 = none).
    owner: u64,
    /// The context it shares objects with, kept alive meanwhile (it stands
    /// for the sharegroup, which Apple keeps while any member lives).
    share_with: Id,
    /// The CAEAGLLayer the context draws into, remembered from
    /// renderbufferStorage:fromDrawable: and kept alive meanwhile.
    drawable: Id,
    /// OpenGL ES 2/3 turned frames: the texture a frame is copied into and
    /// the shader program that draws it to the window, made once per context.
    turn_texture: u32,
    turn_texture_size: (i32, i32),
    turn_framebuffer: u32,
    turn_program: u32,
    turn_uniform: i32,
    turn_sampler_uniform: i32,
    turn_vao: u32,
}

/// The context that uses the window surface: the first one made current
/// (the game's screen context); others (loading threads) get a private
/// surface. Moving a context to another surface later is avoided: on some
/// drivers (the Android emulator's) it loses the bound renderbuffer.
static WINDOW_OWNER: AtomicUsize = AtomicUsize::new(0);

/// The surface a context is made current with.
fn surface_for(egl: &Egl, context: Id, state: &mut State) -> Surface {
    if egl.has_window {
        let owner = match WINDOW_OWNER.compare_exchange(0, context, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => context,
            Err(existing) => existing,
        };
        let window = WINDOW_SURFACE.load(Ordering::SeqCst);
        if owner == context && window != 0 {
            return window;
        }
    }
    if egl.surfaceless {
        return 0;
    }
    hidden_surface(egl, state)
}

/// The context's own 1x1 surface, made on first use.
fn hidden_surface(egl: &Egl, state: &mut State) -> Surface {
    if state.pbuffer == 0 {
        state.pbuffer = unsafe { eglCreatePbufferSurface(egl.display, state.config, [EGL_WIDTH, 1, EGL_HEIGHT, 1, EGL_NONE].as_ptr()) };
    }
    state.pbuffer
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
    /// Whether this thread's current context is an OpenGL ES 1 one.
    static CURRENT_ES1: Cell<bool> = const { Cell::new(false) };
}

/// The thread's current context is an OpenGL ES 1 one: the GL layer then
/// leaves out what only OpenGL ES 2 and 3 have.
pub fn current_is_es1() -> bool {
    CURRENT_ES1.with(|c| c.get())
}

fn set_current_record(context: Id, es1: bool) {
    CURRENT.with(|c| c.set(context));
    CURRENT_ES1.with(|c| c.set(es1));
}

extern "C" fn init_with_api_sharegroup(this: Id, _: Sel, api: usize, sharegroup: Id) -> Id {
    if !(1..=3).contains(&api) {
        crate::log!("graphics: OpenGL ES {api} asked for; only 1, 2 and 3 are offered");
        return 0;
    }
    let egl = egl();
    let config = if api == 1 {
        match egl.es1_config {
            Some(config) => config,
            None => {
                crate::log!("graphics: OpenGL ES 1 asked for, but this system's EGL has no OpenGL ES 1 setting");
                return 0;
            }
        }
    } else {
        egl.config
    };
    let share = if sharegroup != 0 { state_of(sharegroup).lock().unwrap().egl } else { 0 };
    let attributes = [EGL_CONTEXT_CLIENT_VERSION, if api == 1 { 1 } else { 3 }, EGL_NONE];
    let context = unsafe { eglCreateContext(egl.display, config, share, attributes.as_ptr()) };
    if context == 0 {
        crate::log!("graphics: OpenGL ES {api} context not made (error {:#x})", unsafe { eglGetError() });
        return 0;
    }
    property_set(this, "api", api as u64);
    {
        let mut state = state_of(this).lock().unwrap();
        state.egl = context;
        state.config = config;
        state.es1 = api == 1;
        if share != 0 {
            state.share_with = counted::retain(sharegroup);
        }
    }
    counted::track(this, free_context);
    if let Some(observer) = CONTEXT_CREATED.get() {
        observer(context, share);
    }
    crate::log!("graphics: OpenGL ES {api} context {this:#x} made");
    this
}

/// The EAGLContext's last reference went: its EGL context is destroyed (it
/// is current nowhere: being current holds a reference). Objects it shared
/// stay with the other members of its sharegroup, as in EGL.
fn free_context(this: Id) {
    let (context, pbuffer, share_with, drawable) = {
        let mut state = state_of(this).lock().unwrap();
        (std::mem::take(&mut state.egl), std::mem::take(&mut state.pbuffer), std::mem::take(&mut state.share_with), std::mem::take(&mut state.drawable))
    };
    let _ = WINDOW_OWNER.compare_exchange(this, 0, Ordering::SeqCst, Ordering::SeqCst);
    let egl = egl();
    if context != 0 {
        if unsafe { eglDestroyContext(egl.display, context) } != 0 {
            if let Some(observer) = CONTEXT_DESTROYED.get() {
                observer(context);
            }
        } else {
            crate::log!("graphics: context {this:#x} could not be destroyed (EGL error {:#x})", unsafe { eglGetError() });
        }
    }
    if pbuffer != 0 {
        unsafe { eglDestroySurface(egl.display, pbuffer) };
    }
    counted::release(share_with);
    counted::release(drawable);
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
    if context == 0 {
        if unsafe { eglMakeCurrent(egl.display, 0, 0, 0) } == 0 {
            crate::log!("graphics: this thread's context can't be let go (EGL error {:#x})", unsafe { eglGetError() });
            return false;
        }
        if previous != 0 {
            state_of(previous).lock().unwrap().owner = 0;
        }
        set_current_record(0, false);
        counted::release(previous);
        return true;
    }
    // As on Apple, the current context is retained (and the previous one
    // released): a context released while current stays until let go.
    counted::retain(context);
    let holder = state_of(context).lock().unwrap().owner;
    if holder != 0 && holder != me && !take_from_main_thread(context, holder) {
        crate::log!("graphics: context {context:#x} is in use on another thread and can't be taken");
        counted::release(context);
        return false;
    }
    let (ok, es1) = unsafe {
        let mut state = state_of(context).lock().unwrap();
        let surface = surface_for(egl, context, &mut state);
        let ok = eglMakeCurrent(egl.display, surface, surface, state.egl);
        if ok != 0 {
            state.owner = me;
        }
        (ok, state.es1)
    };
    if ok == 0 {
        crate::log!("graphics: context {context:#x} can't be made current on this thread (EGL error {:#x})", unsafe { eglGetError() });
        counted::release(context);
        return false;
    }
    if previous != 0 && previous != context {
        state_of(previous).lock().unwrap().owner = 0;
    }
    set_current_record(context, es1);
    // Already held when it was current here before.
    counted::release(if previous == context { context } else { previous });
    true
}

/// A request to the main thread to let go of a context. If the asking
/// thread gives up waiting, the main thread must no longer act on it.
#[derive(PartialEq)]
enum Handoff {
    Waiting,
    Done,
    GivenUp,
}

/// iPhone games often set a context up on the main thread and then draw
/// with it on their engine thread without letting go of it first; Apple
/// allows that, EGL does not. The main thread lets go from its run loop.
fn take_from_main_thread(context: Id, holder: u64) -> bool {
    if !crate::fw::runloop::is_main_thread_id(holder) {
        return false;
    }
    let handoff = std::sync::Arc::new((Mutex::new(Handoff::Waiting), std::sync::Condvar::new()));
    let signal = handoff.clone();
    crate::fw::runloop::post(crate::fw::runloop::Work::Native(Box::new(move || {
        let mut status = signal.0.lock().unwrap();
        if *status == Handoff::GivenUp {
            return;
        }
        if CURRENT.with(|c| c.get()) == context {
            if unsafe { eglMakeCurrent(egl().display, 0, 0, 0) } != 0 {
                set_current_record(0, false);
                state_of(context).lock().unwrap().owner = 0;
                // This thread's reference (the asking thread holds its own).
                counted::release(context);
            }
        } else {
            // Not really current here any more: the record was out of date.
            let mut state = state_of(context).lock().unwrap();
            if state.owner == holder {
                state.owner = 0;
            }
        }
        *status = Handoff::Done;
        signal.1.notify_all();
    })));
    let mut status = handoff.0.lock().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while *status == Handoff::Waiting {
        let now = std::time::Instant::now();
        if now >= deadline {
            *status = Handoff::GivenUp;
            crate::log!("graphics: the main thread did not let go of context {context:#x} within 10 s");
            return false;
        }
        status = handoff.1.wait_timeout(status, deadline - now).unwrap().0;
    }
    drop(status);
    if state_of(context).lock().unwrap().owner != 0 {
        return false;
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
    {
        let mut state = state_of(this).lock().unwrap();
        state.size = (width, height);
        // The layer is remembered (kept alive) so presenting can see how it
        // is turned on screen; an old one is let go.
        if state.drawable != layer {
            counted::release(state.drawable);
            state.drawable = counted::retain(layer);
        }
    }
    let mut renderbuffer = 0;
    unsafe { gl::glGetIntegerv(gl::RENDERBUFFER_BINDING, &mut renderbuffer) };
    {
        let mut layers = BUFFER_LAYERS.lock().unwrap_or_else(|e| e.into_inner());
        layers.retain(|(b, _)| *b != (this, renderbuffer as u32));
        layers.push(((this, renderbuffer as u32), layer));
    }
    crate::log!("graphics: screen memory {width}x{height} ({})", if format == gl::RGB565 { "16-bit" } else { "32-bit" });
    gl::check_errors("after giving the screen memory");
    true
}

/// Tests: the context's screen memory size, as renderbufferStorage:fromDrawable: sets it.
#[cfg(test)]
pub fn set_drawable_size(context: Id, width: i32, height: i32) {
    state_of(context).lock().unwrap().size = (width, height);
}

/// Tests: a hidden surface of this size stands in for the window (0 x 0: no window again).
#[cfg(test)]
pub fn use_test_window(width: i32, height: i32) {
    let egl = egl();
    let surface = if width > 0 && height > 0 {
        let surface = unsafe { eglCreatePbufferSurface(egl.display, egl.config, [EGL_WIDTH, width, EGL_HEIGHT, height, EGL_NONE].as_ptr()) };
        assert_ne!(surface, 0, "a test window surface was made (EGL error {:#x})", unsafe { eglGetError() });
        surface
    } else {
        0
    };
    let old = WINDOW_SURFACE.swap(surface, Ordering::SeqCst);
    if old != 0 {
        // Freed once no context has it current.
        unsafe { eglDestroySurface(egl.display, old) };
    }
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

/// The linear part (a, b, c, d) of a transform rounded to the quarter turn
/// it is nearest to: a signed permutation matrix (90, 180 or 270 degrees).
/// None when it is straight (identity) or not close to a quarter turn.
fn quarter_turn(a: f64, b: f64, c: f64, d: f64) -> Option<(f64, f64, f64, f64)> {
    // A rotation has equal column lengths; the columns of a turned, scaled
    // view are scaled alike, so this recovers the unit turn.
    let length = (a * a + b * b).sqrt();
    if !length.is_finite() || length <= 0.0 {
        return None;
    }
    let round = |value: f64| -> Option<f64> {
        [-1.0, 0.0, 1.0].into_iter().find(|&whole| (value / length - whole).abs() < 1e-4)
    };
    let (a, b, c, d) = (round(a)?, round(b)?, round(c)?, round(d)?);
    // A turn keeps the picture the same way up: a rotation, not a mirror.
    if (a * d - b * c - 1.0).abs() >= 1e-6 {
        return None;
    }
    // Identity is straight: nothing to turn.
    (a != 1.0 || b != 0.0 || c != 0.0 || d != 1.0).then_some((a, b, c, d))
}

/// The texture coordinates (u, v) of the present quad's four corners, in
/// order (-1,-1), (1,-1), (-1,1), (1,1) in normalized device coordinates,
/// for a layer turned on screen by the linear part (a, b, c, d). A straight
/// layer (or a turn that is not a quarter turn) gives today's corners
/// (0,0), (1,0), (0,1), (1,1).
fn present_corners(turn: Option<(f64, f64, f64, f64)>) -> [f32; 8] {
    let Some((a, b, c, d)) = turn else {
        return [0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0];
    };
    // A quad corner n=(nx,ny) (GL, y up) is the screen point s=(nx,-ny)
    // (UIKit, y down); the layer point is p = R⁻¹·s, where R is the turn
    // [[a,c],[b,d]] and R⁻¹ is [[d,-c],[-b,a]]. Its texture coordinate is
    // u=(p.x+1)/2, v=(1-p.y)/2 (rows are bottom-up; the layer's top is the
    // buffer's last row).
    let mut out = [0.0f32; 8];
    for (corner, (nx, ny)) in out.chunks_exact_mut(2).zip([(-1.0f64, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)]) {
        let (sx, sy) = (nx, -ny);
        let (px, py) = (d * sx - c * sy, -b * sx + a * sy);
        corner[0] = ((px + 1.0) * 0.5) as f32;
        corner[1] = ((1.0 - py) * 0.5) as f32;
    }
    out
}

/// How a context's layer is turned on screen: the linear part of its view's
/// place on the screen, rounded to a quarter turn; None when it is straight
/// (or the layer has no view to turn it).
/// The turn a frame of `size` (its buffer, width x height) is shown with:
/// only a quarter turn of a picture not drawn wide. A game that turns its own
/// drawing draws its buffer wide already (SpellCraft, Dungeon Plunder), and
/// turning that again put the whole picture on its side.
fn turn_for_picture(turn: Option<(f64, f64, f64, f64)>, (width, height): (i32, i32)) -> Option<(f64, f64, f64, f64)> {
    turn.filter(|&(a, ..)| a == 0.0 && width <= height)
}

fn layer_turn(layer: Id) -> Option<(f64, f64, f64, f64)> {
    let view = crate::fw::layers::view_of(layer);
    if view == 0 {
        return None;
    }
    // The picture is drawn straight into the layer's buffer, so its turn is
    // the old-game window turn (the screen turning the window), or, for a GL
    // view hand-turned by the game with no window turn, the view's own
    // transform. The window's own compensation on the view (place_upright) is
    // UIKit's, which the raw picture does not get, so it is left out here.
    let turn = match crate::fw::uikit::window_turn() {
        Some(window_turn) => window_turn,
        None => crate::fw::uikit::view_state(view).transform,
    };
    quarter_turn(turn.a, turn.b, turn.c, turn.d)
}

static FRAMES: AtomicUsize = AtomicUsize::new(0);

pub fn frames_shown() -> usize {
    FRAMES.load(Ordering::Relaxed)
}

/// A gap between two shown frames longer than this is logged as a slow frame
/// (the plan's smoothness goal: no repeated pauses over 50 ms).
const SLOW_FRAME_MS: f64 = 50.0;
static SLOW_FRAMES: AtomicUsize = AtomicUsize::new(0);

/// When the last frame was shown; cleared while the app is away, so time
/// spent away isn't counted as a slow frame.
static LAST_FRAME_AT: Mutex<Option<std::time::Instant>> = Mutex::new(None);

/// Logs the gap since the last shown frame when it is a slow frame; returns
/// the number of slow frames so far.
fn note_frame_gap(frame: usize) -> usize {
    let now = std::time::Instant::now();
    let previous = LAST_FRAME_AT.lock().unwrap().replace(now);
    if let Some(previous) = previous {
        let gap_ms = now.duration_since(previous).as_secs_f64() * 1000.0;
        if gap_ms > SLOW_FRAME_MS {
            SLOW_FRAMES.fetch_add(1, Ordering::Relaxed);
            crate::log!("graphics: slow frame {gap_ms:.1} ms before frame {frame} at {:.1} s", crate::report::seconds_since_start());
        }
    }
    SLOW_FRAMES.load(Ordering::Relaxed)
}

/// presentRenderbuffer: shows the bound renderbuffer.
extern "C" fn present(this: Id, _: Sel, _target: Enum) -> bool {
    // An older screen's buffer still drawing while a newer one has the
    // window: its frame stays off the screen, as its layer would be fading
    // out behind the new one on an iPhone.
    let mut renderbuffer = 0;
    unsafe { gl::glGetIntegerv(gl::RENDERBUFFER_BINDING, &mut renderbuffer) };
    let (source, on_screen) = frame_source((this, renderbuffer as u32));
    {
        let mut sources = SOURCES.lock().unwrap_or_else(|e| e.into_inner());
        let before = sources.front();
        if !sources.shows(source, crate::fw::runloop::media_time(), on_screen) {
            return true;
        }
        if before != Some(source) {
            // A new layer's frame: the picture list says so soon (pictures.rs gl_shown).
            crate::fw::pictures::draw_soon();
        }
    }
    let egl = egl();
    let frame = FRAMES.fetch_add(1, Ordering::Relaxed) + 1;
    let mut state = state_of(this).lock().unwrap();
    let (width, height) = state.size;
    let window = WINDOW_SURFACE.load(Ordering::SeqCst);
    let current = unsafe { eglGetCurrentSurface(EGL_DRAW) };
    if window != 0 && current != window {
        // A context without the window shows a frame: it takes the window
        // over (rare: games show frames from their screen context), or the
        // app came back with a new window.
        WINDOW_OWNER.store(this, Ordering::SeqCst);
        // A context's first surface sets its viewport and scissor box to the
        // surface's size (EGL); the game's own ones stay.
        let (mut viewport, mut scissor) = ([0; 4], [0; 4]);
        unsafe {
            gl::glGetIntegerv(gl::VIEWPORT, viewport.as_mut_ptr());
            gl::glGetIntegerv(gl::SCISSOR_BOX, scissor.as_mut_ptr());
        }
        if unsafe { eglMakeCurrent(egl.display, window, window, state.egl) } == 0 {
            crate::log!("graphics: frame {frame} can't be shown: the window is in use (EGL error {:#x})", unsafe { eglGetError() });
            SOURCES.lock().unwrap_or_else(|e| e.into_inner()).let_go(source);
            return false;
        }
        unsafe {
            gl::glViewport(viewport[0], viewport[1], viewport[2], viewport[3]);
            gl::glScissor(scissor[0], scissor[1], scissor[2], scissor[3]);
        }
    } else if egl.has_window && window == 0 && WINDOW_OWNER.load(Ordering::SeqCst) == this && current != 0 && current != state.pbuffer {
        // The app went away: let go of the old window.
        let hidden = if egl.surfaceless { 0 } else { hidden_surface(egl, &mut state) };
        unsafe { eglMakeCurrent(egl.display, hidden, hidden, state.egl) };
    }
    if WINDOW_OWNER.load(Ordering::SeqCst) == this {
        SHOWN_SURFACE.store(unsafe { eglGetCurrentSurface(EGL_DRAW) }, Ordering::SeqCst);
    }
    if frame <= 3 {
        // OpenGL ES 1 has one framebuffer binding (the same number as ES 3's draw one).
        let target = if state.es1 { gl::FRAMEBUFFER } else { gl::DRAW_FRAMEBUFFER };
        let status = unsafe { gl::glCheckFramebufferStatus(target) };
        let mut bound = 0;
        unsafe { gl::glGetIntegerv(gl::DRAW_FRAMEBUFFER_BINDING, &mut bound) };
        crate::log!("graphics: frame {frame}: the game's framebuffer {bound} is {}", if status == gl::FRAMEBUFFER_COMPLETE { "complete".to_string() } else { format!("not complete ({status:#x})") });
    }
    // A frame still waiting from an earlier refresh reaches the screen
    // before this one is drawn over it.
    if window != 0 && unsafe { eglGetCurrentSurface(EGL_DRAW) } == window && frame_wait(|slots| slots.before_draw(crate::fw::runloop::media_time())) {
        unsafe { eglSwapBuffers(egl.display, window) };
    }
    if state.es1 {
        let turn = turn_for_picture(layer_turn(state.drawable), state.size);
        unsafe { present_es1(egl, &mut state, frame, window, turn) };
    } else {
        let turn = turn_for_picture(layer_turn(state.drawable), state.size);
        unsafe { present_es3(egl, &mut state, frame, window, turn) };
    }
    drop(state);
    gl::check_errors(&format!("in frame {frame}"));
    let slow_frames = note_frame_gap(frame);
    // Test switch IOS_FRAME_MARK_EVERY: these lines more often, for test taps
    // timed by frame without saving pictures (which is slow on the PC).
    static MARK_EVERY: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let mark_every = *MARK_EVERY.get_or_init(|| {
        std::env::var("IOS_FRAME_MARK_EVERY").ok().and_then(|v| v.parse::<usize>().ok()).filter(|&n| n > 0).unwrap_or(300)
    });
    if frame <= 3 || frame % mark_every == 0 {
        crate::log!("graphics: frame {frame} shown ({width}x{height}) at {:.1} s, {slow_frames} slow so far", crate::report::seconds_since_start());
    }
    if let Some(limit) = std::env::var("IOS_FRAME_LIMIT").ok().and_then(|v| v.parse::<usize>().ok()) {
        if frame >= limit {
            crate::log!("graphics: {frame} frames shown; stopping as asked (IOS_FRAME_LIMIT)");
            crate::report::finish(0);
        }
    }
    true
}

/// OpenGL ES 3: the renderbuffer is blitted to the window; a layer turned
/// on screen (a quarter turn) is instead copied into a texture and drawn as
/// a turned quad, so the picture comes out upright.
unsafe fn present_es3(egl: &Egl, state: &mut State, frame: usize, window: Surface, turn: Option<(f64, f64, f64, f64)>) {
    let (width, height) = state.size;
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
        if window != 0 {
            let turned = match turn {
                Some(t) if !transform_feedback_active() => Some(t),
                _ => None,
            };
            if let Some(t) = turned {
                present_es3_turned(egl, state, window, t);
            } else {
                let scissor = gl::glIsEnabled(gl::SCISSOR_TEST);
                let discard = gl::glIsEnabled(gl::RASTERIZER_DISCARD);
                let mut mask = [0u8; 4];
                gl::glGetBooleanv(gl::COLOR_WRITEMASK, mask.as_mut_ptr());
                gl::glDisable(gl::SCISSOR_TEST);
                gl::glDisable(gl::RASTERIZER_DISCARD);
                gl::glColorMask(1, 1, 1, 1);
                let (mut window_width, mut window_height) = (0, 0);
                eglQuerySurface(egl.display, window, EGL_WIDTH, &mut window_width);
                eglQuerySurface(egl.display, window, EGL_HEIGHT, &mut window_height);
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
            }
            show_frame(egl, window);
        } else if !egl.has_window {
            save_frame_if_asked(frame, width, height, false, turn);
        }
        gl::glBindFramebuffer(gl::READ_FRAMEBUFFER, read as u32);
        gl::glBindFramebuffer(gl::DRAW_FRAMEBUFFER, draw as u32);
    }
}

/// Whether transform feedback is active on the current context: drawing the
/// present quad with it on is not allowed, so a turned frame falls back to
/// the straight blit then.
fn transform_feedback_active() -> bool {
    let mut active = [0u8; 1];
    unsafe { gl::glGetBooleanv(gl::TRANSFORM_FEEDBACK_ACTIVE, active.as_mut_ptr()) };
    active[0] != 0
}

/// The game's OpenGL ES 2/3 state that drawing the turned present quad
/// changes (everything is put back after).
struct Es3Saved {
    program: i32,
    vao: i32,
    active_texture: i32,
    texture_2d: i32,
    sampler: i32,
    viewport: [i32; 4],
    clear: [f32; 4],
    mask: [u8; 4],
    /// Switched-off capabilities that were on (switched back on after).
    enabled: Vec<Enum>,
}

impl Es3Saved {
    unsafe fn save() -> Self {
        unsafe {
            let get = |name| {
                let mut value = 0;
                gl::glGetIntegerv(name, &mut value);
                value
            };
            let on = |cap| gl::glIsEnabled(cap) != 0;
            let mut viewport = [0; 4];
            gl::glGetIntegerv(gl::VIEWPORT, viewport.as_mut_ptr());
            let mut clear = [0f32; 4];
            gl::glGetFloatv(gl::COLOR_CLEAR_VALUE, clear.as_mut_ptr());
            let mut mask = [0u8; 4];
            gl::glGetBooleanv(gl::COLOR_WRITEMASK, mask.as_mut_ptr());
            let capabilities = [
                gl::BLEND, gl::DEPTH_TEST, gl::STENCIL_TEST, gl::CULL_FACE, gl::POLYGON_OFFSET_FILL,
                gl::SAMPLE_ALPHA_TO_COVERAGE, gl::SAMPLE_COVERAGE, gl::SCISSOR_TEST, gl::RASTERIZER_DISCARD, gl::DITHER,
            ];
            let enabled = capabilities.into_iter().filter(|&cap| on(cap)).collect();
            Es3Saved {
                program: get(gl::CURRENT_PROGRAM),
                vao: get(gl::VERTEX_ARRAY_BINDING),
                active_texture: get(gl::ACTIVE_TEXTURE),
                texture_2d: {
                    gl::glActiveTexture(gl::TEXTURE0);
                    get(gl::TEXTURE_BINDING_2D)
                },
                sampler: get(gl::SAMPLER_BINDING),
                viewport,
                clear,
                mask,
                enabled,
            }
        }
    }

    unsafe fn restore(&self) {
        unsafe {
            gl::glUseProgram(self.program as u32);
            gl::glBindVertexArray(self.vao as u32);
            gl::glActiveTexture(gl::TEXTURE0);
            gl::glBindTexture(gl::TEXTURE_2D, self.texture_2d as u32);
            gl::glBindSampler(0, self.sampler as u32);
            gl::glActiveTexture(self.active_texture as Enum);
            for &cap in &self.enabled {
                gl::glEnable(cap);
            }
            gl::glViewport(self.viewport[0], self.viewport[1], self.viewport[2], self.viewport[3]);
            gl::glClearColor(self.clear[0], self.clear[1], self.clear[2], self.clear[3]);
            gl::glColorMask(self.mask[0], self.mask[1], self.mask[2], self.mask[3]);
        }
    }
}

/// The tiny shader program that draws a present quad with no vertex buffers:
/// the position comes from gl_VertexID and the texture coordinate from the
/// turn (a four-float inverse-turn matrix), made once per context.
unsafe fn make_present_program() -> (u32, i32, i32) {
    unsafe {
        let vertex = b"#version 300 es\nuniform vec4 u_turn;\nout vec2 v_uv;\nvoid main() {\n    vec2 n = vec2(float((gl_VertexID & 1) * 2 - 1), float((gl_VertexID >> 1) * 2 - 1));\n    vec2 s = vec2(n.x, -n.y);\n    vec2 p = vec2(dot(u_turn.xy, s), dot(u_turn.zw, s));\n    gl_Position = vec4(n, 0.0, 1.0);\n    v_uv = vec2((p.x + 1.0) * 0.5, (1.0 - p.y) * 0.5);\n}\n";
        let fragment = b"#version 300 es\nprecision mediump float;\nuniform sampler2D u_texture;\nin vec2 v_uv;\nout vec4 v_color;\nvoid main() {\n    v_color = texture(u_texture, v_uv);\n}\n";
        let program = gl::glCreateProgram();
        let compile = |kind: gl::Enum, text: &[u8]| -> u32 {
            let shader = gl::glCreateShader(kind);
            gl::glShaderSource(shader, 1, &text.as_ptr(), &(text.len() as i32));
            gl::glCompileShader(shader);
            let mut ok = 0;
            gl::glGetShaderiv(shader, gl::COMPILE_STATUS, &mut ok);
            if ok == 0 {
                let mut log = vec![0u8; 1024];
                let mut length = 0;
                gl::glGetShaderInfoLog(shader, log.len() as i32, &mut length, log.as_mut_ptr());
                log.truncate(length.max(0) as usize);
                crate::log!("graphics: present shader did not compile: {}", String::from_utf8_lossy(&log).trim());
            }
            gl::glAttachShader(program, shader);
            shader
        };
        let vertex_shader = compile(gl::VERTEX_SHADER, vertex);
        let fragment_shader = compile(gl::FRAGMENT_SHADER, fragment);
        gl::glLinkProgram(program);
        let mut ok = 0;
        gl::glGetProgramiv(program, gl::LINK_STATUS, &mut ok);
        if ok == 0 {
            let mut log = vec![0u8; 1024];
            let mut length = 0;
            gl::glGetProgramInfoLog(program, log.len() as i32, &mut length, log.as_mut_ptr());
            log.truncate(length.max(0) as usize);
            crate::log!("graphics: present program did not link: {}", String::from_utf8_lossy(&log).trim());
        }
        gl::glDeleteShader(vertex_shader);
        gl::glDeleteShader(fragment_shader);
        let turn = gl::glGetUniformLocation(program, b"u_turn\0".as_ptr() as *const libc::c_char);
        let texture = gl::glGetUniformLocation(program, b"u_texture\0".as_ptr() as *const libc::c_char);
        (program, turn, texture)
    }
}

/// Draws the turned picture: copies the read framebuffer's renderbuffer into
/// the context's own texture, then draws it over the window (framebuffer 0)
/// as a quad, exactly as the straight path blits it. Everything the draw
/// touches is put back.
unsafe fn present_es3_turned(egl: &Egl, state: &mut State, window: Surface, turn: (f64, f64, f64, f64)) {
    let (width, height) = state.size;
    unsafe {
        let saved = Es3Saved::save();
        // The scissor box and colour mask would clip the copy and the draw;
        // everything is put back by `saved`.
        for &cap in &[gl::BLEND, gl::DEPTH_TEST, gl::STENCIL_TEST, gl::CULL_FACE, gl::POLYGON_OFFSET_FILL, gl::SAMPLE_ALPHA_TO_COVERAGE, gl::SAMPLE_COVERAGE, gl::SCISSOR_TEST, gl::RASTERIZER_DISCARD, gl::DITHER] {
            gl::glDisable(cap);
        }
        gl::glColorMask(1, 1, 1, 1);
        // The picture's texture (RGBA8; an RGB565 source is converted on the copy).
        if state.turn_texture == 0 {
            gl::glGenTextures(1, &mut state.turn_texture);
        }
        gl::glActiveTexture(gl::TEXTURE0);
        gl::glBindTexture(gl::TEXTURE_2D, state.turn_texture);
        gl::glBindSampler(0, 0);
        let size = (width.max(1), height.max(1));
        if state.turn_texture_size != size {
            gl::glTexImage2D(gl::TEXTURE_2D, 0, gl::RGBA8 as i32, size.0, size.1, 0, gl::RGBA, gl::UNSIGNED_BYTE, std::ptr::null());
            gl::glTexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
            gl::glTexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
            gl::glTexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
            gl::glTexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
            state.turn_texture_size = size;
        }
        if state.turn_framebuffer == 0 {
            gl::glGenFramebuffers(1, &mut state.turn_framebuffer);
            gl::glBindFramebuffer(gl::DRAW_FRAMEBUFFER, state.turn_framebuffer);
            gl::glFramebufferTexture2D(gl::DRAW_FRAMEBUFFER, gl::COLOR_ATTACHMENT0, gl::TEXTURE_2D, state.turn_texture, 0);
        } else {
            gl::glBindFramebuffer(gl::DRAW_FRAMEBUFFER, state.turn_framebuffer);
        }
        // The read framebuffer (the renderbuffer) into our texture.
        gl::glBlitFramebuffer(0, 0, width, height, 0, 0, width, height, gl::COLOR_BUFFER_BIT, gl::NEAREST);
        // Draw the quad over the window.
        gl::glBindFramebuffer(gl::DRAW_FRAMEBUFFER, 0);
        let (mut window_width, mut window_height) = (0, 0);
        eglQuerySurface(egl.display, window, EGL_WIDTH, &mut window_width);
        eglQuerySurface(egl.display, window, EGL_HEIGHT, &mut window_height);
        gl::glClearColor(0.0, 0.0, 0.0, 1.0);
        gl::glClear(gl::COLOR_BUFFER_BIT);
        let (x0, y0, x1, y1) = picture_area(window_width, window_height);
        gl::glViewport(x0, y0, x1 - x0, y1 - y0);
        if state.turn_program == 0 {
            (state.turn_program, state.turn_uniform, state.turn_sampler_uniform) = make_present_program();
        }
        gl::glUseProgram(state.turn_program);
        gl::glUniform1i(state.turn_sampler_uniform, 0);
        // The inverse turn R⁻¹ = [[d,-c],[-b,a]], as the shader's u_turn.
        let (a, b, c, d) = turn;
        let turn = [d as f32, -c as f32, -b as f32, a as f32];
        gl::glUniform4fv(state.turn_uniform, 1, turn.as_ptr());
        if state.turn_vao == 0 {
            gl::glGenVertexArrays(1, &mut state.turn_vao);
        }
        gl::glBindVertexArray(state.turn_vao);
        gl::glDrawArrays(gl::TRIANGLE_STRIP, 0, 4);
        // The iPhone screen ignores alpha (see the straight path).
        gl::glColorMask(0, 0, 0, 1);
        gl::glClear(gl::COLOR_BUFFER_BIT);
        saved.restore();
    }
}


/// What an OpenGL ES 1 context offers that the present quad must switch
/// off or step around (asked once per context).
#[derive(Clone, Copy)]
struct Es1Features {
    texture_units: u32,
    clip_planes: u32,
    cube_maps: bool,
    matrix_palette: bool,
    point_size_array: bool,
}

unsafe fn es1_features() -> Es1Features {
    let get = |name| {
        let mut value = 0;
        unsafe { gl::glGetIntegerv(name, &mut value) };
        value.max(0) as u32
    };
    Es1Features {
        texture_units: get(gl::es1::MAX_TEXTURE_UNITS).max(1),
        clip_planes: get(gl::es1::MAX_CLIP_PLANES),
        cube_maps: gl::system_has_extension("GL_OES_texture_cube_map"),
        matrix_palette: gl::system_has_extension("GL_OES_matrix_palette"),
        point_size_array: gl::system_has_extension("GL_OES_point_size_array"),
    }
}

/// OpenGL ES 1 has no framebuffer blit: the presented renderbuffer is copied
/// into a texture (on the graphics chip) and drawn to the window as one
/// fixed-function quad. Every bit of state the quad touches is put back.
unsafe fn present_es1(egl: &Egl, state: &mut State, frame: usize, window: Surface, turn: Option<(f64, f64, f64, f64)>) {
    use gl::es1::*;
    let (width, height) = state.size;
    unsafe {
        let mut renderbuffer = 0;
        gl::glGetIntegerv(gl::RENDERBUFFER_BINDING, &mut renderbuffer);
        let mut framebuffer = 0;
        gl::glGetIntegerv(FRAMEBUFFER_BINDING, &mut framebuffer);
        if state.present_framebuffer == 0 {
            gl::glGenFramebuffers(1, &mut state.present_framebuffer);
        }
        gl::glBindFramebuffer(gl::FRAMEBUFFER, state.present_framebuffer);
        if state.present_source != renderbuffer as u32 {
            gl::glFramebufferRenderbuffer(gl::FRAMEBUFFER, gl::COLOR_ATTACHMENT0, gl::RENDERBUFFER, renderbuffer as u32);
            state.present_source = renderbuffer as u32;
        }
        if window != 0 {
            let features = *state.es1_features.get_or_insert_with(|| es1_features());
            let saved = Es1Saved::save(&features);
            let (mut window_width, mut window_height) = (0, 0);
            eglQuerySurface(egl.display, window, EGL_WIDTH, &mut window_width);
            eglQuerySurface(egl.display, window, EGL_HEIGHT, &mut window_height);
            draw_es1_frame(state, &features, (width, height), (window_width, window_height), turn);
            saved.restore(&features);
            show_frame(egl, window);
        } else if !egl.has_window {
            save_frame_if_asked(frame, width, height, true, turn);
        }
        gl::glBindFramebuffer(gl::FRAMEBUFFER, framebuffer as u32);
    }
}

/// The game's OpenGL ES 1 state that drawing the present quad changes.
struct Es1Saved {
    viewport: [i32; 4],
    clear: [f32; 4],
    mask: [u8; 4],
    /// Switched-off capabilities that were on (switched back on after).
    enabled: Vec<Enum>,
    active_texture: i32,
    client_texture: i32,
    /// Per texture unit: 2D textures and cube maps on, texture coordinates array on.
    unit_2d: Vec<bool>,
    unit_cube: Vec<bool>,
    unit_coords: Vec<bool>,
    texture: i32,
    environment: i32,
    matrix_mode: i32,
    matrices: [[f32; 16]; 3],
    array_buffer: i32,
    vertices: Option<ClientArray>,
    coords: ClientArray,
    /// Other arrays the draw must not read (colours, normals, point sizes, palette).
    other_arrays: Vec<Enum>,
}

/// A client array's layout, to set it back as it was.
struct ClientArray {
    size: i32,
    kind: i32,
    stride: i32,
    pointer: usize,
    buffer: i32,
}

impl ClientArray {
    unsafe fn save(size: Enum, kind: Enum, stride: Enum, pointer: Enum, buffer: Enum) -> Self {
        let get = |name| {
            let mut value = 0;
            unsafe { gl::glGetIntegerv(name, &mut value) };
            value
        };
        let mut address = 0usize;
        unsafe { gl::es1::glGetPointerv(pointer, &mut address) };
        ClientArray { size: get(size), kind: get(kind), stride: get(stride), pointer: address, buffer: get(buffer) }
    }
}

impl Es1Saved {
    unsafe fn save(features: &Es1Features) -> Self {
        use gl::es1::*;
        unsafe {
            let get = |name| {
                let mut value = 0;
                gl::glGetIntegerv(name, &mut value);
                value
            };
            let on = |cap| gl::glIsEnabled(cap) != 0;
            let mut viewport = [0; 4];
            gl::glGetIntegerv(gl::VIEWPORT, viewport.as_mut_ptr());
            let mut clear = [0f32; 4];
            gl::glGetFloatv(gl::COLOR_CLEAR_VALUE, clear.as_mut_ptr());
            let mut mask = [0u8; 4];
            gl::glGetBooleanv(gl::COLOR_WRITEMASK, mask.as_mut_ptr());
            let mut capabilities = vec![LIGHTING, ALPHA_TEST, gl::BLEND, DEPTH_TEST, STENCIL_TEST, CULL_FACE, FOG, gl::SCISSOR_TEST, COLOR_LOGIC_OP];
            capabilities.extend((0..features.clip_planes).map(|i| CLIP_PLANE0 + i));
            if features.matrix_palette {
                capabilities.push(MATRIX_PALETTE);
            }
            let enabled = capabilities.into_iter().filter(|&cap| on(cap)).collect();
            let active_texture = get(ACTIVE_TEXTURE);
            let client_texture = get(CLIENT_ACTIVE_TEXTURE);
            let (mut unit_2d, mut unit_cube, mut unit_coords) = (Vec::new(), Vec::new(), Vec::new());
            for unit in 0..features.texture_units {
                glActiveTexture(TEXTURE0 + unit);
                glClientActiveTexture(TEXTURE0 + unit);
                unit_2d.push(on(TEXTURE_2D));
                unit_cube.push(features.cube_maps && on(TEXTURE_CUBE_MAP));
                unit_coords.push(on(TEXTURE_COORD_ARRAY));
            }
            glActiveTexture(TEXTURE0);
            glClientActiveTexture(TEXTURE0);
            // Unit 0 draws the quad: its texture, environment, matrix,
            // coordinate generation (cube maps) and coordinates array.
            let texture = get(TEXTURE_BINDING_2D);
            let mut environment = 0;
            glGetTexEnviv(TEXTURE_ENV, TEXTURE_ENV_MODE, &mut environment);
            let matrix_mode = get(MATRIX_MODE);
            let mut matrices = [[0f32; 16]; 3];
            for (matrix, name) in matrices.iter_mut().zip([MODELVIEW_MATRIX, PROJECTION_MATRIX, TEXTURE_MATRIX]) {
                gl::glGetFloatv(name, matrix.as_mut_ptr());
            }
            let array_buffer = get(ARRAY_BUFFER_BINDING);
            let vertices = on(VERTEX_ARRAY).then(|| ClientArray::save(VERTEX_ARRAY_SIZE, VERTEX_ARRAY_TYPE, VERTEX_ARRAY_STRIDE, VERTEX_ARRAY_POINTER, VERTEX_ARRAY_BUFFER_BINDING));
            let coords = ClientArray::save(TEXTURE_COORD_ARRAY_SIZE, TEXTURE_COORD_ARRAY_TYPE, TEXTURE_COORD_ARRAY_STRIDE, TEXTURE_COORD_ARRAY_POINTER, TEXTURE_COORD_ARRAY_BUFFER_BINDING);
            let mut arrays = vec![COLOR_ARRAY, NORMAL_ARRAY];
            if features.point_size_array {
                arrays.push(POINT_SIZE_ARRAY);
            }
            if features.matrix_palette {
                arrays.extend([MATRIX_INDEX_ARRAY, WEIGHT_ARRAY]);
            }
            let other_arrays = arrays.into_iter().filter(|&array| on(array)).collect();
            let mut saved = Es1Saved {
                viewport, clear, mask, enabled, active_texture, client_texture, unit_2d, unit_cube, unit_coords, texture,
                environment, matrix_mode, matrices, array_buffer, vertices, coords, other_arrays,
            };
            // Unit 0's coordinate generation: set back with the capabilities (on unit 0).
            if features.cube_maps && on(TEXTURE_GEN_STR) {
                saved.enabled.push(TEXTURE_GEN_STR);
            }
            saved
        }
    }

    unsafe fn restore(&self, features: &Es1Features) {
        use gl::es1::*;
        unsafe {
            // Unit 0 first (the texture matrix and generation are its own).
            glActiveTexture(TEXTURE0);
            glClientActiveTexture(TEXTURE0);
            for &cap in &self.enabled {
                gl::glEnable(cap);
            }
            for &array in &self.other_arrays {
                glEnableClientState(array);
            }
            glMatrixMode(TEXTURE);
            glLoadMatrixf(self.matrices[2].as_ptr());
            glMatrixMode(PROJECTION);
            glLoadMatrixf(self.matrices[1].as_ptr());
            glMatrixMode(MODELVIEW);
            glLoadMatrixf(self.matrices[0].as_ptr());
            glMatrixMode(self.matrix_mode as Enum);
            glTexEnvi(TEXTURE_ENV, TEXTURE_ENV_MODE, self.environment);
            gl::glBindTexture(TEXTURE_2D, self.texture as u32);
            let set_array = |array: &ClientArray, set: unsafe fn(i32, Enum, i32, usize)| {
                gl::glBindBuffer(ARRAY_BUFFER, array.buffer as u32);
                set(array.size, array.kind as Enum, array.stride, array.pointer);
            };
            match &self.vertices {
                Some(vertices) => set_array(vertices, glVertexPointer),
                None => glDisableClientState(VERTEX_ARRAY),
            }
            set_array(&self.coords, glTexCoordPointer);
            gl::glBindBuffer(ARRAY_BUFFER, self.array_buffer as u32);
            for unit in 0..features.texture_units as usize {
                glActiveTexture(TEXTURE0 + unit as u32);
                glClientActiveTexture(TEXTURE0 + unit as u32);
                let set = |on: bool, cap: Enum| if on { gl::glEnable(cap) } else { gl::glDisable(cap) };
                set(self.unit_2d[unit], TEXTURE_2D);
                if features.cube_maps && self.unit_cube[unit] {
                    gl::glEnable(TEXTURE_CUBE_MAP);
                }
                if self.unit_coords[unit] {
                    glEnableClientState(TEXTURE_COORD_ARRAY);
                } else {
                    glDisableClientState(TEXTURE_COORD_ARRAY);
                }
            }
            glActiveTexture(self.active_texture as Enum);
            glClientActiveTexture(self.client_texture as Enum);
            gl::glViewport(self.viewport[0], self.viewport[1], self.viewport[2], self.viewport[3]);
            gl::glClearColor(self.clear[0], self.clear[1], self.clear[2], self.clear[3]);
            gl::glColorMask(self.mask[0], self.mask[1], self.mask[2], self.mask[3]);
        }
    }
}

/// Copies the bound framebuffer's picture into the context's texture and
/// draws it over the window (framebuffer 0), as `present_es3` blits it.
unsafe fn draw_es1_frame(state: &mut State, features: &Es1Features, (width, height): (i32, i32), (window_width, window_height): (i32, i32), turn: Option<(f64, f64, f64, f64)>) {
    use gl::es1::*;
    unsafe {
        for &cap in &[LIGHTING, ALPHA_TEST, gl::BLEND, DEPTH_TEST, STENCIL_TEST, CULL_FACE, FOG, gl::SCISSOR_TEST, COLOR_LOGIC_OP] {
            gl::glDisable(cap);
        }
        for plane in 0..features.clip_planes {
            gl::glDisable(CLIP_PLANE0 + plane);
        }
        if features.matrix_palette {
            gl::glDisable(MATRIX_PALETTE);
            glDisableClientState(MATRIX_INDEX_ARRAY);
            glDisableClientState(WEIGHT_ARRAY);
        }
        if features.point_size_array {
            glDisableClientState(POINT_SIZE_ARRAY);
        }
        glDisableClientState(COLOR_ARRAY);
        glDisableClientState(NORMAL_ARRAY);
        for unit in (0..features.texture_units).rev() {
            glActiveTexture(TEXTURE0 + unit);
            glClientActiveTexture(TEXTURE0 + unit);
            gl::glDisable(TEXTURE_2D);
            if features.cube_maps {
                gl::glDisable(TEXTURE_CUBE_MAP);
            }
            glDisableClientState(TEXTURE_COORD_ARRAY);
        }
        // Now on unit 0.
        if features.cube_maps {
            gl::glDisable(TEXTURE_GEN_STR);
        }
        gl::glEnable(TEXTURE_2D);
        if state.present_texture == 0 {
            glGenTextures(1, &mut state.present_texture);
        }
        gl::glBindTexture(TEXTURE_2D, state.present_texture);
        // Sides of a power of two: OpenGL ES 1 needn't take other sizes.
        let size = ((width.max(1) as u32).next_power_of_two() as i32, (height.max(1) as u32).next_power_of_two() as i32);
        if state.present_texture_size != size {
            // RGB: a copy into RGBA would need alpha in the picture (16-bit screens have none).
            gl::glTexImage2D(TEXTURE_2D, 0, RGB as i32, size.0, size.1, 0, RGB, gl::UNSIGNED_BYTE, std::ptr::null());
            gl::glTexParameteri(TEXTURE_2D, TEXTURE_MIN_FILTER, gl::LINEAR as i32);
            gl::glTexParameteri(TEXTURE_2D, TEXTURE_MAG_FILTER, gl::LINEAR as i32);
            gl::glTexParameteri(TEXTURE_2D, TEXTURE_WRAP_S, CLAMP_TO_EDGE as i32);
            gl::glTexParameteri(TEXTURE_2D, TEXTURE_WRAP_T, CLAMP_TO_EDGE as i32);
            state.present_texture_size = size;
        }
        glCopyTexSubImage2D(TEXTURE_2D, 0, 0, 0, 0, 0, width, height);
        glTexEnvi(TEXTURE_ENV, TEXTURE_ENV_MODE, REPLACE as i32);
        for mode in [TEXTURE, PROJECTION, MODELVIEW] {
            glMatrixMode(mode);
            glLoadIdentity();
        }
        gl::glBindFramebuffer(gl::FRAMEBUFFER, 0);
        gl::glColorMask(1, 1, 1, 1);
        gl::glClearColor(0.0, 0.0, 0.0, 1.0);
        gl::glClear(gl::COLOR_BUFFER_BIT);
        let (x0, y0, x1, y1) = picture_area(window_width, window_height);
        gl::glViewport(x0, y0, x1 - x0, y1 - y0);
        // The quad's corners stay; a turned layer's texture coordinates turn
        // (scaled by the used part of the power-of-two texture).
        let (u, v) = (width as f32 / size.0 as f32, height as f32 / size.1 as f32);
        let corners: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
        let base = present_corners(turn);
        let mut coords = [0f32; 8];
        for i in 0..4 {
            coords[i * 2] = base[i * 2] * u;
            coords[i * 2 + 1] = base[i * 2 + 1] * v;
        }
        gl::glBindBuffer(ARRAY_BUFFER, 0);
        glEnableClientState(VERTEX_ARRAY);
        glVertexPointer(2, FLOAT, 0, corners.as_ptr() as usize);
        glEnableClientState(TEXTURE_COORD_ARRAY);
        glTexCoordPointer(2, FLOAT, 0, coords.as_ptr() as usize);
        glDrawArrays(TRIANGLE_STRIP, 0, 4);
        // The iPhone screen ignores alpha (see present_es3).
        gl::glColorMask(0, 0, 0, 1);
        gl::glClear(gl::COLOR_BUFFER_BIT);
    }
}

/// The bound framebuffer's RGBA pixels (bottom-up, tightly packed rows),
/// with the pixel-store settings the game left, put back after.
fn read_pixels(width: i32, height: i32, es1: bool) -> Vec<u8> {
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    if es1 {
        // OpenGL ES 1 has only the row alignment (no pixel buffers or row lengths).
        unsafe {
            let mut alignment = 4;
            gl::glGetIntegerv(gl::PACK_ALIGNMENT, &mut alignment);
            gl::glPixelStorei(gl::PACK_ALIGNMENT, 4);
            gl::glReadPixels(0, 0, width, height, gl::RGBA, gl::UNSIGNED_BYTE, pixels.as_mut_ptr());
            gl::glPixelStorei(gl::PACK_ALIGNMENT, alignment);
        }
    } else {
        unsafe {
            let mut pack_buffer = 0;
            gl::glGetIntegerv(gl::PIXEL_PACK_BUFFER_BINDING, &mut pack_buffer);
            // Tightly packed rows into our buffer, whatever the game had set.
            let settings = [gl::PACK_ALIGNMENT, gl::PACK_ROW_LENGTH, gl::PACK_SKIP_ROWS, gl::PACK_SKIP_PIXELS];
            let mut saved = [0i32; 4];
            for (name, value) in settings.iter().zip(saved.iter_mut()) {
                gl::glGetIntegerv(*name, value);
            }
            gl::glBindBuffer(gl::PIXEL_PACK_BUFFER, 0);
            for (name, value) in settings.iter().zip([4, 0, 0, 0]) {
                gl::glPixelStorei(*name, value);
            }
            gl::glReadPixels(0, 0, width, height, gl::RGBA, gl::UNSIGNED_BYTE, pixels.as_mut_ptr());
            for (name, value) in settings.iter().zip(saved) {
                gl::glPixelStorei(*name, value);
            }
            gl::glBindBuffer(gl::PIXEL_PACK_BUFFER, pack_buffer as u32);
        }
    }
    pixels
}

/// IOS_FRAMES_DIR saves frames as PNG pictures: the first, and every
/// IOS_FRAME_EVERY-th after it (default 60). Reads the bound framebuffer.
fn save_frame_if_asked(frame: usize, width: i32, height: i32, es1: bool, turn: Option<(f64, f64, f64, f64)>) {
    let Ok(folder) = std::env::var("IOS_FRAMES_DIR") else { return };
    let every = std::env::var("IOS_FRAME_EVERY").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(60).max(1);
    if frame != 1 && frame % every != 0 {
        return;
    }
    let mut pixels = read_pixels(width, height, es1);
    // The iPhone screen ignores alpha; games often leave it at 0.
    for pixel in pixels.chunks_exact_mut(4) {
        pixel[3] = 255;
    }
    // A turned layer's picture is saved turned too, so the PNG shows what
    // the screen shows.
    let (save_width, save_height, pixels) = match turn {
        Some(t) => turn_pixels(t, width as usize, height as usize, &pixels),
        None => (width as usize, height as usize, pixels),
    };
    let path = format!("{folder}/frame-{frame:05}.png");
    let _ = std::fs::create_dir_all(&folder);
    match std::fs::write(&path, crate::fw::png::encode(save_width as u32, save_height as u32, &pixels, true)) {
        Ok(()) => crate::log!("graphics: frame {frame} saved to {path}"),
        Err(e) => crate::log!("graphics: frame {frame} not saved to {path}: {e}"),
    }
}

/// A CGImage of the last frame `layer` presented. Its context is found in
/// BUFFER_LAYERS and must be current on this thread (never made current for
/// another); its present framebuffer is bound for reading, the pixels are
/// read and turned (GL is bottom-up) into a straight-RGBA picture. None when
/// no such context is known, it isn't this thread's, or it has no frame yet.
pub fn last_frame_image(layer: Id) -> Option<Id> {
    let (context, renderbuffer) = BUFFER_LAYERS
        .lock().unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(_, found)| *found == layer)
        .map(|&(buffer, _)| buffer)?;
    let (width, height, es1, framebuffer) = {
        let state = state_of(context).lock().unwrap_or_else(|e| e.into_inner());
        if state.present_source != renderbuffer || state.owner != this_thread() {
            return None;
        }
        if state.present_framebuffer == 0 || state.size.0 <= 0 || state.size.1 <= 0 {
            return None;
        }
        (state.size.0, state.size.1, state.es1, state.present_framebuffer)
    };
    let pixels = if es1 {
        unsafe {
            let mut previous = 0;
            gl::glGetIntegerv(gl::es1::FRAMEBUFFER_BINDING, &mut previous);
            gl::glBindFramebuffer(gl::FRAMEBUFFER, framebuffer);
            let pixels = read_pixels(width, height, true);
            gl::glBindFramebuffer(gl::FRAMEBUFFER, previous as u32);
            pixels
        }
    } else {
        unsafe {
            let mut previous = 0;
            gl::glGetIntegerv(gl::READ_FRAMEBUFFER_BINDING, &mut previous);
            gl::glBindFramebuffer(gl::READ_FRAMEBUFFER, framebuffer);
            let pixels = read_pixels(width, height, false);
            gl::glBindFramebuffer(gl::READ_FRAMEBUFFER, previous as u32);
            pixels
        }
    };
    // GL rows run bottom-up; a CGImage's run from the top. Alpha 255: the
    // iPhone screen ignores it and games often leave it at 0.
    let (width, height) = (width as usize, height as usize);
    let mut flipped = vec![0u8; width * height * 4];
    for row in 0..height {
        let from = (height - 1 - row) * width * 4;
        flipped[row * width * 4..(row + 1) * width * 4].copy_from_slice(&pixels[from..from + width * 4]);
    }
    for pixel in flipped.chunks_exact_mut(4) {
        pixel[3] = 255;
    }
    let bitmap = crate::fw::graphics::Bitmap {
        width,
        height,
        bits_per_pixel: 32,
        bytes_per_row: width * 4,
        info: crate::fw::graphics::ALPHA_LAST,
        space: crate::fw::graphics::Space::Rgb,
        data: flipped,
    };
    Some(crate::fw::graphics::make_image(bitmap))
}

/// Turns a saved frame's pixels the way the screen shows them: for a layer
/// turned by the linear part (a, b, c, d), the picture is rotated by the
/// turn's inverse, so a portrait buffer is saved landscape (and upright).
/// Returns the new width, height and bottom-up pixels.
fn turn_pixels(turn: (f64, f64, f64, f64), width: usize, height: usize, pixels: &[u8]) -> (usize, usize, Vec<u8>) {
    let (a, b, _c, _d) = turn;
    let (w, h) = (width, height);
    // A quarter turn by 90 degrees swaps the sides; a half turn keeps them.
    let (out_w, out_h) = if a == 0.0 { (h, w) } else { (w, h) };
    let mut out = vec![0u8; out_w * out_h * 4];
    for y in 0..out_h {
        for x in 0..out_w {
            // Where this output pixel's picture came from, in the saved
            // (bottom-up) buffer, for each quarter turn.
            let src = if a == 0.0 && b == -1.0 {
                (h - 1 - x) * w + y
            } else if a == 0.0 {
                x * w + (w - 1 - y)
            } else if a == -1.0 {
                (h - 1 - y) * w + (w - 1 - x)
            } else {
                y * w + x
            };
            let (src, dst) = (src * 4, (y * out_w + x) * 4);
            out[dst..dst + 4].copy_from_slice(&pixels[src..src + 4]);
        }
    }
    (out_w, out_h, out)
}


extern "C" fn retain(this: Id, _: Sel) -> Id {
    counted::retain(this)
}

extern "C" fn release(this: Id, _: Sel) {
    counted::release(this)
}

extern "C" fn autorelease(this: Id, _: Sel) -> Id {
    counted::autorelease(this)
}

extern "C" fn retain_count(this: Id, _: Sel) -> usize {
    counted::count(this).unwrap_or(1)
}

pub fn register(r: &mut Registry) {
    let context = define_class("EAGLContext", "NSObject", 8);
    // Contexts count references, so a freed one gives back its EGL context.
    counted::count_class(context);
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
            ("retain", retain as usize),
            ("release", release as usize),
            ("autorelease", autorelease as usize),
            ("retainCount", retain_count as usize),
        ],
    );
    r.data("kEAGLDrawablePropertyRetainedBacking", leak(crate::fw::string::make("EAGLDrawablePropertyRetainedBacking")));
    r.data("kEAGLDrawablePropertyColorFormat", leak(crate::fw::string::make("EAGLDrawablePropertyColorFormat")));
    r.data("kEAGLColorFormatRGBA8", leak(crate::fw::string::make("EAGLColorFormatRGBA8")));
    r.data("kEAGLColorFormatRGB565", leak(crate::fw::string::make("EAGLColorFormatRGB565")));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A new GL view's frames show from the picture list that puts it on the
    /// screen, and an old one's until the list without it goes, as an
    /// iPhone shows a layer's frames with the commit that adds it; before
    /// any list, its window says.
    #[test]
    fn a_gl_views_frames_show_with_the_list_that_puts_it_up() {
        assert!(!frames_show(true, Some(false)), "the garden waits for its list");
        assert!(frames_show(false, Some(true)), "the hall shows until the list without it goes");
        assert!(frames_show(true, Some(true)) && !frames_show(false, Some(false)));
        assert!(frames_show(true, None) && !frames_show(false, None), "no list yet: in a window or not");
    }

    /// The present quad's texture coordinates: straight stays straight, and
    /// the two window turns turn the picture a quarter turn each way.
    #[test]
    fn present_corners_turn_the_picture_and_straight_stays_straight() {
        let straight = [0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        assert_eq!(present_corners(None), straight);
        // Not close to a quarter turn: straight too.
        assert_eq!(quarter_turn(0.7071, -0.7071, 0.7071, 0.7071), None, "a diagonal turn is straight");
        // Orientation 3's window turn (a=0, b=-1, c=1, d=0): the picture is
        // turned a quarter turn one way.
        assert_eq!(present_corners(Some((0.0, -1.0, 1.0, 0.0))), [0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0]);
        // Orientation 4's (a=0, b=1, c=-1, d=0): the other way.
        assert_eq!(present_corners(Some((0.0, 1.0, -1.0, 0.0))), [1.0, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
    }

    /// Only a picture drawn tall (an upright portrait buffer, Monster Life's
    /// 640x1136) is turned by a quarter turn; a game that draws its picture
    /// wide already (SpellCraft, Dungeon Plunder) is shown straight, and so
    /// is a half turn (its shape can't tell).
    #[test]
    fn only_tall_pictures_are_turned() {
        let quarter = Some((0.0, -1.0, 1.0, 0.0));
        assert_eq!(turn_for_picture(quarter, (640, 1136)), quarter);
        assert_eq!(turn_for_picture(quarter, (1136, 640)), None);
        assert_eq!(turn_for_picture(quarter, (1024, 768)), None);
        assert_eq!(turn_for_picture(Some((-1.0, 0.0, 0.0, -1.0)), (640, 1136)), None);
        assert_eq!(turn_for_picture(None, (640, 1136)), None);
    }
}

