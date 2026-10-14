//! NativeActivity games: Android starts them by calling their
//! ANativeActivity_onCreate (the stand-in has one, tools/standin) instead of
//! Java loading the library. The bridge gives the 32-bit code its own copy of
//! the activity (32-bit layout, handles for the window, input queue and
//! assets) and passes each of Android's activity events (start, pause, window
//! made, input queue ready...) to the callbacks the game sets in it.
//!
//! Also the input queue and input event functions such games read touches
//! and keys with. Queues and events are 64-bit host objects, so the guest
//! gets handles (the same ones egl.rs gives windows and loopers).

/// Functions handled here. Only the phone build has them; the PC tool's
/// `imports` report counts them as handled.
pub const NAMES: &[&str] = &[
    "ANativeActivity_finish",
    "ANativeActivity_setWindowFlags",
    "ANativeActivity_showSoftInput",
    "ANativeActivity_hideSoftInput",
    "AInputQueue_attachLooper",
    "AInputQueue_detachLooper",
    "AInputQueue_hasEvents",
    "AInputQueue_getEvent",
    "AInputQueue_preDispatchEvent",
    "AInputQueue_finishEvent",
    "AInputEvent_getType",
    "AInputEvent_getDeviceId",
    "AInputEvent_getSource",
    "AKeyEvent_getAction",
    "AKeyEvent_getFlags",
    "AKeyEvent_getKeyCode",
    "AKeyEvent_getScanCode",
    "AKeyEvent_getMetaState",
    "AKeyEvent_getRepeatCount",
    "AKeyEvent_getDownTime",
    "AKeyEvent_getEventTime",
    "AMotionEvent_getAction",
    "AMotionEvent_getFlags",
    "AMotionEvent_getMetaState",
    "AMotionEvent_getButtonState",
    "AMotionEvent_getEdgeFlags",
    "AMotionEvent_getDownTime",
    "AMotionEvent_getEventTime",
    "AMotionEvent_getXOffset",
    "AMotionEvent_getYOffset",
    "AMotionEvent_getXPrecision",
    "AMotionEvent_getYPrecision",
    "AMotionEvent_getPointerCount",
    "AMotionEvent_getPointerId",
    "AMotionEvent_getToolType",
    "AMotionEvent_getRawX",
    "AMotionEvent_getRawY",
    "AMotionEvent_getX",
    "AMotionEvent_getY",
    "AMotionEvent_getPressure",
    "AMotionEvent_getSize",
    "AMotionEvent_getTouchMajor",
    "AMotionEvent_getTouchMinor",
    "AMotionEvent_getOrientation",
    "AMotionEvent_getAxisValue",
    "AMotionEvent_getHistorySize",
];

#[cfg(not(target_os = "android"))]
use crate::runtime::Handler;

#[cfg(target_os = "android")]
mod host {
    use std::collections::HashMap;
    use std::ffi::{c_char, c_void, CStr};
    use std::sync::Mutex;

    use jni_sys::{jobject, JNIEnv, JavaVM};

    use crate::handler;
    use crate::imports::{android, egl, jni_host};
    use crate::runtime::{call_guest, engine, hold_gil, mem, shared, ArgSource, Handler, Ret};

    type Pointer = *mut c_void;

    /// <android/native_activity.h>, 64-bit.
    #[repr(C)]
    struct Callbacks {
        on_start: Option<extern "C" fn(*mut Activity)>,
        on_resume: Option<extern "C" fn(*mut Activity)>,
        on_save_instance_state: Option<extern "C" fn(*mut Activity, *mut usize) -> Pointer>,
        on_pause: Option<extern "C" fn(*mut Activity)>,
        on_stop: Option<extern "C" fn(*mut Activity)>,
        on_destroy: Option<extern "C" fn(*mut Activity)>,
        on_window_focus_changed: Option<extern "C" fn(*mut Activity, i32)>,
        on_native_window_created: Option<extern "C" fn(*mut Activity, Pointer)>,
        on_native_window_resized: Option<extern "C" fn(*mut Activity, Pointer)>,
        on_native_window_redraw_needed: Option<extern "C" fn(*mut Activity, Pointer)>,
        on_native_window_destroyed: Option<extern "C" fn(*mut Activity, Pointer)>,
        on_input_queue_created: Option<extern "C" fn(*mut Activity, Pointer)>,
        on_input_queue_destroyed: Option<extern "C" fn(*mut Activity, Pointer)>,
        on_content_rect_changed: Option<extern "C" fn(*mut Activity, *const [i32; 4])>,
        on_configuration_changed: Option<extern "C" fn(*mut Activity)>,
        on_low_memory: Option<extern "C" fn(*mut Activity)>,
    }

