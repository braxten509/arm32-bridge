//! NSBundle, NSUserDefaults, NSNotificationCenter, NSProcessInfo, NSLog and
//! the app's folders (Documents, Library, Caches, tmp).

use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::{collections, plist, string};
use crate::objc::runtime::{self, lock, Class, Id, Sel};
use crate::symbols::{leak, Registry};
use std::sync::{Mutex, OnceLock};

// ----- folders -----

static INFO: OnceLock<Id> = OnceLock::new();

pub fn bundle_identifier() -> String {
    let info = info_dictionary_object();
    let value = collections::dictionary_get(info, string::make("CFBundleIdentifier"));
    if value == 0 { "unknown.app".into() } else { string::get(value) }
}

/// The game's own writable folder (like an iPhone app's home folder).
pub fn home() -> String {
    static HOME: OnceLock<String> = OnceLock::new();
    HOME.get_or_init(|| {
        let base = std::env::var("IOS_HOME").unwrap_or_else(|_| {
            let data = std::env::var("XDG_DATA_HOME")
                .unwrap_or_else(|_| format!("{}/.local/share", std::env::var("HOME").unwrap_or_else(|_| "/tmp".into())));
            format!("{data}/gamebridge-ios/{}", bundle_identifier())
        });
        for sub in ["Documents", "Library/Preferences", "Library/Caches", "Library/Application Support", "tmp"] {
            let _ = std::fs::create_dir_all(format!("{base}/{sub}"));
        }
        crate::log!("app home folder: {base}");
        base
    })
    .clone()
}

fn app_dir() -> String {
    crate::loader::loaded().app_dir.clone()
}

fn info_dictionary_object() -> Id {
    *INFO.get_or_init(|| {
        let path = format!("{}/Info.plist", app_dir());
        match std::fs::read(&path).ok().and_then(|bytes| plist::parse(&bytes)) {
            Some(value) => plist::to_object(&value),
            None => {
                crate::log!("can't read {path}");
                collections::dictionary_from(&[])
            }
        }
    })
}

const NS_LIBRARY_DIRECTORY: usize = 5;
const NS_DOCUMENT_DIRECTORY: usize = 9;
const NS_CACHES_DIRECTORY: usize = 13;
const NS_APPLICATION_SUPPORT_DIRECTORY: usize = 14;

extern "C" fn search_path_for_directories(directory: usize, _domains: usize, _expand: bool) -> Id {
    let home = home();
    let path = match directory {
        NS_DOCUMENT_DIRECTORY => format!("{home}/Documents"),
        NS_LIBRARY_DIRECTORY => format!("{home}/Library"),
        NS_CACHES_DIRECTORY => format!("{home}/Library/Caches"),
        NS_APPLICATION_SUPPORT_DIRECTORY => format!("{home}/Library/Application Support"),
        other => {
            crate::log!("NSSearchPathForDirectoriesInDomains: folder kind {other} not known, answered none");
            return collections::array_from(&[]);
        }
    };
    collections::array_from(&[string::make(&path)])
}

extern "C" fn home_directory() -> Id {
    string::make(&home())
}

extern "C" fn temporary_directory() -> Id {
    string::make(&format!("{}/tmp/", home()))
}

// ----- NSLog -----

crate::libsystem::libc::variadic!(ios_nslog, 1, ios_nslog_impl);

#[no_mangle]
extern "C" fn ios_nslog_impl(format: Id, va: *const u64) {
    crate::log!("NSLog: {}", string::format_object(format, va));
}

extern "C" fn nslogv(format: Id, va: *const u64) {
    ios_nslog_impl(format, va);
}

extern "C" fn set_uncaught_exception_handler(_handler: usize) {}

// ----- NSBundle: { isa, folder path string } -----

static MAIN_BUNDLE: OnceLock<Id> = OnceLock::new();

fn bundle_path_of(this: Id) -> String {
    string::get(unsafe { *((this + 8) as *const Id) })
}

extern "C" fn main_bundle(_: Class, _: Sel) -> Id {
    *MAIN_BUNDLE.get_or_init(|| {
        let object = runtime::alloc_instance(runtime::class_named("NSBundle"));
        unsafe { *((object + 8) as *mut Id) = string::make(&app_dir()) };
        object
    })
}

