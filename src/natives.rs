//! Connects a game's Java `native` methods to its 32-bit library at start-up,
//! so one prebuilt bridge works for every game.
//!
//! For each `Java_*` function the 32-bit library exports, the Java class and
//! method are worked out from the name, the method's exact types are read
//! through reflection, and Android is told (RegisterNatives) to call a small
//! generated stub for it. The game's own RegisterNatives calls go through the
//! same path. Every stub loads its method number into x16 and jumps to one
//! shared entry, which saves the argument registers and calls `dispatch`;
//! `dispatch` decodes them by the method's types and forwards the call into
//! the 32-bit code (`app::forward`).

use std::ffi::{c_void, CString};
use std::sync::atomic::AtomicU32;
use std::sync::{Mutex, OnceLock};

use jni_sys::*;

use crate::app::{forward, HostArg};
use crate::jni_names::{demangle, kinds, mangle};

/// A Java native method connected to 32-bit code.
struct Native {
    /// The `Java_*` name (also used to spot engine hooks such as `_nativeRender`).
    symbol: String,
    /// Argument kinds after env and this (Z B C S I J F D L), and the return kind.
    params: Vec<u8>,
    returns: u8,
    /// Guest function address.
    address: AtomicU32,
}

static NATIVES: Mutex<Vec<&'static Native>> = Mutex::new(Vec::new());

/// (class with `/`, method, signature) of every method connected to 32-bit code.
static CONNECTED: Mutex<Vec<(String, String, String)>> = Mutex::new(Vec::new());

/// Asked by the stand-in helper libraries (tools/standin) so they don't
/// replace a method the bridge connected with their do-nothing version.
#[no_mangle]
pub extern "C" fn arm32_bridge_connected(class: *const libc::c_char, method: *const libc::c_char, signature: *const libc::c_char) -> i32 {
    let text = |pointer: *const libc::c_char| unsafe { std::ffi::CStr::from_ptr(pointer) }.to_string_lossy().into_owned();
    let wanted = (text(class), text(method), text(signature));
    CONNECTED.lock().unwrap().contains(&wanted) as i32
}

/// Bytes per generated stub.
const STUB_SIZE: usize = 32;
const STUB_MEMORY: usize = 64 << 10;

struct Stubs {
    base: *mut u8,
    used: usize,
}
unsafe impl Send for Stubs {}

static STUBS: Mutex<Option<Stubs>> = Mutex::new(None);

extern "C" {
    fn __clear_cache(begin: *mut libc::c_char, end: *mut libc::c_char);
    fn arm32_bridge_native_entry();
}

// x16 = method number. Saves x0-x7 and d0-d7, passes them and the caller's
// stack arguments to `dispatch`, and returns its result in both x0 and d0
// (the caller reads whichever its return type uses).
std::arch::global_asm!(
    ".global arm32_bridge_native_entry",
    ".p2align 2",
    "arm32_bridge_native_entry:",
    "stp x29, x30, [sp, #-16]!",
    "mov x29, sp",
    "sub sp, sp, #128",
    "stp x0, x1, [sp, #0]",
    "stp x2, x3, [sp, #16]",
    "stp x4, x5, [sp, #32]",
    "stp x6, x7, [sp, #48]",
    "stp d0, d1, [sp, #64]",
    "stp d2, d3, [sp, #80]",
    "stp d4, d5, [sp, #96]",
    "stp d6, d7, [sp, #112]",
    "mov x0, x16",
    "mov x1, sp",
    "add x2, x29, #16",
    "bl {dispatch}",
    "fmov d0, x0",
    "mov sp, x29",
    "ldp x29, x30, [sp], #16",
    "ret",
    dispatch = sym dispatch,
);

/// A stub that calls the shared entry with x16 = `index`.
fn make_stub(index: usize) -> *mut c_void {
    let mut stubs = STUBS.lock().unwrap();
    let stubs = stubs.get_or_insert_with(|| {
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                STUB_MEMORY,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert!(base != libc::MAP_FAILED, "could not map memory for native method stubs");
        Stubs { base: base as *mut u8, used: 0 }
    });
    assert!(stubs.used + STUB_SIZE <= STUB_MEMORY && index < 1 << 16, "too many native methods");
    let entry = arm32_bridge_native_entry as *const () as u64;
    let words: [u32; 8] = [
        0xD280_0010 | ((index as u32) << 5), // movz x16, #index
        0x5800_0071,                         // ldr x17, [pc, #12] (the address below)
        0xD61F_0220,                         // br x17
        0xD503_201F,                         // nop (aligns the address)
        entry as u32,
        (entry >> 32) as u32,
        0,
        0,
    ];
    let at = unsafe { stubs.base.add(stubs.used) };
    unsafe {
        std::ptr::copy_nonoverlapping(words.as_ptr() as *const u8, at, STUB_SIZE);
        __clear_cache(at as *mut libc::c_char, at.add(STUB_SIZE) as *mut libc::c_char);
    }
    stubs.used += STUB_SIZE;
    at as *mut c_void
}