    #[repr(C)]
    pub struct Activity {
        callbacks: *mut Callbacks,
        vm: *mut JavaVM,
        env: *mut JNIEnv,
        clazz: jobject,
        internal_data_path: *const c_char,
        external_data_path: *const c_char,
        sdk_version: i32,
        instance: Pointer,
        asset_manager: Pointer,
        obb_path: *const c_char,
    }

    /// The guest's ANativeActivity (ARM32): callbacks, vm, env, clazz,
    /// internalDataPath, externalDataPath, sdkVersion, instance, assetManager,
    /// obbPath, 4 bytes each. Its callback table (16 pointers, the same order
    /// as `Callbacks`) and a scratch ARect follow it in the same block.
    const GUEST_ACTIVITY_SIZE: u32 = 40;
    const GUEST_CALLBACKS_SIZE: u32 = 16 * 4;
    const GUEST_RECT_SIZE: u32 = 16;

    /// Callback slots, as in `Callbacks`.
    const ON_START: u32 = 0;
    const ON_RESUME: u32 = 1;
    const ON_SAVE_INSTANCE_STATE: u32 = 2;
    const ON_PAUSE: u32 = 3;
    const ON_STOP: u32 = 4;
    const ON_DESTROY: u32 = 5;
    const ON_WINDOW_FOCUS_CHANGED: u32 = 6;
    const ON_NATIVE_WINDOW_CREATED: u32 = 7;
    const ON_NATIVE_WINDOW_RESIZED: u32 = 8;
    const ON_NATIVE_WINDOW_REDRAW_NEEDED: u32 = 9;
    const ON_NATIVE_WINDOW_DESTROYED: u32 = 10;
    const ON_INPUT_QUEUE_CREATED: u32 = 11;
    const ON_INPUT_QUEUE_DESTROYED: u32 = 12;
    const ON_CONTENT_RECT_CHANGED: u32 = 13;
    const ON_CONFIGURATION_CHANGED: u32 = 14;
    const ON_LOW_MEMORY: u32 = 15;

    /// Host activity -> guest activity, and back (for ANativeActivity_finish).
    static ACTIVITIES: Mutex<Vec<(usize, u32)>> = Mutex::new(Vec::new());

    fn guest_activity(activity: *mut Activity) -> Option<u32> {
        ACTIVITIES.lock().unwrap().iter().find(|(host, _)| *host == activity as usize).map(|&(_, guest)| guest)
    }

    fn host_activity(guest: u32) -> *mut Activity {
        let found = ACTIVITIES.lock().unwrap().iter().find(|(_, g)| *g == guest).map(|&(host, _)| host);
        found.unwrap_or(0) as *mut Activity
    }

    fn guest_string(text: *const c_char) -> u32 {
        if text.is_null() {
            return 0;
        }
        let bytes = unsafe { CStr::from_ptr(text) }.to_bytes().to_vec();
        crate::runtime::heap().strdup(&bytes)
    }

    /// Called by the stand-in's ANativeActivity_onCreate on Android's main thread.
    #[no_mangle]
    pub extern "C" fn arm32_bridge_native_activity(
        activity: *mut Activity,
        saved_state: Pointer,
        saved_state_size: usize,
        original: *const c_char,
    ) {
        let original = std::path::PathBuf::from(unsafe { CStr::from_ptr(original) }.to_string_lossy().into_owned());
        if let Err(error) = start(activity, saved_state, saved_state_size, &original) {
            crate::log::line(&format!("[bridge] couldn't start {} as a NativeActivity: {error}", original.display()));
            unsafe { ANativeActivity_finish(activity.cast()) };
        }
    }

