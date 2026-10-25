//! Apple's runtime calls for looking at and changing classes while the game
//! runs: methods (swizzling), instance variables, properties, protocols and
//! classes made at run time.

use super::runtime::{self, cstr, lock, Class, ClassObject, Id, Imp, Sel};
use crate::symbols::{leak, leak_cstr, Registry};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

unsafe fn read<T: Copy>(address: usize) -> T {
    std::ptr::read_unaligned(address as *const T)
}

/// A class's compiled description (class_ro_t).
fn class_ro(class: Class) -> usize {
    unsafe { (*(class as *const ClassObject)).data & !7 }
}

// ----- methods -----

/// Our own lasting copy of a C string (Apple copies a new method's type
/// string, so the caller may free or reuse its buffer).
fn copy_cstr(address: usize) -> usize {
    if address == 0 {
        return 0;
    }
    let bytes = unsafe { std::ffi::CStr::from_ptr(address as *const libc::c_char) }.to_bytes_with_nul();
    Box::leak(bytes.to_vec().into_boxed_slice()).as_ptr() as usize
}

extern "C" fn get_instance_method(class: Class, sel: Sel) -> usize {
    if class == 0 || sel == 0 {
        return 0;
    }
    let mut rt = lock();
    match rt.owner_of(class, sel) {
        Some(owner) => rt.method_object(owner, sel, 0),
        None => 0,
    }
}

extern "C" fn get_class_method(class: Class, sel: Sel) -> usize {
    if class == 0 {
        return 0;
    }
    let meta = {
        let rt = lock();
        match rt.classes.get(&class) {
            Some(rec) if rec.meta => class,
            Some(rec) => rec.partner,
            None => return 0,
        }
    };
    get_instance_method(meta, sel)
}

/// Adds a method unless the class itself already has one by that name (a
/// parent's method is overridden, as on Apple's runtime).
extern "C" fn add_method(class: Class, sel: Sel, imp: Imp, types: usize) -> bool {
    if class == 0 || sel == 0 {
        return false;
    }
    let mut rt = lock();
    match rt.classes.get(&class) {
        Some(rec) if !rec.methods.contains_key(&sel) => {}
        _ => return false,
    }
    rt.add_method(class, sel, imp);
    rt.method_object(class, sel, copy_cstr(types));
    true
}

/// Gives the class's own old code for the method, or 0 if it was added.
extern "C" fn replace_method(class: Class, sel: Sel, imp: Imp, types: usize) -> Imp {
    if class == 0 || sel == 0 {
        return 0;
    }
    let mut rt = lock();
    let Some(rec) = rt.classes.get(&class) else { return 0 };
    let old = rec.methods.get(&sel).copied();
    rt.add_method(class, sel, imp);
    if old.is_none() {
        rt.method_object(class, sel, copy_cstr(types));
    }
    old.unwrap_or(0)
}

extern "C" fn method_set_implementation(method: usize, imp: Imp) -> Imp {
    if method == 0 || imp == 0 {
        return 0;
    }
    lock().set_method_imp(method, imp)
}

// ----- class facts -----

extern "C" fn get_superclass(class: Class) -> Class {
    if class == 0 {
        return 0;
    }
    match lock().classes.get(&class) {
        Some(rec) => rec.superclass,
        None => unsafe { (*(class as *const ClassObject)).superclass },
    }
}

extern "C" fn is_meta_class(class: Class) -> bool {
    if class == 0 {
        return false;
    }
    match lock().classes.get(&class) {
        Some(rec) => rec.meta,
        None => unsafe { read::<u32>(class_ro(class)) & 1 != 0 },
    }
}

extern "C" fn get_meta_class(name: *const libc::c_char) -> Class {
    let name = unsafe { cstr(name as usize) };
    let rt = lock();
    match rt.by_name.get(&name).and_then(|c| rt.classes.get(c)) {
        Some(rec) => rec.partner,
        None => {
            crate::log!("objc_getMetaClass: no class {name}");
            0
        }
    }
}

extern "C" fn get_required_class(name: *const libc::c_char) -> Class {
    let name = unsafe { cstr(name as usize) };
    let class = runtime::class_named(&name);
    if class == 0 {
        crate::log!("STOP: objc_getRequiredClass: no class {name}");
        crate::report::finish(1);
    }
    class
}

