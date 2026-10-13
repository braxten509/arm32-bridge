//! The real Java side: guest JNIEnv calls go to the app's actual (64-bit)
//! JVM. Java references and method/field IDs are 64-bit host pointers, so
//! the guest gets 32-bit handles instead (in an unmapped address range, so
//! a guest that dereferences one faults loudly).
//!
//! Local references made while Java is calling into the game are released
//! when that call returns, like the JVM does with its own local references.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::{c_void, CStr};
use std::sync::atomic::{AtomicPtr, Ordering};

use jni_sys::*;

use crate::runtime::{mem, shared, stop_with_fault, without_gil, ArgSource, Args, Ret, Uc, VaList};

const OBJECT_BASE: u32 = 0xD000_0000;
const MEMBER_BASE: u32 = 0xD800_0000;
const JNI_ABORT_MODE: u32 = 2;
const JNI_COMMIT_MODE: u32 = 1;

static HOST_VM: AtomicPtr<JavaVM> = AtomicPtr::new(std::ptr::null_mut());

thread_local! {
    static HOST_ENV: Cell<*mut JNIEnv> = const { Cell::new(std::ptr::null_mut()) };
    static ATTACHED_BY_US: Cell<bool> = const { Cell::new(false) };
    static LOCAL_FRAMES: RefCell<Vec<Vec<u32>>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone)]
pub struct Member {
    pub host: usize,
    pub name: String,
    pub signature: String,
}

#[derive(Default)]
pub struct Refs {
    objects: Vec<usize>,
    free: Vec<u32>,
    members: Vec<Member>,
    member_index: HashMap<usize, u32>,
    /// Guest copies of strings/arrays handed out by Get*Chars/Get*Elements:
    /// guest pointer → (host pointer, element bytes, length in elements).
    copies: HashMap<u32, (usize, u32, u32)>,
}

pub fn set_vm(vm: *mut JavaVM) {
    HOST_VM.store(vm, Ordering::SeqCst);
}

pub fn active() -> bool {
    !HOST_VM.load(Ordering::SeqCst).is_null()
}

/// The host JNIEnv for this thread, attaching the thread to the JVM if a
/// guest thread is the first to use Java.
pub fn env() -> *mut JNIEnv {
    let current = HOST_ENV.with(|e| e.get());
    if !current.is_null() {
        return current;
    }
    let vm = HOST_VM.load(Ordering::SeqCst);
    let mut env: *mut c_void = std::ptr::null_mut();
    unsafe {
        if ((**vm).GetEnv.unwrap())(vm, &mut env, JNI_VERSION_1_6) != JNI_OK {
            ((**vm).AttachCurrentThread.unwrap())(vm, &mut env, std::ptr::null_mut());
            ATTACHED_BY_US.with(|a| a.set(true));
        }
    }
    HOST_ENV.with(|e| e.set(env as *mut JNIEnv));
    env as *mut JNIEnv
}

pub fn detach_current_thread() {
    if ATTACHED_BY_US.with(|a| a.replace(false)) {
        let vm = HOST_VM.load(Ordering::SeqCst);
        unsafe { ((**vm).DetachCurrentThread.unwrap())(vm) };
        HOST_ENV.with(|e| e.set(std::ptr::null_mut()));
    }
}

/// Marks a call from Java into the game: uses its JNIEnv and frees the
/// guest handles for local references when dropped.
pub struct Entry {
    previous_env: *mut JNIEnv,
}

pub fn enter(env: *mut JNIEnv) -> Entry {
    let previous_env = HOST_ENV.with(|e| e.replace(env));
    LOCAL_FRAMES.with(|f| f.borrow_mut().push(Vec::new()));
    Entry { previous_env }
}

impl Drop for Entry {
    fn drop(&mut self) {
        let frame = LOCAL_FRAMES.with(|f| f.borrow_mut().pop()).unwrap_or_default();
        let mut shared = shared();
        for handle in frame {
            release_slot(&mut shared.state.jni_refs, handle);
        }
        HOST_ENV.with(|e| e.set(self.previous_env));
    }
}

fn release_slot(refs: &mut Refs, handle: u32) {
    if handle >= OBJECT_BASE && handle < MEMBER_BASE {
        let slot = (handle - OBJECT_BASE) / 4 - 1;
        if let Some(object) = refs.objects.get_mut(slot as usize) {
            if *object != 0 {
                *object = 0;
                refs.free.push(slot);
            }
        }
    }
}

