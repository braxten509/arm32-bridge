//! EGL, ANativeWindow and ALooper for games that set up their own drawing
//! surface (Unity does; cocos2d-x leaves it to Java's GLSurfaceView).
//! Displays, configs, surfaces, contexts, windows and loopers are 64-bit host
//! pointers, so the guest gets small handles instead. A host pointer always
//! maps to the same handle, so "is this the current context?" checks work.

/// Functions handled here. Only the phone build has them; the PC tool's
/// `imports` report counts them as handled.
pub const NAMES: &[&str] = &[
    "eglGetDisplay",
    "eglInitialize",
    "eglTerminate",
    "eglBindAPI",
    "eglQueryAPI",
    "eglReleaseThread",
    "eglWaitClient",
    "eglWaitGL",
    "eglWaitNative",
    "eglGetConfigs",
    "eglChooseConfig",
    "eglGetConfigAttrib",
    "eglCreateWindowSurface",
    "eglCreatePbufferSurface",
    "eglDestroySurface",
    "eglQuerySurface",
    "eglSurfaceAttrib",
    "eglCreateContext",
    "eglDestroyContext",
    "eglQueryContext",
    "eglMakeCurrent",
    "eglGetCurrentContext",
    "eglGetCurrentSurface",
    "eglGetCurrentDisplay",
    "eglSwapBuffers",
    "eglSwapInterval",
    "eglQueryString",
    "eglGetError",
    "eglCreateSyncKHR",
    "eglDestroySyncKHR",
    "eglClientWaitSyncKHR",
    "eglGetSyncAttribKHR",
    "eglGetFrameTimestampSupportedANDROID",
    "eglGetFrameTimestampsANDROID",
    "eglGetNextFrameIdANDROID",
    "eglPresentationTimeANDROID",
    "ANativeWindow_fromSurface",
    "ANativeWindow_toSurface",
    "ANativeWindow_acquire",
    "ANativeWindow_release",
    "ANativeWindow_getWidth",
    "ANativeWindow_getHeight",
    "ANativeWindow_getFormat",
    "ANativeWindow_setBuffersGeometry",
    "ALooper_forThread",
    "ALooper_prepare",
    "ALooper_acquire",
    "ALooper_release",
    "ALooper_wake",
    "ALooper_pollAll",
    "ALooper_pollOnce",
    "ASensorManager_getInstance",
    "ASensorManager_getSensorList",
    "ASensorManager_getDefaultSensor",
    "ASensorManager_createEventQueue",
    "ASensorManager_destroyEventQueue",
    "ASensor_getName",
    "ASensor_getVendor",
    "ASensor_getType",
    "ASensor_getResolution",
    "ASensor_getMinDelay",
    "ASensorEventQueue_enableSensor",
    "ASensorEventQueue_disableSensor",
    "ASensorEventQueue_setEventRate",
    "ASensorEventQueue_hasEvents",
    "ASensorEventQueue_getEvents",
];

#[cfg(not(target_os = "android"))]
use crate::runtime::Handler;

#[cfg(target_os = "android")]
mod host {
    use std::collections::HashMap;
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use crate::handler;
    use crate::imports::jni_host;
    use crate::runtime::{mem, shared, ArgSource, Handler, Ret};

    type Pointer = *mut c_void;