/// Names of the classes in one loaded program, in a malloc'd list ending in
/// NULL; the names themselves stay owned by the program.
extern "C" fn copy_class_names_for_image(image: *const libc::c_char, count: *mut u32) -> usize {
    let set_count = |n: usize| {
        if !count.is_null() {
            unsafe { *count = n as u32 };
        }
    };
    set_count(0);
    if image.is_null() {
        return 0;
    }
    let path = unsafe { cstr(image as usize) };
    if path != crate::fw::paths::apple_executable_path() {
        crate::log!("objc_copyClassNamesForImage: {path} is not a loaded program, no classes");
        return 0;
    }
    let names: Vec<usize> = runtime::image_classes().into_iter().map(|c| unsafe { read::<usize>(class_ro(c) + 24) }).collect();
    if names.is_empty() {
        return 0;
    }
    let list = unsafe { libc::calloc(names.len() + 1, 8) } as *mut usize;
    for (index, &name) in names.iter().enumerate() {
        unsafe { *list.add(index) = name };
    }
    set_count(names.len());
    list as usize
}

// ----- classes made at run time -----

/// Classes made by objc_allocateClassPair/objc_initializeClassPair that are
/// not registered yet (their names can't be found until then).
static PENDING: Mutex<Option<HashSet<Class>>> = Mutex::new(None);

/// A new name and a usable parent (none for a new root class), as Apple asks.
fn may_make_class(rt: &runtime::Runtime, name: &str, superclass: Class) -> bool {
    let taken = rt.by_name.get(name).and_then(|c| rt.classes.get(c)).map(|r| !r.placeholder).unwrap_or(false);
    let parent_ok = superclass == 0 || rt.classes.get(&superclass).map(|r| !r.meta).unwrap_or(false);
    !taken && parent_ok
}

fn make_class_pair(superclass: Class, name: *const libc::c_char, class: Class, meta: Class) -> Class {
    if name.is_null() {
        return 0;
    }
    let name = unsafe { cstr(name as usize) };
    let mut rt = lock();
    if !may_make_class(&rt, &name, superclass) {
        crate::log!("objc_allocateClassPair {name}: name in use or parent unknown");
        return 0;
    }
    rt.build_class_pair(class, meta, &name, superclass, 0);
    PENDING.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashSet::new).insert(class);
    class
}

/// Class and metaclass each get `extra` bytes after them (object_getIndexedIvars).
extern "C" fn allocate_class_pair(superclass: Class, name: *const libc::c_char, extra: usize) -> Class {
    let size = std::mem::size_of::<ClassObject>() + extra;
    let class = unsafe { libc::calloc(1, size) } as usize;
    let meta = unsafe { libc::calloc(1, size) } as usize;
    let made = make_class_pair(superclass, name, class, meta);
    if made == 0 {
        unsafe {
            libc::free(class as *mut _);
            libc::free(meta as *mut _);
        }
    }
    made
}

extern "C" fn initialize_class_pair(superclass: Class, name: *const libc::c_char, class: Class, meta: Class) -> Class {
    if class == 0 || meta == 0 {
        return 0;
    }
    make_class_pair(superclass, name, class, meta)
}

extern "C" fn register_class_pair(class: Class) {
    let pending = PENDING.lock().unwrap_or_else(|e| e.into_inner()).as_mut().map(|p| p.remove(&class)).unwrap_or(false);
    let mut rt = lock();
    let Some(name) = rt.classes.get(&class).map(|r| r.name.clone()) else { return };
    if !pending {
        crate::log!("objc_registerClassPair: {name} was not made by objc_allocateClassPair or is registered already");
        return;
    }
    // Until now the class's description may have changed (code that copies a
    // compiled class writes its field list and final size there), so the
    // sizes objects get are taken from it now.
    let meta = rt.classes[&class].partner;
    for c in [class, meta] {
        let size = unsafe { read::<u32>(class_ro(c) + 8) } as usize;
        if let Some(rec) = rt.classes.get_mut(&c) {
            rec.instance_size = size.max(8);
        }
    }
    if let Some(old) = rt.by_name.insert(name.clone(), class) {
        crate::log!("objc_registerClassPair: {name} replaces a stand-in class ({old:#x})");
    }
}

extern "C" fn read_class_pair(class: Class, _image_info: usize) -> Class {
    if class == 0 {
        return 0;
    }
    runtime::read_class_pair(class)
}

