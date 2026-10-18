//! NSObject: the root of every class.

use crate::objc::runtime::{self, lock, Class, Id, Sel};
use crate::symbols::Registry;

extern "C" fn alloc(class: Class, _: Sel) -> Id {
    runtime::alloc_instance(class)
}

extern "C" fn alloc_with_zone(class: Class, _: Sel, _zone: usize) -> Id {
    runtime::alloc_instance(class)
}

extern "C" fn new(class: Class, _: Sel) -> Id {
    let object = runtime::send(class, "alloc", &[]);
    runtime::send(object, "init", &[])
}

extern "C" fn return_self(this: Id, _: Sel) -> Id {
    this
}

extern "C" fn nothing(_this: Id, _: Sel) {}

extern "C" fn retain_count(_this: Id, _: Sel) -> usize {
    1
}

extern "C" fn class_of_instance(this: Id, _: Sel) -> Class {
    lock().class_of(this)
}

extern "C" fn superclass_of_instance(this: Id, _: Sel) -> Class {
    let rt = lock();
    let class = rt.class_of(this);
    rt.classes.get(&class).map(|r| r.superclass).unwrap_or(0)
}

extern "C" fn superclass_of_class(this: Class, _: Sel) -> Class {
    lock().classes.get(&this).map(|r| r.superclass).unwrap_or(0)
}

extern "C" fn is_kind_of(this: Id, _: Sel, class: Class) -> bool {
    let rt = lock();
    rt.is_subclass(rt.class_of(this), class)
}

extern "C" fn is_member_of(this: Id, _: Sel, class: Class) -> bool {
    lock().class_of(this) == class
}

extern "C" fn is_subclass_of(this: Class, _: Sel, class: Class) -> bool {
    lock().is_subclass(this, class)
}

extern "C" fn responds_to(this: Id, _: Sel, selector: Sel) -> bool {
    let class = lock().class_of(this);
    lock().lookup(class, selector).is_some()
}

extern "C" fn instances_respond_to(this: Class, _: Sel, selector: Sel) -> bool {
    lock().lookup(this, selector).is_some()
}

extern "C" fn conforms_to(this: Id, _: Sel, protocol: usize) -> bool {
    let class = lock().class_of(this);
    let class = if lock().classes.get(&class).map(|r| r.meta).unwrap_or(false) { this } else { class };
    runtime::conforms(class, protocol)
}

extern "C" fn class_conforms_to(this: Class, _: Sel, protocol: usize) -> bool {
    runtime::conforms(this, protocol)
}

extern "C" fn hash(this: Id, _: Sel) -> usize {
    this
}

extern "C" fn is_equal(this: Id, _: Sel, other: Id) -> bool {
    this == other
}

extern "C" fn description(this: Id, _: Sel) -> Id {
    let name = {
        let rt = lock();
        let class = rt.class_of(this);
        rt.class_name(class)
    };
    crate::fw::string::make(&format!("<{name}: {this:#x}>"))
}

extern "C" fn class_description(this: Class, _: Sel) -> Id {
    let name = lock().class_name(this);
    crate::fw::string::make(&name)
}

extern "C" fn copy(this: Id, _: Sel) -> Id {
    runtime::send(this, "copyWithZone:", &[0])
}

extern "C" fn mutable_copy(this: Id, _: Sel) -> Id {
    runtime::send(this, "mutableCopyWithZone:", &[0])
}

extern "C" fn perform(this: Id, _: Sel, selector: Sel) -> Id {
    let class = lock().class_of(this);
    let found = lock().lookup(class, selector);
    match found {
        Some(imp) => unsafe { std::mem::transmute::<usize, extern "C" fn(Id, Sel) -> Id>(imp)(this, selector) },
        None => {
            let name = lock().sel_name(selector);
            runtime::send(this, &name, &[])
        }
    }
}

extern "C" fn perform_with(this: Id, _: Sel, selector: Sel, a: Id) -> Id {
    let name = lock().sel_name(selector);
    runtime::send(this, &name, &[a])
}

extern "C" fn perform_with_with(this: Id, _: Sel, selector: Sel, a: Id, b: Id) -> Id {
    let name = lock().sel_name(selector);
    runtime::send(this, &name, &[a, b])
}

extern "C" fn dealloc(this: Id, _: Sel) {
    // Memory is not given back yet (reference counting comes later).
    let _ = this;
}

// ----- key-value observing: observers are kept; changes are sent by kvo_notify -----

#[derive(Clone)]
struct KeyObserver {
    object: Id,
    key: String,
    observer: Id,
    context: usize,
}

static KEY_OBSERVERS: std::sync::Mutex<Vec<KeyObserver>> = std::sync::Mutex::new(Vec::new());