    fn start(activity: *mut Activity, saved_state: Pointer, saved_state_size: usize, original: &std::path::Path) -> Result<(), String> {
        let host = unsafe { &mut *activity };
        // Java may not have loaded the library itself; then this is the first the bridge hears of it.
        crate::app::load_original(host.vm, host.env, original)?;
        let _lock = hold_gil();
        let _entry = jni_host::enter(host.env);
        let uc = engine();
        let on_create = shared()
            .guest_exports
            .get("ANativeActivity_onCreate")
            .copied()
            .ok_or("the game library has no ANativeActivity_onCreate")?;

        let block = crate::runtime::heap().calloc(1, GUEST_ACTIVITY_SIZE + GUEST_CALLBACKS_SIZE + GUEST_RECT_SIZE);
        let clazz = unsafe { ((**host.env).NewGlobalRef.unwrap())(host.env, host.clazz) };
        // One at a time: each of these takes the shared state for itself.
        let guest_env = shared().state.jni.env;
        let guest_clazz = jni_host::to_guest_global(clazz);
        let asset_manager = android::asset_manager_handle(host.asset_manager);
        let (internal_path, external_path, obb_path) =
            (guest_string(host.internal_data_path), guest_string(host.external_data_path), guest_string(host.obb_path));
        let fields = [
            block + GUEST_ACTIVITY_SIZE,
            crate::app::guest_vm(),
            guest_env,
            guest_clazz,
            internal_path,
            external_path,
            host.sdk_version as u32,
            0,
            asset_manager,
            obb_path,
        ];
        for (i, value) in fields.iter().enumerate() {
            mem().write_u32(block + i as u32 * 4, *value);
        }
        ACTIVITIES.lock().unwrap().push((activity as usize, block));

        // Every event goes to the guest's callback (a missing one is skipped there).
        let callbacks = unsafe { &mut *host.callbacks };
        callbacks.on_start = Some(on_start);
        callbacks.on_resume = Some(on_resume);
        callbacks.on_save_instance_state = Some(on_save_instance_state);
        callbacks.on_pause = Some(on_pause);
        callbacks.on_stop = Some(on_stop);
        callbacks.on_destroy = Some(on_destroy);
        callbacks.on_window_focus_changed = Some(on_window_focus_changed);
        callbacks.on_native_window_created = Some(on_native_window_created);
        callbacks.on_native_window_resized = Some(on_native_window_resized);
        callbacks.on_native_window_redraw_needed = Some(on_native_window_redraw_needed);
        callbacks.on_native_window_destroyed = Some(on_native_window_destroyed);
        callbacks.on_input_queue_created = Some(on_input_queue_created);
        callbacks.on_input_queue_destroyed = Some(on_input_queue_destroyed);
        callbacks.on_content_rect_changed = Some(on_content_rect_changed);
        callbacks.on_configuration_changed = Some(on_configuration_changed);
        callbacks.on_low_memory = Some(on_low_memory);

        let saved = if saved_state.is_null() || saved_state_size == 0 {
            0
        } else {
            let copy = crate::runtime::heap().malloc(saved_state_size as u32);
            unsafe { std::ptr::copy_nonoverlapping(saved_state as *const u8, mem().host(copy), saved_state_size) };
            copy
        };
        crate::log::line(&format!("[bridge] starting NativeActivity {}", original.display()));
        call_guest(uc, on_create, &[block, saved, saved_state_size as u32]).map_err(|e| format!("ANativeActivity_onCreate failed: {e}"))?;
        Ok(())
    }

    /// Calls the guest's callback in `slot` with the guest activity and `extra` arguments.
    fn forward(activity: *mut Activity, slot: u32, extra: &[u32]) -> u32 {
        let Some(guest) = guest_activity(activity) else { return 0 };
        let _lock = hold_gil();
        let _entry = jni_host::enter(unsafe { (*activity).env });
        let uc = engine();
        let table = mem().read_u32(guest);
        let function = if table == 0 { 0 } else { mem().read_u32(table + slot * 4) };
        if function == 0 {
            return 0;
        }
        let mut arguments = vec![guest];
        arguments.extend_from_slice(extra);
        match call_guest(uc, function, &arguments) {
            Ok(value) => value as u32,
            Err(error) => {
                crate::log::line(&format!("[bridge] NativeActivity callback {slot} failed: {error}"));
                std::process::abort();
            }
        }
    }