/// Sets the class pointer and runs the C++ constructors of the class and its
/// parents (root first). The memory must already be zeroed.
extern "C" fn construct_instance(class: Class, bytes: usize) -> Id {
    if class == 0 || bytes == 0 {
        return 0;
    }
    unsafe { *(bytes as *mut usize) = class };
    let construct = runtime::sel(".cxx_construct");
    let constructors: Vec<Imp> = {
        let rt = lock();
        let mut list = Vec::new();
        let mut current = class;
        while let Some(rec) = rt.classes.get(&current) {
            list.extend(rec.methods.get(&construct).copied());
            current = rec.superclass;
        }
        list
    };
    for imp in constructors.into_iter().rev() {
        let f: extern "C" fn(Id, Sel) -> Id = unsafe { std::mem::transmute(imp) };
        if f(bytes, construct) == 0 {
            return 0;
        }
    }
    bytes
}

/// NSAllocateObject(class, extra bytes, zone): the root allocator (no +alloc
/// runs): a zeroed instance with `extra` more bytes, C++ constructors run.
extern "C" fn allocate_object(class: Class, extra: usize, _zone: usize) -> Id {
    if class == 0 {
        return 0;
    }
    let size = lock().classes.get(&class).map(|r| r.instance_size).unwrap_or(16).max(16);
    let bytes = unsafe { libc::calloc(1, size + extra) } as usize;
    construct_instance(class, bytes)
}

/// NSDefaultMallocZone(): zones are only names here (every allocation uses malloc).
extern "C" fn default_malloc_zone() -> usize {
    static ZONE: [usize; 4] = [0; 4];
    ZONE.as_ptr() as usize
}

// ----- instance variables -----

/// ivar_t { int32_t *offset; const char *name; const char *type; u32 alignment; u32 size; }
pub(crate) extern "C" fn get_instance_variable(class: Class, name: *const libc::c_char) -> usize {
    if class == 0 || name.is_null() {
        return 0;
    }
    let wanted = unsafe { cstr(name as usize) };
    let rt = lock();
    let mut current = class;
    while let Some(rec) = rt.classes.get(&current) {
        unsafe {
            let ivars = read::<usize>(class_ro(current) + 48);
            if ivars != 0 {
                let entry_size = read::<u32>(ivars) as usize;
                let count = read::<u32>(ivars + 4) as usize;
                for index in 0..count {
                    let entry = ivars + 8 + index * entry_size;
                    if cstr(read::<usize>(entry + 8)) == wanted {
                        return entry;
                    }
                }
            }
        }
        current = rec.superclass;
    }
    0
}

extern "C" fn object_get_ivar(object: Id, ivar: usize) -> Id {
    if object == 0 || ivar == 0 {
        return 0;
    }
    unsafe {
        let offset_cell = read::<usize>(ivar);
        let offset = if offset_cell == 0 { 0 } else { read::<i32>(offset_cell) as isize };
        read::<usize>((object as isize + offset) as usize)
    }
}

/// The bytes after the object's own fields (its class's size, word-aligned).
extern "C" fn object_get_indexed_ivars(object: Id) -> usize {
    if object == 0 {
        return 0;
    }
    let rt = lock();
    match rt.classes.get(&rt.class_of(object)) {
        Some(rec) => object + ((rec.instance_size + 7) & !7),
        None => 0,
    }
}

// ----- protocols -----

extern "C" fn get_protocol(name: *const libc::c_char) -> usize {
    runtime::protocol_named(&unsafe { cstr(name as usize) })
}

extern "C" fn protocol_get_name(protocol: usize) -> usize {
    static NIL: &[u8] = b"nil\0";
    if protocol == 0 {
        return NIL.as_ptr() as usize;
    }
    unsafe { read::<usize>(protocol + 8) }
}

#[repr(C)]
pub struct MethodDescription {
    name: Sel,
    types: usize,
}

