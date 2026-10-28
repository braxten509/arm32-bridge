//! Text from the phone app's keyboard (Java strings) and its way in,
//! nativeType. Built for the phone, and for PC tests with a stand-in JNI
//! environment.

use jni_sys::*;
#[cfg(test)]
use std::ffi::c_char;

/// A Java string's UTF-16 units, as Java holds them (GetStringUTFChars'
/// modified UTF-8 would split an emoji into two halves).
pub unsafe fn java_units(env: *mut JNIEnv, value: jstring) -> Vec<u16> {
    let functions = &**env;
    let length = (functions.GetStringLength.unwrap())(env, value).max(0) as usize;
    let chars = (functions.GetStringChars.unwrap())(env, value, std::ptr::null_mut());
    if chars.is_null() {
        return Vec::new();
    }
    let units = std::slice::from_raw_parts(chars, length).to_vec();
    (functions.ReleaseStringChars.unwrap())(env, value, chars);
    units
}

/// nativeType(text): letters typed on the phone's keyboard, "\b" for
/// delete and "\n" for return (fw/typing.rs).
#[no_mangle]
pub unsafe extern "C" fn Java_dev_braxten_gamebridge_IosGameActivity_nativeType(env: *mut JNIEnv, _this: jobject, typed: jstring) {
    let units = java_units(env, typed);
    crate::fw::runloop::post(crate::fw::runloop::Work::Native(Box::new(move || crate::fw::typing::type_java_text(&units))));
}

/// For tests: `text` typed through nativeType with a stand-in JNI table,
/// then the posted typing run on the main loop. Whether every string the
/// table handed out was given back.
#[cfg(test)]
pub fn type_through_jni(text: &str) -> bool {
    use std::sync::Mutex;
    static HANDED: Mutex<Vec<usize>> = Mutex::new(Vec::new());
    unsafe extern "system" fn length(_: *mut JNIEnv, value: jstring) -> jsize {
        (*(value as *const Vec<u16>)).len() as jsize
    }
    unsafe extern "system" fn chars(_: *mut JNIEnv, value: jstring, _: *mut jboolean) -> *const jchar {
        let units = (*(value as *const Vec<u16>)).clone().into_boxed_slice();
        let at = Box::leak(units).as_ptr();
        HANDED.lock().unwrap().push(at as usize);
        at
    }
    unsafe extern "system" fn release(_: *mut JNIEnv, _: jstring, at: *const jchar) {
        HANDED.lock().unwrap().retain(|&a| a != at as usize);
    }
    // Java's modified UTF-8: each UTF-16 unit on its own (an emoji's two halves apart).
    unsafe extern "system" fn utf_chars(_: *mut JNIEnv, value: jstring, _: *mut jboolean) -> *const c_char {
        let mut bytes = Vec::new();
        for &unit in &*(value as *const Vec<u16>) {
            let c = unit as u32;
            match c {
                1..=0x7F => bytes.push(c as u8),
                0..=0x7FF => bytes.extend([0xC0 | (c >> 6) as u8, 0x80 | (c & 0x3F) as u8]),
                _ => bytes.extend([0xE0 | (c >> 12) as u8, 0x80 | ((c >> 6) & 0x3F) as u8, 0x80 | (c & 0x3F) as u8]),
            }
        }
        bytes.push(0);
        let at = Box::leak(bytes.into_boxed_slice()).as_ptr() as *const c_char;
        HANDED.lock().unwrap().push(at as usize);
        at
    }
    unsafe extern "system" fn utf_release(_: *mut JNIEnv, _: jstring, at: *const c_char) {
        HANDED.lock().unwrap().retain(|&a| a != at as usize);
    }
    unsafe {
        let mut table: JNINativeInterface_ = std::mem::zeroed();
        table.GetStringLength = Some(length);
        table.GetStringChars = Some(chars);
        table.ReleaseStringChars = Some(release);
        table.GetStringUTFChars = Some(utf_chars);
        table.ReleaseStringUTFChars = Some(utf_release);
        let mut interface: *const JNINativeInterface_ = &table;
        let value: Vec<u16> = text.encode_utf16().collect();
        HANDED.lock().unwrap().clear();
        Java_dev_braxten_gamebridge_IosGameActivity_nativeType(&mut interface, std::ptr::null_mut(), &value as *const Vec<u16> as jstring);
    }
    crate::fw::runloop::run_main_loop_briefly();
    HANDED.lock().unwrap().is_empty()
}