/// Called by the shared entry: `registers` = x0-x7 then d0-d7, `stack` = the
/// caller's stack arguments (8-byte slots).
extern "C" fn dispatch(index: u64, registers: *const u64, stack: *const u64) -> u64 {
    let native = NATIVES.lock().unwrap()[index as usize];
    let registers = unsafe { std::slice::from_raw_parts(registers, 16) };
    let env = registers[0] as *mut JNIEnv;
    let this = registers[1] as jobject;
    let (mut next_x, mut next_d, mut next_stack) = (2, 0, 0);
    let mut take = |float: bool| -> u64 {
        if float && next_d < 8 {
            next_d += 1;
            registers[8 + next_d - 1]
        } else if !float && next_x < 8 {
            next_x += 1;
            registers[next_x - 1]
        } else {
            next_stack += 1;
            unsafe { *stack.add(next_stack - 1) }
        }
    };
    let mut args = Vec::with_capacity(native.params.len() + 1);
    args.push(HostArg::Object(this));
    for &kind in &native.params {
        args.push(match kind {
            b'Z' => HostArg::Int(take(false) as u8 as i32),
            b'B' => HostArg::Int(take(false) as i8 as i32),
            b'C' => HostArg::Int(take(false) as u16 as i32),
            b'S' => HostArg::Int(take(false) as i16 as i32),
            b'I' => HostArg::Int(take(false) as i32),
            b'J' => HostArg::Long(take(false) as i64),
            b'F' => HostArg::Float(f32::from_bits(take(true) as u32)),
            b'D' => HostArg::Double(f64::from_bits(take(true))),
            _ => HostArg::Object(take(false) as usize as jobject),
        });
    }
    forward(env, &native.symbol, &native.address, &args, native.returns).bits()
}

macro_rules! jcall {
    ($env:expr, $function:ident $(, $arg:expr)*) => {
        unsafe { ((**$env).$function.unwrap())($env $(, $arg)*) }
    };
}

fn clear_exception(env: *mut JNIEnv) -> bool {
    if jcall!(env, ExceptionCheck) != 0 {
        jcall!(env, ExceptionClear);
        true
    } else {
        false
    }
}

fn java_string(env: *mut JNIEnv, string: jobject) -> String {
    if string.is_null() {
        return String::new();
    }
    let chars = jcall!(env, GetStringUTFChars, string as jstring, std::ptr::null_mut());
    let text = unsafe { std::ffi::CStr::from_ptr(chars) }.to_string_lossy().into_owned();
    jcall!(env, ReleaseStringUTFChars, string as jstring, chars);
    text
}

/// Reflection methods used to read native method signatures.
struct Reflection {
    class_get_name: jmethodID,
    get_declared_methods: jmethodID,
    method_get_name: jmethodID,
    get_modifiers: jmethodID,
    get_parameter_types: jmethodID,
    get_return_type: jmethodID,
}

fn reflection(env: *mut JNIEnv) -> Option<&'static Reflection> {
    struct Shared(Reflection);
    unsafe impl Send for Shared {}
    unsafe impl Sync for Shared {}
    static REFLECTION: OnceLock<Option<Shared>> = OnceLock::new();
    REFLECTION
        .get_or_init(|| {
            let class = jcall!(env, FindClass, c"java/lang/Class".as_ptr());
            let method = jcall!(env, FindClass, c"java/lang/reflect/Method".as_ptr());
            if class.is_null() || method.is_null() {
                clear_exception(env);
                return None;
            }
            let id = |owner: jclass, name: &std::ffi::CStr, signature: &std::ffi::CStr| {
                jcall!(env, GetMethodID, owner, name.as_ptr(), signature.as_ptr())
            };
            let reflection = Reflection {
                class_get_name: id(class, c"getName", c"()Ljava/lang/String;"),
                get_declared_methods: id(class, c"getDeclaredMethods", c"()[Ljava/lang/reflect/Method;"),
                method_get_name: id(method, c"getName", c"()Ljava/lang/String;"),
                get_modifiers: id(method, c"getModifiers", c"()I"),
                get_parameter_types: id(method, c"getParameterTypes", c"()[Ljava/lang/Class;"),
                get_return_type: id(method, c"getReturnType", c"()Ljava/lang/Class;"),
            };
            if clear_exception(env) {
                return None;
            }
            Some(Shared(reflection))
        })
        .as_ref()
        .map(|shared| &shared.0)
}

/// The JNI type descriptor of a `java.lang.Class`.
fn descriptor(env: *mut JNIEnv, reflection: &Reflection, class: jobject) -> String {
    let name = java_string(env, jcall!(env, CallObjectMethodA, class, reflection.class_get_name, std::ptr::null()));
    match name.as_str() {
        "boolean" => "Z".into(),
        "byte" => "B".into(),
        "char" => "C".into(),
        "short" => "S".into(),
        "int" => "I".into(),
        "long" => "J".into(),
        "float" => "F".into(),
        "double" => "D".into(),
        "void" => "V".into(),
        name if name.starts_with('[') => name.replace('.', "/"),
        name => format!("L{};", name.replace('.', "/")),
    }
}

