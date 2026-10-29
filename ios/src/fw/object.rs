//! NSObject: the root of every class.

use crate::objc::runtime::{self, lock, Class, Id, Sel};
use crate::symbols::Registry;

/// Like Apple's _objc_rootAlloc: a class with its own +allocWithZone: (a
/// singleton keeping its one copy) is asked through it.
extern "C" fn alloc(class: Class, _: Sel) -> Id {
    match runtime::imp_for(class, runtime::sel("allocWithZone:")) {
        Some(imp) if imp as usize != alloc_with_zone as usize => runtime::send(class, "allocWithZone:", &[0]),
        _ => runtime::alloc_instance(class),
    }
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

// Reference counting: nothing is freed yet, but a 32-bit game's objects are
// counted for the "would dealloc" log (counted.rs).
extern "C" fn retain(this: Id, _: Sel) -> Id {
    crate::fw::counted::game_retain(this);
    this
}

extern "C" fn release(this: Id, _: Sel) {
    crate::fw::counted::game_release(this);
}

extern "C" fn autorelease(this: Id, _: Sel) -> Id {
    crate::fw::counted::game_autorelease(this);
    this
}

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

/// Sends setValue:forKey: for each entry; NSNull stands for nil, as on Apple.
extern "C" fn set_values_for_keys(this: Id, _: Sel, values: Id) {
    let null = runtime::send(runtime::class_named("NSNull"), "null", &[]);
    for (key, value) in crate::fw::collections::dictionary_pairs(values) {
        let value = if value == null { 0 } else { value };
        runtime::send(this, "setValue:forKey:", &[value, key]);
    }
}

extern "C" fn copy(this: Id, _: Sel) -> Id {
    runtime::send(this, "copyWithZone:", &[0])
}

extern "C" fn mutable_copy(this: Id, _: Sel) -> Id {
    runtime::send(this, "mutableCopyWithZone:", &[0])
}

extern "C" fn perform(this: Id, _: Sel, selector: Sel) -> Id {
    // Through runtime::send, as -performSelector:withObject: goes: a setter
    // of an observed key announces the change.
    let name = lock().sel_name(selector);
    runtime::send(this, &name, &[])
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

// ----- key-value observing -----
//
// As Foundation: an observer of an object's key path hears of a change when
// the key's setter (set<Key>:) is sent to that object (automatic
// notification, unless +automaticallyNotifiesObserversForKey: says no),
// when -didChangeValueForKey: follows -willChangeValueForKey:, and when the
// framework changes a value itself (kvo_notify). Apple swaps the object's
// class for a subclass with notifying setters; here the message senders ask
// `observed_setter_key` instead (objc_msgSend, runtime::send, and 32-bit
// games' messages), which gives the same notifications to every sender
// that goes through objc_msgSend. For a key path "a.b" the setter of its
// first key on the object counts, and so do the keys each key depends on
// (+keyPathsForValuesAffecting<Key>).

/// NSKeyValueObservingOptions.
const OPTION_NEW: usize = 1;
const OPTION_OLD: usize = 2;
const OPTION_INITIAL: usize = 4;
const OPTION_PRIOR: usize = 8;

#[derive(Clone)]
struct KeyObserver {
    /// Tells this registration apart from a same-looking one.
    serial: u64,
    object: Id,
    path: String,
    observer: Id,
    options: usize,
    context: usize,
    /// The object each key of the path is read from: `object`, then the
    /// value of the first key, and so on (shorter when one is nil). As
    /// Apple's, a change of any of these keys is a change of the path.
    chain: Vec<Id>,
    /// The keys the path's keys depend on (+keyPathsForValuesAffecting<Key>,
    /// followed to their own objects and dependents): a change of one of
    /// these is a change of the path too, as Apple's.
    extra: Vec<(Id, String)>,
}

impl KeyObserver {
    fn keys(&self) -> impl Iterator<Item = &str> {
        self.path.split('.')
    }

    /// Every (object, key) whose change is a change of this path.
    fn links(&self) -> impl Iterator<Item = (Id, &str)> {
        self.chain.iter().copied().zip(self.keys()).chain(self.extra.iter().map(|(o, k)| (*o, k.as_str())))
    }

    /// Whether `key` of `object` is a link of this path.
    fn watches(&self, object: Id, key: &str) -> bool {
        self.links().any(|(o, k)| o == object && k == key)
    }
}

/// The key paths `key` of `object` depends on: its class's
/// +keyPathsForValuesAffectingValueForKey: (asked once per class and key).
fn affecting_paths(object: Id, key: &str) -> Vec<String> {
    static ASKED: std::sync::Mutex<Option<std::collections::HashMap<(Class, String), Vec<String>>>> = std::sync::Mutex::new(None);
    let class = lock().class_of(object);
    let id = (class, key.to_string());
    if let Some(paths) = ASKED.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&id)) {
        return paths.clone();
    }
    // NSObject's class method: an NSProxy (its own root) has none, and
    // sending it there would fall to the proxy's abstract forwarding.
    let set = if runtime::responds(class, "keyPathsForValuesAffectingValueForKey:") {
        runtime::send(class, "keyPathsForValuesAffectingValueForKey:", &[crate::fw::string::make(key)])
    } else {
        0
    };
    let paths: Vec<String> = if set == 0 {
        Vec::new()
    } else {
        // Apple refuses a key that depends on itself.
        crate::fw::collections::array_items(runtime::send(set, "allObjects", &[])).into_iter().map(crate::fw::string::get).filter(|p| p != key).collect()
    };
    ASKED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Default::default).insert(id, paths.clone());
    paths
}

/// The dependent links of a path's chain (see `KeyObserver::extra`).
fn dependents_of(chain: &[Id], path: &str) -> Vec<(Id, String)> {
    fn add(object: Id, key: &str, out: &mut Vec<(Id, String)>, depth: usize) {
        if depth > 8 {
            return;
        }
        for dependent in affecting_paths(object, key) {
            let chain = chain_of(object, &dependent);
            for (o, k) in chain.iter().zip(dependent.split('.')) {
                if !out.iter().any(|(oo, kk)| oo == o && kk == k) {
                    out.push((*o, k.to_string()));
                    add(*o, k, out, depth + 1);
                }
            }
        }
    }
    let mut out = Vec::new();
    for (object, key) in chain.iter().zip(path.split('.')) {
        add(*object, key, &mut out, 0);
    }
    out
}

/// +keyPathsForValuesAffectingValueForKey:: +keyPathsForValuesAffecting<Key>
/// when the class has it, else an empty set.
extern "C" fn key_paths_affecting(class: Class, _: Sel, key: Id) -> Id {
    let name = format!("keyPathsForValuesAffecting{}", capitalized(&crate::fw::string::get(key)));
    if runtime::responds(class, &name) {
        return runtime::send(class, &name, &[]);
    }
    crate::fw::misc::set_from(&[])
}

/// The objects along `path` from `object` (see `KeyObserver::chain`).
fn chain_of(object: Id, path: &str) -> Vec<Id> {
    let keys: Vec<&str> = path.split('.').collect();
    let mut chain = vec![object];
    for key in &keys[..keys.len() - 1] {
        let next = runtime::send(*chain.last().unwrap(), "valueForKey:", &[crate::fw::string::make(key)]);
        if next == 0 {
            break;
        }
        chain.push(next);
    }
    chain
}

/// Follows the path again after a link changed, so the observer watches the
/// new objects and no longer the old ones.
fn refollow(watchers: &[KeyObserver]) {
    for watcher in watchers.iter().filter(|w| w.path.contains('.') || !w.extra.is_empty()) {
        let chain = chain_of(watcher.object, &watcher.path);
        let extra = dependents_of(&chain, &watcher.path);
        let mut o = observing();
        let o = &mut *o;
        if let Some(w) = o.observers.iter_mut().find(|w| w.serial == watcher.serial) {
            let before = link_setters(w);
            let objects_before = link_objects(w);
            w.chain = chain;
            w.extra = extra;
            let after = link_setters(w);
            let objects_after = link_objects(w);
            // Most changes only swap the objects along the path, not its keys.
            if before != after {
                count_setters(o, before, false);
                count_setters(o, after, true);
            }
            count_links(o, objects_before, false);
            count_links(o, objects_after, true);
        }
    }
}

struct Observing {
    observers: Vec<KeyObserver>,
    /// The setters of the observed keys ("setSpell:", "_setSpell:"), by
    /// name and by selector.
    setters: std::collections::BTreeSet<String>,
    setter_sels: std::collections::BTreeSet<Sel>,
    /// How many links of all observers have each setter name.
    setter_counts: std::collections::BTreeMap<String, usize>,
    /// How many links of all observers each object has: a setter sent to
    /// any other object announces nothing.
    link_objects: std::collections::BTreeMap<Id, usize>,
    next_serial: u64,
}

static OBSERVING: std::sync::Mutex<Observing> =
    std::sync::Mutex::new(Observing { observers: Vec::new(), setters: std::collections::BTreeSet::new(), setter_sels: std::collections::BTreeSet::new(), setter_counts: std::collections::BTreeMap::new(), link_objects: std::collections::BTreeMap::new(), next_serial: 0 });

thread_local! {
    /// Changes this thread announced by -willChangeValueForKey: and not yet
    /// done: the object, key, and each observer's serial with its old value.
    /// Per thread, as Apple's (_NSKeyValuePopPendingNotificationPerThread):
    /// two threads changing the same key keep their own old values.
    static PENDING: std::cell::RefCell<Vec<(Id, String, Vec<(u64, Id)>)>> = const { std::cell::RefCell::new(Vec::new()) };
}
/// How many observers there are: message senders skip all of this at 0.
static OBSERVER_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn observing() -> std::sync::MutexGuard<'static, Observing> {
    OBSERVING.lock().unwrap_or_else(|e| e.into_inner())
}

fn setter_names(key: &str) -> [String; 2] {
    let mut chars = key.chars();
    let capitalized: String = chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default();
    [format!("set{capitalized}:"), format!("_set{capitalized}:")]
}

/// The setter names of an observer's links, one pair per link (so adding
/// and taking away the same links balance).
fn link_setters(w: &KeyObserver) -> Vec<String> {
    w.links().flat_map(|(_, key)| setter_names(key)).collect()
}

/// The objects of an observer's links, one per link.
fn link_objects(w: &KeyObserver) -> Vec<Id> {
    w.links().map(|(object, _)| object).collect()
}

/// Counts link objects in (or out), as `count_setters` does setter names.
fn count_links(o: &mut Observing, objects: Vec<Id>, add: bool) {
    for object in objects {
        let count = o.link_objects.entry(object).or_insert(0);
        if add {
            *count += 1;
        } else {
            *count = count.saturating_sub(1);
            if *count == 0 {
                o.link_objects.remove(&object);
            }
        }
    }
}