extern "C" fn add_key_observer(this: Id, _: Sel, observer: Id, key: Id, _options: usize, context: usize) {
    let key = crate::fw::string::get(key);
    KEY_OBSERVERS.lock().unwrap().push(KeyObserver { object: this, key, observer, context });
}

extern "C" fn remove_key_observer(this: Id, _: Sel, observer: Id, key: Id) {
    let key = crate::fw::string::get(key);
    KEY_OBSERVERS.lock().unwrap().retain(|o| !(o.object == this && o.observer == observer && o.key == key));
}

extern "C" fn remove_key_observer_context(this: Id, s: Sel, observer: Id, key: Id, _context: usize) {
    remove_key_observer(this, s, observer, key);
}

/// Tells observers of `key` on `object` that it now holds `value`.
pub fn kvo_notify(object: Id, key: &str, value: Id) {
    let list: Vec<KeyObserver> = KEY_OBSERVERS.lock().unwrap().iter().filter(|o| o.object == object && o.key == key).cloned().collect();
    for o in list {
        let change = crate::fw::collections::dictionary_from(&[
            (crate::fw::string::make("new"), value),
            (crate::fw::string::make("kind"), crate::fw::collections::number_int(1)),
        ]);
        runtime::send(o.observer, "observeValueForKeyPath:ofObject:change:context:", &[crate::fw::string::make(key), object, change, o.context]);
    }
}

pub fn define(_: &mut Registry) -> Class {
    let class = lock().define_class("NSObject", 0, 8);
    let m = |name: &str, imp: usize| {
        let mut rt = lock();
        let s = rt.sel(name);
        rt.add_method(class, s, imp);
    };
    m("init", return_self as usize);
    m("self", return_self as usize);
    m("retain", return_self as usize);
    m("autorelease", return_self as usize);
    m("release", nothing as usize);
    m("dealloc", dealloc as usize);
    m("retainCount", retain_count as usize);
    m("class", class_of_instance as usize);
    m("superclass", superclass_of_instance as usize);
    m("isKindOfClass:", is_kind_of as usize);
    m("isMemberOfClass:", is_member_of as usize);
    m("respondsToSelector:", responds_to as usize);
    m("conformsToProtocol:", conforms_to as usize);
    m("hash", hash as usize);
    m("isEqual:", is_equal as usize);
    m("description", description as usize);
    m("debugDescription", description as usize);
    m("copy", copy as usize);
    m("mutableCopy", mutable_copy as usize);
    m("performSelector:", perform as usize);
    m("performSelector:withObject:", perform_with as usize);
    m("performSelector:withObject:withObject:", perform_with_with as usize);
    m("zone", hash_zero as usize);
    m("addObserver:forKeyPath:options:context:", add_key_observer as usize);
    m("removeObserver:forKeyPath:", remove_key_observer as usize);
    m("removeObserver:forKeyPath:context:", remove_key_observer_context as usize);
    m("willChangeValueForKey:", ignore_one as usize);
    m("didChangeValueForKey:", ignore_one as usize);
    let meta = lock().classes[&class].partner;
    let c = |name: &str, imp: usize| {
        let mut rt = lock();
        let s = rt.sel(name);
        rt.add_method(meta, s, imp);
    };
    c("alloc", alloc as usize);
    c("allocWithZone:", alloc_with_zone as usize);
    c("new", new as usize);
    c("class", return_self as usize);
    c("superclass", superclass_of_class as usize);
    c("initialize", nothing as usize);
    c("load", nothing as usize);
    c("isSubclassOfClass:", is_subclass_of as usize);
    c("instancesRespondToSelector:", instances_respond_to as usize);
    c("conformsToProtocol:", class_conforms_to as usize);
    c("description", class_description as usize);
    c("copyWithZone:", return_self_with as usize);
    class
}

extern "C" fn ignore_one(_this: Id, _: Sel, _value: usize) {}

extern "C" fn hash_zero(_this: Id, _: Sel) -> usize {
    0
}

extern "C" fn return_self_with(this: Id, _: Sel, _a: usize) -> Id {
    this
}

/// Adds instance methods to one of our classes.
pub fn methods(class: Class, list: &[(&str, usize)]) {
    let mut rt = lock();
    for (name, imp) in list {
        let s = rt.sel(name);
        rt.add_method(class, s, *imp);
    }
}

/// Adds class methods to one of our classes.
pub fn class_methods(class: Class, list: &[(&str, usize)]) {
    let mut rt = lock();
    let meta = rt.classes[&class].partner;
    for (name, imp) in list {
        let s = rt.sel(name);
        rt.add_method(meta, s, *imp);
    }
}

pub fn define_class(name: &str, parent: &str, instance_size: usize) -> Class {
    let mut rt = lock();
    let parent = rt.by_name.get(parent).copied().unwrap_or_else(|| panic!("parent class {parent} not defined yet"));
    rt.define_class(name, parent, instance_size)
}