/// Looks through the protocol's method list of one kind, then the
/// protocols it includes.
fn find_description(protocol: usize, name: &str, required: bool, instance: bool, depth: usize) -> Option<usize> {
    if protocol == 0 || depth > 16 {
        return None;
    }
    let offset = match (required, instance) {
        (true, true) => 24,
        (true, false) => 32,
        (false, true) => 40,
        (false, false) => 48,
    };
    unsafe {
        let list = read::<usize>(protocol + offset);
        if list != 0 {
            let entry_size = (read::<u32>(list) & !3) as usize;
            let count = read::<u32>(list + 4) as usize;
            for index in 0..count {
                let entry = list + 8 + index * entry_size;
                if cstr(read::<usize>(entry)) == name {
                    return Some(read::<usize>(entry + 8));
                }
            }
        }
        let included = read::<usize>(protocol + 16);
        if included != 0 {
            let count = read::<u64>(included) as usize;
            for index in 0..count {
                let child = read::<usize>(included + 8 + index * 8);
                if let Some(types) = find_description(child, name, required, instance, depth + 1) {
                    return Some(types);
                }
            }
        }
    }
    None
}

extern "C" fn protocol_get_method_description(protocol: usize, sel: Sel, required: u8, instance: u8) -> MethodDescription {
    if protocol == 0 || sel == 0 {
        return MethodDescription { name: 0, types: 0 };
    }
    let name = lock().sel_name(sel);
    match find_description(protocol, &name, required != 0, instance != 0, 0) {
        Some(types) => MethodDescription { name: sel, types },
        None => MethodDescription { name: 0, types: 0 },
    }
}

/// A malloc'd array of records of `record_words` 8-byte words each, ended by
/// one all-zero record as Apple's lists are (callers without a count walk to
/// it), or 0 for none. The caller frees it with free().
fn malloc_list(items: &[usize], record_words: usize, count: *mut u32) -> usize {
    if !count.is_null() {
        unsafe { *count = (items.len() / record_words) as u32 };
    }
    if items.is_empty() {
        return 0;
    }
    let list = unsafe { libc::calloc(items.len() + record_words, 8) } as *mut usize;
    for (index, &item) in items.iter().enumerate() {
        unsafe { *list.add(index) = item };
    }
    list as usize
}

/// The protocol's own methods of one kind (not those of the protocols it
/// includes) as objc_method_description { SEL name; char *types; } pairs.
extern "C" fn protocol_copy_method_descriptions(protocol: usize, required: u8, instance: u8, count: *mut u32) -> usize {
    if protocol == 0 {
        return malloc_list(&[], 2, count);
    }
    let offset = match (required != 0, instance != 0) {
        (true, true) => 24,
        (true, false) => 32,
        (false, true) => 40,
        (false, false) => 48,
    };
    let mut words = Vec::new();
    unsafe {
        let list = read::<usize>(protocol + offset);
        if list != 0 {
            let entry_size = (read::<u32>(list) & !3) as usize;
            let mut rt = lock();
            for index in 0..read::<u32>(list + 4) as usize {
                let entry = list + 8 + index * entry_size;
                words.push(rt.sel(&cstr(read::<usize>(entry))));
                words.push(read::<usize>(entry + 8));
            }
        }
    }
    malloc_list(&words, 2, count)
}

/// The protocols this one includes, as the runtime knows them by name.
extern "C" fn protocol_copy_protocols(protocol: usize, count: *mut u32) -> usize {
    let mut found = Vec::new();
    if protocol != 0 {
        unsafe {
            let included = read::<usize>(protocol + 16);
            if included != 0 {
                for index in 0..read::<u64>(included) as usize {
                    let child = read::<usize>(included + 8 + index * 8);
                    let known = runtime::protocol_named(&cstr(read::<usize>(child + 8)));
                    found.push(if known != 0 { known } else { child });
                }
            }
        }
    }
    malloc_list(&found, 1, count)
}

/// Adds a protocol unless the class itself already takes it on.
extern "C" fn add_protocol(class: Class, protocol: usize) -> bool {
    if class == 0 || protocol == 0 {
        return false;
    }
    let target = unsafe { cstr(read::<usize>(protocol + 8)) };
    let mut rt = lock();
    let Some(rec) = rt.classes.get_mut(&class) else { return false };
    if rec.protocols.iter().any(|&p| runtime::protocol_matches(p, &target, 0)) {
        return false;
    }
    rec.protocols.push(protocol);
    true
}

// ----- properties -----

/// Properties added with class_addProperty: property_t { name; attributes }.
static ADDED_PROPERTIES: Mutex<Option<HashMap<Class, Vec<(String, usize)>>>> = Mutex::new(None);

