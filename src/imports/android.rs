//! libandroid's asset functions (reading files packed inside the APK).
//! Host AAssetManager / AAsset / AAssetDir pointers are 64-bit, so the guest
//! gets small handles; data is read straight into guest memory.

/// Functions handled here. Only the phone build has them; the PC tool's
/// `imports` report counts them as handled.
pub const NAMES: &[&str] = &[
    "AAssetManager_fromJava",
    "AAssetManager_open",
    "AAssetManager_openDir",
    "AAssetDir_getNextFileName",
    "AAssetDir_close",
    "AAsset_read",
    "AAsset_seek",
    "AAsset_getLength",
    "AAsset_getLength64",
    "AAsset_getRemainingLength",
    "AAsset_getBuffer",
    "AAsset_openFileDescriptor",
    "AAsset_close",
    "AConfiguration_new",
    "AConfiguration_delete",
    "AConfiguration_fromAssetManager",
    "AConfiguration_getOrientation",
    "AConfiguration_getLanguage",
    "AConfiguration_getCountry",
    "AConfiguration_getSdkVersion",
    "AConfiguration_getDensity",
    "AConfiguration_getKeyboard",
    "AConfiguration_getNavigation",
    "AConfiguration_getTouchscreen",
    "AConfiguration_getScreenSize",
    "AConfiguration_getScreenLong",
    "AConfiguration_getScreenWidthDp",
    "AConfiguration_getScreenHeightDp",
    "AConfiguration_getSmallestScreenWidthDp",
    "AConfiguration_getUiModeType",
    "AConfiguration_getUiModeNight",
    "AConfiguration_getMcc",
    "AConfiguration_getMnc",
];

#[cfg(not(target_os = "android"))]
use crate::runtime::Handler;

#[cfg(target_os = "android")]
mod host {
    use std::collections::HashMap;
    use std::ffi::c_void;

    use crate::handler;
    use crate::imports::jni_host;
    use crate::runtime::{mem, shared, ArgSource, Handler, Ret};

    #[link(name = "android")]
    extern "C" {
        fn AAssetManager_fromJava(env: *mut jni_sys::JNIEnv, manager: jni_sys::jobject) -> *mut c_void;
        fn AAssetManager_open(manager: *mut c_void, name: *const libc::c_char, mode: i32) -> *mut c_void;
        fn AAssetManager_openDir(manager: *mut c_void, name: *const libc::c_char) -> *mut c_void;
        fn AAssetDir_getNextFileName(dir: *mut c_void) -> *const libc::c_char;
        fn AAssetDir_close(dir: *mut c_void);
        fn AAsset_read(asset: *mut c_void, buffer: *mut c_void, count: usize) -> i32;
        fn AAsset_seek(asset: *mut c_void, offset: libc::off_t, whence: i32) -> libc::off_t;
        fn AAsset_getLength(asset: *mut c_void) -> libc::off_t;
        fn AAsset_getLength64(asset: *mut c_void) -> i64;
        fn AAsset_getRemainingLength(asset: *mut c_void) -> libc::off_t;
        fn AAsset_getBuffer(asset: *mut c_void) -> *const c_void;
        fn AAsset_openFileDescriptor(asset: *mut c_void, start: *mut libc::off_t, length: *mut libc::off_t) -> i32;
        fn AAsset_close(asset: *mut c_void);
        fn AConfiguration_new() -> *mut c_void;
        fn AConfiguration_delete(config: *mut c_void);
        fn AConfiguration_fromAssetManager(config: *mut c_void, manager: *mut c_void);
        fn AConfiguration_getOrientation(config: *mut c_void) -> i32;
        fn AConfiguration_getLanguage(config: *mut c_void, out: *mut libc::c_char);
        fn AConfiguration_getCountry(config: *mut c_void, out: *mut libc::c_char);
        fn AConfiguration_getSdkVersion(config: *mut c_void) -> i32;
        fn AConfiguration_getDensity(config: *mut c_void) -> i32;
        fn AConfiguration_getKeyboard(config: *mut c_void) -> i32;
        fn AConfiguration_getNavigation(config: *mut c_void) -> i32;
        fn AConfiguration_getTouchscreen(config: *mut c_void) -> i32;
        fn AConfiguration_getScreenSize(config: *mut c_void) -> i32;
        fn AConfiguration_getScreenLong(config: *mut c_void) -> i32;
        fn AConfiguration_getScreenWidthDp(config: *mut c_void) -> i32;
        fn AConfiguration_getScreenHeightDp(config: *mut c_void) -> i32;
        fn AConfiguration_getSmallestScreenWidthDp(config: *mut c_void) -> i32;
        fn AConfiguration_getUiModeType(config: *mut c_void) -> i32;
        fn AConfiguration_getUiModeNight(config: *mut c_void) -> i32;
        fn AConfiguration_getMcc(config: *mut c_void) -> i32;
        fn AConfiguration_getMnc(config: *mut c_void) -> i32;
    }

