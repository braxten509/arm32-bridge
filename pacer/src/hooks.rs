//! The game's calls the helper takes over, and putting them in place.
//!
//! Only the game's own libraries (in its install folder or data folder) are
//! changed, never Android's, with one exception: libnativeloader's
//! android_dlopen_ext, the call behind Java's System.loadLibrary, so libraries
//! the game loads from Java are seen at once. A safety-net thread watches the
//! loader's library count for loads that came another way (on the emulator's
//! ARM translator, System.loadLibrary doesn't go through ARM code at all).

use std::collections::{HashMap, HashSet};
use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::got::{self, Image};
use crate::pacing;

const DLOPEN: usize = 0;
const ANDROID_DLOPEN_EXT: usize = 1;
const DLSYM: usize = 2;
const NANOSLEEP: usize = 3;
const CLOCK_NANOSLEEP: usize = 4;
const USLEEP: usize = 5;
const EGL_SWAP_BUFFERS: usize = 6;
const EGL_SWAP_BUFFERS_WITH_DAMAGE: usize = 7;
const EGL_SWAP_INTERVAL: usize = 8;
const EGL_CREATE_WINDOW_SURFACE: usize = 9;
const EGL_GET_PROC_ADDRESS: usize = 10;
const VK_GET_INSTANCE_PROC_ADDR: usize = 11;
const VK_GET_DEVICE_PROC_ADDR: usize = 12;
const VK_QUEUE_PRESENT: usize = 13;
const VK_CREATE_ANDROID_SURFACE: usize = 14;
const SET_FRAME_RATE: usize = 15;
const SET_FRAME_RATE_WITH_STRATEGY: usize = 16;
const IL2CPP_ADD_INTERNAL_CALL: usize = 17;
const MONO_ADD_INTERNAL_CALL: usize = 18;
const IL2CPP_RUNTIME_INVOKE: usize = 19;
const MONO_RUNTIME_INVOKE: usize = 20;

const SYMBOLS: [&str; 21] = [
    "dlopen",
    "android_dlopen_ext",
    "dlsym",
    "nanosleep",
    "clock_nanosleep",
    "usleep",
    "eglSwapBuffers",
    "eglSwapBuffersWithDamageKHR",
    "eglSwapInterval",
    "eglCreateWindowSurface",
    "eglGetProcAddress",
    "vkGetInstanceProcAddr",
    "vkGetDeviceProcAddr",
    "vkQueuePresentKHR",
    "vkCreateAndroidSurfaceKHR",
    "ANativeWindow_setFrameRate",
    "ANativeWindow_setFrameRateWithChangeStrategy",
    "il2cpp_add_internal_call",
    "mono_add_internal_call",
    "il2cpp_runtime_invoke",
    "mono_runtime_invoke",
];

/// The game's own function behind each call (the first one seen).
static REAL: [AtomicUsize; 21] = [const { AtomicUsize::new(0) }; 21];

extern "C" {
    static mut gamebridge_real_dlopen: usize;
    static mut gamebridge_real_android_dlopen_ext: usize;
    static mut gamebridge_real_dlsym: usize;
    fn gamebridge_dlopen(name: *const c_char, flags: c_int) -> *mut c_void;
    fn gamebridge_android_dlopen_ext(name: *const c_char, flags: c_int, info: *const c_void) -> *mut c_void;
    fn gamebridge_dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    pub fn gamebridge_loader_start();
    pub fn gamebridge_loader_passes_caller() -> c_int;
}

fn index(symbol: &str) -> Option<usize> {
    SYMBOLS.iter().position(|known| *known == symbol)
}