/// Bundles opened by path, one per folder (Apple hands back the same one).
static BUNDLES: Mutex<Option<std::collections::HashMap<String, Id>>> = Mutex::new(None);

/// The bundle of an existing folder (a relative path is inside the app's
/// own folder, where mainBundle looks); None for anything else, as on Apple.
fn bundle_for_path(path: Id, fresh: Option<Id>) -> Option<Id> {
    if path == 0 {
        return None;
    }
    let mut text = string::get(path);
    if !text.starts_with('/') {
        text = format!("{}/{text}", app_dir());
    }
    while text.len() > 1 && text.ends_with('/') {
        text.pop();
    }
    if !std::path::Path::new(&text).is_dir() {
        return None;
    }
    if crate::loader::try_loaded().is_some() && text == app_dir().trim_end_matches('/') {
        return Some(main_bundle(0, 0));
    }
    let mut guard = BUNDLES.lock().unwrap();
    let object = *guard.get_or_insert_with(Default::default).entry(text.clone()).or_insert_with(|| {
        let object = fresh.unwrap_or_else(|| runtime::alloc_instance(runtime::class_named("NSBundle")));
        unsafe { *((object + 8) as *mut Id) = string::make(&text) };
        object
    });
    Some(object)
}

extern "C" fn bundle_with_path(_: Class, _: Sel, path: Id) -> Id {
    bundle_for_path(path, None).unwrap_or(0)
}

extern "C" fn bundle_init_with_path(this: Id, _: Sel, path: Id) -> Id {
    bundle_for_path(path, Some(this)).unwrap_or(0)
}

extern "C" fn bundle_path(this: Id, _: Sel) -> Id {
    string::make(&bundle_path_of(this))
}

extern "C" fn executable_path(_this: Id, _: Sel) -> Id {
    string::make(&crate::loader::loaded().path)
}

extern "C" fn bundle_identifier_method(_this: Id, _: Sel) -> Id {
    string::make(&bundle_identifier())
}

extern "C" fn info_dictionary(_this: Id, _: Sel) -> Id {
    info_dictionary_object()
}

extern "C" fn object_for_info_key(_this: Id, _: Sel, key: Id) -> Id {
    collections::dictionary_get(info_dictionary_object(), key)
}

fn find_resource(this: Id, name: Id, kind: Id, folder: Id) -> Option<String> {
    let mut file = string::get(name);
    let kind = string::get(kind);
    if !kind.is_empty() {
        file = format!("{file}.{kind}");
    }
    let base = bundle_path_of(this);
    let folder = string::get(folder);
    let candidates = if folder.is_empty() {
        vec![format!("{base}/{file}"), format!("{base}/English.lproj/{file}"), format!("{base}/en.lproj/{file}")]
    } else {
        vec![format!("{base}/{folder}/{file}")]
    };
    candidates.into_iter().find(|path| std::path::Path::new(path).exists())
}

extern "C" fn path_for_resource(this: Id, _: Sel, name: Id, kind: Id) -> Id {
    find_resource(this, name, kind, 0).map(|p| string::make(&p)).unwrap_or(0)
}

extern "C" fn path_for_resource_in(this: Id, _: Sel, name: Id, kind: Id, folder: Id) -> Id {
    find_resource(this, name, kind, folder).map(|p| string::make(&p)).unwrap_or(0)
}

extern "C" fn localized_string(_this: Id, _: Sel, key: Id, value: Id, _table: Id) -> Id {
    if value != 0 && string::get(value).len() > 0 { value } else { key }
}

extern "C" fn preferred_localizations(_this: Id, _: Sel) -> Id {
    collections::array_from(&[string::make("en")])
}

// ----- NSUserDefaults: values kept in memory and saved as a property list -----

struct Defaults {
    values: Id,
    registered: Id,
}

static DEFAULTS: Mutex<Option<Defaults>> = Mutex::new(None);
static DEFAULTS_OBJECT: OnceLock<Id> = OnceLock::new();