/// Whether the class or a parent declares the property (compiled or added).
fn has_property(class: Class, name: &str) -> bool {
    let added = ADDED_PROPERTIES.lock().unwrap_or_else(|e| e.into_inner());
    let rt = lock();
    let mut current = class;
    while let Some(rec) = rt.classes.get(&current) {
        if added.as_ref().and_then(|m| m.get(&current)).map(|l| l.iter().any(|(n, _)| n == name)).unwrap_or(false) {
            return true;
        }
        unsafe {
            let list = read::<usize>(class_ro(current) + 64);
            if list != 0 {
                let entry_size = read::<u32>(list) as usize;
                let count = read::<u32>(list + 4) as usize;
                if (0..count).any(|i| cstr(read::<usize>(list + 8 + i * entry_size)) == name) {
                    return true;
                }
            }
        }
        current = rec.superclass;
    }
    false
}

/// Apple's attribute string: one letter (or a "quoted" longer name) then
/// the value, separated by commas, e.g. T@"NSString",C,V_title.
pub fn attribute_string(attributes: &[(String, String)]) -> String {
    attributes
        .iter()
        .map(|(name, value)| if name.len() > 1 { format!("\"{name}\"{value}") } else { format!("{name}{value}") })
        .collect::<Vec<_>>()
        .join(",")
}

/// Reads an attribute string back into (name, value) pairs.
pub fn parse_attributes(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for part in text.split(',').filter(|p| !p.is_empty()) {
        if let Some(rest) = part.strip_prefix('"') {
            let end = rest.find('"').unwrap_or(rest.len());
            let value = rest.get(end + 1..).unwrap_or("");
            out.push((rest[..end].to_string(), value.to_string()));
        } else {
            let split = part.chars().next().map(|c| c.len_utf8()).unwrap_or(0);
            out.push((part[..split].to_string(), part[split..].to_string()));
        }
    }
    out
}

/// objc_property_attribute_t { const char *name; const char *value; }
unsafe fn read_attribute_list(list: usize, count: u32) -> Vec<(String, String)> {
    (0..count as usize)
        .map(|i| (cstr(read::<usize>(list + i * 16)), cstr(read::<usize>(list + i * 16 + 8))))
        .collect()
}

extern "C" fn add_property(class: Class, name: *const libc::c_char, attributes: usize, count: u32) -> bool {
    if class == 0 || name.is_null() || !lock().classes.contains_key(&class) {
        return false;
    }
    let name = unsafe { cstr(name as usize) };
    if has_property(class, &name) {
        return false;
    }
    let pairs = if attributes == 0 { Vec::new() } else { unsafe { read_attribute_list(attributes, count) } };
    let property = leak([leak_cstr(&name), leak_cstr(&attribute_string(&pairs))]);
    ADDED_PROPERTIES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .entry(class)
        .or_default()
        .push((name, property));
    true
}

/// One malloc'd block: the attribute records, an empty record, then the
/// strings they point at (the caller frees it with free()).
extern "C" fn copy_attribute_list(property: usize, count: *mut u32) -> usize {
    let set_count = |n: usize| {
        if !count.is_null() {
            unsafe { *count = n as u32 };
        }
    };
    set_count(0);
    if property == 0 {
        return 0;
    }
    let pairs = parse_attributes(&unsafe { cstr(read::<usize>(property + 8)) });
    if pairs.is_empty() {
        return 0;
    }
    let table = (pairs.len() + 1) * 16;
    let text: usize = pairs.iter().map(|(n, v)| n.len() + v.len() + 2).sum();
    let block = unsafe { libc::calloc(1, table + text) } as usize;
    let mut cursor = block + table;
    let mut put = |s: &str| unsafe {
        std::ptr::copy_nonoverlapping(s.as_ptr(), cursor as *mut u8, s.len());
        let at = cursor;
        cursor += s.len() + 1;
        at
    };
    for (index, (name, value)) in pairs.iter().enumerate() {
        let (n, v) = (put(name), put(value));
        unsafe {
            *((block + index * 16) as *mut usize) = n;
            *((block + index * 16 + 8) as *mut usize) = v;
        }
    }
    set_count(pairs.len());
    block
}