/// The helper's stand-in for call `index`.
fn ours(index: usize) -> usize {
    match index {
        DLOPEN => gamebridge_dlopen as *const () as usize,
        ANDROID_DLOPEN_EXT => gamebridge_android_dlopen_ext as *const () as usize,
        DLSYM => gamebridge_dlsym as *const () as usize,
        NANOSLEEP => nanosleep as *const () as usize,
        CLOCK_NANOSLEEP => clock_nanosleep as *const () as usize,
        USLEEP => usleep as *const () as usize,
        EGL_SWAP_BUFFERS => egl_swap_buffers as *const () as usize,
        EGL_SWAP_BUFFERS_WITH_DAMAGE => egl_swap_buffers_with_damage as *const () as usize,
        EGL_SWAP_INTERVAL => egl_swap_interval as *const () as usize,
        EGL_CREATE_WINDOW_SURFACE => egl_create_window_surface as *const () as usize,
        EGL_GET_PROC_ADDRESS => egl_get_proc_address as *const () as usize,
        VK_GET_INSTANCE_PROC_ADDR => vk_get_instance_proc_addr as *const () as usize,
        VK_GET_DEVICE_PROC_ADDR => vk_get_device_proc_addr as *const () as usize,
        VK_QUEUE_PRESENT => vk_queue_present as *const () as usize,
        VK_CREATE_ANDROID_SURFACE => vk_create_android_surface as *const () as usize,
        SET_FRAME_RATE => set_frame_rate as *const () as usize,
        SET_FRAME_RATE_WITH_STRATEGY => set_frame_rate_with_strategy as *const () as usize,
        IL2CPP_ADD_INTERNAL_CALL | MONO_ADD_INTERNAL_CALL => add_internal_call_for(index),
        IL2CPP_RUNTIME_INVOKE => il2cpp_runtime_invoke as *const () as usize,
        MONO_RUNTIME_INVOKE => mono_runtime_invoke as *const () as usize,
        _ => unreachable!(),
    }
}

fn add_internal_call_for(index: usize) -> usize {
    if index == IL2CPP_ADD_INTERNAL_CALL { il2cpp_add_internal_call as *const () as usize } else { mono_add_internal_call as *const () as usize }
}

/// Remembers `found` as the game's function for call `index`. False when a
/// different function was already seen: then this one is left alone.
fn remember(index: usize, found: usize) -> bool {
    match REAL[index].compare_exchange(0, found, Ordering::SeqCst, Ordering::SeqCst) {
        Ok(_) => {
            unsafe {
                match index {
                    DLOPEN => gamebridge_real_dlopen = found,
                    ANDROID_DLOPEN_EXT => gamebridge_real_android_dlopen_ext = found,
                    DLSYM => gamebridge_real_dlsym = found,
                    _ => {}
                }
            }
            true
        }
        Err(known) => known == found,
    }
}

fn real<T: Copy>(index: usize) -> T {
    let address = REAL[index].load(Ordering::SeqCst);
    assert!(address != 0, "{} used before it was found", SYMBOLS[index]);
    unsafe { std::mem::transmute_copy(&address) }
}

/// A function the game looked up by name (dlsym, eglGetProcAddress,
/// vkGet*ProcAddr): its stand-in when there is one.
#[no_mangle]
pub extern "C" fn gamebridge_swap_symbol(name: *const c_char, found: *mut c_void) -> *mut c_void {
    let Ok(name) = unsafe { CStr::from_ptr(name) }.to_str() else { return found };
    let Some(index) = index(name) else { return found };
    if remember(index, found as usize) { ours(index) as *mut c_void } else { found }
}

/// Folders holding the game's own libraries.
fn game_folders() -> &'static Vec<String> {
    static FOLDERS: OnceLock<Vec<String>> = OnceLock::new();
    FOLDERS.get_or_init(|| {
        let mut folders = Vec::new();
        if let Some(own) = own_image() {
            if let Some(folder) = install_folder(&own.name) {
                folders.push(folder);
            }
        }
        let package = crate::package();
        for root in ["/data/data/", "/data/user/0/", "/data/user_de/0/"] {
            folders.push(format!("{root}{package}/"));
        }
        folders
    })
}

/// The game's install folder from a library path inside it:
/// `/data/app/~~x/pkg-y/lib/arm64/libz.so` or `/data/app/~~x/pkg-y/split_config.arm64_v8a.apk!/lib/arm64-v8a/libz.so`.
pub fn install_folder(path: &str) -> Option<String> {
    let inside = match path.find(".apk!/") {
        Some(end) => &path[..end],
        None => &path[..path.find("/lib/")? + 1],
    };
    Some(inside[..inside.rfind('/')? + 1].to_string())
}