    #[link(name = "EGL")]
    extern "C" {
        fn eglGetDisplay(native: Pointer) -> Pointer;
        fn eglInitialize(display: Pointer, major: *mut i32, minor: *mut i32) -> u32;
        fn eglTerminate(display: Pointer) -> u32;
        fn eglBindAPI(api: u32) -> u32;
        fn eglQueryAPI() -> u32;
        fn eglReleaseThread() -> u32;
        fn eglWaitClient() -> u32;
        fn eglWaitGL() -> u32;
        fn eglWaitNative(engine: i32) -> u32;
        fn eglGetConfigs(display: Pointer, configs: *mut Pointer, size: i32, count: *mut i32) -> u32;
        fn eglChooseConfig(
            display: Pointer,
            attributes: *const i32,
            configs: *mut Pointer,
            size: i32,
            count: *mut i32,
        ) -> u32;
        fn eglGetConfigAttrib(display: Pointer, config: Pointer, attribute: i32, value: *mut i32) -> u32;
        fn eglCreateWindowSurface(display: Pointer, config: Pointer, window: Pointer, attributes: *const i32) -> Pointer;
        fn eglCreatePbufferSurface(display: Pointer, config: Pointer, attributes: *const i32) -> Pointer;
        fn eglDestroySurface(display: Pointer, surface: Pointer) -> u32;
        fn eglQuerySurface(display: Pointer, surface: Pointer, attribute: i32, value: *mut i32) -> u32;
        fn eglSurfaceAttrib(display: Pointer, surface: Pointer, attribute: i32, value: i32) -> u32;
        fn eglCreateContext(display: Pointer, config: Pointer, share: Pointer, attributes: *const i32) -> Pointer;
        fn eglDestroyContext(display: Pointer, context: Pointer) -> u32;
        fn eglQueryContext(display: Pointer, context: Pointer, attribute: i32, value: *mut i32) -> u32;
        fn eglMakeCurrent(display: Pointer, draw: Pointer, read: Pointer, context: Pointer) -> u32;
        fn eglGetCurrentContext() -> Pointer;
        fn eglGetCurrentSurface(which: i32) -> Pointer;
        fn eglGetCurrentDisplay() -> Pointer;
        fn eglSwapBuffers(display: Pointer, surface: Pointer) -> u32;
        fn eglSwapInterval(display: Pointer, interval: i32) -> u32;
        fn eglQueryString(display: Pointer, name: i32) -> *const libc::c_char;
        fn eglGetError() -> i32;
        fn eglGetProcAddress(name: *const libc::c_char) -> Pointer;
    }

    #[link(name = "android")]
    extern "C" {
        fn ANativeWindow_fromSurface(env: *mut jni_sys::JNIEnv, surface: jni_sys::jobject) -> Pointer;
        fn ANativeWindow_toSurface(env: *mut jni_sys::JNIEnv, window: Pointer) -> jni_sys::jobject;
        fn ANativeWindow_acquire(window: Pointer);
        fn ANativeWindow_release(window: Pointer);
        fn ANativeWindow_getWidth(window: Pointer) -> i32;
        fn ANativeWindow_getHeight(window: Pointer) -> i32;
        fn ANativeWindow_getFormat(window: Pointer) -> i32;
        fn ANativeWindow_setBuffersGeometry(window: Pointer, width: i32, height: i32, format: i32) -> i32;
        fn ALooper_forThread() -> Pointer;
        fn ALooper_prepare(options: i32) -> Pointer;
        fn ALooper_acquire(looper: Pointer);
        fn ALooper_release(looper: Pointer);
        fn ALooper_wake(looper: Pointer);
        fn ALooper_pollAll(timeout: i32, fd: *mut i32, events: *mut i32, data: *mut Pointer) -> i32;
        fn ALooper_pollOnce(timeout: i32, fd: *mut i32, events: *mut i32, data: *mut Pointer) -> i32;
        fn ASensorManager_getInstance() -> Pointer;
        fn ASensorManager_getSensorList(manager: Pointer, list: *mut *const Pointer) -> i32;
        fn ASensorManager_getDefaultSensor(manager: Pointer, kind: i32) -> Pointer;
        fn ASensorManager_createEventQueue(manager: Pointer, looper: Pointer, ident: i32, callback: Pointer, data: Pointer) -> Pointer;
        fn ASensorManager_destroyEventQueue(manager: Pointer, queue: Pointer) -> i32;
        fn ASensor_getName(sensor: Pointer) -> *const libc::c_char;
        fn ASensor_getVendor(sensor: Pointer) -> *const libc::c_char;
        fn ASensor_getType(sensor: Pointer) -> i32;
        fn ASensor_getResolution(sensor: Pointer) -> f32;
        fn ASensor_getMinDelay(sensor: Pointer) -> i32;
        fn ASensorEventQueue_enableSensor(queue: Pointer, sensor: Pointer) -> i32;
        fn ASensorEventQueue_disableSensor(queue: Pointer, sensor: Pointer) -> i32;
        fn ASensorEventQueue_setEventRate(queue: Pointer, sensor: Pointer, microseconds: i32) -> i32;
        fn ASensorEventQueue_hasEvents(queue: Pointer) -> i32;
        fn ASensorEventQueue_getEvents(queue: Pointer, events: *mut u8, count: usize) -> isize;
    }

