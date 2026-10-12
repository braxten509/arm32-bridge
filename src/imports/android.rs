//! libandroid's asset functions (reading files packed inside the APK).
//! Host AAssetManager / AAsset / AAssetDir pointers are 64-bit, so the guest
//! gets small handles; data is read straight into guest memory.

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
        fn AAsset_getRemainingLength(asset: *mut c_void) -> libc::off_t;
        fn AAsset_getBuffer(asset: *mut c_void) -> *const c_void;
        fn AAsset_openFileDescriptor(asset: *mut c_void, start: *mut libc::off_t, length: *mut libc::off_t) -> i32;
        fn AAsset_close(asset: *mut c_void);
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

    fn get(handle: u32) -> *mut c_void {
        shared().state.assets.objects.get(&handle).copied().unwrap_or(0) as *mut c_void
    }

    fn remove(handle: u32) -> *mut c_void {
        let mut shared = shared();
        let assets = &mut shared.state.assets;
        if let Some(copy) = assets.copies.remove(&handle) {
            shared.heap.free(copy);
        }
        shared.state.assets.objects.remove(&handle).unwrap_or(0) as *mut c_void
    }

    fn remember_copy(handle: u32, copy: u32) {
        let old = shared().state.assets.copies.insert(handle, copy);
        if let Some(old) = old {
            shared().heap.free(old);
        }
    }

    pub fn handler(name: &str) -> Option<Handler> {
        match name {
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
                let copy = shared().heap.strdup(&bytes);
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
                let copy = shared().heap.malloc(length.max(1));
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
pub use host::{handler, Assets};

#[cfg(not(target_os = "android"))]
#[derive(Default)]
pub struct Assets;

#[cfg(not(target_os = "android"))]
pub fn handler(_name: &str) -> Option<Handler> {
    None
}