pub fn register(r: &mut Registry) {
    r.function("class_getInstanceMethod", get_instance_method as usize);
    r.function("class_getClassMethod", get_class_method as usize);
    r.function("class_addMethod", add_method as usize);
    r.function("class_replaceMethod", replace_method as usize);
    r.function("method_setImplementation", method_set_implementation as usize);
    r.function("class_getSuperclass", get_superclass as usize);
    r.function("class_isMetaClass", is_meta_class as usize);
    r.function("objc_getMetaClass", get_meta_class as usize);
    r.function("objc_getRequiredClass", get_required_class as usize);
    r.function("objc_copyClassNamesForImage", copy_class_names_for_image as usize);
    r.function("objc_allocateClassPair", allocate_class_pair as usize);
    r.function("objc_initializeClassPair", initialize_class_pair as usize);
    r.function("objc_registerClassPair", register_class_pair as usize);
    r.function("objc_readClassPair", read_class_pair as usize);
    r.function("objc_constructInstance", construct_instance as usize);
    r.function("NSAllocateObject", allocate_object as usize);
    r.function("NSDefaultMallocZone", default_malloc_zone as usize);
    r.function("class_getInstanceVariable", get_instance_variable as usize);
    r.function("object_getIvar", object_get_ivar as usize);
    r.function("object_getIndexedIvars", object_get_indexed_ivars as usize);
    r.function("objc_getProtocol", get_protocol as usize);
    r.function("protocol_getName", protocol_get_name as usize);
    r.function("protocol_getMethodDescription", protocol_get_method_description as usize);
    r.function("protocol_copyMethodDescriptionList", protocol_copy_method_descriptions as usize);
    r.function("protocol_copyProtocolList", protocol_copy_protocols as usize);
    r.function("class_addProtocol", add_protocol as usize);
    r.function("class_addProperty", add_property as usize);
    r.function("property_copyAttributeList", copy_attribute_list as usize);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    extern "C" fn answer_one(_: Id, _: Sel) -> usize {
        1
    }
    extern "C" fn answer_two(_: Id, _: Sel) -> usize {
        2
    }
    extern "C" fn answer_three(_: Id, _: Sel) -> usize {
        3
    }

    fn new_class(name: &str, superclass: Class) -> Class {
        let c = CString::new(name).unwrap();
        let class = allocate_class_pair(superclass, c.as_ptr(), 0);
        register_class_pair(class);
        class
    }

    fn call(object: Id, name: &str) -> usize {
        runtime::send(object, name, &[])
    }

    /// Swizzling a parent's method after its answer was cached reaches
    /// both the parent and a child that inherits it.
    #[test]
    fn set_implementation_reaches_cached_callers() {
        let parent = new_class("ReflectTestParent", 0);
        let child = new_class("ReflectTestChild", parent);
        let s = runtime::sel("reflectTestValue");
        assert!(add_method(parent, s, answer_one as usize, 0));
        let object = runtime::alloc_instance(child);
        assert_eq!(call(object, "reflectTestValue"), 1);
        let method = get_instance_method(child, s);
        assert_ne!(method, 0);
        assert_eq!(method, get_instance_method(parent, s));
        assert_eq!(method_set_implementation(method, answer_two as usize), answer_one as usize);
        assert_eq!(call(object, "reflectTestValue"), 2);
        assert_eq!(call(runtime::alloc_instance(parent), "reflectTestValue"), 2);
    }

    /// class_addMethod overrides a parent but refuses a second add;
    /// class_replaceMethod swaps it, and the Method handle follows.
    #[test]
    fn add_and_replace_keep_tables_and_handles_in_step() {
        let parent = new_class("ReflectTestBase", 0);
        let child = new_class("ReflectTestDerived", parent);
        let s = runtime::sel("reflectTestOther");
        assert!(add_method(parent, s, answer_one as usize, 0));
        let object = runtime::alloc_instance(child);
        assert_eq!(call(object, "reflectTestOther"), 1);
        assert!(add_method(child, s, answer_two as usize, 0));
        assert!(!add_method(child, s, answer_three as usize, 0));
        assert_eq!(call(object, "reflectTestOther"), 2);
        let method = get_instance_method(child, s);
        assert_eq!(replace_method(child, s, answer_three as usize, 0), answer_two as usize);
        assert_eq!(call(object, "reflectTestOther"), 3);
        assert_eq!(unsafe { (*(method as *const runtime::MethodObject)).imp }, answer_three as usize);
        assert_eq!(call(runtime::alloc_instance(parent), "reflectTestOther"), 1);
    }

    /// A class made at run time is hidden until registered, and its class
    /// object has room for the extra bytes asked for.
    #[test]
    fn class_pairs_register_and_give_extra_bytes() {
        let name = CString::new("ReflectTestMade").unwrap();
        let class = allocate_class_pair(0, name.as_ptr(), 32);
        assert_ne!(class, 0);
        assert_eq!(runtime::class_named("ReflectTestMade"), 0);
        register_class_pair(class);
        assert_eq!(runtime::class_named("ReflectTestMade"), class);
        assert_eq!(allocate_class_pair(0, name.as_ptr(), 0), 0);
        assert!(!is_meta_class(class));
        let meta = get_meta_class(name.as_ptr());
        assert!(is_meta_class(meta));
        assert_eq!(object_get_indexed_ivars(class), class + std::mem::size_of::<ClassObject>());
    }

    fn isa(object: usize) -> usize {
        unsafe { (*(object as *const ClassObject)).isa }
    }

    /// Code that copies a compiled class into a new pair reads where the
    /// parent's fields end, writes its own field list and final size, then
    /// registers; objects then get that size and the field is found.
    #[test]
    fn copied_class_metadata_sets_sizes() {
        let parent = lock().define_class("ReflectTestSized", 0, 24);
        let name = CString::new("ReflectTestCopied").unwrap();
        let class = allocate_class_pair(parent, name.as_ptr(), 0);
        let ro = class_ro(class);
        let meta = lock().classes[&class].partner;
        unsafe {
            assert_eq!((read::<u32>(ro + 4), read::<u32>(ro + 8)), (24, 24));
            assert_eq!((read::<u32>(class_ro(meta) + 4), read::<u32>(class_ro(meta) + 8)), (40, 40));
            // ivar_list_t { entsize 32, count 1 } + ivar_t { offset*, name, type, alignment 3, size 8 }
            let ivars = leak([
                32 | (1usize << 32),
                leak(24i32),
                leak_cstr("_extra"),
                leak_cstr("@"),
                3 | (8usize << 32),
            ]);
            *((ro + 48) as *mut usize) = ivars;
            *((ro + 8) as *mut u32) = 32;
            register_class_pair(class);
            assert_eq!(lock().classes[&class].instance_size, 32);
            let object = runtime::alloc_instance(class);
            *((object + 24) as *mut usize) = 0x1234;
            assert_eq!(object_get_indexed_ivars(object), object + 32);
            let field = CString::new("_extra").unwrap();
            let ivar = get_instance_variable(class, field.as_ptr());
            assert_eq!(ivar, ivars + 8);
            assert_eq!(object_get_ivar(object, ivar), 0x1234);
        }
    }

    /// Every hierarchy's metaclasses point at its own root metaclass, and a
    /// compiler-laid-out root class can be read in.
    #[test]
    fn root_classes_keep_their_own_hierarchy() {
        let root_name = CString::new("ReflectTestRootA").unwrap();
        let root = allocate_class_pair(0, root_name.as_ptr(), 0);
        let sizes = |c: Class| unsafe { (read::<u32>(class_ro(c) + 4), read::<u32>(class_ro(c) + 8)) };
        let first_meta = lock().classes[&root].partner;
        assert_eq!(sizes(root), (0, 8));
        assert_eq!(sizes(first_meta), (40, 40));
        // The root metaclass grows (as class_addIvar on it would); its
        // children's metaclasses start and end where it ends.
        unsafe { *((class_ro(first_meta) + 8) as *mut u32) = 64 };
        register_class_pair(root);
        let child = new_class("ReflectTestRootAChild", root);
        let child_meta = lock().classes[&child].partner;
        assert_eq!(sizes(child_meta), (64, 64));
        assert_eq!(object_get_indexed_ivars(child), child + 64);
        let other = new_class("ReflectTestRootB", 0);
        let (root_meta, child_meta, other_meta) = {
            let rt = lock();
            (rt.classes[&root].partner, rt.classes[&child].partner, rt.classes[&other].partner)
        };
        assert_eq!(isa(root_meta), root_meta);
        assert_eq!(get_superclass(root_meta), root);
        assert_eq!(isa(child_meta), root_meta);
        assert_eq!(isa(other_meta), other_meta);
        assert_ne!(other_meta, root_meta);

        // class_ro_t words: flags | instance_start << 32, instance_size, ivar_layout, name, ...
        let name = leak_cstr("ReflectTestReadRoot");
        let class_ro_words = leak([2usize, 8, 0, name, 0, 0, 0, 0, 0]);
        let meta_ro_words = leak([3usize, 40, 0, name, 0, 0, 0, 0, 0]);
        let class = leak(ClassObject { isa: 0, superclass: 0, cache: 0, vtable: 0, data: class_ro_words });
        let meta = leak(ClassObject { isa: 0, superclass: class, cache: 0, vtable: 0, data: meta_ro_words });
        unsafe {
            (*(class as *mut ClassObject)).isa = meta;
            (*(meta as *mut ClassObject)).isa = meta;
        }
        assert_eq!(read_class_pair(class, 0), class);
        assert_eq!(runtime::class_named("ReflectTestReadRoot"), class);
        assert!(is_meta_class(meta));
        assert_eq!(get_superclass(meta), class);
        let s = runtime::sel("reflectTestRootValue");
        assert!(add_method(class, s, answer_two as usize, 0));
        assert_eq!(call(runtime::alloc_instance(class), "reflectTestRootValue"), 2);
    }

    /// A new method's type string is the runtime's own copy.
    #[test]
    fn method_types_outlive_the_callers_buffer() {
        let class = new_class("ReflectTestTypes", 0);
        let (added, replaced) = (runtime::sel("reflectTestAdded"), runtime::sel("reflectTestReplaced"));
        let mut buffer = *b"v@:\0";
        assert!(add_method(class, added, answer_one as usize, buffer.as_ptr() as usize));
        assert_eq!(replace_method(class, replaced, answer_two as usize, buffer.as_ptr() as usize), 0);
        buffer.copy_from_slice(b"XXX\0");
        std::hint::black_box(&buffer);
        for s in [added, replaced] {
            let method = get_instance_method(class, s);
            let types = unsafe { (*(method as *const runtime::MethodObject)).types };
            assert_eq!(unsafe { cstr(types) }, "v@:");
        }
    }

    /// protocol_t { isa, name, protocols, instanceMethods, classMethods, optional... }
    #[test]
    fn protocol_lists_are_copied() {
        let child = leak([0usize, leak_cstr("ReflectTestChildProtocol"), 0, 0, 0, 0, 0, 0, 0]);
        let entry = |name: &str, types: &str| [leak_cstr(name), leak_cstr(types), 0];
        let (first, second) = (entry("reflectFirst", "v@:"), entry("reflectSecond:", "v@:@"));
        let methods = leak([24 | (2usize << 32), first[0], first[1], 0, second[0], second[1], 0]);
        let included = leak([1usize, child]);
        let protocol = leak([0usize, leak_cstr("ReflectTestProtocol"), included, methods, 0, 0, 0, 0, 0]);
        let mut count = 99u32;
        let list = protocol_copy_method_descriptions(protocol, 1, 1, &mut count);
        assert_eq!(count, 2);
        unsafe {
            // Two pairs, then the zeroed pair that ends the list.
            let pairs = std::slice::from_raw_parts(list as *const usize, 6);
            assert_eq!(pairs, [runtime::sel("reflectFirst"), first[1], runtime::sel("reflectSecond:"), second[1], 0, 0]);
            libc::free(list as *mut _);
        }
        assert_eq!(protocol_copy_method_descriptions(protocol, 0, 1, &mut count), 0);
        assert_eq!(count, 0);
        let list = protocol_copy_protocols(protocol, &mut count);
        assert_eq!(count, 1);
        assert_eq!(unsafe { std::slice::from_raw_parts(list as *const usize, 2) }, [child, 0]);
        unsafe { libc::free(list as *mut _) };
    }

    #[test]
    fn property_attributes_round_trip() {
        let pairs = vec![
            ("T".to_string(), "@\"NSString\"".to_string()),
            ("C".to_string(), String::new()),
            ("custom".to_string(), "x".to_string()),
            ("V".to_string(), "_title".to_string()),
        ];
        let text = attribute_string(&pairs);
        assert_eq!(text, "T@\"NSString\",C,\"custom\"x,V_title");
        assert_eq!(parse_attributes(&text), pairs);
    }
}