    extern "C" fn on_start(activity: *mut Activity) {
        forward(activity, ON_START, &[]);
    }
    extern "C" fn on_resume(activity: *mut Activity) {
        forward(activity, ON_RESUME, &[]);
    }
    extern "C" fn on_pause(activity: *mut Activity) {
        forward(activity, ON_PAUSE, &[]);
    }
    extern "C" fn on_stop(activity: *mut Activity) {
        forward(activity, ON_STOP, &[]);
    }
    extern "C" fn on_destroy(activity: *mut Activity) {
        forward(activity, ON_DESTROY, &[]);
        ACTIVITIES.lock().unwrap().retain(|(host, _)| *host != activity as usize);
    }
    extern "C" fn on_configuration_changed(activity: *mut Activity) {
        forward(activity, ON_CONFIGURATION_CHANGED, &[]);
    }
    extern "C" fn on_low_memory(activity: *mut Activity) {
        forward(activity, ON_LOW_MEMORY, &[]);
    }
    extern "C" fn on_window_focus_changed(activity: *mut Activity, focused: i32) {
        forward(activity, ON_WINDOW_FOCUS_CHANGED, &[focused as u32]);
    }
    extern "C" fn on_native_window_created(activity: *mut Activity, window: Pointer) {
        forward(activity, ON_NATIVE_WINDOW_CREATED, &[egl::handle_for(window)]);
    }
    extern "C" fn on_native_window_resized(activity: *mut Activity, window: Pointer) {
        forward(activity, ON_NATIVE_WINDOW_RESIZED, &[egl::handle_for(window)]);
    }
    extern "C" fn on_native_window_redraw_needed(activity: *mut Activity, window: Pointer) {
        forward(activity, ON_NATIVE_WINDOW_REDRAW_NEEDED, &[egl::handle_for(window)]);
    }
    extern "C" fn on_native_window_destroyed(activity: *mut Activity, window: Pointer) {
        forward(activity, ON_NATIVE_WINDOW_DESTROYED, &[egl::handle_for(window)]);
    }
    extern "C" fn on_input_queue_created(activity: *mut Activity, queue: Pointer) {
        forward(activity, ON_INPUT_QUEUE_CREATED, &[egl::handle_for(queue)]);
    }
    extern "C" fn on_input_queue_destroyed(activity: *mut Activity, queue: Pointer) {
        forward(activity, ON_INPUT_QUEUE_DESTROYED, &[egl::handle_for(queue)]);
    }
    extern "C" fn on_content_rect_changed(activity: *mut Activity, rect: *const [i32; 4]) {
        let Some(guest) = guest_activity(activity) else { return };
        let scratch = guest + GUEST_ACTIVITY_SIZE + GUEST_CALLBACKS_SIZE;
        let rect = unsafe { *rect };
        for (i, value) in rect.iter().enumerate() {
            mem().write_u32(scratch + i as u32 * 4, *value as u32);
        }
        forward(activity, ON_CONTENT_RECT_CHANGED, &[scratch]);
    }
    /// The guest's saved state is a guest malloc block; Android frees what it gets with its own free().
    extern "C" fn on_save_instance_state(activity: *mut Activity, size_out: *mut usize) -> Pointer {
        let Some(guest) = guest_activity(activity) else { return std::ptr::null_mut() };
        let size_slot = guest + GUEST_ACTIVITY_SIZE + GUEST_CALLBACKS_SIZE;
        mem().write_u32(size_slot, 0);
        let state = forward(activity, ON_SAVE_INSTANCE_STATE, &[size_slot]);
        let size = mem().read_u32(size_slot) as usize;
        unsafe { *size_out = 0 };
        if state == 0 || size == 0 {
            return std::ptr::null_mut();
        }
        let copy = unsafe { libc::malloc(size) };
        if !copy.is_null() {
            unsafe { std::ptr::copy_nonoverlapping(mem().host(state), copy as *mut u8, size) };
            unsafe { *size_out = size };
        }
        crate::runtime::heap().free(state);
        copy
    }