fn new_slot(object: jobject) -> u32 {
    if object.is_null() {
        return 0;
    }
    let mut shared = shared();
    let refs = &mut shared.state.jni_refs;
    let slot = match refs.free.pop() {
        Some(slot) => {
            refs.objects[slot as usize] = object as usize;
            slot
        }
        None => {
            refs.objects.push(object as usize);
            refs.objects.len() as u32 - 1
        }
    };
    OBJECT_BASE + (slot + 1) * 4
}

/// Guest handle for a host local reference (freed when the current call from Java returns).
pub fn to_guest(object: jobject) -> u32 {
    let handle = new_slot(object);
    if handle != 0 {
        LOCAL_FRAMES.with(|f| {
            if let Some(frame) = f.borrow_mut().last_mut() {
                frame.push(handle);
            }
        });
    }
    handle
}

/// Guest handle for a host global reference (lives until the guest deletes it).
pub fn to_guest_global(object: jobject) -> u32 {
    new_slot(object)
}

pub fn to_host(handle: u32) -> jobject {
    if handle == 0 {
        return std::ptr::null_mut();
    }
    let shared = shared();
    let refs = &shared.state.jni_refs;
    if handle >= OBJECT_BASE && handle < MEMBER_BASE {
        let slot = ((handle - OBJECT_BASE) / 4).wrapping_sub(1) as usize;
        return refs.objects.get(slot).copied().unwrap_or(0) as jobject;
    }
    std::ptr::null_mut()
}

fn forget_local(handle: u32) {
    LOCAL_FRAMES.with(|f| {
        for frame in f.borrow_mut().iter_mut().rev() {
            if let Some(position) = frame.iter().position(|h| *h == handle) {
                frame.swap_remove(position);
                return;
            }
        }
    });
    release_slot(&mut shared().state.jni_refs, handle);
}

fn member_handle(host: usize, name: String, signature: String) -> u32 {
    if host == 0 {
        return 0;
    }
    let mut shared = shared();
    let refs = &mut shared.state.jni_refs;
    if let Some(&index) = refs.member_index.get(&host) {
        return MEMBER_BASE + index * 4;
    }
    refs.members.push(Member { host, name, signature });
    let index = refs.members.len() as u32 - 1;
    refs.member_index.insert(host, index);
    MEMBER_BASE + index * 4
}

fn member(handle: u32) -> Option<Member> {
    let shared = shared();
    let index = handle.checked_sub(MEMBER_BASE)? / 4;
    shared.state.jni_refs.members.get(index as usize).cloned()
}

/// Parameter type letters and the return type letter of a JNI signature.
pub fn parse_signature(signature: &str) -> (Vec<u8>, u8) {
    let bytes = signature.as_bytes();
    let mut parameters = Vec::new();
    let mut i = 1;
    while i < bytes.len() && bytes[i] != b')' {
        let start = bytes[i];
        while bytes[i] == b'[' {
            i += 1;
        }
        if bytes[i] == b'L' {
            while bytes[i] != b';' {
                i += 1;
            }
        }
        i += 1;
        parameters.push(if start == b'[' { b'L' } else { start });
    }
    let return_type = bytes.get(i + 1).copied().unwrap_or(b'V');
    (parameters, if return_type == b'[' { b'L' } else { return_type })
}

enum Source<'a> {
    /// `...` arguments: small ints are promoted to int, float to double.
    Variadic(&'a mut Args),
    List(VaList),
    /// A guest `jvalue[]` (8 bytes per entry).
    Array(u32),
}

fn host_arguments(parameters: &[u8], source: Source) -> Vec<jvalue> {
    let mut values = Vec::with_capacity(parameters.len());
    let read = |kind: u8, index: usize, source: &mut Source| -> jvalue {
        let (word, wide): (u32, u64) = match source {
            Source::Array(base) => {
                let at = *base + index as u32 * 8;
                (mem().read_u32(at), mem().read_u64(at))
            }
            Source::Variadic(args) => match kind {
                b'J' | b'D' | b'F' => (0, args.u64()),
                _ => (args.u32(), 0),
            },
            Source::List(list) => match kind {
                b'J' | b'D' | b'F' => (0, list.u64()),
                _ => (list.u32(), 0),
            },
        };
        let promoted = !matches!(source, Source::Array(_));
        match kind {
            b'Z' => jvalue { z: word as u8 },
            b'B' => jvalue { b: word as i8 },
            b'C' => jvalue { c: word as u16 },
            b'S' => jvalue { s: word as i16 },
            b'I' => jvalue { i: word as i32 },
            b'J' => jvalue { j: wide as i64 },
            b'F' if promoted => jvalue { f: f64::from_bits(wide) as f32 },
            b'F' => jvalue { f: f32::from_bits(wide as u32) },
            b'D' => jvalue { d: f64::from_bits(wide) },
            _ => jvalue { l: to_host(if matches!(source, Source::Array(_)) { wide as u32 } else { word }) },
        }
    };
    let mut source = source;
    for (index, kind) in parameters.iter().enumerate() {
        values.push(read(*kind, index, &mut source));
    }
    values
}