fn defaults_path() -> String {
    format!("{}/Library/Preferences/{}.plist", home(), bundle_identifier())
}

fn with_defaults<R>(f: impl FnOnce(&mut Defaults) -> R) -> R {
    let mut guard = DEFAULTS.lock().unwrap();
    if guard.is_none() {
        let values = std::fs::read(defaults_path())
            .ok()
            .and_then(|bytes| plist::parse(&bytes))
            .map(|v| plist::to_object(&v))
            .filter(|&o| collections::is_dictionary(o))
            .unwrap_or_else(|| runtime::send(collections::dictionary_from(&[]), "mutableCopy", &[]));
        let registered = runtime::send(collections::dictionary_from(&[]), "mutableCopy", &[]);
        *guard = Some(Defaults { values, registered });
    }
    f(guard.as_mut().unwrap())
}

fn defaults_get(key: Id) -> Id {
    with_defaults(|d| {
        let value = collections::dictionary_get(d.values, key);
        if value != 0 { value } else { collections::dictionary_get(d.registered, key) }
    })
}

fn defaults_set(key: Id, value: Id) {
    let values = with_defaults(|d| d.values);
    if value == 0 {
        runtime::send(values, "removeObjectForKey:", &[key]);
    } else {
        runtime::send(values, "setObject:forKey:", &[value, key]);
    }
}

extern "C" fn standard_defaults(_: Class, _: Sel) -> Id {
    *DEFAULTS_OBJECT.get_or_init(|| runtime::alloc_instance(runtime::class_named("NSUserDefaults")))
}

extern "C" fn defaults_object_for_key(_this: Id, _: Sel, key: Id) -> Id {
    defaults_get(key)
}

extern "C" fn defaults_string_for_key(_this: Id, _: Sel, key: Id) -> Id {
    let value = defaults_get(key);
    if string::is_string(value) {
        value
    } else if collections::is_number(value) {
        runtime::send(value, "stringValue", &[])
    } else {
        0
    }
}

extern "C" fn defaults_typed_object(_this: Id, _: Sel, key: Id) -> Id {
    defaults_get(key)
}

fn number_of(value: Id) -> f64 {
    if collections::is_number(value) {
        collections::number_as_f64(value)
    } else if string::is_string(value) {
        let text = string::get(value);
        match text.as_str() {
            "YES" | "yes" | "true" => 1.0,
            _ => text.trim().parse().unwrap_or(0.0),
        }
    } else {
        0.0
    }
}

extern "C" fn defaults_bool(_this: Id, _: Sel, key: Id) -> bool {
    number_of(defaults_get(key)) != 0.0
}

extern "C" fn defaults_integer(_this: Id, _: Sel, key: Id) -> isize {
    number_of(defaults_get(key)) as isize
}

extern "C" fn defaults_float(_this: Id, _: Sel, key: Id) -> f32 {
    number_of(defaults_get(key)) as f32
}

extern "C" fn defaults_double(_this: Id, _: Sel, key: Id) -> f64 {
    number_of(defaults_get(key))
}

extern "C" fn defaults_set_object(_this: Id, _: Sel, value: Id, key: Id) {
    defaults_set(key, value);
}

extern "C" fn defaults_set_bool(_this: Id, _: Sel, value: u8, key: Id) {
    defaults_set(key, collections::number_bool(value != 0));
}

extern "C" fn defaults_set_integer(_this: Id, _: Sel, value: isize, key: Id) {
    defaults_set(key, collections::number_int(value as i64));
}

extern "C" fn defaults_set_float(_this: Id, _: Sel, value: f32, key: Id) {
    defaults_set(key, collections::number_float(value as f64));
}

extern "C" fn defaults_set_double(_this: Id, _: Sel, value: f64, key: Id) {
    defaults_set(key, collections::number_float(value));
}

extern "C" fn defaults_remove(_this: Id, _: Sel, key: Id) {
    defaults_set(key, 0);
}

extern "C" fn defaults_register(_this: Id, _: Sel, values: Id) {
    let registered = with_defaults(|d| d.registered);
    runtime::send(registered, "addEntriesFromDictionary:", &[values]);
}