    #[link(name = "android")]
    extern "C" {
        fn ANativeActivity_finish(activity: Pointer);
        fn ANativeActivity_setWindowFlags(activity: Pointer, add: u32, remove: u32);
        fn ANativeActivity_showSoftInput(activity: Pointer, flags: u32);
        fn ANativeActivity_hideSoftInput(activity: Pointer, flags: u32);
        fn AInputQueue_attachLooper(queue: Pointer, looper: Pointer, ident: i32, callback: Pointer, data: Pointer);
        fn AInputQueue_detachLooper(queue: Pointer);
        fn AInputQueue_hasEvents(queue: Pointer) -> i32;
        fn AInputQueue_getEvent(queue: Pointer, event: *mut Pointer) -> i32;
        fn AInputQueue_preDispatchEvent(queue: Pointer, event: Pointer) -> i32;
        fn AInputQueue_finishEvent(queue: Pointer, event: Pointer, handled: i32);
        fn AInputEvent_getType(event: Pointer) -> i32;
        fn AInputEvent_getDeviceId(event: Pointer) -> i32;
        fn AInputEvent_getSource(event: Pointer) -> i32;
        fn AKeyEvent_getAction(event: Pointer) -> i32;
        fn AKeyEvent_getFlags(event: Pointer) -> i32;
        fn AKeyEvent_getKeyCode(event: Pointer) -> i32;
        fn AKeyEvent_getScanCode(event: Pointer) -> i32;
        fn AKeyEvent_getMetaState(event: Pointer) -> i32;
        fn AKeyEvent_getRepeatCount(event: Pointer) -> i32;
        fn AKeyEvent_getDownTime(event: Pointer) -> i64;
        fn AKeyEvent_getEventTime(event: Pointer) -> i64;
        fn AMotionEvent_getAction(event: Pointer) -> i32;
        fn AMotionEvent_getFlags(event: Pointer) -> i32;
        fn AMotionEvent_getMetaState(event: Pointer) -> i32;
        fn AMotionEvent_getButtonState(event: Pointer) -> i32;
        fn AMotionEvent_getEdgeFlags(event: Pointer) -> i32;
        fn AMotionEvent_getDownTime(event: Pointer) -> i64;
        fn AMotionEvent_getEventTime(event: Pointer) -> i64;
        fn AMotionEvent_getXOffset(event: Pointer) -> f32;
        fn AMotionEvent_getYOffset(event: Pointer) -> f32;
        fn AMotionEvent_getXPrecision(event: Pointer) -> f32;
        fn AMotionEvent_getYPrecision(event: Pointer) -> f32;
        fn AMotionEvent_getPointerCount(event: Pointer) -> usize;
        fn AMotionEvent_getPointerId(event: Pointer, index: usize) -> i32;
        fn AMotionEvent_getToolType(event: Pointer, index: usize) -> i32;
        fn AMotionEvent_getRawX(event: Pointer, index: usize) -> f32;
        fn AMotionEvent_getRawY(event: Pointer, index: usize) -> f32;
        fn AMotionEvent_getX(event: Pointer, index: usize) -> f32;
        fn AMotionEvent_getY(event: Pointer, index: usize) -> f32;
        fn AMotionEvent_getPressure(event: Pointer, index: usize) -> f32;
        fn AMotionEvent_getSize(event: Pointer, index: usize) -> f32;
        fn AMotionEvent_getTouchMajor(event: Pointer, index: usize) -> f32;
        fn AMotionEvent_getTouchMinor(event: Pointer, index: usize) -> f32;
        fn AMotionEvent_getOrientation(event: Pointer, index: usize) -> f32;
        fn AMotionEvent_getAxisValue(event: Pointer, axis: i32, index: usize) -> f32;
        fn AMotionEvent_getHistorySize(event: Pointer) -> usize;
    }

    /// A guest looper callback (int callback(int fd, int events, void* data)) and its data.
    struct GuestCallback {
        function: u32,
        data: u32,
    }

    /// Looper callbacks registered by (looper or queue handle, fd); kept until replaced.
    static LOOPER_CALLBACKS: Mutex<Option<HashMap<(u32, i32), Box<GuestCallback>>>> = Mutex::new(None);

    /// The host looper calls this on the polling thread (inside ALooper_poll*, which
    /// runs without the guest lock), and it runs the guest's callback.
    extern "C" fn looper_callback(fd: i32, events: i32, data: Pointer) -> i32 {
        let callback = unsafe { &*(data as *const GuestCallback) };
        let _lock = hold_gil();
        let uc = engine();
        match call_guest(uc, callback.function, &[fd as u32, events as u32, callback.data]) {
            Ok(value) => value as i32,
            Err(error) => {
                crate::log::line(&format!("[bridge] looper callback failed: {error}"));
                0
            }
        }
    }

