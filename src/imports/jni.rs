//! A pretend Java side. The game gets a JavaVM and a JNIEnv whose function
//! tables point at bridge stand-ins; every class lookup and Java call is
//! logged and answered with a harmless value (or a known answer from
//! `pretend_answer`). References are opaque handles in an unmapped range,
//! so a game that dereferences one faults loudly instead of reading junk.

use crate::handler;
use crate::runtime::{mem, shared, stop_with_fault, stub_for, ArgSource, Args, Handler, Ret, Uc};

const HANDLE_BASE: u32 = 0xD000_0000;
const JNI_VERSION_1_6: u32 = 0x0001_0006;

/// Answers for Java methods the game asks about at start-up. The package name
/// comes from ARM32_PACKAGE (the PC tool has no real app to ask).
fn pretend_answer(method: &str) -> Option<String> {
    let package = std::env::var("ARM32_PACKAGE").unwrap_or_else(|_| "app".into());
    Some(match method {
        "getCocos2dxPackageName" => package,
        "getCocos2dxWritablePath" => format!("/data/data/{package}/files"),
        "getCurrentLanguage" | "getCurrentLanguageCode" => "en".into(),
        "getDeviceModel" => "Pixel 10".into(),
        "getVersion" => "1.0".into(),
        _ => return None,
    })
}

#[derive(Clone, Debug)]
pub enum JavaObject {
    Class(String),
    Text(String),
    Method { class: String, name: String, signature: String },
    Field { class: String, name: String, signature: String },
    Instance(String),
    Array { element_size: u32, length: u32, data: u32 },
}

#[derive(Default)]
pub struct Jni {
    pub vm: u32,
    pub env: u32,
    objects: Vec<JavaObject>,
    /// (class, name, signature, guest function) from RegisterNatives.
    pub natives: Vec<(String, String, String, u32)>,
    pub java_calls: Vec<String>,
}

impl Jni {
    pub fn add(&mut self, object: JavaObject) -> u32 {
        self.objects.push(object);
        HANDLE_BASE + (self.objects.len() as u32) * 8
    }
    pub fn get(&self, handle: u32) -> Option<&JavaObject> {
        if handle < HANDLE_BASE + 8 {
            return None;
        }
        self.objects.get(((handle - HANDLE_BASE) / 8 - 1) as usize)
    }
    fn class_handle(&mut self, name: &str) -> u32 {
        for (i, object) in self.objects.iter().enumerate() {
            if let JavaObject::Class(existing) = object {
                if existing == name {
                    return HANDLE_BASE + (i as u32 + 1) * 8;
                }
            }
        }
        self.add(JavaObject::Class(name.to_string()))
    }
    fn class_name(&self, handle: u32) -> String {
        match self.get(handle) {
            Some(JavaObject::Class(name)) | Some(JavaObject::Instance(name)) => name.clone(),
            Some(JavaObject::Text(_)) => "java/lang/String".into(),
            _ => format!("<object {handle:#x}>"),
        }
    }
    pub fn text(&mut self, text: &str) -> u32 {
        self.add(JavaObject::Text(text.to_string()))
    }
}

const VM_FUNCTIONS: [&str; 8] = [
    "vm.reserved0",
    "vm.reserved1",
    "vm.reserved2",
    "DestroyJavaVM",
    "AttachCurrentThread",
    "DetachCurrentThread",
    "GetEnv",
    "AttachCurrentThreadAsDaemon",
];

const TYPES: [&str; 10] = ["Object", "Boolean", "Byte", "Char", "Short", "Int", "Long", "Float", "Double", "Void"];
const PRIMITIVES: [&str; 8] = ["Boolean", "Byte", "Char", "Short", "Int", "Long", "Float", "Double"];