fn own_image() -> Option<Image> {
    let here = own_image as *const () as usize;
    got::images().into_iter().find(|image| image.contains(here))
}

fn is_game_library(image: &Image) -> bool {
    game_folders().iter().any(|folder| image.name.starts_with(folder.as_str()))
}

fn short(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

/// Libraries already handled, with one slot that was changed and the value
/// written (to notice a library unloaded and loaded again at the same place,
/// which brings back its original slots).
type Done = HashMap<(usize, String), Option<(usize, usize)>>;

static DONE: Mutex<Option<Done>> = Mutex::new(None);
/// A pass was asked for while another thread was running one.
static PENDING: AtomicBool = AtomicBool::new(false);

/// Puts the stand-ins into every library loaded since the last time. `by`
/// says what noticed the new libraries (for the log).
///
/// Never waits for another thread's pass: a library's start-up code runs with
/// Android's loader lock held and may load more libraries, while the thread in
/// a pass waits for that same lock to list them. Instead the running pass is
/// told to go round once more.
pub fn patch_new(by: &str) {
    // Worked out before the pass: it can't list libraries again from inside one.
    game_folders();
    let own = own_image().map(|image| image.base);
    PENDING.store(true, Ordering::SeqCst);
    loop {
        let Ok(mut done) = DONE.try_lock() else { return };
        while PENDING.swap(false, Ordering::SeqCst) {
            patch_pass(done.get_or_insert_with(HashMap::new), own, by);
        }
        drop(done);
        // Asked again between the last pass and letting go: go round if no one else took over.
        if !PENDING.load(Ordering::SeqCst) {
            return;
        }
    }
}

/// One pass over every library, inside the loader lock (see got::with_each_image).
fn patch_pass(done: &mut Done, own: Option<usize>, by: &str) {
    let mut present = HashSet::new();
    got::with_each_image(|image| {
        let key = (image.base, image.name.clone());
        present.insert(key.clone());
        if let Some(record) = done.get(&key) {
            let still_ours = record.is_none_or(|(slot, value)| unsafe { (*(slot as *const AtomicUsize)).load(Ordering::SeqCst) } == value);
            if still_ours {
                return;
            }
        }
        let record = patch_image(image, own, by);
        done.insert(key, record);
    });
    // Forget unloaded libraries: one loaded later at the same place is new.
    done.retain(|key, _| present.contains(key));
}

/// Puts the stand-ins into one library; returns one changed slot and its new value.
fn patch_image(image: &Image, own: Option<usize>, by: &str) -> Option<(usize, usize)> {
    if Some(image.base) == own {
        return None;
    }
    let game = is_game_library(image);
    let loader = image.name.ends_with("/libnativeloader.so");
    if !game && !loader {
        return None;
    }
    let wanted = |name: &str| if game { index(name).is_some() } else { name == "android_dlopen_ext" };
    let mut hooked = Vec::new();
    let mut record = None;
    for (name, slot) in image.import_slots(&wanted) {
        let index = index(&name).unwrap();
        let current = unsafe { (*(slot as *const AtomicUsize)).load(Ordering::SeqCst) };
        if current == 0 || current == ours(index) {
            continue;
        }
        if !remember(index, current) {
            crate::log(&format!("{} calls another {name} ({current:#x}); left alone", short(&image.name)));
            continue;
        }
        if image.write_slot(slot, ours(index)) {
            hooked.push(name);
            record = Some((slot, ours(index)));
        } else {
            crate::log(&format!("couldn't change {name} in {}", short(&image.name)));
        }
    }
    if game {
        hooked.sort();
        hooked.dedup();
        crate::log(&format!("{} ({by}): {}", short(&image.name), if hooked.is_empty() { "nothing to change".to_string() } else { hooked.join(", ") }));
    }
    record
}

#[no_mangle]
pub extern "C" fn gamebridge_after_load() {
    patch_new("after a load");
}

/// The safety net: watches the loader's library count.
pub fn start_watching() {
    std::thread::Builder::new()
        .name("GameBridgePacer".into())
        .spawn(|| {
            let start = std::time::Instant::now();
            let mut last = got::load_count();
            let counted = last != u64::MAX;
            loop {
                let fast = start.elapsed() < Duration::from_secs(30);
                std::thread::sleep(Duration::from_millis(match (counted, fast) {
                    (true, true) => 1,
                    (true, false) => 20,
                    (false, _) => 50,
                }));
                let count = got::load_count();
                if !counted || count != last {
                    last = count;
                    patch_new("watcher");
                }
            }
        })
        .ok();
}

/// Game libraries that were already loaded before the helper started (they
/// may have made calls before the stand-ins were in place).
pub fn loaded_before_start() -> Vec<String> {
    let own = own_image().map(|image| image.base);
    got::images()
        .iter()
        .filter(|image| is_game_library(image) && Some(image.base) != own)
        .map(|image| short(&image.name).to_string())
        .collect()
}

/// Logs `what` the first time it happens (index into a small table of firsts).
fn first(index: usize, what: &str) {
    static SEEN: [AtomicBool; 4] = [const { AtomicBool::new(false) }; 4];
    if !SEEN[index].swap(true, Ordering::Relaxed) {
        crate::log(&format!("first {what} (thread {})", crate::thread_name()));
    }
}

// Sleeps: frame limiters on the thread that shows frames end early.

fn shortened(request: *const libc::timespec) -> Option<libc::timespec> {
    if request.is_null() {
        return None;
    }
    let request = unsafe { *request };
    if request.tv_sec < 0 || !(0..1_000_000_000).contains(&request.tv_nsec) {
        return None;
    }
    let wait = Duration::new(request.tv_sec as u64, request.tv_nsec as u32);
    let limited = pacing::frame_limit(wait);
    if limited < wait {
        first(1, &format!("frame-limiter sleep shortened ({wait:?} -> {limited:?})"));
    }
    (limited < wait).then(|| libc::timespec { tv_sec: limited.as_secs() as _, tv_nsec: limited.subsec_nanos() as _ })
}

extern "C" fn nanosleep(request: *const libc::timespec, remaining: *mut libc::timespec) -> c_int {
    let call: extern "C" fn(*const libc::timespec, *mut libc::timespec) -> c_int = real(NANOSLEEP);
    match shortened(request) {
        Some(short) => call(&short, remaining),
        None => call(request, remaining),
    }
}

extern "C" fn clock_nanosleep(clock: libc::clockid_t, flags: c_int, request: *const libc::timespec, remaining: *mut libc::timespec) -> c_int {
    let call: extern "C" fn(libc::clockid_t, c_int, *const libc::timespec, *mut libc::timespec) -> c_int = real(CLOCK_NANOSLEEP);
    match shortened(request).filter(|_| flags & libc::TIMER_ABSTIME == 0) {
        Some(short) => call(clock, flags, &short, remaining),
        None => call(clock, flags, request, remaining),
    }
}

extern "C" fn usleep(microseconds: libc::c_uint) -> c_int {
    let call: extern "C" fn(libc::c_uint) -> c_int = real(USLEEP);
    let wait = Duration::from_micros(microseconds as u64);
    let limited = pacing::frame_limit(wait);
    if limited < wait {
        first(1, &format!("frame-limiter sleep shortened ({wait:?} -> {limited:?})"));
    }
    call(limited.as_micros() as libc::c_uint)
}

// Frames shown.

extern "C" fn egl_swap_buffers(display: *mut c_void, surface: *mut c_void) -> u32 {
    let call: extern "C" fn(*mut c_void, *mut c_void) -> u32 = real(EGL_SWAP_BUFFERS);
    let result = call(display, surface);
    pacing::frame_shown();
    first(0, "frame shown");
    result
}

extern "C" fn egl_swap_buffers_with_damage(display: *mut c_void, surface: *mut c_void, rectangles: *const i32, count: i32) -> u32 {
    let call: extern "C" fn(*mut c_void, *mut c_void, *const i32, i32) -> u32 = real(EGL_SWAP_BUFFERS_WITH_DAMAGE);
    let result = call(display, surface, rectangles, count);
    pacing::frame_shown();
    first(0, "frame shown");
    result
}

extern "C" fn vk_queue_present(queue: *mut c_void, info: *const c_void) -> i32 {
    let call: extern "C" fn(*mut c_void, *const c_void) -> i32 = real(VK_QUEUE_PRESENT);
    let result = call(queue, info);
    pacing::frame_shown();
    first(0, "frame shown");
    result
}

/// eglSwapInterval(2) shows a frame every second refresh: one is enough.
extern "C" fn egl_swap_interval(display: *mut c_void, interval: i32) -> u32 {
    let call: extern "C" fn(*mut c_void, i32) -> u32 = real(EGL_SWAP_INTERVAL);
    if interval > 1 && pacing::setting().fps.is_some() {
        static LOGGED: AtomicBool = AtomicBool::new(false);
        if !LOGGED.swap(true, Ordering::Relaxed) {
            crate::log(&format!("eglSwapInterval({interval}) -> 1"));
        }
        return call(display, 1);
    }
    call(display, interval)
}

// The screen's refresh rate.

/// Asks Android to run the game's window at the target rate.
fn request_rate(window: *mut c_void) {
    let Some(fps) = pacing::setting().fps else { return };
    if window.is_null() {
        return;
    }
    static SET: OnceLock<usize> = OnceLock::new();
    // It lives in libnativewindow (Android 11+), which isn't loaded for everyone
    // (RTLD_DEFAULT doesn't see it), so it is opened by name.
    let function = *SET.get_or_init(|| unsafe {
        let library = libc::dlopen(c"libnativewindow.so".as_ptr(), libc::RTLD_NOW);
        let function = if library.is_null() { 0 } else { libc::dlsym(library, c"ANativeWindow_setFrameRate".as_ptr()) as usize };
        if function == 0 {
            crate::log("ANativeWindow_setFrameRate not found (Android older than 11?): can't ask for a refresh rate");
        }
        function
    });
    if function == 0 {
        return;
    }
    let set: extern "C" fn(*mut c_void, f32, i8) -> i32 = unsafe { std::mem::transmute(function) };
    let result = set(window, fps, 0);
    crate::log(&format!("asked for {fps:.0} Hz on the game's window (result {result})"));
}

fn raised(rate: f32) -> f32 {
    match pacing::setting().fps {
        Some(fps) if rate > 0.0 && rate < fps => fps,
        _ => rate,
    }
}

extern "C" fn set_frame_rate(window: *mut c_void, rate: f32, compatibility: i8) -> i32 {
    let call: extern "C" fn(*mut c_void, f32, i8) -> i32 = real(SET_FRAME_RATE);
    call(window, raised(rate), compatibility)
}

extern "C" fn set_frame_rate_with_strategy(window: *mut c_void, rate: f32, compatibility: i8, strategy: i8) -> i32 {
    let call: extern "C" fn(*mut c_void, f32, i8, i8) -> i32 = real(SET_FRAME_RATE_WITH_STRATEGY);
    call(window, raised(rate), compatibility, strategy)
}

extern "C" fn egl_create_window_surface(display: *mut c_void, config: *mut c_void, window: *mut c_void, attributes: *const i32) -> *mut c_void {
    let call: extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *const i32) -> *mut c_void = real(EGL_CREATE_WINDOW_SURFACE);
    let surface = call(display, config, window, attributes);
    first(2, "eglCreateWindowSurface");
    if !surface.is_null() {
        request_rate(window);
    }
    surface
}