    const HANDLE_BASE: u32 = 0xDC00_0000;

    #[derive(Default)]
    pub struct Assets {
        objects: HashMap<u32, usize>,
        next: u32,
        /// Guest copies made by AAsset_getBuffer / AAssetDir_getNextFileName, freed on close.
        copies: HashMap<u32, u32>,
    }

    fn add(object: *mut c_void) -> u32 {
        if object.is_null() {
            return 0;
        }
        let mut shared = shared();
        let assets = &mut shared.state.assets;
        assets.next += 4;
        let handle = HANDLE_BASE + assets.next;
        assets.objects.insert(handle, object as usize);
        handle
    }

    /// The guest handle for a host AAssetManager (NativeActivity hands the game one).
    pub fn asset_manager_handle(manager: *mut c_void) -> u32 {
        add(manager)
    }

    fn get(handle: u32) -> *mut c_void {
        shared().state.assets.objects.get(&handle).copied().unwrap_or(0) as *mut c_void
    }

    fn remove(handle: u32) -> *mut c_void {
        let mut shared = shared();
        let assets = &mut shared.state.assets;
        if let Some(copy) = assets.copies.remove(&handle) {
            crate::runtime::heap().free(copy);
        }
        shared.state.assets.objects.remove(&handle).unwrap_or(0) as *mut c_void
    }

    fn remember_copy(handle: u32, copy: u32) {
        let old = shared().state.assets.copies.insert(handle, copy);
        if let Some(old) = old {
            crate::runtime::heap().free(old);
        }
    }