    /// ASensorEvent is 104 bytes with the same layout on 32- and 64-bit
    /// Android (fixed-size fields only), so events are copied as bytes.
    const SENSOR_EVENT_SIZE: usize = 104;

    /// Guest copies of sensor lists (by manager) and names/vendors (by sensor
    /// and which), kept for the process, as the host keeps the originals.
    static SENSOR_COPIES: Mutex<Option<HashMap<(u32, u32), u32>>> = Mutex::new(None);

    fn sensor_copy(key: (u32, u32), make: impl FnOnce() -> u32) -> u32 {
        let mut copies = SENSOR_COPIES.lock().unwrap();
        *copies.get_or_insert_with(HashMap::new).entry(key).or_insert_with(make)
    }

    fn guest_c_string(text: *const libc::c_char) -> u32 {
        if text.is_null() {
            return 0;
        }
        let bytes = unsafe { std::ffi::CStr::from_ptr(text) }.to_bytes().to_vec();
        crate::runtime::heap().strdup(&bytes)
    }

    /// An EGL extension function, looked up once through eglGetProcAddress.
    macro_rules! extension {
        ($name:literal, fn($($t:ty),*) -> $r:ty) => {{
            static ADDRESS: AtomicUsize = AtomicUsize::new(0);
            let mut address = ADDRESS.load(Ordering::Relaxed);
            if address == 0 {
                address = unsafe { eglGetProcAddress(concat!($name, "\0").as_ptr().cast()) } as usize;
                ADDRESS.store(address, Ordering::Relaxed);
            }
            if address == 0 {
                crate::log::line(concat!("[bridge] this phone has no ", $name));
                None
            } else {
                let function: unsafe extern "C" fn($($t),*) -> $r = unsafe { std::mem::transmute(address) };
                Some(function)
            }
        }};
    }

    /// Handles are opaque numbers, never dereferenced by the game.
    const HANDLE_BASE: u32 = 0xDD00_0000;

    #[derive(Default)]
    struct Handles {
        to_host: HashMap<u32, usize>,
        to_guest: HashMap<usize, u32>,
        next: u32,
    }

    static HANDLES: Mutex<Option<Handles>> = Mutex::new(None);

    /// The guest handle for a host pointer (0 stays 0: EGL_NO_*).
    fn guest(object: Pointer) -> u32 {
        if object.is_null() {
            return 0;
        }
        let mut handles = HANDLES.lock().unwrap();
        let handles = handles.get_or_insert_with(Handles::default);
        if let Some(&handle) = handles.to_guest.get(&(object as usize)) {
            return handle;
        }
        handles.next += 4;
        let handle = HANDLE_BASE + handles.next;
        handles.to_host.insert(handle, object as usize);
        handles.to_guest.insert(object as usize, handle);
        handle
    }

    /// Drops a handle whose object is gone (fences come and go every frame).
    fn forget(handle: u32) {
        let mut handles = HANDLES.lock().unwrap();
        if let Some(handles) = handles.as_mut() {
            if let Some(object) = handles.to_host.remove(&handle) {
                handles.to_guest.remove(&object);
            }
        }
    }

    fn host(handle: u32) -> Pointer {
        if handle == 0 {
            return std::ptr::null_mut();
        }
        let handles = HANDLES.lock().unwrap();
        let found = handles.as_ref().and_then(|h| h.to_host.get(&handle).copied());
        if found.is_none() {
            crate::log::line(&format!("[bridge] EGL/window handle {handle:#x} is unknown"));
        }
        found.unwrap_or(0) as Pointer
    }