extern "C" fn defaults_synchronize(_this: Id, _: Sel) -> bool {
    let values = with_defaults(|d| d.values);
    match plist::from_object(values) {
        Some(value) => {
            let path = defaults_path();
            let temporary = format!("{path}.part");
            std::fs::write(&temporary, plist::to_xml(&value)).and_then(|_| std::fs::rename(&temporary, &path)).is_ok()
        }
        None => false,
    }
}

extern "C" fn defaults_dictionary(_this: Id, _: Sel) -> Id {
    let (values, registered) = with_defaults(|d| (d.values, d.registered));
    let merged = runtime::send(registered, "mutableCopy", &[]);
    runtime::send(merged, "addEntriesFromDictionary:", &[values]);
    merged
}

extern "C" fn defaults_persistent_domain(_this: Id, _: Sel, name: Id) -> Id {
    if string::get(name) != bundle_identifier() {
        return 0;
    }
    let values = with_defaults(|d| d.values);
    runtime::send(values, "copy", &[])
}

extern "C" fn defaults_set_persistent_domain(_this: Id, _: Sel, domain: Id, name: Id) {
    if string::get(name) != bundle_identifier() {
        crate::log!("settings for another app ({}) were not saved", string::get(name));
        return;
    }
    let copy = runtime::send(domain, "mutableCopy", &[]);
    with_defaults(|d| d.values = copy);
}

extern "C" fn defaults_remove_persistent_domain(this: Id, s: Sel, name: Id) {
    defaults_set_persistent_domain(this, s, collections::dictionary_from(&[]), name);
}

// ----- NSNotificationCenter -----

#[derive(Clone)]
struct Observer {
    target: Id,
    selector: Sel,
    name: Option<String>,
    sender: Id,
    block: usize,
}

static OBSERVERS: Mutex<Vec<Observer>> = Mutex::new(Vec::new());
static CENTER: OnceLock<Id> = OnceLock::new();

extern "C" fn default_center(_: Class, _: Sel) -> Id {
    *CENTER.get_or_init(|| runtime::alloc_instance(runtime::class_named("NSNotificationCenter")))
}

fn name_of(name: Id) -> Option<String> {
    if name == 0 { None } else { Some(string::get(name)) }
}

extern "C" fn add_observer(_this: Id, _: Sel, target: Id, selector: Sel, name: Id, sender: Id) {
    OBSERVERS.lock().unwrap().push(Observer { target, selector, name: name_of(name), sender, block: 0 });
}

extern "C" fn add_observer_block(_this: Id, _: Sel, name: Id, sender: Id, _queue: Id, block: usize) -> Id {
    let block = crate::libsystem::mach::block_copy(block as *mut _) as usize;
    let token = runtime::alloc_instance(runtime::class_named("NSObject"));
    OBSERVERS.lock().unwrap().push(Observer { target: token, selector: 0, name: name_of(name), sender, block });
    token
}

extern "C" fn remove_observer(_this: Id, _: Sel, target: Id) {
    OBSERVERS.lock().unwrap().retain(|o| o.target != target);
}

extern "C" fn remove_observer_named(_this: Id, _: Sel, target: Id, name: Id, _sender: Id) {
    let name = name_of(name);
    OBSERVERS.lock().unwrap().retain(|o| !(o.target == target && (name.is_none() || o.name == name)));
}

fn notification(name: Id, sender: Id, info: Id) -> Id {
    let object = runtime::alloc_instance(runtime::class_named("NSNotification"));
    unsafe {
        *((object + 8) as *mut Id) = name;
        *((object + 16) as *mut Id) = sender;
        *((object + 24) as *mut Id) = info;
    }
    object
}

pub fn post(name: &str, sender: Id, info: Id) {
    let note = notification(string::make(name), sender, info);
    deliver(note);
}