/// The JNIEnv function table, in the exact order of `JNINativeInterface`.
pub fn env_function_names() -> Vec<String> {
    let mut names: Vec<String> = [
        "reserved0", "reserved1", "reserved2", "reserved3", "GetVersion", "DefineClass", "FindClass",
        "FromReflectedMethod", "FromReflectedField", "ToReflectedMethod", "GetSuperclass", "IsAssignableFrom",
        "ToReflectedField", "Throw", "ThrowNew", "ExceptionOccurred", "ExceptionDescribe", "ExceptionClear",
        "FatalError", "PushLocalFrame", "PopLocalFrame", "NewGlobalRef", "DeleteGlobalRef", "DeleteLocalRef",
        "IsSameObject", "NewLocalRef", "EnsureLocalCapacity", "AllocObject", "NewObject", "NewObjectV",
        "NewObjectA", "GetObjectClass", "IsInstanceOf", "GetMethodID",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let calls = |prefix: &str, names: &mut Vec<String>| {
        for t in TYPES {
            for suffix in ["", "V", "A"] {
                names.push(format!("{prefix}{t}Method{suffix}"));
            }
        }
    };
    calls("Call", &mut names);
    calls("CallNonvirtual", &mut names);
    names.push("GetFieldID".into());
    names.extend(TYPES[..9].iter().map(|t| format!("Get{t}Field")));
    names.extend(TYPES[..9].iter().map(|t| format!("Set{t}Field")));
    names.push("GetStaticMethodID".into());
    calls("CallStatic", &mut names);
    names.push("GetStaticFieldID".into());
    names.extend(TYPES[..9].iter().map(|t| format!("GetStatic{t}Field")));
    names.extend(TYPES[..9].iter().map(|t| format!("SetStatic{t}Field")));
    for name in [
        "NewString", "GetStringLength", "GetStringChars", "ReleaseStringChars", "NewStringUTF",
        "GetStringUTFLength", "GetStringUTFChars", "ReleaseStringUTFChars", "GetArrayLength", "NewObjectArray",
        "GetObjectArrayElement", "SetObjectArrayElement",
    ] {
        names.push(name.into());
    }
    for pattern in ["New{}Array", "Get{}ArrayElements", "Release{}ArrayElements", "Get{}ArrayRegion", "Set{}ArrayRegion"] {
        names.extend(PRIMITIVES.iter().map(|p| pattern.replace("{}", p)));
    }
    for name in [
        "RegisterNatives", "UnregisterNatives", "MonitorEnter", "MonitorExit", "GetJavaVM", "GetStringRegion",
        "GetStringUTFRegion", "GetPrimitiveArrayCritical", "ReleasePrimitiveArrayCritical", "GetStringCritical",
        "ReleaseStringCritical", "NewWeakGlobalRef", "DeleteWeakGlobalRef", "ExceptionCheck",
        "NewDirectByteBuffer", "GetDirectBufferAddress", "GetDirectBufferCapacity", "GetObjectRefType",
    ] {
        names.push(name.into());
    }
    names
}

/// Builds the guest JavaVM and JNIEnv. Returns (vm, env) guest pointers.
pub fn create_vm(_uc: &mut Uc) -> (u32, u32) {
    let names = env_function_names();
    assert_eq!(names.len(), 233, "JNIEnv table must match JNINativeInterface");

    let env_table = shared().misc_alloc(names.len() as u32 * 4, 4);
    for (i, name) in names.iter().enumerate() {
        let stub = stub_for(&format!("jni:{name}"));
        mem().write_u32(env_table + i as u32 * 4, stub);
    }
    let vm_table = shared().misc_alloc(VM_FUNCTIONS.len() as u32 * 4, 4);
    for (i, name) in VM_FUNCTIONS.iter().enumerate() {
        let stub = stub_for(&format!("jni:{name}"));
        mem().write_u32(vm_table + i as u32 * 4, stub);
    }
    let env = shared().misc_alloc(4, 4);
    let vm = shared().misc_alloc(4, 4);
    mem().write_u32(env, env_table);
    mem().write_u32(vm, vm_table);
    shared().state.jni.vm = vm;
    shared().state.jni.env = env;
    (vm, env)
}

fn note_java_call(text: String) {
    eprintln!("[java] {text}");
    shared().state.jni.java_calls.push(text);
}

/// The value a pretend Java method returns, by the return type in its signature.
fn java_answer(method: u32, how: &str) -> Ret {
    let (class, name, signature) = match shared().state.jni.get(method) {
        Some(JavaObject::Method { class, name, signature }) => (class.clone(), name.clone(), signature.clone()),
        _ => (String::from("?"), format!("<method {method:#x}>"), String::from("()V")),
    };
    let return_type = signature.rsplit(')').next().unwrap_or("V").to_string();
    let answer = pretend_answer(&name);
    let result = match return_type.as_bytes().first() {
        Some(b'V') => Ret::Void,
        Some(b'J') => Ret::U64(0),
        Some(b'F') => Ret::F32(0.0),
        Some(b'D') => Ret::F64(0.0),
        Some(b'L') if return_type == "Ljava/lang/String;" => Ret::U32(shared().state.jni.text(answer.as_deref().unwrap_or(""))),
        Some(b'L') | Some(b'[') => {
            let type_name = return_type.trim_start_matches('L').trim_end_matches(';').to_string();
            Ret::U32(shared().state.jni.add(JavaObject::Instance(type_name)))
        }
        _ => Ret::U32(0),
    };
    let shown = match (&result, answer) {
        (Ret::U32(_), Some(text)) if return_type == "Ljava/lang/String;" => format!(" -> \"{text}\""),
        (Ret::U32(_), None) if return_type == "Ljava/lang/String;" => " -> \"\"".to_string(),
        (Ret::Void, _) => String::new(),
        (Ret::U32(handle), _) if return_type.starts_with('L') || return_type.starts_with('[') => format!(" -> object {handle:#x}"),
        _ => " -> 0".to_string(),
    };
    note_java_call(format!("{how} {class}.{name}{signature}{shown}"));
    result
}

fn element_size(name: &str) -> u32 {
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

/// One handler for every JNI entry; it switches on the stub being called.
fn dispatch(uc: &mut Uc, a: &mut Args) -> Ret {
    let full_name = uc.get_data().current_call;
    let name = full_name.strip_prefix("jni:").unwrap_or(full_name);
    if super::jni_host::active() {
        return super::jni_host::dispatch(uc, name, a);
    }
    let memory = mem();

    // JavaVM functions take the VM first; the rest take the JNIEnv first.
    match name {
        "GetEnv" | "AttachCurrentThread" | "AttachCurrentThreadAsDaemon" => {
            let (_vm, out) = (a.u32(), a.u32());
            let env = shared().state.jni.env;
            memory.write_u32(out, env);
            return Ret::U32(0);
        }
        "DetachCurrentThread" | "DestroyJavaVM" => return Ret::U32(0),
        _ => {}
    }
    let _env = a.u32();

    if let Some(rest) = name.strip_prefix("Call") {
        // Call<Type>Method[V|A], CallStatic..., CallNonvirtual... (obj or class, [class,] method, args...)
        let how = if rest.starts_with("Static") { "static call" } else { "call" };
        let _target = a.u32();
        if rest.starts_with("Nonvirtual") {
            let _class = a.u32();
        }
        let method = a.u32();
        return java_answer(method, how);
    }
    if let Some(rest) = name.strip_prefix("New").filter(|r| r.ends_with("Array") && *r != "ObjectArray") {
        let length = a.u32();
        let size = element_size(rest);
        let data = crate::runtime::heap().calloc(length.max(1), size);
        return Ret::U32(shared().state.jni.add(JavaObject::Array { element_size: size, length, data }));
    }
    if name.starts_with("Get") && name.ends_with("ArrayElements") {
        let (array, is_copy) = (a.u32(), a.u32());
        if is_copy != 0 {
            memory.write_u8(is_copy, 0);
        }
        return match shared().state.jni.get(array) {
            Some(JavaObject::Array { data, .. }) => Ret::U32(*data),
            _ => Ret::U32(0),
        };
    }
    if name.starts_with("Release") && name.ends_with("ArrayElements") {
        return Ret::Void;
    }
    if (name.starts_with("Get") || name.starts_with("Set")) && name.ends_with("ArrayRegion") {
        let (array, start, length, buffer) = (a.u32(), a.u32(), a.u32(), a.u32());
        if let Some(JavaObject::Array { element_size, data, .. }) = shared().state.jni.get(array).cloned() {
            let (from, to) = (data + start * element_size, buffer);
            let (from, to) = if name.starts_with("Get") { (from, to) } else { (to, from) };
            let bytes = memory.bytes(from, (length * element_size) as usize).to_vec();
            memory.write_bytes(to, &bytes);
        }
        return Ret::Void;
    }
    if name.contains("Field") && !name.ends_with("FieldID") {
        let (object, field) = (a.u32(), a.u32());
        let description = match shared().state.jni.get(field) {
            Some(JavaObject::Field { class, name, signature }) => format!("{class}.{name} {signature}"),
            _ => format!("<field {field:#x}>"),
        };
        let _ = object;
        note_java_call(format!("{name} {description}"));
        return if name.starts_with("Set") {
            Ret::Void
        } else if name.contains("Long") {
            Ret::U64(0)
        } else if name.contains("Double") {
            Ret::F64(0.0)
        } else if name.contains("Float") {
            Ret::F32(0.0)
        } else {
            Ret::U32(0)
        };
    }

    match name {
        "GetVersion" => Ret::U32(JNI_VERSION_1_6),
        "FindClass" => {
            let class = memory.string(a.u32());
            note_java_call(format!("FindClass {class}"));
            Ret::U32(shared().state.jni.class_handle(&class))
        }
        "GetObjectClass" => {
            let object = a.u32();
            let class = shared().state.jni.class_name(object);
            Ret::U32(shared().state.jni.class_handle(&class))
        }
        "GetSuperclass" => Ret::U32(shared().state.jni.class_handle("java/lang/Object")),
        "IsAssignableFrom" | "IsInstanceOf" => Ret::U32(1),
        "NewGlobalRef" | "NewLocalRef" | "NewWeakGlobalRef" => Ret::U32(a.u32()),
        "DeleteGlobalRef" | "DeleteLocalRef" | "DeleteWeakGlobalRef" => Ret::Void,
        "IsSameObject" => {
            let (x, y) = (a.u32(), a.u32());
            Ret::U32((x == y) as u32)
        }
        "PushLocalFrame" | "EnsureLocalCapacity" | "MonitorEnter" | "MonitorExit" => Ret::U32(0),
        "PopLocalFrame" => Ret::U32(a.u32()),
        "ExceptionOccurred" | "ExceptionCheck" => Ret::U32(0),
        "ExceptionClear" | "ExceptionDescribe" => Ret::Void,
        "Throw" | "ThrowNew" => {
            note_java_call(format!("{name} (ignored)"));
            Ret::U32(0)
        }
        "FatalError" => {
            let message = memory.string(a.u32());
            stop_with_fault(uc, format!("JNI FatalError: {message}"));
            Ret::Void
        }
        "GetMethodID" | "GetStaticMethodID" | "GetFieldID" | "GetStaticFieldID" => {
            let (class, member, signature) = (a.u32(), a.u32(), a.u32());
            let class = shared().state.jni.class_name(class);
            let (member, signature) = (memory.string(member), memory.string(signature));
            note_java_call(format!("{name} {class}.{member}{signature}"));
            let object = if name.contains("Method") {
                JavaObject::Method { class, name: member, signature }
            } else {
                JavaObject::Field { class, name: member, signature }
            };
            Ret::U32(shared().state.jni.add(object))
        }
        "AllocObject" | "NewObject" | "NewObjectV" | "NewObjectA" => {
            let class = a.u32();
            let class = shared().state.jni.class_name(class);
            note_java_call(format!("{name} {class}"));
            Ret::U32(shared().state.jni.add(JavaObject::Instance(class)))
        }
        "NewStringUTF" => {
            let text = memory.string(a.u32());
            Ret::U32(shared().state.jni.text(&text))
        }
        "NewString" => {
            let (characters, length) = (a.u32(), a.u32());
            let units: Vec<u16> = (0..length).map(|i| memory.read_u16(characters + i * 2)).collect();
            let text = String::from_utf16_lossy(&units);
            Ret::U32(shared().state.jni.text(&text))
        }
        "GetStringUTFChars" | "GetStringCritical" | "GetStringChars" => {
            let (string, is_copy) = (a.u32(), a.u32());
            let text = match shared().state.jni.get(string) {
                Some(JavaObject::Text(text)) => text.clone(),
                _ => String::new(),
            };
            if is_copy != 0 {
                memory.write_u8(is_copy, 1);
            }
            if name == "GetStringUTFChars" {
                Ret::U32(crate::runtime::heap().strdup(text.as_bytes()))
            } else {
                let units: Vec<u16> = text.encode_utf16().chain([0]).collect();
                let buffer = crate::runtime::heap().malloc(units.len() as u32 * 2);
                for (i, unit) in units.iter().enumerate() {
                    memory.write_u16(buffer + i as u32 * 2, *unit);
                }
                Ret::U32(buffer)
            }
        }
        "ReleaseStringUTFChars" | "ReleaseStringChars" | "ReleaseStringCritical" => {
            let (_string, characters) = (a.u32(), a.u32());
            crate::runtime::heap().free(characters);
            Ret::Void
        }
        "GetStringUTFLength" | "GetStringLength" => {
            let string = a.u32();
            Ret::U32(match shared().state.jni.get(string) {
                Some(JavaObject::Text(text)) if name == "GetStringUTFLength" => text.len() as u32,
                Some(JavaObject::Text(text)) => text.encode_utf16().count() as u32,
                _ => 0,
            })
        }
        "GetStringUTFRegion" => {
            let (string, start, length, buffer) = (a.u32(), a.u32(), a.u32(), a.u32());
            if let Some(JavaObject::Text(text)) = shared().state.jni.get(string).cloned() {
                let part: String = text.chars().skip(start as usize).take(length as usize).collect();
                memory.write_bytes(buffer, part.as_bytes());
                memory.write_u8(buffer + part.len() as u32, 0);
            }
            Ret::Void
        }
        "GetArrayLength" => {
            let array = a.u32();
            Ret::U32(match shared().state.jni.get(array) {
                Some(JavaObject::Array { length, .. }) => *length,
                _ => 0,
            })
        }
        "NewObjectArray" => {
            let (length, _class, initial) = (a.u32(), a.u32(), a.u32());
            let data = crate::runtime::heap().calloc(length.max(1), 4);
            for i in 0..length {
                memory.write_u32(data + i * 4, initial);
            }
            Ret::U32(shared().state.jni.add(JavaObject::Array { element_size: 4, length, data }))
        }
        "GetObjectArrayElement" => {
            let (array, index) = (a.u32(), a.u32());
            Ret::U32(match shared().state.jni.get(array) {
                Some(JavaObject::Array { data, length, .. }) if index < *length => memory.read_u32(data + index * 4),
                _ => 0,
            })
        }
        "SetObjectArrayElement" => {
            let (array, index, value) = (a.u32(), a.u32(), a.u32());
            if let Some(JavaObject::Array { data, length, .. }) = shared().state.jni.get(array) {
                if index < *length {
                    memory.write_u32(data + index * 4, value);
                }
            }
            Ret::Void
        }
        "RegisterNatives" => {
            let (class, methods, count) = (a.u32(), a.u32(), a.u32());
            let class = shared().state.jni.class_name(class);
            for i in 0..count {
                let entry = methods + i * 12;
                let (method, signature) = (memory.string(memory.read_u32(entry)), memory.string(memory.read_u32(entry + 4)));
                let function = memory.read_u32(entry + 8);
                note_java_call(format!("RegisterNatives {class}.{method}{signature} -> {function:#010x}"));
                shared().state.jni.natives.push((class.clone(), method, signature, function));
            }
            Ret::U32(0)
        }
        "UnregisterNatives" => Ret::U32(0),
        "GetJavaVM" => {
            let out = a.u32();
            let vm = shared().state.jni.vm;
            memory.write_u32(out, vm);
            Ret::U32(0)
        }
        "GetPrimitiveArrayCritical" => {
            let (array, is_copy) = (a.u32(), a.u32());
            if is_copy != 0 {
                memory.write_u8(is_copy, 0);
            }
            Ret::U32(match shared().state.jni.get(array) {
                Some(JavaObject::Array { data, .. }) => *data,
                _ => 0,
            })
        }
        "ReleasePrimitiveArrayCritical" => Ret::Void,
        "GetObjectRefType" => Ret::U32(1),
        "GetDirectBufferAddress" | "NewDirectByteBuffer" => Ret::U32(0),
        "GetDirectBufferCapacity" => Ret::U64(u64::MAX),
        _ => {
            note_java_call(format!("{name} (not handled, returned 0)"));
            Ret::U32(0)
        }
    }
}

pub fn handler(_name: &str) -> Option<Handler> {
    handler!(|uc, a| dispatch(uc, a))
}