    /// Host callback and data for a guest callback and data: the guest's data
    /// pointer travels as is when there is no callback (it comes back from ALooper_poll*).
    pub fn looper_callback_for(owner: u32, fd: i32, function: u32, data: u32) -> (Pointer, Pointer) {
        if function == 0 {
            return (std::ptr::null_mut(), data as usize as Pointer);
        }
        let callback = Box::new(GuestCallback { function, data });
        let pointer = &*callback as *const GuestCallback as Pointer;
        LOOPER_CALLBACKS.lock().unwrap().get_or_insert_with(HashMap::new).insert((owner, fd), callback);
        (looper_callback as Pointer, pointer)
    }

    fn event(handle: u32) -> Pointer {
        egl::object_for(handle)
    }

    pub fn handler(name: &str) -> Option<Handler> {
        macro_rules! event_int {
            ($function:ident) => {
                handler!(|uc, a| Ret::U32(unsafe { $function(event(a.u32())) } as u32))
            };
        }
        macro_rules! event_long {
            ($function:ident) => {
                handler!(|uc, a| Ret::U64(unsafe { $function(event(a.u32())) } as u64))
            };
        }
        macro_rules! event_float {
            ($function:ident) => {
                handler!(|uc, a| Ret::F32(unsafe { $function(event(a.u32())) }))
            };
        }
        macro_rules! pointer_int {
            ($function:ident) => {
                handler!(|uc, a| {
                    let (event_handle, index) = (a.u32(), a.u32());
                    Ret::U32(unsafe { $function(event(event_handle), index as usize) } as u32)
                })
            };
        }
        macro_rules! pointer_float {
            ($function:ident) => {
                handler!(|uc, a| {
                    let (event_handle, index) = (a.u32(), a.u32());
                    Ret::F32(unsafe { $function(event(event_handle), index as usize) })
                })
            };
        }
        match name {
            "ANativeActivity_finish" => handler!(|uc, a| {
                let activity = host_activity(a.u32());
                if !activity.is_null() {
                    unsafe { ANativeActivity_finish(activity.cast()) };
                }
                Ret::Void
            }),
            "ANativeActivity_setWindowFlags" => handler!(|uc, a| {
                let (activity, add, remove) = (host_activity(a.u32()), a.u32(), a.u32());
                if !activity.is_null() {
                    unsafe { ANativeActivity_setWindowFlags(activity.cast(), add, remove) };
                }
                Ret::Void
            }),
            "ANativeActivity_showSoftInput" => handler!(|uc, a| {
                let (activity, flags) = (host_activity(a.u32()), a.u32());
                if !activity.is_null() {
                    unsafe { ANativeActivity_showSoftInput(activity.cast(), flags) };
                }
                Ret::Void
            }),
            "ANativeActivity_hideSoftInput" => handler!(|uc, a| {
                let (activity, flags) = (host_activity(a.u32()), a.u32());
                if !activity.is_null() {
                    unsafe { ANativeActivity_hideSoftInput(activity.cast(), flags) };
                }
                Ret::Void
            }),
            "AInputQueue_attachLooper" => handler!(|uc, a| {
                let (queue, looper, ident, callback, data) = (a.u32(), a.u32(), a.i32(), a.u32(), a.u32());
                let (host_callback, host_data) = looper_callback_for(queue, -1, callback, data);
                unsafe { AInputQueue_attachLooper(egl::object_for(queue), egl::object_for(looper), ident, host_callback, host_data) };
                Ret::Void
            }),
            "AInputQueue_detachLooper" => handler!(|uc, a| {
                unsafe { AInputQueue_detachLooper(egl::object_for(a.u32())) };
                Ret::Void
            }),
            "AInputQueue_hasEvents" => handler!(|uc, a| Ret::U32(unsafe { AInputQueue_hasEvents(egl::object_for(a.u32())) } as u32)),
            "AInputQueue_getEvent" => handler!(|uc, a| {
                let (queue, out) = (a.u32(), a.u32());
                let mut host_event: Pointer = std::ptr::null_mut();
                let result = unsafe { AInputQueue_getEvent(egl::object_for(queue), &mut host_event) };
                if result >= 0 && out != 0 {
                    mem().write_u32(out, egl::handle_for(host_event));
                }
                Ret::U32(result as u32)
            }),
            "AInputQueue_preDispatchEvent" => handler!(|uc, a| {
                let (queue, event_handle) = (a.u32(), a.u32());
                Ret::U32(unsafe { AInputQueue_preDispatchEvent(egl::object_for(queue), event(event_handle)) } as u32)
            }),
            // The event goes back to Android's pool; its handle goes with it.
            "AInputQueue_finishEvent" => handler!(|uc, a| {
                let (queue, event_handle, handled) = (a.u32(), a.u32(), a.i32());
                unsafe { AInputQueue_finishEvent(egl::object_for(queue), event(event_handle), handled) };
                egl::forget_handle(event_handle);
                Ret::Void
            }),
            "AInputEvent_getType" => event_int!(AInputEvent_getType),
            "AInputEvent_getDeviceId" => event_int!(AInputEvent_getDeviceId),
            "AInputEvent_getSource" => event_int!(AInputEvent_getSource),
            "AKeyEvent_getAction" => event_int!(AKeyEvent_getAction),
            "AKeyEvent_getFlags" => event_int!(AKeyEvent_getFlags),
            "AKeyEvent_getKeyCode" => event_int!(AKeyEvent_getKeyCode),
            "AKeyEvent_getScanCode" => event_int!(AKeyEvent_getScanCode),
            "AKeyEvent_getMetaState" => event_int!(AKeyEvent_getMetaState),
            "AKeyEvent_getRepeatCount" => event_int!(AKeyEvent_getRepeatCount),
            "AKeyEvent_getDownTime" => event_long!(AKeyEvent_getDownTime),
            "AKeyEvent_getEventTime" => event_long!(AKeyEvent_getEventTime),
            "AMotionEvent_getAction" => event_int!(AMotionEvent_getAction),
            "AMotionEvent_getFlags" => event_int!(AMotionEvent_getFlags),
            "AMotionEvent_getMetaState" => event_int!(AMotionEvent_getMetaState),
            "AMotionEvent_getButtonState" => event_int!(AMotionEvent_getButtonState),
            "AMotionEvent_getEdgeFlags" => event_int!(AMotionEvent_getEdgeFlags),
            "AMotionEvent_getDownTime" => event_long!(AMotionEvent_getDownTime),
            "AMotionEvent_getEventTime" => event_long!(AMotionEvent_getEventTime),
            "AMotionEvent_getXOffset" => event_float!(AMotionEvent_getXOffset),
            "AMotionEvent_getYOffset" => event_float!(AMotionEvent_getYOffset),
            "AMotionEvent_getXPrecision" => event_float!(AMotionEvent_getXPrecision),
            "AMotionEvent_getYPrecision" => event_float!(AMotionEvent_getYPrecision),
            "AMotionEvent_getPointerCount" => event_int!(AMotionEvent_getPointerCount),
            "AMotionEvent_getHistorySize" => event_int!(AMotionEvent_getHistorySize),
            "AMotionEvent_getPointerId" => pointer_int!(AMotionEvent_getPointerId),
            "AMotionEvent_getToolType" => pointer_int!(AMotionEvent_getToolType),
            "AMotionEvent_getRawX" => pointer_float!(AMotionEvent_getRawX),
            "AMotionEvent_getRawY" => pointer_float!(AMotionEvent_getRawY),
            "AMotionEvent_getX" => pointer_float!(AMotionEvent_getX),
            "AMotionEvent_getY" => pointer_float!(AMotionEvent_getY),
            "AMotionEvent_getPressure" => pointer_float!(AMotionEvent_getPressure),
            "AMotionEvent_getSize" => pointer_float!(AMotionEvent_getSize),
            "AMotionEvent_getTouchMajor" => pointer_float!(AMotionEvent_getTouchMajor),
            "AMotionEvent_getTouchMinor" => pointer_float!(AMotionEvent_getTouchMinor),
            "AMotionEvent_getOrientation" => pointer_float!(AMotionEvent_getOrientation),
            "AMotionEvent_getAxisValue" => handler!(|uc, a| {
                let (event_handle, axis, index) = (a.u32(), a.i32(), a.u32());
                Ret::F32(unsafe { AMotionEvent_getAxisValue(event(event_handle), axis, index as usize) })
            }),
            _ => None,
        }
    }
}

#[cfg(target_os = "android")]
pub use host::{handler, looper_callback_for};

#[cfg(not(target_os = "android"))]
pub fn handler(_name: &str) -> Option<Handler> {
    None
}
