//! `dev.braxten.gamebridge.Offline.nativeAnswer` (offline/java): one request from the game's
//! HTTP client in, the whole HTTP answer out, or null when the host has none.

use super::{Offline, Pack, Request, PACK_FILE};
use jni_sys::{jbyteArray, jclass, jint, jobjectArray, jsize, jstring, JNIEnv};
use std::path::PathBuf;
use std::sync::OnceLock;

macro_rules! jcall {
    ($env:expr, $function:ident $(, $arg:expr)*) => {
        unsafe { ((**$env).$function.unwrap())($env $(, $arg)*) }
    };
}

fn text(env: *mut JNIEnv, string: jstring) -> String {
    if string.is_null() {
        return String::new();
    }
    let chars = jcall!(env, GetStringUTFChars, string, std::ptr::null_mut());
    if chars.is_null() {
        // Out of memory: Java's OutOfMemoryError is pending and ends the request.
        return String::new();
    }
    let out = unsafe { std::ffi::CStr::from_ptr(chars) }.to_string_lossy().into_owned();
    jcall!(env, ReleaseStringUTFChars, string, chars);
    out
}

fn bytes(env: *mut JNIEnv, array: jbyteArray) -> Vec<u8> {
    if array.is_null() {
        return Vec::new();
    }
    let length = jcall!(env, GetArrayLength, array) as usize;
    let mut out = vec![0u8; length];
    jcall!(env, GetByteArrayRegion, array, 0, length as jsize, out.as_mut_ptr() as *mut i8);
    out
}

/// The app's own files folder (/data/user/0/<package>/files), from the process name.
fn files_dir() -> PathBuf {
    let name = std::fs::read("/proc/self/cmdline").unwrap_or_default();
    let name = String::from_utf8_lossy(name.split(|&b| b == 0).next().unwrap_or_default()).into_owned();
    let package = name.split(':').next().unwrap_or_default().to_string();
    PathBuf::from(format!("/data/user/0/{package}/files"))
}

/// The game's offline answers, loaded at the first request (None without a pack).
fn offline() -> Option<&'static Offline> {
    static OFFLINE: OnceLock<Option<Offline>> = OnceLock::new();
    OFFLINE
        .get_or_init(|| {
            let path = crate::app::library_directory().join(PACK_FILE);
            let loaded = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display())).and_then(Pack::parse).and_then(|pack| {
                let state = files_dir().join("gamebridge-offline");
                let _ = std::fs::create_dir_all(&state);
                Offline::new(pack, state)
            });
            match loaded {
                Ok(offline) => {
                    crate::log::line(&format!("[offline] {} hosts, {} files", offline.hosts(), offline.files()));
                    Some(offline)
                }
                Err(error) => {
                    crate::log::line(&format!("[offline] no answers: {error}"));
                    None
                }
            }
        })
        .as_ref()
}

#[no_mangle]
pub extern "C" fn Java_dev_braxten_gamebridge_Offline_nativeAnswer(
    env: *mut JNIEnv,
    _class: jclass,
    scheme: jstring,
    host: jstring,
    port: jint,
    method: jstring,
    uri: jstring,
    headers: jobjectArray,
    body: jbyteArray,
) -> jbyteArray {
    let mut request = Request { scheme: text(env, scheme), host: text(env, host), port, method: text(env, method), uri: text(env, uri), body: bytes(env, body), ..Default::default() };
    if !headers.is_null() {
        for i in 0..jcall!(env, GetArrayLength, headers) {
            let line = jcall!(env, GetObjectArrayElement, headers, i);
            let line_text = text(env, line as jstring);
            jcall!(env, DeleteLocalRef, line);
            if let Some((name, value)) = line_text.split_once(':') {
                request.headers.push((name.trim().to_string(), value.trim().to_string()));
            }
        }
    }
    if jcall!(env, ExceptionCheck) != 0 {
        return std::ptr::null_mut();
    }
    let Some(answer) = offline().and_then(|offline| offline.answer(&request)) else {
        crate::log::line(&format!("[offline] no answer: {} {}{}", request.method, request.host, request.uri));
        return std::ptr::null_mut();
    };
    if answer.status != 200 {
        crate::log::line(&format!("[offline] {} {} {}{}", answer.status, request.method, request.host, request.uri));
    }
    let out = answer.to_bytes();
    let array = jcall!(env, NewByteArray, out.len() as jsize);
    if array.is_null() {
        return array;
    }
    jcall!(env, SetByteArrayRegion, array, 0, out.len() as jsize, out.as_ptr() as *const i8);
    array
}