/// Signatures of `class`'s native methods called `name`.
fn native_signatures(env: *mut JNIEnv, class: jclass, name: &str) -> Vec<String> {
    const NATIVE: jint = 0x100;
    let Some(reflection) = reflection(env) else { return Vec::new() };
    let methods = jcall!(env, CallObjectMethodA, class, reflection.get_declared_methods, std::ptr::null());
    if clear_exception(env) || methods.is_null() {
        return Vec::new();
    }
    let mut signatures = Vec::new();
    for i in 0..jcall!(env, GetArrayLength, methods as jarray) {
        let method = jcall!(env, GetObjectArrayElement, methods as jobjectArray, i);
        let modifiers = jcall!(env, CallIntMethodA, method, reflection.get_modifiers, std::ptr::null());
        let method_name = java_string(env, jcall!(env, CallObjectMethodA, method, reflection.method_get_name, std::ptr::null()));
        if modifiers & NATIVE != 0 && method_name == name {
            let types = jcall!(env, CallObjectMethodA, method, reflection.get_parameter_types, std::ptr::null());
            let mut signature = String::from("(");
            for j in 0..jcall!(env, GetArrayLength, types as jarray) {
                let parameter = jcall!(env, GetObjectArrayElement, types as jobjectArray, j);
                signature.push_str(&descriptor(env, reflection, parameter));
                jcall!(env, DeleteLocalRef, parameter);
            }
            signature.push(')');
            let returned = jcall!(env, CallObjectMethodA, method, reflection.get_return_type, std::ptr::null());
            signature.push_str(&descriptor(env, reflection, returned));
            jcall!(env, DeleteLocalRef, returned);
            jcall!(env, DeleteLocalRef, types);
            signatures.push(signature);
        }
        jcall!(env, DeleteLocalRef, method);
    }
    jcall!(env, DeleteLocalRef, methods);
    clear_exception(env);
    signatures
}

/// Tells Android to send calls of `class.name` (with `signature`) to the guest
/// function at `address`. Returns false if Android refused.
fn register(env: *mut JNIEnv, class: jclass, class_name: &str, symbol: String, name: &str, signature: &str, address: u32) -> bool {
    let Some((params, returns)) = kinds(signature) else { return false };
    let native: &'static Native = Box::leak(Box::new(Native { symbol, params, returns, address: AtomicU32::new(address) }));
    let index = {
        let mut natives = NATIVES.lock().unwrap();
        natives.push(native);
        natives.len() - 1
    };
    let (Ok(name), Ok(signature)) = (CString::new(name), CString::new(signature)) else { return false };
    let method = JNINativeMethod { name: name.as_ptr() as *mut _, signature: signature.as_ptr() as *mut _, fnPtr: make_stub(index) };
    let result = jcall!(env, RegisterNatives, class, &method, 1);
    let ok = !clear_exception(env) && result == 0;
    if ok {
        CONNECTED.lock().unwrap().push((class_name.into(), name.to_string_lossy().into_owned(), signature.to_string_lossy().into_owned()));
    }
    ok
}

/// Connects every `Java_*` export of the 32-bit library to its Java method.
/// Must run while Java is loading this library (JNI_OnLoad), so FindClass
/// sees the app's classes.
pub fn register_exports(env: *mut JNIEnv, exports: &[(String, u32)]) {
    let (mut connected, mut skipped) = (0, Vec::new());
    for (symbol, address) in exports {
        let Some((class_name, method, arguments)) = demangle(symbol) else { continue };
        let Ok(class_c) = CString::new(class_name.clone()) else { continue };
        jcall!(env, PushLocalFrame, 16);
        let class = jcall!(env, FindClass, class_c.as_ptr());
        let mut done = false;
        if !clear_exception(env) && !class.is_null() {
            let signatures = native_signatures(env, class, &method);
            let signature = match &arguments {
                Some(arguments) => signatures.iter().find(|s| s[1..s.find(')').unwrap_or(1)] == *arguments),
                None => signatures.first(),
            };
            if let Some(signature) = signature {
                done = register(env, class, &class_name, symbol.clone(), &method, signature, *address);
            }
        }
        jcall!(env, PopLocalFrame, std::ptr::null_mut());
        if done {
            connected += 1;
        } else {
            skipped.push(symbol.as_str());
        }
    }
    crate::log::line(&format!(
        "[bridge] connected {connected} native methods; {} exports without a matching Java method",
        skipped.len()
    ));
    for symbol in skipped.iter().take(20) {
        crate::log::line(&format!("[bridge]   not connected: {symbol}"));
    }
}

/// The game's own RegisterNatives: (name, signature, guest function) for `class`.
pub fn register_from_guest(env: *mut JNIEnv, class: jclass, methods: &[(String, String, u32)]) -> bool {
    let reflection = reflection(env);
    let class_name = match reflection {
        Some(reflection) => java_string(env, jcall!(env, CallObjectMethodA, class, reflection.class_get_name, std::ptr::null())),
        None => String::new(),
    }
    .replace('.', "/");
    let mut all = true;
    for (name, signature, address) in methods {
        let symbol = format!("Java_{}_{}", mangle(&class_name), mangle(name));
        all &= register(env, class, &class_name, symbol, name, signature, *address);
    }
    all
}