    pub fn handler(name: &str) -> Option<Handler> {
        /// AConfiguration getters that return a plain int (0 for an unknown handle).
        macro_rules! config_int {
            ($function:ident) => {
                handler!(|uc, a| {
                    let config = get(a.u32());
                    Ret::U32(if config.is_null() { 0 } else { (unsafe { $function(config) }) as u32 })
                })
            };
        }
        /// getLanguage/getCountry write two letters into the caller's buffer.
        macro_rules! config_letters {
            ($function:ident) => {
                handler!(|uc, a| {
                    let (config, out) = (get(a.u32()), a.u32());
                    let mut letters = [0 as libc::c_char; 2];
                    if !config.is_null() {
                        unsafe { $function(config, letters.as_mut_ptr()) };
                    }
                    mem().write_bytes(out, &[letters[0] as u8, letters[1] as u8]);
                    Ret::Void
                })
            };
        }
        match name {
            "AConfiguration_getLanguage" => config_letters!(AConfiguration_getLanguage),
            "AConfiguration_getCountry" => config_letters!(AConfiguration_getCountry),
            "AConfiguration_getSdkVersion" => config_int!(AConfiguration_getSdkVersion),
            "AConfiguration_getDensity" => config_int!(AConfiguration_getDensity),
            "AConfiguration_getKeyboard" => config_int!(AConfiguration_getKeyboard),
            "AConfiguration_getNavigation" => config_int!(AConfiguration_getNavigation),
            "AConfiguration_getTouchscreen" => config_int!(AConfiguration_getTouchscreen),
            "AConfiguration_getScreenSize" => config_int!(AConfiguration_getScreenSize),
            "AConfiguration_getScreenLong" => config_int!(AConfiguration_getScreenLong),
            "AConfiguration_getScreenWidthDp" => config_int!(AConfiguration_getScreenWidthDp),
            "AConfiguration_getScreenHeightDp" => config_int!(AConfiguration_getScreenHeightDp),
            "AConfiguration_getSmallestScreenWidthDp" => config_int!(AConfiguration_getSmallestScreenWidthDp),
            "AConfiguration_getUiModeType" => config_int!(AConfiguration_getUiModeType),
            "AConfiguration_getUiModeNight" => config_int!(AConfiguration_getUiModeNight),
            "AConfiguration_getMcc" => config_int!(AConfiguration_getMcc),
            "AConfiguration_getMnc" => config_int!(AConfiguration_getMnc),
            // AConfiguration objects get handles like the asset ones.
            "AConfiguration_new" => handler!(|uc, a| Ret::U32(add(unsafe { AConfiguration_new() }))),
            "AConfiguration_delete" => handler!(|uc, a| {
                let config = remove(a.u32());
                if !config.is_null() {
                    unsafe { AConfiguration_delete(config) };
                }
                Ret::Void
            }),
            "AConfiguration_fromAssetManager" => handler!(|uc, a| {
                let (config, manager) = (get(a.u32()), get(a.u32()));
                if !config.is_null() && !manager.is_null() {
                    unsafe { AConfiguration_fromAssetManager(config, manager) };
                }
                Ret::Void
            }),
            "AConfiguration_getOrientation" => handler!(|uc, a| {
                let config = get(a.u32());
                Ret::U32(if config.is_null() { 0 } else { (unsafe { AConfiguration_getOrientation(config) }) as u32 })
            }),
            "AAssetManager_fromJava" => handler!(|uc, a| {
                let (_env, manager) = (a.u32(), a.u32());
                let env = jni_host::env();
                Ret::U32(add(unsafe { AAssetManager_fromJava(env, jni_host::to_host(manager)) }))
            }),
            "AAssetManager_open" => handler!(|uc, a| {
                let (manager, name, mode) = (a.u32(), a.u32(), a.i32());
                let name = mem().host(name) as *const libc::c_char;
                Ret::U32(add(unsafe { AAssetManager_open(get(manager), name, mode) }))
            }),
            "AAssetManager_openDir" => handler!(|uc, a| {
                let (manager, name) = (a.u32(), a.u32());
                let name = mem().host(name) as *const libc::c_char;
                Ret::U32(add(unsafe { AAssetManager_openDir(get(manager), name) }))
            }),
            "AAssetDir_getNextFileName" => handler!(|uc, a| {
                let directory = a.u32();
                let name = unsafe { AAssetDir_getNextFileName(get(directory)) };
                if name.is_null() {
                    return Ret::U32(0);
                }
                let bytes = unsafe { std::ffi::CStr::from_ptr(name) }.to_bytes().to_vec();
                let copy = crate::runtime::heap().strdup(&bytes);
                remember_copy(directory, copy);
                Ret::U32(copy)
            }),
            "AAssetDir_close" => handler!(|uc, a| {
                let directory = remove(a.u32());
                if !directory.is_null() {
                    unsafe { AAssetDir_close(directory) };
                }
                Ret::Void
            }),
            "AAsset_read" => handler!(|uc, a| {
                let (asset, buffer, count) = (a.u32(), a.u32(), a.u32());
                Ret::U32(unsafe { AAsset_read(get(asset), mem().host(buffer).cast(), count as usize) } as u32)
            }),
            "AAsset_seek" => handler!(|uc, a| {
                let (asset, offset, whence) = (a.u32(), a.i32(), a.i32());
                Ret::U32(unsafe { AAsset_seek(get(asset), offset as libc::off_t, whence) } as u32)
            }),
            "AAsset_getLength" => handler!(|uc, a| Ret::U32(unsafe { AAsset_getLength(get(a.u32())) } as u32)),
            "AAsset_getLength64" => handler!(|uc, a| Ret::U64(unsafe { AAsset_getLength64(get(a.u32())) } as u64)),
            "AAsset_getRemainingLength" => {
                handler!(|uc, a| Ret::U32(unsafe { AAsset_getRemainingLength(get(a.u32())) } as u32))
            }
            "AAsset_getBuffer" => handler!(|uc, a| {
                let asset = a.u32();
                let length = unsafe { AAsset_getLength(get(asset)) } as u32;
                let data = unsafe { AAsset_getBuffer(get(asset)) } as *const u8;
                if data.is_null() {
                    return Ret::U32(0);
                }
                let copy = crate::runtime::heap().malloc(length.max(1));
                unsafe { std::ptr::copy_nonoverlapping(data, mem().host(copy), length as usize) };
                remember_copy(asset, copy);
                Ret::U32(copy)
            }),
            "AAsset_openFileDescriptor" => handler!(|uc, a| {
                let (asset, start_out, length_out) = (a.u32(), a.u32(), a.u32());
                let (mut start, mut length): (libc::off_t, libc::off_t) = (0, 0);
                let fd = unsafe { AAsset_openFileDescriptor(get(asset), &mut start, &mut length) };
                mem().write_u32(start_out, start as u32);
                mem().write_u32(length_out, length as u32);
                Ret::U32(fd as u32)
            }),
            "AAsset_close" => handler!(|uc, a| {
                let asset = remove(a.u32());
                if !asset.is_null() {
                    unsafe { AAsset_close(asset) };
                }
                Ret::Void
            }),
            _ => None,
        }
    }
}

#[cfg(target_os = "android")]
pub use host::{asset_manager_handle, handler, Assets};

#[cfg(not(target_os = "android"))]
#[derive(Default)]
pub struct Assets;

#[cfg(not(target_os = "android"))]
pub fn handler(_name: &str) -> Option<Handler> {
    None
}