    fn int_pointer(guest: u32) -> *mut i32 {
        mem().host_or_null(guest) as *mut i32
    }

    /// Writes host configs to the guest's 32-bit EGLConfig array.
    fn write_configs(guest_array: u32, configs: &[Pointer], count: i32) {
        if guest_array == 0 {
            return;
        }
        for (i, config) in configs.iter().take(count.max(0) as usize).enumerate() {
            mem().write_u32(guest_array + i as u32 * 4, guest(*config));
        }
    }

    /// Polls the host looper. The data pointer handed back is the game's own
    /// 32-bit value (only fds the game added, such as sensor queues, report one).
    fn poll(once: bool, timeout: i32, fd_out: u32, events_out: u32, data_out: u32) -> i32 {
        let (mut fd, mut events, mut data) = (0i32, 0i32, std::ptr::null_mut());
        let result = unsafe {
            if once {
                ALooper_pollOnce(timeout, &mut fd, &mut events, &mut data)
            } else {
                ALooper_pollAll(timeout, &mut fd, &mut events, &mut data)
            }
        };
        if fd_out != 0 {
            mem().write_u32(fd_out, fd as u32);
        }
        if events_out != 0 {
            mem().write_u32(events_out, events as u32);
        }
        if data_out != 0 {
            // Only fds the game added (sensor queues) report data here, and their
            // data is the game's own 32-bit value.
            mem().write_u32(data_out, data as usize as u32);
        }
        result
    }

    pub fn current_context() -> usize {
        unsafe { eglGetCurrentContext() as usize }
    }