extern "C" fn vk_create_android_surface(instance: *mut c_void, info: *const c_void, allocator: *const c_void, surface: *mut u64) -> i32 {
    let call: extern "C" fn(*mut c_void, *const c_void, *const c_void, *mut u64) -> i32 = real(VK_CREATE_ANDROID_SURFACE);
    let result = call(instance, info, allocator, surface);
    if result == 0 && !info.is_null() {
        // VkAndroidSurfaceCreateInfoKHR: type, next, flags, then the window at byte 24.
        request_rate(unsafe { *((info as *const u8).add(24) as *const *mut c_void) });
    }
    result
}

// Functions looked up by name.

extern "C" fn egl_get_proc_address(name: *const c_char) -> *mut c_void {
    let call: extern "C" fn(*const c_char) -> *mut c_void = real(EGL_GET_PROC_ADDRESS);
    let found = call(name);
    if found.is_null() || name.is_null() { found } else { gamebridge_swap_symbol(name, found) }
}

extern "C" fn vk_get_instance_proc_addr(instance: *mut c_void, name: *const c_char) -> *mut c_void {
    let call: extern "C" fn(*mut c_void, *const c_char) -> *mut c_void = real(VK_GET_INSTANCE_PROC_ADDR);
    let found = call(instance, name);
    if found.is_null() || name.is_null() { found } else { gamebridge_swap_symbol(name, found) }
}