macro_rules! jcall {
    ($env:expr, $function:ident $(, $arg:expr)*) => {
        unsafe { ((**$env).$function.unwrap())($env $(, $arg)*) }
    };
}

fn guest_string(pointer: u32) -> String {
    mem().string(pointer)
}

fn host_c(pointer: u32) -> *const std::ffi::c_char {
    mem().host_or_null(pointer) as *const std::ffi::c_char
}

thread_local! {
    /// The Java method handle this thread called last (for the "[slow]" log).
    static LAST_METHOD: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// "name signature" of the Java method this thread called last.
pub fn last_method() -> String {
    member(LAST_METHOD.with(|m| m.get())).map(|m| format!("{}{}", m.name, m.signature)).unwrap_or_default()
}

/// Calls a Java method with the guest's arguments and converts the result.
fn call_method(name: &str, target: u32, class: u32, method: u32, a: &mut Args) -> Ret {
    LAST_METHOD.with(|m| m.set(method));
    let Some(member) = member(method) else {
        crate::log::line(&format!("[bridge] {name} with unknown method id {method:#x}"));
        return Ret::U32(0);
    };
    let (parameters, return_type) = parse_signature(&member.signature);
    let source = if name.ends_with("MethodV") {
        Source::List(VaList { pointer: a.u32() })
    } else if name.ends_with("MethodA") {
        Source::Array(a.u32())
    } else {
        Source::Variadic(a)
    };
    let values = host_arguments(&parameters, source);
    let env = env();
    #[cfg(target_os = "android")]
    crate::sound_preload::observe_java_call(env, &member.name, &member.signature, &values);
    let id = member.host as jmethodID;
    let target_object = to_host(target);
    let class_object = to_host(class);
    let arguments = values.as_ptr();
    let is_static = name.starts_with("CallStatic");
    let is_nonvirtual = name.starts_with("CallNonvirtual");
    // Java can block or call back into the game, so let other guest threads run.
    without_gil(|| unsafe {
        macro_rules! pick {
            ($virtual:ident, $nonvirtual:ident, $static_call:ident) => {
                if is_static {
                    ((**env).$static_call.unwrap())(env, target_object, id, arguments)
                } else if is_nonvirtual {
                    ((**env).$nonvirtual.unwrap())(env, target_object, class_object, id, arguments)
                } else {
                    ((**env).$virtual.unwrap())(env, target_object, id, arguments)
                }
            };
        }
        match return_type {
            b'V' => {
                pick!(CallVoidMethodA, CallNonvirtualVoidMethodA, CallStaticVoidMethodA);
                HostResult::Void
            }
            b'Z' => HostResult::Word(pick!(CallBooleanMethodA, CallNonvirtualBooleanMethodA, CallStaticBooleanMethodA) as u32),
            b'B' => HostResult::Word(pick!(CallByteMethodA, CallNonvirtualByteMethodA, CallStaticByteMethodA) as u32),
            b'C' => HostResult::Word(pick!(CallCharMethodA, CallNonvirtualCharMethodA, CallStaticCharMethodA) as u32),
            b'S' => HostResult::Word(pick!(CallShortMethodA, CallNonvirtualShortMethodA, CallStaticShortMethodA) as u32),
            b'I' => HostResult::Word(pick!(CallIntMethodA, CallNonvirtualIntMethodA, CallStaticIntMethodA) as u32),
            b'J' => HostResult::Wide(pick!(CallLongMethodA, CallNonvirtualLongMethodA, CallStaticLongMethodA) as u64),
            b'F' => HostResult::Float(pick!(CallFloatMethodA, CallNonvirtualFloatMethodA, CallStaticFloatMethodA)),
            b'D' => HostResult::Double(pick!(CallDoubleMethodA, CallNonvirtualDoubleMethodA, CallStaticDoubleMethodA)),
            _ => HostResult::Object(pick!(CallObjectMethodA, CallNonvirtualObjectMethodA, CallStaticObjectMethodA) as usize),
        }
    })
    .into_ret()
}

enum HostResult {
    Void,
    Word(u32),
    Wide(u64),
    Float(f32),
    Double(f64),
    Object(usize),
}

impl HostResult {
    fn into_ret(self) -> Ret {
        match self {
            HostResult::Void => Ret::Void,
            HostResult::Word(value) => Ret::U32(value),
            HostResult::Wide(value) => Ret::U64(value),
            HostResult::Float(value) => Ret::F32(value),
            HostResult::Double(value) => Ret::F64(value),
            HostResult::Object(object) => Ret::U32(to_guest(object as jobject)),
        }
    }
}

fn field_type(field: &Member) -> u8 {
    match field.signature.as_bytes().first() {
        Some(b'[') => b'L',
        Some(&c) => c,
        None => b'I',
    }
}

fn get_field(name: &str, target: u32, field: u32) -> Ret {
    let Some(member) = member(field) else { return Ret::U32(0) };
    let env = env();
    let object = to_host(target);
    let id = member.host as jfieldID;
    let is_static = name.starts_with("GetStatic");
    unsafe {
        macro_rules! get {
            ($instance:ident, $static_get:ident) => {
                if is_static {
                    ((**env).$static_get.unwrap())(env, object, id)
                } else {
                    ((**env).$instance.unwrap())(env, object, id)
                }
            };
        }
        match field_type(&member) {
            b'Z' => Ret::U32(get!(GetBooleanField, GetStaticBooleanField) as u32),
            b'B' => Ret::U32(get!(GetByteField, GetStaticByteField) as u32),
            b'C' => Ret::U32(get!(GetCharField, GetStaticCharField) as u32),
            b'S' => Ret::U32(get!(GetShortField, GetStaticShortField) as u32),
            b'I' => Ret::U32(get!(GetIntField, GetStaticIntField) as u32),
            b'J' => Ret::U64(get!(GetLongField, GetStaticLongField) as u64),
            b'F' => Ret::F32(get!(GetFloatField, GetStaticFloatField)),
            b'D' => Ret::F64(get!(GetDoubleField, GetStaticDoubleField)),
            _ => Ret::U32(to_guest(get!(GetObjectField, GetStaticObjectField))),
        }
    }
}

fn set_field(name: &str, target: u32, field: u32, a: &mut Args) -> Ret {
    let Some(member) = member(field) else { return Ret::Void };
    let env = env();
    let object = to_host(target);
    let id = member.host as jfieldID;
    let is_static = name.starts_with("SetStatic");
    unsafe {
        macro_rules! set {
            ($instance:ident, $static_set:ident, $value:expr) => {
                if is_static {
                    ((**env).$static_set.unwrap())(env, object, id, $value)
                } else {
                    ((**env).$instance.unwrap())(env, object, id, $value)
                }
            };
        }
        match field_type(&member) {
            b'Z' => set!(SetBooleanField, SetStaticBooleanField, a.u32() as u8),
            b'B' => set!(SetByteField, SetStaticByteField, a.u32() as i8),
            b'C' => set!(SetCharField, SetStaticCharField, a.u32() as u16),
            b'S' => set!(SetShortField, SetStaticShortField, a.u32() as i16),
            b'I' => set!(SetIntField, SetStaticIntField, a.i32()),
            b'J' => set!(SetLongField, SetStaticLongField, a.i64()),
            b'F' => set!(SetFloatField, SetStaticFloatField, a.f32()),
            b'D' => set!(SetDoubleField, SetStaticDoubleField, a.f64()),
            _ => set!(SetObjectField, SetStaticObjectField, to_host(a.u32())),
        }
    }
    Ret::Void
}

fn element_size_of(name: &str) -> u32 {
    if name.contains("Boolean") || name.contains("Byte") {
        1
    } else if name.contains("Char") || name.contains("Short") {
        2
    } else if name.contains("Long") || name.contains("Double") {
        8
    } else {
        4
    }
}

/// Element size of a primitive array, from its class name ("[B", "[I"...).
fn array_element_size(env: *mut JNIEnv, array: jobject) -> u32 {
    let class = jcall!(env, GetObjectClass, array);
    let class_class = jcall!(env, GetObjectClass, class);
    let get_name = jcall!(env, GetMethodID, class_class, c"getName".as_ptr(), c"()Ljava/lang/String;".as_ptr());
    let name = jcall!(env, CallObjectMethodA, class, get_name, std::ptr::null());
    let chars = jcall!(env, GetStringUTFChars, name, std::ptr::null_mut());
    let size = match unsafe { *chars.add(1) } as u8 {
        b'Z' | b'B' => 1,
        b'C' | b'S' => 2,
        b'J' | b'D' => 8,
        _ => 4,
    };
    jcall!(env, ReleaseStringUTFChars, name, chars);
    for local in [name, class_class, class] {
        jcall!(env, DeleteLocalRef, local);
    }
    size
}

/// Copies `bytes` of host memory into a new guest buffer and remembers it.
fn guest_copy(host: *const u8, element_size: u32, length: u32, extra: u32) -> u32 {
    let bytes = element_size * length;
    let guest = shared().heap.malloc(bytes + extra);
    unsafe { std::ptr::copy_nonoverlapping(host, mem().host(guest), bytes as usize) };
    for i in 0..extra {
        mem().write_u8(guest + bytes + i, 0);
    }
    shared().state.jni_refs.copies.insert(guest, (host as usize, element_size, length));
    guest
}

fn take_copy(guest: u32) -> Option<(usize, u32, u32)> {
    shared().state.jni_refs.copies.remove(&guest)
}

/// Handles one guest JNIEnv / JavaVM function against the real JVM.
pub fn dispatch(uc: &mut Uc, name: &str, a: &mut Args) -> Ret {
    match name {
        "GetEnv" => {
            let (_vm, out, _version) = (a.u32(), a.u32(), a.u32());
            let vm = HOST_VM.load(Ordering::SeqCst);
            let mut host_env: *mut c_void = std::ptr::null_mut();
            let attached = HOST_ENV.with(|e| !e.get().is_null())
                || unsafe { ((**vm).GetEnv.unwrap())(vm, &mut host_env, JNI_VERSION_1_6) } == JNI_OK;
            if attached {
                if !host_env.is_null() {
                    HOST_ENV.with(|e| e.set(host_env as *mut JNIEnv));
                }
                mem().write_u32(out, shared().state.jni.env);
                return Ret::U32(0);
            }
            mem().write_u32(out, 0);
            return Ret::U32(JNI_EDETACHED as u32);
        }
        "AttachCurrentThread" | "AttachCurrentThreadAsDaemon" => {
            let (_vm, out) = (a.u32(), a.u32());
            env();
            mem().write_u32(out, shared().state.jni.env);
            return Ret::U32(0);
        }
        "DetachCurrentThread" => {
            detach_current_thread();
            return Ret::U32(0);
        }
        "DestroyJavaVM" => return Ret::U32(u32::MAX),
        _ => {}
    }
    let _guest_env = a.u32();
    let env = env();

    if name.starts_with("Call") {
        let target = a.u32();
        let class = if name.starts_with("CallNonvirtual") { a.u32() } else { 0 };
        let method = a.u32();
        return call_method(name, target, class, method, a);
    }
    if name.starts_with("NewObject") && name != "NewObjectArray" {
        let (class, method) = (a.u32(), a.u32());
        let Some(member) = member(method) else { return Ret::U32(0) };
        let (parameters, _) = parse_signature(&member.signature);
        let source = match name {
            "NewObjectV" => Source::List(VaList { pointer: a.u32() }),
            "NewObjectA" => Source::Array(a.u32()),
            _ => Source::Variadic(a),
        };
        let values = host_arguments(&parameters, source);
        let class = to_host(class);
        let id = member.host as jmethodID;
        let object = without_gil(|| jcall!(env, NewObjectA, class, id, values.as_ptr()) as usize);
        return Ret::U32(to_guest(object as jobject));
    }
    if (name.starts_with("Get") || name.starts_with("Set")) && name.ends_with("Field") {
        let (target, field) = (a.u32(), a.u32());
        return if name.starts_with("Get") { get_field(name, target, field) } else { set_field(name, target, field, a) };
    }
    if name.starts_with("New") && name.ends_with("Array") && name != "NewObjectArray" {
        let length = a.i32();
        let array = unsafe {
            match element_size_of(name) {
                _ if name.contains("Boolean") => ((**env).NewBooleanArray.unwrap())(env, length),
                _ if name.contains("Byte") => ((**env).NewByteArray.unwrap())(env, length),
                _ if name.contains("Char") => ((**env).NewCharArray.unwrap())(env, length),
                _ if name.contains("Short") => ((**env).NewShortArray.unwrap())(env, length),
                _ if name.contains("Int") => ((**env).NewIntArray.unwrap())(env, length),
                _ if name.contains("Long") => ((**env).NewLongArray.unwrap())(env, length),
                _ if name.contains("Float") => ((**env).NewFloatArray.unwrap())(env, length),
                _ => ((**env).NewDoubleArray.unwrap())(env, length),
            }
        };
        return Ret::U32(to_guest(array));
    }
    if name.ends_with("ArrayElements") || name.ends_with("ArrayCritical") {
        let size = element_size_of(name);
        if name.starts_with("Get") {
            let (array, is_copy) = (a.u32(), a.u32());
            let host_array = to_host(array);
            let size = if name.ends_with("Critical") { array_element_size(env, host_array) } else { size };
            let length = jcall!(env, GetArrayLength, host_array) as u32;
            // Every primitive Get*ArrayElements returns a void-compatible pointer.
            let host = jcall!(env, GetPrimitiveArrayCritical, host_array, std::ptr::null_mut()) as *const u8;
            if host.is_null() {
                return Ret::U32(0);
            }
            let bytes = size * length;
            let guest = shared().heap.malloc(bytes.max(1));
            unsafe { std::ptr::copy_nonoverlapping(host, mem().host(guest), bytes as usize) };
            jcall!(env, ReleasePrimitiveArrayCritical, host_array, host as *mut c_void, JNI_ABORT);
            shared().state.jni_refs.copies.insert(guest, (0, size, length));
            if is_copy != 0 {
                mem().write_u8(is_copy, 1);
            }
            return Ret::U32(guest);
        }
        let (array, elements, mode) = (a.u32(), a.u32(), a.u32());
        let Some((_, size, length)) = take_copy(elements) else { return Ret::Void };
        let host_array = to_host(array);
        if mode != JNI_ABORT_MODE {
            let host = jcall!(env, GetPrimitiveArrayCritical, host_array, std::ptr::null_mut()) as *mut u8;
            if !host.is_null() {
                unsafe { std::ptr::copy_nonoverlapping(mem().host(elements), host, (size * length) as usize) };
                jcall!(env, ReleasePrimitiveArrayCritical, host_array, host as *mut c_void, 0);
            }
        }
        if mode == JNI_COMMIT_MODE {
            shared().state.jni_refs.copies.insert(elements, (0, size, length));
        } else {
            shared().heap.free(elements);
        }
        return Ret::Void;
    }
    if name.ends_with("ArrayRegion") {
        let (array, start, length, buffer) = (a.u32(), a.i32(), a.i32(), a.u32());
        let host_array = to_host(array);
        let host_buffer = mem().host(buffer) as *mut c_void;
        let size = element_size_of(name) as usize;
        // Region copies go straight between the Java array and guest memory.
        let host = jcall!(env, GetPrimitiveArrayCritical, host_array, std::ptr::null_mut()) as *mut u8;
        if host.is_null() {
            return Ret::Void;
        }
        let offset = start as usize * size;
        let bytes = length as usize * size;
        unsafe {
            if name.starts_with("Get") {
                std::ptr::copy_nonoverlapping(host.add(offset), host_buffer as *mut u8, bytes);
            } else {
                std::ptr::copy_nonoverlapping(host_buffer as *const u8, host.add(offset), bytes);
            }
        }
        let mode = if name.starts_with("Get") { JNI_ABORT } else { 0 };
        jcall!(env, ReleasePrimitiveArrayCritical, host_array, host as *mut c_void, mode);
        return Ret::Void;
    }

    match name {
        "GetVersion" => Ret::U32(jcall!(env, GetVersion) as u32),
        "FindClass" => {
            let class_name = a.u32();
            Ret::U32(to_guest(jcall!(env, FindClass, host_c(class_name))))
        }
        "GetSuperclass" => Ret::U32(to_guest(jcall!(env, GetSuperclass, to_host(a.u32())))),
        "GetObjectClass" => Ret::U32(to_guest(jcall!(env, GetObjectClass, to_host(a.u32())))),
        "IsInstanceOf" => {
            let (object, class) = (a.u32(), a.u32());
            Ret::U32(jcall!(env, IsInstanceOf, to_host(object), to_host(class)) as u32)
        }
        "IsAssignableFrom" => {
            let (x, y) = (a.u32(), a.u32());
            Ret::U32(jcall!(env, IsAssignableFrom, to_host(x), to_host(y)) as u32)
        }
        "IsSameObject" => {
            let (x, y) = (a.u32(), a.u32());
            Ret::U32(jcall!(env, IsSameObject, to_host(x), to_host(y)) as u32)
        }
        "NewGlobalRef" => Ret::U32(to_guest_global(jcall!(env, NewGlobalRef, to_host(a.u32())))),
        "NewWeakGlobalRef" => Ret::U32(to_guest_global(jcall!(env, NewWeakGlobalRef, to_host(a.u32())))),
        "NewLocalRef" => Ret::U32(to_guest(jcall!(env, NewLocalRef, to_host(a.u32())))),
        "DeleteGlobalRef" | "DeleteWeakGlobalRef" | "DeleteLocalRef" => {
            let handle = a.u32();
            let object = to_host(handle);
            if !object.is_null() {
                match name {
                    "DeleteGlobalRef" => jcall!(env, DeleteGlobalRef, object),
                    "DeleteWeakGlobalRef" => jcall!(env, DeleteWeakGlobalRef, object),
                    _ => jcall!(env, DeleteLocalRef, object),
                }
                forget_local(handle);
            }
            Ret::Void
        }
        "PushLocalFrame" => {
            let capacity = a.i32();
            let result = jcall!(env, PushLocalFrame, capacity);
            LOCAL_FRAMES.with(|f| f.borrow_mut().push(Vec::new()));
            Ret::U32(result as u32)
        }
        "PopLocalFrame" => {
            let result = to_host(a.u32());
            let kept = jcall!(env, PopLocalFrame, result);
            let frame = LOCAL_FRAMES.with(|f| f.borrow_mut().pop()).unwrap_or_default();
            for handle in frame {
                release_slot(&mut shared().state.jni_refs, handle);
            }
            Ret::U32(to_guest(kept))
        }
        "EnsureLocalCapacity" => Ret::U32(jcall!(env, EnsureLocalCapacity, a.i32()) as u32),
        "ExceptionCheck" => Ret::U32(jcall!(env, ExceptionCheck) as u32),
        "ExceptionOccurred" => Ret::U32(to_guest(jcall!(env, ExceptionOccurred))),
        "ExceptionClear" => {
            jcall!(env, ExceptionClear);
            Ret::Void
        }
        "ExceptionDescribe" => {
            jcall!(env, ExceptionDescribe);
            Ret::Void
        }
        "Throw" => Ret::U32(jcall!(env, Throw, to_host(a.u32())) as u32),
        "ThrowNew" => {
            let (class, message) = (a.u32(), a.u32());
            Ret::U32(jcall!(env, ThrowNew, to_host(class), host_c(message)) as u32)
        }
        "FatalError" => {
            let message = guest_string(a.u32());
            stop_with_fault(uc, format!("JNI FatalError: {message}"));
            Ret::Void
        }
        "GetMethodID" | "GetStaticMethodID" | "GetFieldID" | "GetStaticFieldID" => {
            let (class, member_name, signature) = (a.u32(), a.u32(), a.u32());
            let class = to_host(class);
            let host = match name {
                "GetMethodID" => jcall!(env, GetMethodID, class, host_c(member_name), host_c(signature)) as usize,
                "GetStaticMethodID" => {
                    jcall!(env, GetStaticMethodID, class, host_c(member_name), host_c(signature)) as usize
                }
                "GetFieldID" => jcall!(env, GetFieldID, class, host_c(member_name), host_c(signature)) as usize,
                _ => jcall!(env, GetStaticFieldID, class, host_c(member_name), host_c(signature)) as usize,
            };
            if host == 0 {
                crate::log::line(&format!(
                    "[bridge] Java has no {} {} {}",
                    if name.contains("Field") { "field" } else { "method" },
                    guest_string(member_name),
                    guest_string(signature)
                ));
            }
            Ret::U32(member_handle(host, guest_string(member_name), guest_string(signature)))
        }
        "AllocObject" => Ret::U32(to_guest(jcall!(env, AllocObject, to_host(a.u32())))),
        "NewStringUTF" => {
            let text = a.u32();
            Ret::U32(to_guest(jcall!(env, NewStringUTF, host_c(text))))
        }
        "NewString" => {
            let (characters, length) = (a.u32(), a.i32());
            Ret::U32(to_guest(jcall!(env, NewString, mem().host(characters) as *const u16, length)))
        }
        "GetStringLength" => Ret::U32(jcall!(env, GetStringLength, to_host(a.u32())) as u32),
        "GetStringUTFLength" => Ret::U32(jcall!(env, GetStringUTFLength, to_host(a.u32())) as u32),
        "GetStringUTFChars" => {
            let (string, is_copy) = (a.u32(), a.u32());
            let host_string = to_host(string);
            let host = jcall!(env, GetStringUTFChars, host_string, std::ptr::null_mut());
            if host.is_null() {
                return Ret::U32(0);
            }
            let length = unsafe { CStr::from_ptr(host) }.to_bytes().len() as u32;
            let guest = guest_copy(host as *const u8, 1, length, 1);
            jcall!(env, ReleaseStringUTFChars, host_string, host);
            if is_copy != 0 {
                mem().write_u8(is_copy, 1);
            }
            Ret::U32(guest)
        }
        "GetStringChars" | "GetStringCritical" => {
            let (string, is_copy) = (a.u32(), a.u32());
            let host_string = to_host(string);
            let length = jcall!(env, GetStringLength, host_string) as u32;
            let host = jcall!(env, GetStringChars, host_string, std::ptr::null_mut());
            if host.is_null() {
                return Ret::U32(0);
            }
            let guest = guest_copy(host as *const u8, 2, length, 2);
            jcall!(env, ReleaseStringChars, host_string, host);
            if is_copy != 0 {
                mem().write_u8(is_copy, 1);
            }
            Ret::U32(guest)
        }
        "ReleaseStringUTFChars" | "ReleaseStringChars" | "ReleaseStringCritical" => {
            let (_string, characters) = (a.u32(), a.u32());
            if take_copy(characters).is_some() {
                shared().heap.free(characters);
            }
            Ret::Void
        }
        "GetStringRegion" | "GetStringUTFRegion" => {
            let (string, start, length, buffer) = (a.u32(), a.i32(), a.i32(), a.u32());
            let host_string = to_host(string);
            if name == "GetStringRegion" {
                jcall!(env, GetStringRegion, host_string, start, length, mem().host(buffer) as *mut u16);
            } else {
                jcall!(env, GetStringUTFRegion, host_string, start, length, mem().host(buffer) as *mut std::ffi::c_char);
            }
            Ret::Void
        }
        "GetArrayLength" => Ret::U32(jcall!(env, GetArrayLength, to_host(a.u32())) as u32),
        "NewObjectArray" => {
            let (length, class, initial) = (a.i32(), a.u32(), a.u32());
            Ret::U32(to_guest(jcall!(env, NewObjectArray, length, to_host(class), to_host(initial))))
        }
        "GetObjectArrayElement" => {
            let (array, index) = (a.u32(), a.i32());
            Ret::U32(to_guest(jcall!(env, GetObjectArrayElement, to_host(array), index)))
        }
        "SetObjectArrayElement" => {
            let (array, index, value) = (a.u32(), a.i32(), a.u32());
            jcall!(env, SetObjectArrayElement, to_host(array), index, to_host(value));
            Ret::Void
        }
        "RegisterNatives" => {
            // JNINativeMethod in 32-bit code: name, signature, function (3 words each).
            let (class, methods, count) = (a.u32(), a.u32(), a.u32());
            let methods: Vec<(String, String, u32)> = (0..count)
                .map(|i| {
                    let at = methods + i * 12;
                    let text = |pointer: u32| mem().c_str(mem().read_u32(pointer)).to_string_lossy().into_owned();
                    (text(at), text(at + 4), mem().read_u32(at + 8))
                })
                .collect();
            #[cfg(target_os = "android")]
            let ok = crate::natives::register_from_guest(env, to_host(class), &methods);
            #[cfg(not(target_os = "android"))]
            let ok = { let _ = (class, &methods); false };
            Ret::U32(if ok { 0 } else { u32::MAX })
        }
        "UnregisterNatives" => Ret::U32(0),
        "MonitorEnter" => {
            let object = to_host(a.u32()) as usize;
            Ret::U32(without_gil(|| jcall!(env, MonitorEnter, object as jobject)) as u32)
        }
        "MonitorExit" => Ret::U32(jcall!(env, MonitorExit, to_host(a.u32())) as u32),
        "GetJavaVM" => {
            let out = a.u32();
            mem().write_u32(out, shared().state.jni.vm);
            Ret::U32(0)
        }
        "NewDirectByteBuffer" => {
            let (address, capacity) = (a.u32(), a.i64());
            Ret::U32(to_guest(jcall!(env, NewDirectByteBuffer, mem().host(address) as *mut c_void, capacity)))
        }
        "GetDirectBufferAddress" => {
            let host = jcall!(env, GetDirectBufferAddress, to_host(a.u32())) as *const u8;
            let memory = mem();
            let offset = (host as usize).wrapping_sub(memory.host(0) as usize);
            Ret::U32(if offset < (1usize << 32) { offset as u32 } else { 0 })
        }
        "GetDirectBufferCapacity" => Ret::U64(jcall!(env, GetDirectBufferCapacity, to_host(a.u32())) as u64),
        "GetObjectRefType" => Ret::U32(jcall!(env, GetObjectRefType, to_host(a.u32())) as u32),
        "FromReflectedMethod" | "FromReflectedField" => {
            let member = to_host(a.u32());
            if member.is_null() {
                return Ret::U32(0);
            }
            let host = if name == "FromReflectedMethod" {
                jcall!(env, FromReflectedMethod, member) as usize
            } else {
                jcall!(env, FromReflectedField, member) as usize
            };
            #[cfg(target_os = "android")]
            let described = crate::natives::reflected_member(env, member);
            #[cfg(not(target_os = "android"))]
            let described: Option<(String, String)> = None;
            match described {
                Some((member_name, signature)) => Ret::U32(member_handle(host, member_name, signature)),
                None => Ret::U32(0),
            }
        }
        other => {
            crate::log::line(&format!("[bridge] JNI {other} is not bridged; returned 0"));
            Ret::U32(0)
        }
    }
}