fn deliver(note: Id) {
    let (name, sender) = unsafe { (string::get(*((note + 8) as *const Id)), *((note + 16) as *const Id)) };
    let matching: Vec<Observer> = OBSERVERS
        .lock()
        .unwrap()
        .iter()
        .filter(|o| o.name.as_deref().map(|n| n == name).unwrap_or(true) && (o.sender == 0 || o.sender == sender))
        .cloned()
        .collect();
    for observer in matching {
        if observer.block != 0 {
            let header = observer.block as *const crate::libsystem::mach::BlockHeader;
            let invoke: extern "C" fn(usize, Id) = unsafe { std::mem::transmute((*header).invoke) };
            invoke(observer.block, note);
        } else {
            let selector_name = lock().sel_name(observer.selector);
            runtime::send(observer.target, &selector_name, &[note]);
        }
    }
}

extern "C" fn post_notification(_this: Id, _: Sel, note: Id) {
    deliver(note);
}

extern "C" fn post_name_object(_this: Id, _: Sel, name: Id, sender: Id) {
    deliver(notification(name, sender, 0));
}

extern "C" fn post_name_object_info(_this: Id, _: Sel, name: Id, sender: Id, info: Id) {
    deliver(notification(name, sender, info));
}

extern "C" fn note_name(this: Id, _: Sel) -> Id {
    unsafe { *((this + 8) as *const Id) }
}

extern "C" fn note_object(this: Id, _: Sel) -> Id {
    unsafe { *((this + 16) as *const Id) }
}

extern "C" fn note_user_info(this: Id, _: Sel) -> Id {
    unsafe { *((this + 24) as *const Id) }
}

extern "C" fn note_with_name(_: Class, _: Sel, name: Id, sender: Id) -> Id {
    notification(name, sender, 0)
}

extern "C" fn note_with_name_info(_: Class, _: Sel, name: Id, sender: Id, info: Id) -> Id {
    notification(name, sender, info)
}

// ----- NSProcessInfo -----

static PROCESS_INFO: OnceLock<Id> = OnceLock::new();

extern "C" fn process_info(_: Class, _: Sel) -> Id {
    *PROCESS_INFO.get_or_init(|| runtime::alloc_instance(runtime::class_named("NSProcessInfo")))
}

extern "C" fn process_arguments(_this: Id, _: Sel) -> Id {
    collections::array_from(&[string::make(&crate::loader::loaded().path)])
}

extern "C" fn process_environment(_this: Id, _: Sel) -> Id {
    collections::dictionary_from(&[])
}

extern "C" fn process_name(_this: Id, _: Sel) -> Id {
    let path = &crate::loader::loaded().path;
    string::make(path.rsplit('/').next().unwrap_or("game"))
}

extern "C" fn process_physical_memory(_this: Id, _: Sel) -> u64 {
    1 << 30
}

extern "C" fn process_processor_count(_this: Id, _: Sel) -> usize {
    2
}

extern "C" fn process_os_version_string(_this: Id, _: Sel) -> Id {
    string::make("Version 8.2 (Build 12D508)")
}

extern "C" fn process_system_uptime(_this: Id, _: Sel) -> f64 {
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) };
    t.tv_sec as f64 + t.tv_nsec as f64 / 1e9
}

extern "C" fn process_identifier(_this: Id, _: Sel) -> i32 {
    unsafe { libc::getpid() }
}

extern "C" fn process_globally_unique(_this: Id, _: Sel) -> Id {
    let mut bytes = [0u8; 16];
    unsafe { libc::getrandom(bytes.as_mut_ptr() as *mut _, 16, 0) };
    string::make(&bytes.iter().map(|b| format!("{b:02X}")).collect::<String>())
}

/// An NSString constant whose text is its own name (how Apple defines most keys).
fn named_constant(r: &mut Registry, name: &str) {
    r.data(name, leak(string::make(name)));
}