#[cfg(test)]
thread_local! {
    /// How often this thread looked through the observers for a setter (tests).
    static WALKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Counts setter names in (or out): the observed setters are the names some
/// link still has. Each observer change only touches its own links, not
/// every observer's.
/// Times a setter became observed (see `count_setters`).
pub static SETTERS_ADDED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn count_setters(o: &mut Observing, names: Vec<String>, add: bool) {
    for name in names {
        let count = o.setter_counts.entry(name.clone()).or_insert(0);
        if add {
            *count += 1;
            if *count == 1 {
                o.setter_sels.insert(runtime::sel(&name));
                o.setters.insert(name);
                // 32-bit games' objc_msgSend lanes may hold this setter.
                SETTERS_ADDED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                crate::guest32::objc::bridge::forget_lanes();
            }
        } else {
            *count = count.saturating_sub(1);
            if *count == 0 {
                o.setter_counts.remove(&name);
                o.setter_sels.remove(&runtime::sel(&name));
                o.setters.remove(&name);
            }
        }
    }
    OBSERVER_COUNT.store(o.observers.len(), std::sync::atomic::Ordering::SeqCst);
}

/// How many KVO registrations name `object`, as observer or as observed
/// (for the "would dealloc" log, counted.rs).
pub fn kvo_registrations_naming(object: Id) -> usize {
    observing().observers.iter().filter(|w| w.observer == object || w.object == object).count()
}

/// Whether anything is observed at all (a cheap check for every message).
pub fn kvo_active() -> bool {
    OBSERVER_COUNT.load(std::sync::atomic::Ordering::Relaxed) != 0
}

/// Whether `sel` is the setter of some observed key (on any object).
pub fn is_observed_setter(sel: Sel) -> bool {
    kvo_active() && observing().setter_sels.contains(&sel)
}

/// The key a message announces a change of: `selector` is the setter of a
/// key someone observes on `object`, and its class notifies automatically.
pub fn observed_setter_key(object: Id, selector: &str) -> Option<String> {
    if !kvo_active() || !(selector.starts_with("set") || selector.starts_with("_set")) {
        return None;
    }
    let key = {
        let o = observing();
        if !o.setters.contains(selector) || !o.link_objects.contains_key(&object) {
            return None;
        }
        #[cfg(test)]
        WALKS.with(|w| w.set(w.get() + 1));
        let found = o
            .observers
            .iter()
            .flat_map(|w| w.links())
            .find(|(o, key)| *o == object && setter_names(key).iter().any(|s| s == selector))
            .map(|(_, key)| key.to_string());
        found?
    };
    notifies_automatically(object, &key).then_some(key)
}

/// Whether the class of `object` announces changes of `key` itself.
fn notifies_automatically(object: Id, key: &str) -> bool {
    let class = lock().class_of(object);
    runtime::send(class, "automaticallyNotifiesObserversForKey:", &[crate::fw::string::make(key)]) & 0xFF != 0
}

fn observers_of(object: Id, key: &str) -> Vec<KeyObserver> {
    observing().observers.iter().filter(|w| w.watches(object, key)).cloned().collect()
}

/// A change entry's value: nil is NSNull, as Apple's.
fn change_value(value: Id) -> Id {
    if value == 0 {
        runtime::send(runtime::class_named("NSNull"), "null", &[])
    } else {
        value
    }
}

/// The change is told as one of the observed object's path, wherever along
/// it the change was.
fn tell(watcher: &KeyObserver, new: Option<Id>, old: Option<Id>, prior: bool) {
    let s = crate::fw::string::make;
    let mut entries = vec![(s("kind"), crate::fw::collections::number_int(1))];
    if let Some(new) = new.filter(|_| watcher.options & OPTION_NEW != 0) {
        entries.push((s("new"), change_value(new)));
    }
    if let Some(old) = old.filter(|_| watcher.options & OPTION_OLD != 0) {
        entries.push((s("old"), change_value(old)));
    }
    if prior {
        entries.push((s("notificationIsPrior"), crate::fw::collections::number_bool(true)));
    }
    let change = crate::fw::collections::dictionary_from(&entries);
    runtime::send(watcher.observer, "observeValueForKeyPath:ofObject:change:context:", &[s(&watcher.path), watcher.object, change, watcher.context]);
}

fn value_at(object: Id, path: &str) -> Id {
    runtime::send(object, "valueForKeyPath:", &[crate::fw::string::make(path)])
}

/// -willChangeValueForKey:: notes each observer's old value (for Old) and
/// sends Prior notifications.
pub fn will_change(object: Id, key: &str) {
    let watchers = observers_of(object, key);
    if watchers.is_empty() {
        return;
    }
    let olds: Vec<(u64, Id)> = watchers.iter().map(|w| (w.serial, if w.options & OPTION_OLD != 0 { value_at(w.object, &w.path) } else { 0 })).collect();
    PENDING.with(|p| p.borrow_mut().push((object, key.to_string(), olds.clone())));
    for (watcher, (_, old)) in watchers.iter().zip(&olds) {
        if watcher.options & OPTION_PRIOR != 0 {
            tell(watcher, None, Some(*old), true);
        }
    }
}

/// -didChangeValueForKey:: tells each observer its -willChangeValueForKey:
/// found the new value (and the old one it noted). As Apple's
/// _NSKeyValueDidChange, without a pending will it tells nobody.
pub fn did_change(object: Id, key: &str) {
    let popped = PENDING.with(|p| {
        let mut p = p.borrow_mut();
        let at = p.iter().rposition(|(obj, k, _)| *obj == object && k == key)?;
        Some(p.remove(at).2)
    });
    let Some(olds) = popped else { return };
    let watchers: Vec<KeyObserver> = observing().observers.iter().filter(|w| olds.iter().any(|(serial, _)| *serial == w.serial)).cloned().collect();
    refollow(&watchers);
    if let Some(w) = watchers.iter().find(|w| w.object != object) {
        static LOGGED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        if LOGGED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 5 {
            crate::log!("KVO: a change of {key} along an observed key path ({})", w.path);
        }
    }
    for watcher in &watchers {
        let old = olds.iter().find(|(serial, _)| *serial == watcher.serial).map(|(_, old)| *old).unwrap_or(0);
        let new = if watcher.options & OPTION_NEW != 0 { value_at(watcher.object, &watcher.path) } else { 0 };
        tell(watcher, Some(new), Some(old), false);
    }
}

extern "C" fn add_key_observer(this: Id, _: Sel, observer: Id, path: Id, options: usize, context: usize) {
    let path = crate::fw::string::get(path);
    let chain = chain_of(this, &path);
    let extra = dependents_of(&chain, &path);
    let watcher = {
        let mut o = observing();
        o.next_serial += 1;
        let watcher = KeyObserver { serial: o.next_serial, object: this, path, observer, options, context, chain, extra };
        o.observers.push(watcher.clone());
        count_setters(&mut o, link_setters(&watcher), true);
        count_links(&mut o, link_objects(&watcher), true);
        watcher
    };
    if options & OPTION_INITIAL != 0 {
        let new = if options & OPTION_NEW != 0 { value_at(this, &watcher.path) } else { 0 };
        tell(&watcher, Some(new), None, false);
    }
}

/// Takes away the latest matching registration (and with a context, only
/// one made with it). None: NSRangeException, as Apple's.
fn remove_observer(this: Id, observer: Id, path: Id, context: Option<usize>) {
    let text = crate::fw::string::get(path);
    {
        let mut o = observing();
        if let Some(at) = o.observers.iter().rposition(|w| w.object == this && w.observer == observer && w.path == text && context.is_none_or(|c| c == w.context)) {
            let gone = o.observers.remove(at);
            count_setters(&mut o, link_setters(&gone), false);
            count_links(&mut o, link_objects(&gone), false);
            return;
        }
    }
    let describe = |object: Id| format!("<{} {object:#x}>", crate::fw::string::describe_class_of(object));
    let reason = format!("Cannot remove an observer {} for the key path \"{text}\" from {} because it is not registered as an observer.", describe(observer), describe(this));
    crate::log!("removeObserver:forKeyPath: {reason} Raising NSRangeException");
    crate::fw::exception::raise(exception_named("NSRangeException", &reason));
}

fn exception_named(name: &str, reason: &str) -> Id {
    runtime::send(
        runtime::class_named("NSException"),
        "exceptionWithName:reason:userInfo:",
        &[crate::fw::string::make(name), crate::fw::string::make(reason), 0],
    )
}

extern "C" fn remove_key_observer(this: Id, _: Sel, observer: Id, path: Id) {
    remove_observer(this, observer, path, None);
}

extern "C" fn remove_key_observer_context(this: Id, _: Sel, observer: Id, path: Id, context: usize) {
    remove_observer(this, observer, path, Some(context));
}

extern "C" fn will_change_value_for_key(this: Id, _: Sel, key: Id) {
    will_change(this, &crate::fw::string::get(key));
}

extern "C" fn did_change_value_for_key(this: Id, _: Sel, key: Id) {
    did_change(this, &crate::fw::string::get(key));
}

extern "C" fn automatically_notifies(_class: Class, _: Sel, _key: Id) -> bool {
    true
}

/// Tells observers of `key` on `object` that the framework changed it from
/// `old` to `new`, as a setter's change is told: Prior first, then the change.
pub fn kvo_notify(object: Id, key: &str, old: Id, new: Id) {
    let watchers = observers_of(object, key);
    if watchers.is_empty() {
        return;
    }
    let own = |w: &KeyObserver| w.object == object && w.path == key;
    for watcher in watchers.iter().filter(|w| w.options & OPTION_PRIOR != 0) {
        tell(watcher, None, own(watcher).then_some(old), true);
    }
    refollow(&watchers);
    for watcher in &watchers {
        let value = if own(watcher) { new } else { value_at(watcher.object, &watcher.path) };
        tell(watcher, Some(value), own(watcher).then_some(old), false);
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
    m("retain", retain as usize);
    m("autorelease", autorelease as usize);
    m("release", release as usize);
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
    m("setValuesForKeysWithDictionary:", set_values_for_keys as usize);
    m("mutableCopy", mutable_copy as usize);
    m("performSelector:", perform as usize);
    m("performSelector:withObject:", perform_with as usize);
    m("performSelector:withObject:withObject:", perform_with_with as usize);
    m("zone", hash_zero as usize);
    m("addObserver:forKeyPath:options:context:", add_key_observer as usize);
    m("removeObserver:forKeyPath:", remove_key_observer as usize);
    m("removeObserver:forKeyPath:context:", remove_key_observer_context as usize);
    m("willChangeValueForKey:", will_change_value_for_key as usize);
    m("didChangeValueForKey:", did_change_value_for_key as usize);
    m("valueForKey:", value_for_key as usize);
    m("setValue:forKey:", set_value_for_key as usize);
    m("valueForKeyPath:", value_for_key_path as usize);
    m("setValue:forKeyPath:", set_value_for_key_path as usize);
    m("dictionaryWithValuesForKeys:", dictionary_with_values_for_keys as usize);
    m("valueForUndefinedKey:", value_for_undefined_key as usize);
    m("setValue:forUndefinedKey:", set_value_for_undefined_key as usize);
    m("setNilValueForKey:", set_nil_value_for_key as usize);
    m("doesNotRecognizeSelector:", does_not_recognize as usize);
    m("isProxy", answer_no as usize);
    m("forwardingTargetForSelector:", no_forwarding_target as usize);
    m("forwardInvocation:", forward_invocation as usize);
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
    c("resolveInstanceMethod:", resolve_nothing as usize);
    c("resolveClassMethod:", resolve_nothing as usize);
    c("automaticallyNotifiesObserversForKey:", automatically_notifies as usize);
    c("keyPathsForValuesAffectingValueForKey:", key_paths_affecting as usize);
    c("accessInstanceVariablesDirectly", class_answer_yes as usize);
    class
}

extern "C" fn class_answer_yes(_class: Class, _: Sel) -> bool {
    true
}

/// NSProxy: the other root class. It has only the NSObject protocol's
/// bookkeeping; everything else, -respondsToSelector: and -isKindOfClass:
/// included, is forwarded (-methodSignatureForSelector: and
/// -forwardInvocation:, which a subclass must override).
pub fn define_proxy() -> Class {
    let class = lock().define_class("NSProxy", 0, 8);
    methods(
        class,
        &[
            ("retain", retain as usize),
            ("autorelease", autorelease as usize),
            ("release", release as usize),
            ("dealloc", dealloc as usize),
            ("finalize", nothing as usize),
            ("retainCount", retain_count as usize),
            ("zone", hash_zero as usize),
            ("self", return_self as usize),
            ("class", class_of_instance as usize),
            ("superclass", superclass_of_instance as usize),
            ("hash", hash as usize),
            ("isEqual:", is_equal as usize),
            ("description", description as usize),
            ("debugDescription", description as usize),
            ("isProxy", answer_yes as usize),
            ("performSelector:", perform as usize),
            ("performSelector:withObject:", perform_with as usize),
            ("performSelector:withObject:withObject:", perform_with_with as usize),
            ("doesNotRecognizeSelector:", does_not_recognize as usize),
            ("methodSignatureForSelector:", proxy_method_signature as usize),
            ("forwardInvocation:", proxy_forward_invocation as usize),
        ],
    );
    class_methods(
        class,
        &[
            ("alloc", alloc as usize),
            ("allocWithZone:", alloc_with_zone as usize),
            ("class", return_self as usize),
            ("superclass", superclass_of_class as usize),
            ("initialize", nothing as usize),
            ("load", nothing as usize),
            ("respondsToSelector:", responds_to as usize),
            ("instancesRespondToSelector:", instances_respond_to as usize),
            ("isSubclassOfClass:", is_subclass_of as usize),
            ("conformsToProtocol:", class_conforms_to as usize),
            ("description", class_description as usize),
            ("resolveInstanceMethod:", resolve_nothing as usize),
            ("resolveClassMethod:", resolve_nothing as usize),
        ],
    );
    class
}

extern "C" fn answer_no(_this: Id, _: Sel) -> bool {
    false
}

extern "C" fn answer_yes(_this: Id, _: Sel) -> bool {
    true
}

extern "C" fn resolve_nothing(_class: Class, _: Sel, _selector: Sel) -> bool {
    false
}

extern "C" fn no_forwarding_target(_this: Id, _: Sel, _selector: Sel) -> Id {
    0
}

/// NSObject's -forwardInvocation: gives up: -doesNotRecognizeSelector:.
extern "C" fn forward_invocation(this: Id, _: Sel, invocation: Id) {
    let selector = runtime::send(invocation, "selector", &[]);
    runtime::send(this, "doesNotRecognizeSelector:", &[selector]);
}

/// The NSInvalidArgumentException Apple's NSProxy raises when a subclass
/// left one of its forwarding methods out.
pub fn proxy_abstract_exception(selector: &str) -> Id {
    let reason = format!("-[NSProxy {selector}] called!");
    runtime::send(
        runtime::class_named("NSException"),
        "exceptionWithName:reason:userInfo:",
        &[crate::fw::string::make("NSInvalidArgumentException"), crate::fw::string::make(&reason), 0],
    )
}

extern "C" fn proxy_method_signature(_this: Id, _: Sel, _selector: Sel) -> Id {
    crate::fw::exception::throw(proxy_abstract_exception("methodSignatureForSelector:"))
}

extern "C" fn proxy_forward_invocation(_this: Id, _: Sel, _invocation: Id) {
    crate::fw::exception::throw(proxy_abstract_exception("forwardInvocation:"))
}

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

/// Method types the host's own tables don't have (a 32-bit game's methods,
/// known to its bridge): `class` (a metaclass for class methods) and the
/// selector to an encoding as the host receives the values.
type TypesHook = fn(Class, Sel) -> Option<String>;

static EXTRA_TYPES: std::sync::RwLock<Option<TypesHook>> = std::sync::RwLock::new(None);

pub fn set_extra_method_types(hook: TypesHook) {
    *EXTRA_TYPES.write().unwrap() = Some(hook);
}

/// -doesNotRecognizeSelector: raises NSInvalidArgumentException, as Apple's does.
extern "C" fn does_not_recognize(this: Id, _: Sel, selector: Sel) {
    let name = lock().sel_name(selector);
    crate::fw::exception::raise(unrecognized_selector_exception(this, &name));
}

/// The NSInvalidArgumentException Apple raises for `selector` sent to `this`.
pub fn unrecognized_selector_exception(this: Id, selector: &str) -> Id {
    let (class_name, meta) = {
        let rt = lock();
        let class = rt.class_of(this);
        (rt.class_name(class), rt.classes.get(&class).is_some_and(|r| r.meta))
    };
    let reason = format!("{}[{class_name} {selector}]: unrecognized selector sent to {} {this:#x}", if meta { "+" } else { "-" }, if meta { "class" } else { "instance" });
    runtime::send(
        runtime::class_named("NSException"),
        "exceptionWithName:reason:userInfo:",
        &[crate::fw::string::make("NSInvalidArgumentException"), crate::fw::string::make(&reason), 0],
    )
}

// ----- key-value coding -----

/// The encoding of the method `receiver` answers `sel` with: the bridge's
/// (a 32-bit game's method) first, else the host's own tables.
fn types_of(receiver: Id, sel: Sel) -> Option<String> {
    let class = lock().class_of(receiver);
    let owner = lock().owner_of(class, sel)?;
    let hook = *EXTRA_TYPES.read().unwrap();
    hook.and_then(|h| h(owner, sel)).or_else(|| crate::fw::invocation::method_types(class, sel))
}

/// The type codes of an encoding, one per value (return type first),
/// without qualifiers and offsets: "v24@0:8q16" -> ["v", "@", ":", "q"].
pub fn type_codes(types: &str) -> Vec<String> {
    fn skip(b: &[u8], i: usize) -> usize {
        match b.get(i) {
            None => i,
            Some(b'^') => skip(b, i + 1),
            Some(b'@') => match b.get(i + 1) {
                Some(b'?') => i + 2,
                Some(b'"') => b[i + 2..].iter().position(|&c| c == b'"').map_or(b.len(), |end| i + 3 + end),
                _ => i + 1,
            },
            Some(b'{' | b'(' | b'[') => {
                let mut depth = 0;
                let mut j = i;
                while j < b.len() {
                    match b[j] {
                        b'{' | b'(' | b'[' => depth += 1,
                        b'}' | b')' | b']' => {
                            depth -= 1;
                            if depth == 0 {
                                return j + 1;
                            }
                        }
                        b'"' => j += b[j + 1..].iter().position(|&c| c == b'"').map_or(b.len(), |end| end + 1),
                        _ => {}
                    }
                    j += 1;
                }
                b.len()
            }
            Some(b'b') => i + 1 + b[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count(),
            Some(_) => i + 1,
        }
    }
    let b = types.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && b"rnNoORV".contains(&b[i]) {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        let start = i;
        i = skip(b, i).min(b.len());
        out.push(types[start..i].to_string());
        while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'-') {
            i += 1;
        }
    }
    out
}

/// An encoding with every float code made a double, struct and field names
/// left alone: "{CGAffineTransform=ffffff}" -> "{CGAffineTransform=dddddd}".
pub fn float_as_double(types: &str) -> String {
    let mut out = String::with_capacity(types.len());
    let mut chars = types.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(match c {
            'f' => 'd',
            _ => c,
        });
        let end = match c {
            '{' | '(' => '=',
            '"' => '"',
            _ => continue,
        };
        // A name: copied as written, up to its end mark.
        for n in chars.by_ref() {
            out.push(n);
            if n == end || (end == '=' && matches!(n, '}' | ')')) {
                break;
            }
        }
    }
    out
}

fn capitalized(key: &str) -> String {
    let mut chars = key.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Calls getter `sel` and boxes its result by its return type (numbers in
/// NSNumber, geometry in NSValue; an unknown type is taken as an object).
fn boxed_result(receiver: Id, sel: Sel) -> Id {
    use crate::fw::collections::{number_bool, number_float, number_int, value_with, POINT_TYPE, RANGE_TYPE, RECT_TYPE, SIZE_TYPE, TRANSFORM_TYPE};
    use crate::fw::geometry::{Point, Rect, Size, Transform};
    let Some(imp) = runtime::imp_for(receiver, sel) else { return 0 };
    let code = types_of(receiver, sel).and_then(|t| type_codes(&t).into_iter().next()).unwrap_or_else(|| "@".into());
    let word = || unsafe { std::mem::transmute::<usize, extern "C" fn(Id, Sel) -> u64>(imp)(receiver, sel) };
    match code.as_bytes()[0] {
        b'c' => number_int(word() as i8 as i64),
        b'C' => number_int(word() as u8 as i64),
        b's' => number_int(word() as i16 as i64),
        b'S' => number_int(word() as u16 as i64),
        b'i' | b'l' => number_int(word() as i32 as i64),
        b'I' | b'L' => number_int(word() as u32 as i64),
        b'q' | b'Q' => number_int(word() as i64),
        b'B' => number_bool(word() & 0xFF != 0),
        b'f' => number_float(unsafe { std::mem::transmute::<usize, extern "C" fn(Id, Sel) -> f32>(imp)(receiver, sel) } as f64),
        b'd' => number_float(unsafe { std::mem::transmute::<usize, extern "C" fn(Id, Sel) -> f64>(imp)(receiver, sel) }),
        b'v' => 0,
        b'{' => {
            let call = |kind: &str| -> Id {
                unsafe {
                    match kind {
                        POINT_TYPE => value_with(kind, &std::mem::transmute::<usize, extern "C" fn(Id, Sel) -> Point>(imp)(receiver, sel)),
                        SIZE_TYPE => value_with(kind, &std::mem::transmute::<usize, extern "C" fn(Id, Sel) -> Size>(imp)(receiver, sel)),
                        RECT_TYPE => value_with(kind, &std::mem::transmute::<usize, extern "C" fn(Id, Sel) -> Rect>(imp)(receiver, sel)),
                        TRANSFORM_TYPE => value_with(kind, &std::mem::transmute::<usize, extern "C" fn(Id, Sel) -> Transform>(imp)(receiver, sel)),
                        _ => value_with(kind, &std::mem::transmute::<usize, extern "C" fn(Id, Sel) -> crate::fw::string::Range>(imp)(receiver, sel)),
                    }
                }
            };
            // The same struct in either side's spelling (CGFloat float or double).
            let host = float_as_double(&code).replace("{_NSRange=II}", RANGE_TYPE).replace("{_NSRange=QQ}", RANGE_TYPE);
            match host.as_str() {
                POINT_TYPE | SIZE_TYPE | RECT_TYPE | TRANSFORM_TYPE | RANGE_TYPE => call(&host),
                _ => {
                    crate::log!("valueForKey: a {code} result can't be boxed; nil");
                    0
                }
            }
        }
        _ => word() as Id,
    }
}

/// Apple's order: get<Key>, <key>, is<Key>, _<key>.
fn getter(receiver: Id, key: &str) -> Option<Sel> {
    let cap = capitalized(key);
    [format!("get{cap}"), key.to_string(), format!("is{cap}"), format!("_{key}")]
        .into_iter()
        .find(|name| !name.is_empty() && runtime::responds(receiver, name))
        .map(|name| runtime::sel(&name))
}

/// The instance variable key-value coding falls back to without accessor
/// methods, as Apple's does: _key, _isKey, key, isKey. Its address and type.
/// A field of an object: where it is, its type, and whether it is in a
/// 32-bit game's memory (4-byte long and pointers, 4-byte alignment).
pub struct Field {
    pub at: usize,
    pub kind: String,
    pub guest: bool,
}

/// Fields the host's class tables don't describe (a 32-bit game's objects,
/// known to its bridge): the named field of an object, if it has one.
type FieldsHook = fn(Id, &str) -> Option<Field>;

static EXTRA_FIELDS: std::sync::RwLock<Option<FieldsHook>> = std::sync::RwLock::new(None);

pub fn set_extra_fields(hook: FieldsHook) {
    *EXTRA_FIELDS.write().unwrap() = Some(hook);
}

fn field_for(receiver: Id, key: &str) -> Option<Field> {
    let class = lock().class_of(receiver);
    let cap = capitalized(key);
    let hook = *EXTRA_FIELDS.read().unwrap();
    for name in [format!("_{key}"), format!("_is{cap}"), key.to_string(), format!("is{cap}")] {
        if let Some(field) = hook.and_then(|h| h(receiver, &name)) {
            return Some(field);
        }
        let Ok(name) = std::ffi::CString::new(name) else { continue };
        let ivar = crate::objc::reflect::get_instance_variable(class, name.as_ptr());
        if ivar == 0 {
            continue;
        }
        // ivar_t { int32_t *offset; const char *name; const char *type; ... }
        unsafe {
            let offset_cell = *(ivar as *const usize);
            let offset = if offset_cell == 0 { 0 } else { *(offset_cell as *const i32) as isize };
            let types = *((ivar + 16) as *const *const libc::c_char);
            let kind = if types.is_null() { "@".to_string() } else { std::ffi::CStr::from_ptr(types).to_string_lossy().into_owned() };
            return Some(Field { at: (receiver as isize + offset) as usize, kind, guest: false });
        }
    }
    None
}

fn read_field(field: &Field) -> Option<Id> {
    use crate::fw::collections::{number_bool, number_float, number_int};
    use std::ptr::read_unaligned as read;
    let at = field.at;
    let objects = field.guest.then(crate::fw::system::guest_memory).flatten();
    unsafe {
        Some(match field.kind.as_bytes().first()? {
            b'@' | b'#' => match objects {
                Some(memory) => (memory.object_to_host)(read(at as *const u32)),
                None if field.guest => return None,
                None => read(at as *const Id),
            },
            b'c' => number_int(read(at as *const i8) as i64),
            b'C' => number_int(read(at as *const u8) as i64),
            b'B' => number_bool(read(at as *const u8) != 0),
            b's' => number_int(read(at as *const i16) as i64),
            b'S' => number_int(read(at as *const u16) as i64),
            b'i' => number_int(read(at as *const i32) as i64),
            b'I' => number_int(read(at as *const u32) as i64),
            // A 32-bit game's long is 4 bytes.
            b'l' if field.guest => number_int(read(at as *const i32) as i64),
            b'L' if field.guest => number_int(read(at as *const u32) as i64),
            b'l' | b'q' | b'L' | b'Q' => number_int(read(at as *const i64)),
            b'f' => number_float(read(at as *const f32) as f64),
            b'd' => number_float(read(at as *const f64)),
            _ => return None,
        })
    }
}

/// Stores `value` in a field: objects retained (the old one released),
/// numbers unboxed. False for a type key-value coding can't fill.
fn write_field(field: &Field, value: Id) -> bool {
    use crate::fw::collections::{is_number, number_as_f64, number_as_i64};
    use std::ptr::{read_unaligned as read, write_unaligned as write};
    let at = field.at;
    let integer = || if is_number(value) { number_as_i64(value) } else { crate::fw::string::get(value).trim().parse().unwrap_or(0) };
    let real = || if is_number(value) { number_as_f64(value) } else { crate::fw::string::get(value).trim().parse().unwrap_or(0.0) };
    let objects = field.guest.then(crate::fw::system::guest_memory).flatten();
    unsafe {
        match field.kind.as_bytes().first() {
            Some(b'@') => {
                let new = if value == 0 { 0 } else { runtime::send(value, "retain", &[]) };
                let old = match objects {
                    Some(memory) => {
                        let old = (memory.object_to_host)(read(at as *const u32));
                        write(at as *mut u32, (memory.object_to_guest)(new));
                        old
                    }
                    None if field.guest => return false,
                    None => {
                        let old = read(at as *const Id);
                        write(at as *mut Id, new);
                        old
                    }
                };
                if old != 0 {
                    runtime::send(old, "release", &[]);
                }
            }
            Some(b'c' | b'C' | b'B') => write(at as *mut u8, integer() as u8),
            Some(b's' | b'S') => write(at as *mut u16, integer() as u16),
            Some(b'i' | b'I') => write(at as *mut u32, integer() as u32),
            Some(b'l' | b'L') if field.guest => write(at as *mut u32, integer() as u32),
            Some(b'l' | b'q' | b'L' | b'Q') => write(at as *mut i64, integer()),
            Some(b'f') => write(at as *mut f32, real() as f32),
            Some(b'd') => write(at as *mut f64, real()),
            _ => return false,
        }
    }
    true
}

/// The instance variable key-value coding may use for `key`: only when the
/// class's +accessInstanceVariablesDirectly says YES, as Apple's.
fn ivar_for(this: Id, key: &str) -> Option<Field> {
    let class = lock().class_of(this);
    if runtime::send(class, "accessInstanceVariablesDirectly", &[]) & 0xFF == 0 {
        return None;
    }
    field_for(this, key)
}

/// Whether key-value coding can store a value in `field` (see `write_field`).
fn field_writable(field: &Field) -> bool {
    match field.kind.as_bytes().first() {
        Some(b'@') => !field.guest || crate::fw::system::guest_memory().is_some(),
        Some(b'c' | b'C' | b'B' | b's' | b'S' | b'i' | b'I' | b'l' | b'L' | b'q' | b'Q' | b'f' | b'd') => true,
        _ => false,
    }
}

extern "C" fn value_for_key(this: Id, _: Sel, key: Id) -> Id {
    let name = crate::fw::string::get(key);
    if let Some(sel) = getter(this, &name) {
        return boxed_result(this, sel);
    }
    match ivar_for(this, &name).and_then(|field| read_field(&field)) {
        Some(value) => value,
        None => runtime::send(this, "valueForUndefinedKey:", &[key]),
    }
}

/// Apple's NSUnknownKeyException for `key` of `this`, raised from `method`.
fn raise_unknown_key(this: Id, method: &str, key: Id) {
    let class = crate::fw::string::describe_class_of(this);
    let key_text = crate::fw::string::get(key);
    crate::log!("{method} {class} has no key {key_text}; raising {}", crate::fw::exception::UNDEFINED_KEY);
    let reason = format!("[<{class} {this:#x}> {method}]: this class is not key value coding-compliant for the key {key_text}.");
    let info = crate::fw::collections::dictionary_from(&[
        (crate::fw::string::make("NSTargetObjectUserInfoKey"), this),
        (crate::fw::string::make("NSUnknownUserInfoKey"), key),
    ]);
    let exception = runtime::send(
        runtime::class_named("NSException"),
        "exceptionWithName:reason:userInfo:",
        &[crate::fw::string::make(crate::fw::exception::UNDEFINED_KEY), crate::fw::string::make(&reason), info],
    );
    crate::fw::exception::raise(exception);
}

extern "C" fn value_for_undefined_key(this: Id, _: Sel, key: Id) -> Id {
    raise_unknown_key(this, "valueForUndefinedKey:", key);
    0
}

extern "C" fn set_value_for_undefined_key(this: Id, _: Sel, _value: Id, key: Id) {
    raise_unknown_key(this, "setValue:forUndefinedKey:", key);
}

/// NSObject's -setNilValueForKey: raises NSInvalidArgumentException.
extern "C" fn set_nil_value_for_key(this: Id, _: Sel, key: Id) {
    let class = crate::fw::string::describe_class_of(this);
    let reason = format!("[<{class} {this:#x}> setNilValueForKey]: could not set nil as the value for the key {}.", crate::fw::string::get(key));
    crate::log!("setValue:forKey: {reason} Raising NSInvalidArgumentException");
    crate::fw::exception::raise(exception_named("NSInvalidArgumentException", &reason));
}

/// Calls setter `sel` with `value` unboxed for its argument type.
fn call_setter(receiver: Id, sel: Sel, value: Id, key: Id) {
    use crate::fw::collections::{is_number, number_as_f64, number_as_i64, value_struct};
    use crate::fw::geometry::{Point, Rect, Size};
    let Some(imp) = runtime::imp_for(receiver, sel) else { return };
    let code = types_of(receiver, sel).and_then(|t| type_codes(&t).get(3).cloned()).unwrap_or_else(|| "@".into());
    let first = code.as_bytes()[0];
    if value == 0 && !matches!(first, b'@' | b'#') {
        runtime::send(receiver, "setNilValueForKey:", &[key]);
        return;
    }
    // A setter of an observed key announces the change, as objc_msgSend's
    // does (runtime::send). No setter ran when a nil was refused above.
    let setter = lock().sel_name(sel);
    let observed = observed_setter_key(receiver, &setter);
    if let Some(key) = observed.as_deref() {
        will_change(receiver, key);
    }
    let number = |v: Id| if is_number(v) { number_as_f64(v) } else { crate::fw::string::get(v).trim().parse().unwrap_or(0.0) };
    let integer = |v: Id| if is_number(v) { number_as_i64(v) } else { number(v) as i64 };
    unsafe {
        match first {
            b'f' => std::mem::transmute::<usize, extern "C" fn(Id, Sel, f32)>(imp)(receiver, sel, number(value) as f32),
            b'd' => std::mem::transmute::<usize, extern "C" fn(Id, Sel, f64)>(imp)(receiver, sel, number(value)),
            b'c' | b'C' | b's' | b'S' | b'i' | b'I' | b'l' | b'L' | b'q' | b'Q' | b'B' => {
                std::mem::transmute::<usize, extern "C" fn(Id, Sel, i64)>(imp)(receiver, sel, integer(value))
            }
            b'{' => match float_as_double(&code).as_str() {
                crate::fw::collections::POINT_TYPE => std::mem::transmute::<usize, extern "C" fn(Id, Sel, Point)>(imp)(receiver, sel, value_struct(value)),
                crate::fw::collections::SIZE_TYPE => std::mem::transmute::<usize, extern "C" fn(Id, Sel, Size)>(imp)(receiver, sel, value_struct(value)),
                crate::fw::collections::RECT_TYPE => std::mem::transmute::<usize, extern "C" fn(Id, Sel, Rect)>(imp)(receiver, sel, value_struct(value)),
                _ => crate::log!("setValue:forKey: a {code} argument can't be unboxed; ignored"),
            },
            _ => std::mem::transmute::<usize, extern "C" fn(Id, Sel, Id)>(imp)(receiver, sel, value),
        }
    }
    if let Some(key) = observed {
        did_change(receiver, &key);
    }
}

extern "C" fn set_value_for_key(this: Id, _: Sel, value: Id, key: Id) {
    let name = crate::fw::string::get(key);
    let cap = capitalized(&name);
    match [format!("set{cap}:"), format!("_set{cap}:")].into_iter().find(|n| runtime::responds(this, n)) {
        Some(setter) => call_setter(this, runtime::sel(&setter), value, key),
        None => match ivar_for(this, &name) {
            Some(field) if value == 0 && !field.kind.starts_with('@') => {
                runtime::send(this, "setNilValueForKey:", &[key]);
            }
            // As Apple's _NSSetValueAndNotifyForKeyInIvar: observers hear it.
            Some(field) if field_writable(&field) => {
                let notify = kvo_active() && !observers_of(this, &name).is_empty() && notifies_automatically(this, &name);
                if notify {
                    will_change(this, &name);
                }
                write_field(&field, value);
                if notify {
                    did_change(this, &name);
                }
            }
            _ => {
                runtime::send(this, "setValue:forUndefinedKey:", &[value, key]);
            }
        },
    }
}

/// The members of a collection (arrays, sets), None for anything else.
fn members(object: Id) -> Option<Vec<Id>> {
    if crate::fw::collections::is_array(object) {
        return Some(crate::fw::collections::array_items(object));
    }
    let set = runtime::class_named("NSSet");
    (set != 0 && is_kind_of(object, 0, set)).then(|| crate::fw::collections::array_items(runtime::send(object, "allObjects", &[])))
}

/// @count, @sum, @avg, @max, @min, @unionOfObjects and
/// @distinctUnionOfObjects over a collection's members' `rest` key path.
fn collection_operator(list: Vec<Id>, operator: &str, rest: Option<&str>) -> Id {
    use crate::fw::collections::{is_number, number_as_f64, number_float, number_int};
    let null = runtime::send(runtime::class_named("NSNull"), "null", &[]);
    let values = || -> Vec<Id> {
        list.iter()
            .map(|&item| match rest {
                Some(path) => runtime::send(item, "valueForKeyPath:", &[crate::fw::string::make(path)]),
                None => item,
            })
            .filter(|&v| v != 0 && v != null)
            .collect()
    };
    let numbers = || -> Vec<f64> { values().into_iter().map(|v| if is_number(v) { number_as_f64(v) } else { crate::fw::string::get(v).trim().parse().unwrap_or(0.0) }).collect() };
    let whole = |total: f64| if total.fract() == 0.0 && total.abs() < 9.0e15 { number_int(total as i64) } else { number_float(total) };
    match operator {
        // Apple ignores a key path after @count.
        "@count" => number_int(list.len() as i64),
        "@sum" => whole(numbers().iter().sum()),
        "@avg" => {
            let all = numbers();
            if all.is_empty() { 0 } else { number_float(all.iter().sum::<f64>() / all.len() as f64) }
        }
        "@max" | "@min" => {
            let want = if operator == "@max" { std::cmp::Ordering::Greater } else { std::cmp::Ordering::Less };
            values().into_iter().fold(0, |best, v| {
                if best == 0 || crate::fw::predicate::compare_values(v, best, 0) == Some(want) { v } else { best }
            })
        }
        "@unionOfObjects" => crate::fw::collections::array_from(&values()),
        "@distinctUnionOfObjects" => {
            let mut kept: Vec<Id> = Vec::new();
            for v in values() {
                if !kept.iter().any(|&k| k == v || runtime::send(k, "isEqual:", &[v]) & 0xFF != 0) {
                    kept.push(v);
                }
            }
            crate::fw::collections::array_from(&kept)
        }
        other => {
            crate::log!("valueForKeyPath: collection operator {other} not known; nil");
            0
        }
    }
}

extern "C" fn value_for_key_path(this: Id, _: Sel, path: Id) -> Id {
    let text = crate::fw::string::get(path);
    if text.starts_with('@') {
        if let Some(list) = members(this) {
            let (name, rest) = match text.split_once('.') {
                Some((name, rest)) => (name, Some(rest)),
                None => (text.as_str(), None),
            };
            return collection_operator(list, name, rest);
        }
    }
    match text.split_once('.') {
        None => runtime::send(this, "valueForKey:", &[path]),
        Some((first, rest)) => {
            let value = runtime::send(this, "valueForKey:", &[crate::fw::string::make(first)]);
            if value == 0 { 0 } else { runtime::send(value, "valueForKeyPath:", &[crate::fw::string::make(rest)]) }
        }
    }
}

extern "C" fn set_value_for_key_path(this: Id, _: Sel, value: Id, path: Id) {
    let text = crate::fw::string::get(path);
    match text.rsplit_once('.') {
        None => {
            runtime::send(this, "setValue:forKey:", &[value, path]);
        }
        Some((owner, key)) => {
            let target = runtime::send(this, "valueForKeyPath:", &[crate::fw::string::make(owner)]);
            if target != 0 {
                runtime::send(target, "setValue:forKey:", &[value, crate::fw::string::make(key)]);
            }
        }
    }
}

/// Each key's value; nil ones as NSNull.
extern "C" fn dictionary_with_values_for_keys(this: Id, _: Sel, keys: Id) -> Id {
    let null = runtime::send(runtime::class_named("NSNull"), "null", &[]);
    let pairs: Vec<(Id, Id)> = crate::fw::collections::array_items(keys)
        .into_iter()
        .map(|key| {
            let value = runtime::send(this, "valueForKey:", &[key]);
            (key, if value == 0 { null } else { value })
        })
        .collect();
    crate::fw::collections::dictionary_from(&pairs)
}

/// An array's valueForKey: is each member's value (nil as NSNull).
extern "C" fn array_value_for_key(this: Id, _: Sel, key: Id) -> Id {
    let null = runtime::send(runtime::class_named("NSNull"), "null", &[]);
    let values: Vec<Id> = crate::fw::collections::array_items(this)
        .into_iter()
        .map(|item| match runtime::send(item, "valueForKey:", &[key]) {
            0 => null,
            v => v,
        })
        .collect();
    crate::fw::collections::array_from(&values)
}

extern "C" fn array_set_value_for_key(this: Id, _: Sel, value: Id, key: Id) {
    for item in crate::fw::collections::array_items(this) {
        runtime::send(item, "setValue:forKey:", &[value, key]);
    }
}

/// A set's valueForKey: is the set of its members' values (nils left out).
extern "C" fn set_members_value_for_key(this: Id, _: Sel, key: Id) -> Id {
    let values: Vec<Id> = crate::fw::collections::array_items(runtime::send(this, "allObjects", &[]))
        .into_iter()
        .map(|item| runtime::send(item, "valueForKey:", &[key]))
        .filter(|&v| v != 0)
        .collect();
    crate::fw::misc::set_from(&values)
}

/// Key-value coding on collections (after NSArray and NSSet exist).
pub fn register_collection_coding() {
    methods(
        runtime::class_named("NSArray"),
        &[("valueForKey:", array_value_for_key as usize), ("setValue:forKey:", array_set_value_for_key as usize)],
    );
    methods(runtime::class_named("NSSet"), &[("valueForKey:", set_members_value_for_key as usize)]);
}

pub fn define_class(name: &str, parent: &str, instance_size: usize) -> Class {
    let mut rt = lock();
    let parent = rt.by_name.get(parent).copied().unwrap_or_else(|| panic!("parent class {parent} not defined yet"));
    rt.define_class(name, parent, instance_size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fw::{collections, string};

    extern "C" fn thing_title(this: Id, _: Sel) -> Id {
        crate::fw::uikit::property_get(this, "title").unwrap_or(0) as Id
    }
    extern "C" fn thing_set_title(this: Id, _: Sel, title: Id) {
        crate::fw::uikit::property_set(this, "title", title as u64);
    }
    extern "C" fn thing_count(this: Id, _: Sel) -> i64 {
        crate::fw::uikit::property_get(this, "count").unwrap_or(0) as i64
    }
    extern "C" fn thing_set_count(this: Id, _: Sel, count: i64) {
        crate::fw::uikit::property_set(this, "count", count as u64);
    }
    extern "C" fn thing_ratio(this: Id, _: Sel) -> f64 {
        crate::fw::uikit::property_get(this, "ratio").map(f64::from_bits).unwrap_or(0.0)
    }
    extern "C" fn thing_set_ratio(this: Id, _: Sel, ratio: f64) {
        crate::fw::uikit::property_set(this, "ratio", ratio.to_bits());
    }
    extern "C" fn thing_child(this: Id, _: Sel) -> Id {
        crate::fw::uikit::property_get(this, "child").unwrap_or(0) as Id
    }
    extern "C" fn thing_set_child(this: Id, _: Sel, child: Id) {
        crate::fw::uikit::property_set(this, "child", child as u64);
    }
    extern "C" fn thing_nothing(_this: Id, _: Sel) -> Id {
        0
    }
    extern "C" fn thing_label(_this: Id, _: Sel) -> Id {
        string::make("plain")
    }
    extern "C" fn thing_get_label(_this: Id, _: Sel) -> Id {
        string::make("get")
    }

    /// Types for the test class's methods, as a 32-bit game's would come.
    fn test_types(class: Class, sel: Sel) -> Option<String> {
        let (class_name, name) = { let rt = lock(); (rt.class_name(class), rt.sel_name(sel)) };
        if class_name != "KvcTestThing" {
            return None;
        }
        Some(match name.as_str() {
            "count" => "q@:",
            "setCount:" => "v@:q",
            "ratio" => "d@:",
            "setRatio:" => "v@:d",
            _ => "@@:",
        }.to_string())
    }

    fn thing(title: &str, count: i64, child: Id) -> Id {
        static CLASS: std::sync::OnceLock<Class> = std::sync::OnceLock::new();
        let class = *CLASS.get_or_init(|| {
            let class = define_class("KvcTestThing", "NSObject", 8);
            methods(
                class,
                &[
                    ("title", thing_title as usize),
                    ("setTitle:", thing_set_title as usize),
                    ("count", thing_count as usize),
                    ("setCount:", thing_set_count as usize),
                    ("ratio", thing_ratio as usize),
                    ("setRatio:", thing_set_ratio as usize),
                    ("child", thing_child as usize),
                    ("setChild:", thing_set_child as usize),
                    ("nothing", thing_nothing as usize),
                    ("label", thing_label as usize),
                    ("getLabel", thing_get_label as usize),
                ],
            );
            set_extra_method_types(test_types);
            class
        });
        let object = runtime::alloc_instance(class);
        crate::fw::uikit::property_set(object, "title", string::make(title) as u64);
        crate::fw::uikit::property_set(object, "count", count as u64);
        crate::fw::uikit::property_set(object, "child", child as u64);
        object
    }

    /// Key-value coding: getters found by name and their results boxed by
    /// the method's type, key paths through objects and dictionaries,
    /// setters fed unboxed values, and the collection operators.
    #[test]
    fn key_paths_reach_through_objects_and_collections() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let s = string::make;
        let inner = thing("inner", 2, 0);
        let outer = thing("outer", 12, inner);
        crate::fw::uikit::property_set(outer, "ratio", 0.5f64.to_bits());
        assert_eq!(string::get(runtime::send(outer, "valueForKey:", &[s("title")])), "outer");
        let count = runtime::send(outer, "valueForKey:", &[s("count")]);
        assert!(collections::is_number(count) && collections::number_as_i64(count) == 12, "count boxed as a number");
        assert_eq!(collections::number_as_f64(runtime::send(outer, "valueForKey:", &[s("ratio")])), 0.5);
        assert_eq!(string::get(runtime::send(outer, "valueForKeyPath:", &[s("child.title")])), "inner");
        runtime::send(outer, "setValue:forKey:", &[collections::number_int(-3), s("count")]);
        assert_eq!(thing_count(outer, 0), -3);
        runtime::send(outer, "setValue:forKey:", &[collections::number_float(0.25), s("ratio")]);
        assert_eq!(thing_ratio(outer, 0), 0.25);
        runtime::send(outer, "setValue:forKeyPath:", &[s("renamed"), s("child.title")]);
        assert_eq!(string::get(thing_title(inner, 0)), "renamed");
        let values = runtime::send(outer, "dictionaryWithValuesForKeys:", &[collections::array_from(&[s("title"), s("nothing")])]);
        let null = runtime::send(runtime::class_named("NSNull"), "null", &[]);
        assert_eq!(collections::dictionary_get(values, s("nothing")), null, "nil comes back as NSNull");

        let list = collections::array_from(&[thing("a", 1, 0), thing("b", 5, 0), thing("c", 3, 0)]);
        let number = |path: &str| collections::number_as_f64(runtime::send(list, "valueForKeyPath:", &[s(path)]));
        assert_eq!(number("@sum.count"), 9.0);
        assert_eq!(number("@max.count"), 5.0);
        assert_eq!(number("@min.count"), 1.0);
        assert_eq!(number("@avg.count"), 3.0);
        assert_eq!(number("@count"), 3.0);
        let titles: Vec<String> = collections::array_items(runtime::send(list, "valueForKey:", &[s("title")])).into_iter().map(string::get).collect();
        assert_eq!(titles, ["a", "b", "c"]);
        let nested = collections::dictionary_from(&[(s("a"), collections::dictionary_from(&[(s("b"), s("deep"))]))]);
        assert_eq!(string::get(runtime::send(nested, "valueForKeyPath:", &[s("a.b")])), "deep");
    }

    /// Apple's search order (get<Key> first), @count ignoring the path after
    /// it, and an unknown key raising NSUnknownKeyException for the game.
    #[test]
    fn key_value_coding_follows_apples_rules() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let s = string::make;
        let one = thing("one", 1, 0);
        assert_eq!(string::get(runtime::send(one, "valueForKey:", &[s("label")])), "get");
        let list = collections::array_from(&[thing("a", 1, 0), thing("b", 2, 0)]);
        assert_eq!(collections::number_as_i64(runtime::send(list, "valueForKeyPath:", &[s("@count.child")])), 2);
        for (raised, what) in [
            (crate::fw::exception::catching(|| runtime::send(one, "valueForKey:", &[s("nope")])).1, "get"),
            (crate::fw::exception::catching(|| runtime::send(one, "setValue:forKey:", &[s("x"), s("nope")])).1, "set"),
        ] {
            let raised = raised.unwrap_or_else(|| panic!("{what} of an unknown key raised nothing"));
            // Apple: the constant NSUndefinedKeyException is @"NSUnknownKeyException".
            let (constant, _) = crate::symbols::lookup("NSUndefinedKeyException").expect("NSUndefinedKeyException exported");
            let constant = string::get(unsafe { *(constant as *const Id) });
            assert_eq!(constant, "NSUnknownKeyException");
            assert_eq!(string::get(runtime::send(raised, "name", &[])), constant);
            assert!(string::get(runtime::send(raised, "reason", &[])).contains("not key value coding-compliant for the key nope"));
        }
        // A raise ends the host method: members after the one that raised are left alone.
        let plain = runtime::send(runtime::class_named("NSObject"), "new", &[]);
        let after = thing("after", 1, 0);
        let mixed = collections::array_from(&[plain, after]);
        let (_, raised) = crate::fw::exception::catching(|| runtime::send(mixed, "setValue:forKey:", &[s("changed"), s("title")]));
        assert!(raised.is_some(), "the plain object has no title");
        assert_eq!(string::get(thing_title(after, 0)), "after");
    }

    /// What a KVO test observer heard: key path, object, the change's "new"
    /// and "old" titles (None when the change has no such entry), context.
    type Heard = (String, Id, Option<String>, Option<String>, usize);
    static HEARD: std::sync::Mutex<Vec<Heard>> = std::sync::Mutex::new(Vec::new());

    extern "C" fn watcher_observe(_this: Id, _: Sel, path: Id, object: Id, change: Id, context: usize) {
        let entry = |key: &str| {
            let value = collections::dictionary_get(change, string::make(key));
            (value != 0).then(|| if string::is_string(value) { string::get(value) } else { string::describe(value) })
        };
        HEARD.lock().unwrap().push((string::get(path), object, entry("new"), entry("old"), context));
    }

    /// Key-value observing as Foundation does it: the key's setter sent to
    /// the observed object (by objc_msgSend, as game code sends it) tells
    /// the observer the old and new values, and so does a
    /// -willChangeValueForKey: / -didChangeValueForKey: pair; Initial
    /// answers at once; other keys and removed observers hear nothing.
    #[test]
    fn observers_hear_setters_and_announced_changes() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let s = string::make;
        let watcher_class = define_class("KvoTestWatcher", "NSObject", 8);
        methods(watcher_class, &[("observeValueForKeyPath:ofObject:change:context:", watcher_observe as usize)]);
        let watcher = runtime::alloc_instance(watcher_class);
        let object = thing("before", 0, 0);
        let take = || std::mem::take(&mut *HEARD.lock().unwrap());
        let text = |t: &str| Some(t.to_string());
        // New | Old | Initial.
        runtime::send(object, "addObserver:forKeyPath:options:context:", &[watcher, s("title"), 1 | 2 | 4, 77]);
        assert_eq!(take(), [("title".to_string(), object, text("before"), None, 77)], "Initial: the value now, no old one");
        let msg_send: extern "C" fn(Id, Sel, Id) = unsafe { std::mem::transmute(runtime::ios_objc_msgSend as usize) };
        msg_send(object, runtime::sel("setTitle:"), s("after"));
        assert_eq!(take(), [("title".to_string(), object, text("after"), text("before"), 77)]);
        runtime::send(object, "willChangeValueForKey:", &[s("title")]);
        crate::fw::uikit::property_set(object, "title", s("quiet") as u64);
        runtime::send(object, "didChangeValueForKey:", &[s("title")]);
        assert_eq!(take(), [("title".to_string(), object, text("quiet"), text("after"), 77)]);
        msg_send(object, runtime::sel("setCount:"), 5);
        assert_eq!(take(), [], "a key nobody observes");
        runtime::send(object, "removeObserver:forKeyPath:", &[watcher, s("title")]);
        msg_send(object, runtime::sel("setTitle:"), s("gone"));
        assert_eq!(take(), [], "removed");
        assert_eq!(string::get(thing_title(object, 0)), "gone");
    }

    static PATH_HEARD: std::sync::Mutex<Vec<(String, Id, Option<String>)>> = std::sync::Mutex::new(Vec::new());

    extern "C" fn path_watcher(_this: Id, _: Sel, path: Id, object: Id, change: Id, _context: usize) {
        let new = collections::dictionary_get(change, string::make("new"));
        PATH_HEARD.lock().unwrap().push((string::get(path), object, (new != 0).then(|| string::get(new))));
    }

    /// An observer of a key path hears a change anywhere along it, as
    /// Apple's: the last object's key, or a middle object's (then it
    /// follows the new middle object, not the old one). SpellCraft's
    /// monster heart watches parent.combatController.monsterHealth.
    #[test]
    fn observers_of_a_key_path_hear_changes_along_it() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let s = string::make;
        let watcher_class = define_class("KvoPathWatcher", "NSObject", 8);
        methods(watcher_class, &[("observeValueForKeyPath:ofObject:change:context:", path_watcher as usize)]);
        let watcher = runtime::alloc_instance(watcher_class);
        let leaf = thing("full", 0, 0);
        let middle = thing("middle", 0, leaf);
        let root = thing("root", 0, middle);
        let path = "child.child.title".to_string();
        let take = || std::mem::take(&mut *PATH_HEARD.lock().unwrap());
        runtime::send(root, "addObserver:forKeyPath:options:context:", &[watcher, s(&path), 1, 0]);
        let msg_send: extern "C" fn(Id, Sel, Id) = unsafe { std::mem::transmute(runtime::ios_objc_msgSend as usize) };
        msg_send(leaf, runtime::sel("setTitle:"), s("hit"));
        assert_eq!(take(), [(path.clone(), root, Some("hit".to_string()))], "the last object's key, told as a change of the root's path");
        let other = thing("other", 0, 0);
        msg_send(middle, runtime::sel("setChild:"), other);
        assert_eq!(take(), [(path.clone(), root, Some("other".to_string()))], "a middle object's key");
        msg_send(leaf, runtime::sel("setTitle:"), s("left behind"));
        assert_eq!(take(), [], "the old last object is no longer on the path");
        msg_send(other, runtime::sel("setTitle:"), s("new"));
        assert_eq!(take(), [(path.clone(), root, Some("new".to_string()))], "the new one is");
        runtime::send(root, "removeObserver:forKeyPath:", &[watcher, s(&path)]);
        msg_send(other, runtime::sel("setTitle:"), s("gone"));
        assert_eq!(take(), [], "removed");
    }

    /// A setter of an observed key sent to an object nobody observes is
    /// passed by without looking through the observers: every message of a
    /// setter's name (SpellCraft sends setSpell: and the like to many
    /// objects) went through all of them before.
    #[test]
    fn a_setter_sent_to_an_unobserved_object_does_not_walk_the_observers() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let s = string::make;
        let watcher_class = define_class("KvoWalkWatcher", "NSObject", 8);
        methods(watcher_class, &[("observeValueForKeyPath:ofObject:change:context:", watcher_observe as usize)]);
        let watcher = runtime::alloc_instance(watcher_class);
        let (watched, other) = (thing("a", 0, 0), thing("b", 0, 0));
        runtime::send(watched, "addObserver:forKeyPath:options:context:", &[watcher, s("title"), 1, 0]);
        let walks = || super::WALKS.with(|w| w.get());
        let before = walks();
        assert_eq!(super::observed_setter_key(other, "setTitle:"), None);
        assert_eq!(walks(), before, "no walk for an object nobody observes");
        assert_eq!(super::observed_setter_key(watched, "setTitle:"), Some("title".to_string()));
        runtime::send(watched, "removeObserver:forKeyPath:", &[watcher, s("title")]);
    }

    static SET_VALUE_HEARD: std::sync::Mutex<Vec<(String, Option<String>, Option<String>)>> = std::sync::Mutex::new(Vec::new());

    extern "C" fn set_value_watcher(_this: Id, _: Sel, path: Id, _object: Id, change: Id, _context: usize) {
        let entry = |key: &str| {
            let value = collections::dictionary_get(change, string::make(key));
            (value != 0).then(|| if string::is_string(value) { string::get(value) } else { string::describe(value) })
        };
        SET_VALUE_HEARD.lock().unwrap().push((string::get(path), entry("new"), entry("old")));
    }

    /// Key-value coding's setter path announces a change of an observed key,
    /// as objc_msgSend's does: SpellCraft sets a wand's spell with
    /// setValue:forKey:.
    #[test]
    fn setting_an_observed_key_by_key_value_coding_notifies() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let s = string::make;
        let watcher_class = define_class("KvcSetValueWatcher", "NSObject", 8);
        methods(watcher_class, &[("observeValueForKeyPath:ofObject:change:context:", set_value_watcher as usize)]);
        let watcher = runtime::alloc_instance(watcher_class);
        let object = thing("before", 0, 0);
        let take = || std::mem::take(&mut *SET_VALUE_HEARD.lock().unwrap());
        let text = |t: &str| Some(t.to_string());
        runtime::send(object, "addObserver:forKeyPath:options:context:", &[watcher, s("title"), 1 | 2, 0]);
        runtime::send(object, "setValue:forKey:", &[s("after"), s("title")]);
        assert_eq!(take(), [("title".to_string(), text("after"), text("before"))], "setValue:forKey: tells the observer once");
        runtime::send(object, "setValue:forKeyPath:", &[s("last"), s("title")]);
        assert_eq!(take(), [("title".to_string(), text("last"), text("after"))], "a key path's last key too");
        runtime::send(object, "removeObserver:forKeyPath:", &[watcher, s("title")]);
        runtime::send(object, "setValue:forKey:", &[s("quiet"), s("title")]);
        assert_eq!(take(), [], "removed");
        assert_eq!(string::get(thing_title(object, 0)), "quiet");
        assert_eq!(string::get(runtime::send(object, "performSelector:", &[runtime::sel("title")])), "quiet", "performSelector: still answers");
    }

    /// What the shared note-taking observer below heard: observer, key path,
    /// "new" and "old" (None when the change has no such entry), and whether
    /// it was a Prior notice.
    type Note = (Id, String, Option<String>, Option<String>, bool);
    static NOTES: std::sync::Mutex<Vec<Note>> = std::sync::Mutex::new(Vec::new());

    extern "C" fn note_observe(this: Id, _: Sel, path: Id, _object: Id, change: Id, _context: usize) {
        let entry = |key: &str| {
            let value = collections::dictionary_get(change, string::make(key));
            (value != 0).then(|| if string::is_string(value) { string::get(value) } else { string::describe(value) })
        };
        let prior = collections::dictionary_get(change, string::make("notificationIsPrior")) != 0;
        NOTES.lock().unwrap().push((this, string::get(path), entry("new"), entry("old"), prior));
    }

    /// A fresh note-taking observer and a way to take what it heard.
    fn note_taker() -> (Id, impl Fn() -> Vec<(String, Option<String>, Option<String>, bool)>) {
        static CLASS: std::sync::OnceLock<Class> = std::sync::OnceLock::new();
        let class = *CLASS.get_or_init(|| {
            let class = define_class("KvoNoteWatcher", "NSObject", 8);
            methods(class, &[("observeValueForKeyPath:ofObject:change:context:", note_observe as usize)]);
            class
        });
        let watcher = runtime::alloc_instance(class);
        let take = move || {
            let mut all = NOTES.lock().unwrap();
            let (mine, others): (Vec<Note>, Vec<Note>) = all.drain(..).partition(|n| n.0 == watcher);
            *all = others;
            mine.into_iter().map(|(_, path, new, old, prior)| (path, new, old, prior)).collect()
        };
        (watcher, take)
    }

    fn text(t: &str) -> Option<String> {
        Some(t.to_string())
    }

    /// -didChangeValueForKey: tells only the observers its
    /// -willChangeValueForKey: found, as Apple's: alone it tells nobody, and
    /// an observer added in between hears nothing of that change.
    #[test]
    fn did_change_tells_only_whom_will_change_found() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let s = string::make;
        let (watcher, take) = note_taker();
        let object = thing("before", 0, 0);
        runtime::send(object, "addObserver:forKeyPath:options:context:", &[watcher, s("title"), 1 | 2, 0]);
        runtime::send(object, "didChangeValueForKey:", &[s("title")]);
        assert_eq!(take(), [], "a did without a will");
        let other = thing("other", 0, 0);
        runtime::send(other, "willChangeValueForKey:", &[s("title")]);
        runtime::send(other, "addObserver:forKeyPath:options:context:", &[watcher, s("title"), 1 | 2, 0]);
        runtime::send(other, "didChangeValueForKey:", &[s("title")]);
        assert_eq!(take(), [], "added between the will and the did");
        runtime::send(object, "willChangeValueForKey:", &[s("title")]);
        runtime::send(object, "didChangeValueForKey:", &[s("title")]);
        assert_eq!(take(), [("title".to_string(), text("before"), text("before"), false)], "a will/did pair still tells");
    }

    extern "C" fn dep_full_title(this: Id, _: Sel) -> Id {
        string::make(&format!("Mr {}", string::get(thing_title(this, 0))))
    }
    extern "C" fn dep_child_title(this: Id, _: Sel) -> Id {
        let child = thing_child(this, 0);
        string::make(&format!("child {}", if child == 0 { String::new() } else { string::get(thing_title(child, 0)) }))
    }
    extern "C" fn dep_affecting_full_title(_class: Class, _: Sel) -> Id {
        crate::fw::misc::set_from(&[string::make("title")])
    }
    extern "C" fn dep_affecting_child_title(_class: Class, _: Sel) -> Id {
        crate::fw::misc::set_from(&[string::make("child.title")])
    }

    /// +keyPathsForValuesAffecting<Key>: observing a derived key observes
    /// the key paths it depends on, and their changes are told as changes of
    /// the derived key (with its old and new values). SpellCraft has about
    /// 140 of these ("item.icon", "spec.hidden", ...).
    #[test]
    fn observers_of_a_derived_key_hear_changes_of_the_keys_it_depends_on() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let s = string::make;
        thing("define the parent class", 0, 0);
        let class = define_class("KvoDepThing", "KvcTestThing", 8);
        methods(class, &[("fullTitle", dep_full_title as usize), ("childTitle", dep_child_title as usize)]);
        class_methods(
            class,
            &[("keyPathsForValuesAffectingFullTitle", dep_affecting_full_title as usize), ("keyPathsForValuesAffectingChildTitle", dep_affecting_child_title as usize)],
        );
        let make = |title: &str, child: Id| {
            let object = runtime::alloc_instance(class);
            crate::fw::uikit::property_set(object, "title", s(title) as u64);
            crate::fw::uikit::property_set(object, "child", child as u64);
            object
        };
        let msg_send: extern "C" fn(Id, Sel, Id) = unsafe { std::mem::transmute(runtime::ios_objc_msgSend as usize) };
        let (watcher, take) = note_taker();
        let object = make("Al", 0);
        runtime::send(object, "addObserver:forKeyPath:options:context:", &[watcher, s("fullTitle"), 1 | 2, 0]);
        msg_send(object, runtime::sel("setTitle:"), s("Bob"));
        assert_eq!(take(), [("fullTitle".to_string(), text("Mr Bob"), text("Mr Al"), false)]);

        let (first, second) = (thing("one", 0, 0), thing("two", 0, 0));
        let parent = make("p", first);
        runtime::send(parent, "addObserver:forKeyPath:options:context:", &[watcher, s("childTitle"), 1, 0]);
        msg_send(first, runtime::sel("setTitle:"), s("uno"));
        assert_eq!(take(), [("childTitle".to_string(), text("child uno"), None, false)], "the end of a dependent key path");
        msg_send(parent, runtime::sel("setChild:"), second);
        assert_eq!(take(), [("childTitle".to_string(), text("child two"), None, false)], "a link of it");
        msg_send(first, runtime::sel("setTitle:"), s("left"));
        assert_eq!(take(), [], "the old child is no longer depended on");
        msg_send(second, runtime::sel("setTitle:"), s("dos"));
        assert_eq!(take(), [("childTitle".to_string(), text("child dos"), None, false)], "the new one is");

        runtime::send(object, "removeObserver:forKeyPath:", &[watcher, s("fullTitle")]);
        runtime::send(parent, "removeObserver:forKeyPath:", &[watcher, s("childTitle")]);
        msg_send(object, runtime::sel("setTitle:"), s("Cy"));
        msg_send(second, runtime::sel("setTitle:"), s("tres"));
        assert_eq!(take(), [], "removed");
    }

    /// A key path through an NSProxy: the proxy's class has no
    /// +keyPathsForValuesAffectingValueForKey: (NSProxy is its own root), so
    /// it is not asked; asking it raised NSProxy's methodSignatureForSelector:
    /// exception (SpellCraft froze on Play).
    #[test]
    fn a_key_path_through_a_proxy_observes_without_an_exception() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let proxy = runtime::alloc_instance(runtime::class_named("NSProxy"));
        let parent = thing("parent", 0, proxy);
        let (watcher, _take) = note_taker();
        let path = string::make("child.title");
        let (_, raised) = crate::fw::exception::catching(|| runtime::send(parent, "addObserver:forKeyPath:options:context:", &[watcher, path, 1, 0]));
        assert_eq!(raised.map(|e| string::get(runtime::send(e, "reason", &[]))), None);
        runtime::send(parent, "removeObserver:forKeyPath:", &[watcher, path]);
    }

    static SINGLETON: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    extern "C" fn singleton_alloc_with_zone(class: Class, _: Sel, _zone: usize) -> Id {
        let object = runtime::alloc_instance(class);
        SINGLETON.store(object, std::sync::atomic::Ordering::SeqCst);
        object
    }

    /// Apple's +alloc goes through a class's own +allocWithZone: (Infinity
    /// Blade's store manager keeps its single copy there; skipped, the store
    /// got a nil observer and the game stopped).
    #[test]
    fn alloc_goes_through_a_classes_own_alloc_with_zone() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let class = define_class("AllocZoneSingleton", "NSObject", 8);
        class_methods(class, &[("allocWithZone:", singleton_alloc_with_zone as usize)]);
        let object = runtime::send(class, "alloc", &[]);
        assert_ne!(object, 0);
        assert_eq!(SINGLETON.load(std::sync::atomic::Ordering::SeqCst), object);
        let made = runtime::send(class, "new", &[]);
        assert_eq!(SINGLETON.load(std::sync::atomic::Ordering::SeqCst), made);
    }

    /// The fields hook that was there before the ivar test put its own in.
    static EARLIER_FIELDS: std::sync::Mutex<Option<FieldsHook>> = std::sync::Mutex::new(None);

    /// Objects of the ivar test classes keep `_title` at offset 8; anything
    /// else goes to the hook that was there before.
    fn ivar_fields(object: Id, name: &str) -> Option<Field> {
        let class_name = { let rt = lock(); rt.class_name(rt.class_of(object)) };
        if matches!(class_name.as_str(), "KvcIvarThing" | "KvcClosedThing") {
            return (name == "_title").then(|| Field { at: object + 8, kind: "@".into(), guest: false });
        }
        let earlier = *EARLIER_FIELDS.lock().unwrap();
        earlier.and_then(|hook| hook(object, name))
    }

    extern "C" fn closed_ivars(_class: Class, _: Sel) -> bool {
        false
    }

    /// Key-value coding of a key that has only an instance variable, as
    /// Foundation's: setValue:forKey: tells the key's observers, and a class
    /// whose +accessInstanceVariablesDirectly says NO has no such key.
    #[test]
    fn key_value_coding_of_an_ivar_notifies_and_asks_the_class_first() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let s = string::make;
        // The hook is the whole process's (wire.rs puts the 32-bit bridge's
        // in): this test's passes other objects on and is taken out after.
        let earlier = std::mem::replace(&mut *EXTRA_FIELDS.write().unwrap(), Some(ivar_fields));
        *EARLIER_FIELDS.lock().unwrap() = earlier;
        struct Restore(Option<FieldsHook>);
        impl Drop for Restore {
            fn drop(&mut self) {
                *EXTRA_FIELDS.write().unwrap_or_else(|e| e.into_inner()) = self.0;
            }
        }
        let _restore = Restore(earlier);
        let open = define_class("KvcIvarThing", "NSObject", 16);
        let closed = define_class("KvcClosedThing", "NSObject", 16);
        class_methods(closed, &[("accessInstanceVariablesDirectly", closed_ivars as usize)]);
        let (watcher, take) = note_taker();
        let object = runtime::alloc_instance(open);
        runtime::send(object, "addObserver:forKeyPath:options:context:", &[watcher, s("title"), 1, 0]);
        runtime::send(object, "setValue:forKey:", &[s("first"), s("title")]);
        assert_eq!(take(), [("title".to_string(), text("first"), None, false)], "an ivar set by setValue:forKey:");
        assert_eq!(string::get(runtime::send(object, "valueForKey:", &[s("title")])), "first");
        let shut = runtime::alloc_instance(closed);
        let (_, raised) = crate::fw::exception::catching(|| runtime::send(shut, "valueForKey:", &[s("title")]));
        assert!(raised.is_some(), "no ivar access: an unknown key");
    }

    /// Foundation's raises: removing an observer that isn't registered
    /// (NSRangeException) and nil for a number key (NSInvalidArgumentException).
    #[test]
    fn removing_a_stranger_and_setting_nil_for_a_number_raise() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let s = string::make;
        let (watcher, _take) = note_taker();
        let object = thing("t", 3, 0);
        let name_of = |raised: Option<Id>| raised.map(|e| string::get(runtime::send(e, "name", &[])));
        let (_, raised) = crate::fw::exception::catching(|| runtime::send(object, "removeObserver:forKeyPath:", &[watcher, s("title")]));
        assert_eq!(name_of(raised), text("NSRangeException"), "not registered");
        runtime::send(object, "addObserver:forKeyPath:options:context:", &[watcher, s("title"), 1, 0]);
        let (_, raised) = crate::fw::exception::catching(|| runtime::send(object, "removeObserver:forKeyPath:", &[watcher, s("title")]));
        assert_eq!(raised, None, "registered");
        let (_, raised) = crate::fw::exception::catching(|| runtime::send(object, "setValue:forKey:", &[0, s("count")]));
        assert_eq!(name_of(raised), text("NSInvalidArgumentException"), "nil for a number");
        assert_eq!(thing_count(object, 0), 3);
    }

    /// The framework's own changes (an operation finishing) give observers
    /// the old value and the Prior notice, as a real setter's do.
    #[test]
    fn framework_changes_tell_the_old_value_and_prior() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let s = string::make;
        let (watcher, take) = note_taker();
        let operation = runtime::send(runtime::class_named("NSOperation"), "new", &[]);
        runtime::send(operation, "addObserver:forKeyPath:options:context:", &[watcher, s("isFinished"), 1 | 2 | 8, 0]);
        runtime::send(operation, "start", &[]);
        let no = string::describe(collections::number_bool(false));
        let yes = string::describe(collections::number_bool(true));
        assert_eq!(take(), [("isFinished".to_string(), None, Some(no.clone()), true), ("isFinished".to_string(), Some(yes), Some(no), false)]);
    }

    /// isFinished as read by the observer during each notice: Prior, then the change.
    static STATE_SEEN: std::sync::Mutex<Vec<(bool, bool)>> = std::sync::Mutex::new(Vec::new());

    extern "C" fn state_observe(_this: Id, _: Sel, _path: Id, object: Id, change: Id, _context: usize) {
        let prior = collections::dictionary_get(change, string::make("notificationIsPrior")) != 0;
        let finished = runtime::send(object, "isFinished", &[]) & 0xFF != 0;
        STATE_SEEN.lock().unwrap().push((prior, finished));
    }

    /// The framework's own changes are announced around the change, as a
    /// setter's are: an observer reading the state in the Prior notice sees
    /// the old state, and in the change notice the new one.
    #[test]
    fn a_prior_notice_of_a_framework_change_comes_before_the_change() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let class = define_class("KvoStateWatcher", "NSObject", 8);
        methods(class, &[("observeValueForKeyPath:ofObject:change:context:", state_observe as usize)]);
        let watcher = runtime::alloc_instance(class);
        let operation = runtime::send(runtime::class_named("NSOperation"), "new", &[]);
        runtime::send(operation, "addObserver:forKeyPath:options:context:", &[watcher, string::make("isFinished"), 1 | 8, 0]);
        runtime::send(operation, "start", &[]);
        assert_eq!(*STATE_SEEN.lock().unwrap(), [(true, false), (false, true)]);
    }

    /// Changes announced on two threads at once keep their own old values:
    /// Apple keeps -willChangeValueForKey:'s pending changes per thread.
    #[test]
    fn pending_changes_belong_to_their_thread() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let s = string::make;
        let (watcher, take) = note_taker();
        let object = thing("one", 0, 0);
        runtime::send(object, "addObserver:forKeyPath:options:context:", &[watcher, s("title"), 1 | 2, 0]);
        let (willed, go) = (std::sync::mpsc::channel::<()>(), std::sync::mpsc::channel::<()>());
        let (willed_tx, willed_rx) = willed;
        let (go_tx, go_rx) = go;
        let other = std::thread::spawn(move || {
            runtime::send(object, "willChangeValueForKey:", &[string::make("title")]);
            willed_tx.send(()).unwrap();
            go_rx.recv().unwrap();
            runtime::send(object, "didChangeValueForKey:", &[string::make("title")]);
        });
        willed_rx.recv().unwrap();
        crate::fw::uikit::property_set(object, "title", s("two") as u64);
        runtime::send(object, "willChangeValueForKey:", &[s("title")]);
        go_tx.send(()).unwrap();
        other.join().unwrap();
        assert_eq!(take(), [("title".to_string(), text("two"), text("one"), false)], "the other thread's did tells its own old value");
        runtime::send(object, "didChangeValueForKey:", &[s("title")]);
        assert_eq!(take(), [("title".to_string(), text("two"), text("two"), false)], "and this thread's its own");
    }

    /// A game object's -retain/-release/-autorelease (NSObject's) count it,
    /// and its last release logs its dealloc at the next safe point.
    #[test]
    fn a_game_objects_last_release_is_logged_as_a_would_be_dealloc() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        use crate::fw::counted;
        let class = define_class("GameCountedThing", "NSObject", 8);
        counted::count_game_class(class);
        let object = runtime::send(runtime::send(class, "alloc", &[]), "init", &[]);
        counted::watch_game_object(object);
        runtime::send(object, "retain", &[]);
        assert_eq!(counted::game_count(object), Some(2));
        runtime::send(object, "release", &[]);
        let mut watch = crate::report::LineWatch::now();
        runtime::send(object, "release", &[]);
        assert_eq!(counted::game_count(object), Some(0));
        counted::dealloc_pending();
        assert!(watch.wait_for(&format!("dealloc GameCountedThing {object:#x}"), std::time::Duration::from_millis(500)), "logged");
        let pooled = runtime::send(runtime::send(class, "alloc", &[]), "init", &[]);
        counted::watch_game_object(pooled);
        let mark = counted::pool_mark();
        runtime::send(pooled, "autorelease", &[]);
        assert_eq!(counted::game_count(pooled), Some(1), "an autorelease waits for its pool");
        counted::pool_drain(mark);
        assert_eq!(counted::game_count(pooled), Some(0));
    }

    /// Fields of a 32-bit game's object: long is 4 bytes, 8-byte values may
    /// sit on 4-byte boundaries, and neighbours are left alone.
    #[test]
    fn fields_read_and_write_in_their_classes_layout() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let mut words = [0u32, 0xAAAA_AAAA, 0, 0, 0xBBBB_BBBB];
        let at = |i: usize, words: &mut [u32; 5]| words.as_mut_ptr() as usize + 4 * i;
        let long = Field { at: at(0, &mut words), kind: "l".into(), guest: true };
        assert!(write_field(&long, collections::number_int(-7)));
        assert_eq!(words[..2], [(-7i32) as u32, 0xAAAA_AAAA]);
        assert_eq!(collections::number_as_i64(read_field(&long).unwrap()), -7);
        let double = Field { at: at(2, &mut words), kind: "d".into(), guest: true };
        assert!(write_field(&double, collections::number_float(2.5)));
        assert_eq!(collections::number_as_f64(read_field(&double).unwrap()), 2.5);
        assert_eq!(words[4], 0xBBBB_BBBB);
    }
}