    pub fn handler(name: &str) -> Option<Handler> {
        match name {
            "eglGetDisplay" => handler!(|uc, a| {
                // EGL_DEFAULT_DISPLAY is 0; Android has no other native display.
                let _native = a.u32();
                Ret::U32(guest(unsafe { eglGetDisplay(std::ptr::null_mut()) }))
            }),
            "eglInitialize" => handler!(|uc, a| {
                let (display, major, minor) = (host(a.u32()), a.u32(), a.u32());
                Ret::U32(unsafe { eglInitialize(display, int_pointer(major), int_pointer(minor)) })
            }),
            "eglTerminate" => handler!(|uc, a| Ret::U32(unsafe { eglTerminate(host(a.u32())) })),
            "eglBindAPI" => handler!(|uc, a| Ret::U32(unsafe { eglBindAPI(a.u32()) })),
            "eglQueryAPI" => handler!(|uc, a| Ret::U32(unsafe { eglQueryAPI() })),
            "eglReleaseThread" => handler!(|uc, a| Ret::U32(unsafe { eglReleaseThread() })),
            "eglWaitClient" => handler!(|uc, a| Ret::U32(unsafe { eglWaitClient() })),
            "eglWaitGL" => handler!(|uc, a| Ret::U32(unsafe { eglWaitGL() })),
            "eglWaitNative" => handler!(|uc, a| Ret::U32(unsafe { eglWaitNative(a.i32()) })),
            "eglGetConfigs" => handler!(|uc, a| {
                let (display, configs, size, count) = (host(a.u32()), a.u32(), a.i32(), a.u32());
                let mut host_configs = vec![std::ptr::null_mut(); size.max(0) as usize];
                let mut found = 0i32;
                let array = if configs == 0 { std::ptr::null_mut() } else { host_configs.as_mut_ptr() };
                let ok = unsafe { eglGetConfigs(display, array, size, &mut found) };
                write_configs(configs, &host_configs, found);
                if count != 0 {
                    mem().write_u32(count, found as u32);
                }
                Ret::U32(ok)
            }),
            "eglChooseConfig" => handler!(|uc, a| {
                let (display, attributes, configs, size, count) = (host(a.u32()), a.u32(), a.u32(), a.i32(), a.u32());
                let mut host_configs = vec![std::ptr::null_mut(); size.max(0) as usize];
                let mut found = 0i32;
                let array = if configs == 0 { std::ptr::null_mut() } else { host_configs.as_mut_ptr() };
                let ok = unsafe { eglChooseConfig(display, int_pointer(attributes), array, size, &mut found) };
                write_configs(configs, &host_configs, found);
                if count != 0 {
                    mem().write_u32(count, found as u32);
                }
                Ret::U32(ok)
            }),
            "eglGetConfigAttrib" => handler!(|uc, a| {
                let (display, config, attribute, value) = (host(a.u32()), host(a.u32()), a.i32(), a.u32());
                Ret::U32(unsafe { eglGetConfigAttrib(display, config, attribute, int_pointer(value)) })
            }),
            "eglCreateWindowSurface" => handler!(|uc, a| {
                let (display, config, window, attributes) = (host(a.u32()), host(a.u32()), host(a.u32()), a.u32());
                Ret::U32(guest(unsafe { eglCreateWindowSurface(display, config, window, int_pointer(attributes)) }))
            }),
            "eglCreatePbufferSurface" => handler!(|uc, a| {
                let (display, config, attributes) = (host(a.u32()), host(a.u32()), a.u32());
                Ret::U32(guest(unsafe { eglCreatePbufferSurface(display, config, int_pointer(attributes)) }))
            }),
            "eglDestroySurface" => handler!(|uc, a| {
                let (display, surface) = (host(a.u32()), host(a.u32()));
                Ret::U32(unsafe { eglDestroySurface(display, surface) })
            }),
            "eglQuerySurface" => handler!(|uc, a| {
                let (display, surface, attribute, value) = (host(a.u32()), host(a.u32()), a.i32(), a.u32());
                Ret::U32(unsafe { eglQuerySurface(display, surface, attribute, int_pointer(value)) })
            }),
            "eglSurfaceAttrib" => handler!(|uc, a| {
                let (display, surface, attribute, value) = (host(a.u32()), host(a.u32()), a.i32(), a.i32());
                Ret::U32(unsafe { eglSurfaceAttrib(display, surface, attribute, value) })
            }),
            "eglCreateContext" => handler!(|uc, a| {
                let (display, config, share, attributes) = (host(a.u32()), host(a.u32()), host(a.u32()), a.u32());
                let context = unsafe { eglCreateContext(display, config, share, int_pointer(attributes)) };
                if !context.is_null() {
                    crate::imports::gles::context_created(context as usize, share as usize);
                }
                Ret::U32(guest(context))
            }),
            "eglDestroyContext" => handler!(|uc, a| {
                let (display, context) = (host(a.u32()), host(a.u32()));
                let done = unsafe { eglDestroyContext(display, context) };
                if done != 0 {
                    crate::imports::gles::context_destroyed(context as usize);
                }
                Ret::U32(done)
            }),
            "eglQueryContext" => handler!(|uc, a| {
                let (display, context, attribute, value) = (host(a.u32()), host(a.u32()), a.i32(), a.u32());
                Ret::U32(unsafe { eglQueryContext(display, context, attribute, int_pointer(value)) })
            }),
            "eglMakeCurrent" => handler!(|uc, a| {
                let (display, draw, read, context) = (host(a.u32()), host(a.u32()), host(a.u32()), host(a.u32()));
                Ret::U32(unsafe { eglMakeCurrent(display, draw, read, context) })
            }),
            "eglGetCurrentContext" => handler!(|uc, a| Ret::U32(guest(unsafe { eglGetCurrentContext() }))),
            "eglGetCurrentSurface" => handler!(|uc, a| Ret::U32(guest(unsafe { eglGetCurrentSurface(a.i32()) }))),
            "eglGetCurrentDisplay" => handler!(|uc, a| Ret::U32(guest(unsafe { eglGetCurrentDisplay() }))),
            "eglSwapBuffers" => handler!(|uc, a| {
                let (display, surface) = (host(a.u32()), host(a.u32()));
                Ret::U32(unsafe { eglSwapBuffers(display, surface) })
            }),
            "eglSwapInterval" => handler!(|uc, a| {
                let (display, interval) = (host(a.u32()), a.i32());
                Ret::U32(unsafe { eglSwapInterval(display, interval) })
            }),
            "eglQueryString" => handler!(|uc, a| {
                let (display, which) = (a.u32(), a.i32());
                let key = format!("eglQueryString:{display:#x}:{which:#x}");
                if let Some(&address) = shared().state.data_imports.get(&key) {
                    return Ret::U32(address);
                }
                let text = unsafe { eglQueryString(host(display), which) };
                if text.is_null() {
                    return Ret::U32(0);
                }
                let text = unsafe { std::ffi::CStr::from_ptr(text) }.to_string_lossy().into_owned();
                Ret::U32(shared().cached_cstr(key, &text))
            }),
            "eglGetError" => handler!(|uc, a| Ret::U32(unsafe { eglGetError() } as u32)),
            "eglCreateSyncKHR" => handler!(|uc, a| {
                let (display, kind, attributes) = (host(a.u32()), a.u32(), a.u32());
                let Some(create) = extension!("eglCreateSyncKHR", fn(Pointer, u32, *const i32) -> Pointer) else {
                    return Ret::U32(0);
                };
                Ret::U32(guest(unsafe { create(display, kind, int_pointer(attributes)) }))
            }),
            "eglDestroySyncKHR" => handler!(|uc, a| {
                let (display, handle) = (host(a.u32()), a.u32());
                let Some(destroy) = extension!("eglDestroySyncKHR", fn(Pointer, Pointer) -> u32) else {
                    return Ret::U32(0);
                };
                let result = unsafe { destroy(display, host(handle)) };
                forget(handle);
                Ret::U32(result)
            }),
            "eglClientWaitSyncKHR" => handler!(|uc, a| {
                let (display, sync, flags, timeout) = (host(a.u32()), host(a.u32()), a.i32(), a.u64());
                let Some(wait) = extension!("eglClientWaitSyncKHR", fn(Pointer, Pointer, i32, u64) -> i32) else {
                    return Ret::U32(0);
                };
                Ret::U32(crate::runtime::without_gil(|| unsafe { wait(display, sync, flags, timeout) }) as u32)
            }),
            "eglGetSyncAttribKHR" => handler!(|uc, a| {
                let (display, sync, attribute, value) = (host(a.u32()), host(a.u32()), a.i32(), a.u32());
                let Some(get) = extension!("eglGetSyncAttribKHR", fn(Pointer, Pointer, i32, *mut i32) -> u32) else {
                    return Ret::U32(0);
                };
                Ret::U32(unsafe { get(display, sync, attribute, int_pointer(value)) })
            }),
            "eglGetFrameTimestampSupportedANDROID" => handler!(|uc, a| {
                let (display, surface, timestamp) = (host(a.u32()), host(a.u32()), a.i32());
                let Some(get) =
                    extension!("eglGetFrameTimestampSupportedANDROID", fn(Pointer, Pointer, i32) -> u32)
                else {
                    return Ret::U32(0);
                };
                Ret::U32(unsafe { get(display, surface, timestamp) })
            }),
            // (display, surface, frame id, count, which timestamps, int64 results)
            "eglGetFrameTimestampsANDROID" => handler!(|uc, a| {
                let (display, surface, frame) = (host(a.u32()), host(a.u32()), a.u64());
                let (count, which, values) = (a.i32(), a.u32(), a.u32());
                let Some(get) = extension!(
                    "eglGetFrameTimestampsANDROID",
                    fn(Pointer, Pointer, u64, i32, *const i32, *mut i64) -> u32
                ) else {
                    return Ret::U32(0);
                };
                let values = mem().host_or_null(values) as *mut i64;
                Ret::U32(unsafe { get(display, surface, frame, count, int_pointer(which), values) })
            }),
            "eglGetNextFrameIdANDROID" => handler!(|uc, a| {
                let (display, surface, frame) = (host(a.u32()), host(a.u32()), a.u32());
                let Some(get) = extension!("eglGetNextFrameIdANDROID", fn(Pointer, Pointer, *mut u64) -> u32) else {
                    return Ret::U32(0);
                };
                Ret::U32(unsafe { get(display, surface, mem().host_or_null(frame) as *mut u64) })
            }),
            "eglPresentationTimeANDROID" => handler!(|uc, a| {
                let (display, surface, time) = (host(a.u32()), host(a.u32()), a.i64());
                let Some(set) = extension!("eglPresentationTimeANDROID", fn(Pointer, Pointer, i64) -> u32) else {
                    return Ret::U32(0);
                };
                Ret::U32(unsafe { set(display, surface, time) })
            }),
            "ANativeWindow_fromSurface" => handler!(|uc, a| {
                let (_env, surface) = (a.u32(), a.u32());
                Ret::U32(guest(unsafe { ANativeWindow_fromSurface(jni_host::env(), jni_host::to_host(surface)) }))
            }),
            "ANativeWindow_toSurface" => handler!(|uc, a| {
                let (_env, window) = (a.u32(), a.u32());
                Ret::U32(jni_host::to_guest(unsafe { ANativeWindow_toSurface(jni_host::env(), host(window)) }))
            }),
            "ANativeWindow_acquire" => handler!(|uc, a| {
                unsafe { ANativeWindow_acquire(host(a.u32())) };
                Ret::Void
            }),
            "ANativeWindow_release" => handler!(|uc, a| {
                unsafe { ANativeWindow_release(host(a.u32())) };
                Ret::Void
            }),
            "ANativeWindow_getWidth" => handler!(|uc, a| Ret::U32(unsafe { ANativeWindow_getWidth(host(a.u32())) } as u32)),
            "ANativeWindow_getHeight" => {
                handler!(|uc, a| Ret::U32(unsafe { ANativeWindow_getHeight(host(a.u32())) } as u32))
            }
            "ANativeWindow_getFormat" => {
                handler!(|uc, a| Ret::U32(unsafe { ANativeWindow_getFormat(host(a.u32())) } as u32))
            }
            "ANativeWindow_setBuffersGeometry" => handler!(|uc, a| {
                let (window, width, height, format) = (host(a.u32()), a.i32(), a.i32(), a.i32());
                Ret::U32(unsafe { ANativeWindow_setBuffersGeometry(window, width, height, format) } as u32)
            }),
            "ALooper_forThread" => handler!(|uc, a| Ret::U32(guest(unsafe { ALooper_forThread() }))),
            "ALooper_prepare" => handler!(|uc, a| Ret::U32(guest(unsafe { ALooper_prepare(a.i32()) }))),
            "ALooper_acquire" => handler!(|uc, a| {
                unsafe { ALooper_acquire(host(a.u32())) };
                Ret::Void
            }),
            "ALooper_release" => handler!(|uc, a| {
                unsafe { ALooper_release(host(a.u32())) };
                Ret::Void
            }),
            "ALooper_wake" => handler!(|uc, a| {
                unsafe { ALooper_wake(host(a.u32())) };
                Ret::Void
            }),
            "ALooper_pollAll" => handler!(|uc, a| {
                let (timeout, fd, events, data) = (a.i32(), a.u32(), a.u32(), a.u32());
                Ret::U32(poll(false, timeout, fd, events, data) as u32)
            }),
            "ALooper_pollOnce" => handler!(|uc, a| {
                let (timeout, fd, events, data) = (a.i32(), a.u32(), a.u32(), a.u32());
                Ret::U32(poll(true, timeout, fd, events, data) as u32)
            }),
            "ASensorManager_getInstance" => handler!(|uc, a| Ret::U32(guest(unsafe { ASensorManager_getInstance() }))),
            "ASensorManager_getSensorList" => handler!(|uc, a| {
                let (manager, out) = (a.u32(), a.u32());
                let mut list: *const Pointer = std::ptr::null();
                let count = unsafe { ASensorManager_getSensorList(host(manager), &mut list) }.max(0);
                if out != 0 {
                    let array = sensor_copy((manager, 0), || {
                        let array = crate::runtime::heap().malloc(count as u32 * 4 + 4);
                        for i in 0..count as usize {
                            mem().write_u32(array + i as u32 * 4, guest(unsafe { *list.add(i) }));
                        }
                        array
                    });
                    mem().write_u32(out, array);
                }
                Ret::U32(count as u32)
            }),
            "ASensorManager_getDefaultSensor" => handler!(|uc, a| {
                let (manager, kind) = (a.u32(), a.i32());
                Ret::U32(guest(unsafe { ASensorManager_getDefaultSensor(host(manager), kind) }))
            }),
            "ASensorManager_createEventQueue" => handler!(|uc, a| {
                let (manager, looper, ident, callback, data) = (a.u32(), a.u32(), a.i32(), a.u32(), a.u32());
                if callback != 0 {
                    // The host looper would call this guest address as host code.
                    crate::log::line("[bridge] ASensorManager_createEventQueue with a callback: not supported, events come by ident only");
                }
                let queue = unsafe {
                    ASensorManager_createEventQueue(host(manager), host(looper), ident, std::ptr::null_mut(), data as usize as Pointer)
                };
                Ret::U32(guest(queue))
            }),
            "ASensorManager_destroyEventQueue" => handler!(|uc, a| {
                let (manager, queue) = (a.u32(), a.u32());
                let result = unsafe { ASensorManager_destroyEventQueue(host(manager), host(queue)) };
                forget(queue);
                Ret::U32(result as u32)
            }),
            "ASensor_getName" => handler!(|uc, a| {
                let sensor = a.u32();
                Ret::U32(sensor_copy((sensor, 1), || guest_c_string(unsafe { ASensor_getName(host(sensor)) })))
            }),
            "ASensor_getVendor" => handler!(|uc, a| {
                let sensor = a.u32();
                Ret::U32(sensor_copy((sensor, 2), || guest_c_string(unsafe { ASensor_getVendor(host(sensor)) })))
            }),
            "ASensor_getType" => handler!(|uc, a| Ret::U32(unsafe { ASensor_getType(host(a.u32())) } as u32)),
            "ASensor_getResolution" => handler!(|uc, a| Ret::F32(unsafe { ASensor_getResolution(host(a.u32())) })),
            "ASensor_getMinDelay" => handler!(|uc, a| Ret::U32(unsafe { ASensor_getMinDelay(host(a.u32())) } as u32)),
            "ASensorEventQueue_enableSensor" => handler!(|uc, a| {
                let (queue, sensor) = (a.u32(), a.u32());
                Ret::U32(unsafe { ASensorEventQueue_enableSensor(host(queue), host(sensor)) } as u32)
            }),
            "ASensorEventQueue_disableSensor" => handler!(|uc, a| {
                let (queue, sensor) = (a.u32(), a.u32());
                Ret::U32(unsafe { ASensorEventQueue_disableSensor(host(queue), host(sensor)) } as u32)
            }),
            "ASensorEventQueue_setEventRate" => handler!(|uc, a| {
                let (queue, sensor, microseconds) = (a.u32(), a.u32(), a.i32());
                Ret::U32(unsafe { ASensorEventQueue_setEventRate(host(queue), host(sensor), microseconds) } as u32)
            }),
            "ASensorEventQueue_hasEvents" => handler!(|uc, a| Ret::U32(unsafe { ASensorEventQueue_hasEvents(host(a.u32())) } as u32)),
            "ASensorEventQueue_getEvents" => handler!(|uc, a| {
                let (queue, events, count) = (a.u32(), a.u32(), a.u32().min(256) as usize);
                let mut buffer = vec![0u8; count * SENSOR_EVENT_SIZE];
                let got = unsafe { ASensorEventQueue_getEvents(host(queue), buffer.as_mut_ptr(), count) };
                if got > 0 {
                    mem().write_bytes(events, &buffer[..got as usize * SENSOR_EVENT_SIZE]);
                }
                Ret::U32(got as i32 as u32)
            }),
            _ => None,
        }
    }
}

#[cfg(target_os = "android")]
pub use host::{current_context, handler};

/// The host EGL context current on this thread (0 = none).
#[cfg(not(target_os = "android"))]
pub fn current_context() -> usize {
    0
}

#[cfg(not(target_os = "android"))]
pub fn handler(_name: &str) -> Option<Handler> {
    None
}