extern "C" fn vk_get_device_proc_addr(device: *mut c_void, name: *const c_char) -> *mut c_void {
    let call: extern "C" fn(*mut c_void, *const c_char) -> *mut c_void = real(VK_GET_DEVICE_PROC_ADDR);
    let found = call(device, name);
    if found.is_null() || name.is_null() { found } else { gamebridge_swap_symbol(name, found) }
}

// Unity's scripting runtime (IL2CPP or Mono).

extern "C" fn il2cpp_add_internal_call(name: *const c_char, function: *const c_void) {
    add_internal_call(IL2CPP_ADD_INTERNAL_CALL, name, function)
}

extern "C" fn mono_add_internal_call(name: *const c_char, function: *const c_void) {
    add_internal_call(MONO_ADD_INTERNAL_CALL, name, function)
}

fn add_internal_call(index: usize, name: *const c_char, function: *const c_void) {
    let call: extern "C" fn(*const c_char, *const c_void) = real(index);
    let function = match unsafe { name.as_ref() }.and_then(|_| unsafe { CStr::from_ptr(name) }.to_str().ok()) {
        Some(text) => crate::unity::internal_call(text, function as usize) as *const c_void,
        None => function,
    };
    call(name, function)
}

type Invoke = extern "C" fn(*mut c_void, *mut c_void, *mut *mut c_void, *mut *mut c_void) -> *mut c_void;