pub fn register(r: &mut Registry) {
    r.function("NSSearchPathForDirectoriesInDomains", search_path_for_directories as usize);
    r.function("NSHomeDirectory", home_directory as usize);
    r.function("NSTemporaryDirectory", temporary_directory as usize);
    r.function("NSLog", ios_nslog as usize);
    r.function("NSLogv", nslogv as usize);
    r.function("NSSetUncaughtExceptionHandler", set_uncaught_exception_handler as usize);

    let bundle = define_class("NSBundle", "NSObject", 16);
    class_methods(bundle, &[("mainBundle", main_bundle as usize), ("bundleWithPath:", bundle_with_path as usize)]);
    methods(
        bundle,
        &[
            ("initWithPath:", bundle_init_with_path as usize),
            ("bundlePath", bundle_path as usize),
            ("resourcePath", bundle_path as usize),
            ("executablePath", executable_path as usize),
            ("bundleIdentifier", bundle_identifier_method as usize),
            ("infoDictionary", info_dictionary as usize),
            ("localizedInfoDictionary", info_dictionary as usize),
            ("objectForInfoDictionaryKey:", object_for_info_key as usize),
            ("pathForResource:ofType:", path_for_resource as usize),
            ("pathForResource:ofType:inDirectory:", path_for_resource_in as usize),
            ("localizedStringForKey:value:table:", localized_string as usize),
            ("preferredLocalizations", preferred_localizations as usize),
        ],
    );

    let defaults = define_class("NSUserDefaults", "NSObject", 8);
    class_methods(defaults, &[("standardUserDefaults", standard_defaults as usize)]);
    methods(
        defaults,
        &[
            ("objectForKey:", defaults_object_for_key as usize),
            ("stringForKey:", defaults_string_for_key as usize),
            ("arrayForKey:", defaults_typed_object as usize),
            ("dictionaryForKey:", defaults_typed_object as usize),
            ("dataForKey:", defaults_typed_object as usize),
            ("boolForKey:", defaults_bool as usize),
            ("integerForKey:", defaults_integer as usize),
            ("floatForKey:", defaults_float as usize),
            ("doubleForKey:", defaults_double as usize),
            ("setObject:forKey:", defaults_set_object as usize),
            ("setValue:forKey:", defaults_set_object as usize),
            ("setBool:forKey:", defaults_set_bool as usize),
            ("setInteger:forKey:", defaults_set_integer as usize),
            ("setFloat:forKey:", defaults_set_float as usize),
            ("setDouble:forKey:", defaults_set_double as usize),
            ("removeObjectForKey:", defaults_remove as usize),
            ("registerDefaults:", defaults_register as usize),
            ("synchronize", defaults_synchronize as usize),
            ("dictionaryRepresentation", defaults_dictionary as usize),
            ("persistentDomainForName:", defaults_persistent_domain as usize),
            ("setPersistentDomain:forName:", defaults_set_persistent_domain as usize),
            ("removePersistentDomainForName:", defaults_remove_persistent_domain as usize),
        ],
    );

    let center = define_class("NSNotificationCenter", "NSObject", 8);
    class_methods(center, &[("defaultCenter", default_center as usize)]);
    methods(
        center,
        &[
            ("addObserver:selector:name:object:", add_observer as usize),
            ("addObserverForName:object:queue:usingBlock:", add_observer_block as usize),
            ("removeObserver:", remove_observer as usize),
            ("removeObserver:name:object:", remove_observer_named as usize),
            ("postNotification:", post_notification as usize),
            ("postNotificationName:object:", post_name_object as usize),
            ("postNotificationName:object:userInfo:", post_name_object_info as usize),
        ],
    );
    let note = define_class("NSNotification", "NSObject", 32);
    methods(note, &[("name", note_name as usize), ("object", note_object as usize), ("userInfo", note_user_info as usize)]);
    class_methods(
        note,
        &[
            ("notificationWithName:object:", note_with_name as usize),
            ("notificationWithName:object:userInfo:", note_with_name_info as usize),
        ],
    );

    let process = define_class("NSProcessInfo", "NSObject", 8);
    class_methods(process, &[("processInfo", process_info as usize)]);
    methods(
        process,
        &[
            ("arguments", process_arguments as usize),
            ("environment", process_environment as usize),
            ("processName", process_name as usize),
            ("physicalMemory", process_physical_memory as usize),
            ("processorCount", process_processor_count as usize),
            ("activeProcessorCount", process_processor_count as usize),
            ("operatingSystemVersionString", process_os_version_string as usize),
            ("systemUptime", process_system_uptime as usize),
            ("processIdentifier", process_identifier as usize),
            ("globallyUniqueString", process_globally_unique as usize),
        ],
    );

    for name in [
        "UIApplicationDidBecomeActiveNotification",
        "UIApplicationDidEnterBackgroundNotification",
        "UIApplicationDidFinishLaunchingNotification",
        "UIApplicationWillEnterForegroundNotification",
        "UIApplicationWillResignActiveNotification",
        "UIApplicationWillTerminateNotification",
        "UIApplicationLaunchOptionsLocalNotificationKey",
        "UIApplicationLaunchOptionsRemoteNotificationKey",
        "UIApplicationLaunchOptionsSourceApplicationKey",
        "UIApplicationLaunchOptionsURLKey",
        "UIDeviceOrientationDidChangeNotification",
        "UIScreenDidConnectNotification",
        "UIScreenDidDisconnectNotification",
        "NSInternalInconsistencyException",
        "NSInvalidArgumentException",
        "NSPOSIXErrorDomain",
        "NSMachErrorDomain",
        "NSURLErrorDomain",
        "NSFilePosixPermissions",
        "NSFileSystemFreeSize",
        "NSFileSystemSize",
    ] {
        named_constant(r, name);
    }
    for (name, value) in [
        ("NSLocalizedDescriptionKey", "NSLocalizedDescription"),
        ("NSLocalizedFailureReasonErrorKey", "NSLocalizedFailureReason"),
        ("NSUnderlyingErrorKey", "NSUnderlyingError"),
        ("NSKeyValueChangeKindKey", "kind"),
        ("NSKeyValueChangeNewKey", "new"),
        ("NSKeyValueChangeOldKey", "old"),
        ("NSKeyValueChangeIndexesKey", "indexes"),
        ("NSKeyValueChangeNotificationIsPriorKey", "notificationIsPrior"),
    ] {
        r.data(name, leak(string::make(value)));
    }
    r.data("NSDefaultRunLoopMode", leak(string::make("kCFRunLoopDefaultMode")));
    r.data("kCFRunLoopDefaultMode", leak(string::make("kCFRunLoopDefaultMode")));
    r.data("kCFBundleIdentifierKey", leak(string::make("CFBundleIdentifier")));
    r.data("kCFBundleVersionKey", leak(string::make("CFBundleVersion")));
    r.data("NSLocaleCountryCode", leak(string::make("kCFLocaleCountryCodeKey")));
    r.data("NSLocaleLanguageCode", leak(string::make("kCFLocaleLanguageCodeKey")));
    r.data("NSLocaleCurrencyCode", leak(string::make("currency")));
    r.data("NSLocaleVariantCode", leak(string::make("kCFLocaleVariantCodeKey")));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundles_open_existing_folders_only() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let class = runtime::class_named("NSBundle");
        assert!(runtime::responds(class, "bundleWithPath:"), "+[NSBundle bundleWithPath:] is missing");
        let folder = std::env::temp_dir().join(format!("gamebridge-bundle-{}", std::process::id()));
        std::fs::create_dir_all(folder.join("Art.bundle")).unwrap();
        std::fs::write(folder.join("Art.bundle/hero.png"), b"png").unwrap();
        let path = folder.join("Art.bundle").to_string_lossy().into_owned();
        let bundle = runtime::send(class, "bundleWithPath:", &[string::make(&path)]);
        assert_ne!(bundle, 0);
        assert_eq!(string::get(runtime::send(bundle, "bundlePath", &[])), path);
        let found = runtime::send(bundle, "pathForResource:ofType:", &[string::make("hero"), string::make("png")]);
        assert_eq!(string::get(found), format!("{path}/hero.png"));
        // The same folder gives the same bundle; a missing one or a file gives nil.
        assert_eq!(runtime::send(class, "bundleWithPath:", &[string::make(&format!("{path}/"))]), bundle);
        assert_eq!(runtime::send(class, "bundleWithPath:", &[string::make(&format!("{path}/nothing"))]), 0);
        assert_eq!(runtime::send(class, "bundleWithPath:", &[string::make(&format!("{path}/hero.png"))]), 0);
        let fresh = runtime::send(runtime::alloc_instance(class), "initWithPath:", &[string::make(&format!("{path}/nothing"))]);
        assert_eq!(fresh, 0);
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