extern "C" fn il2cpp_runtime_invoke(method: *mut c_void, object: *mut c_void, parameters: *mut *mut c_void, exception: *mut *mut c_void) -> *mut c_void {
    crate::unity::on_scripting_call(il2cpp_resolve_icall());
    let call: Invoke = real(IL2CPP_RUNTIME_INVOKE);
    call(method, object, parameters, exception)
}

extern "C" fn mono_runtime_invoke(method: *mut c_void, object: *mut c_void, parameters: *mut *mut c_void, exception: *mut *mut c_void) -> *mut c_void {
    crate::unity::on_scripting_call(None);
    let call: Invoke = real(MONO_RUNTIME_INVOKE);
    call(method, object, parameters, exception)
}

/// IL2CPP's own lookup of internal calls by name.
fn il2cpp_resolve_icall() -> Option<extern "C" fn(*const c_char) -> usize> {
    static FOUND: OnceLock<usize> = OnceLock::new();
    let address = *FOUND.get_or_init(|| unsafe {
        let library = libc::dlopen(c"libil2cpp.so".as_ptr(), libc::RTLD_NOW | libc::RTLD_NOLOAD);
        if library.is_null() { 0 } else { libc::dlsym(library, c"il2cpp_resolve_icall".as_ptr()) as usize }
    });
    (address != 0).then(|| unsafe { std::mem::transmute::<usize, extern "C" fn(*const c_char) -> usize>(address) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_folders() {
        assert_eq!(
            install_folder("/data/app/~~a==/com.game-b==/lib/arm64/libgamebridge_pacer.so").as_deref(),
            Some("/data/app/~~a==/com.game-b==/")
        );
        assert_eq!(
            install_folder("/data/app/~~a==/com.game-b==/split_config.arm64_v8a.apk!/lib/arm64-v8a/libunity.so").as_deref(),
            Some("/data/app/~~a==/com.game-b==/")
        );
        assert_eq!(install_folder("/system/lib64/libc.so"), None);
        assert_eq!(install_folder("libc.so"), None);
    }

    #[test]
    fn every_call_has_a_stand_in() {
        for index in 0..SYMBOLS.len() {
            assert_ne!(ours(index), 0, "{}", SYMBOLS[index]);
        }
    }
}
