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
    crate::loader::app_dir()
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

/// The game's NSSetUncaughtExceptionHandler function (0: none).
pub static UNCAUGHT_EXCEPTION_HANDLER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

extern "C" fn set_uncaught_exception_handler(handler: usize) {
    UNCAUGHT_EXCEPTION_HANDLER.store(handler, std::sync::atomic::Ordering::SeqCst);
}

extern "C" fn get_uncaught_exception_handler() -> usize {
    UNCAUGHT_EXCEPTION_HANDLER.load(std::sync::atomic::Ordering::SeqCst)
}

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

/// -[NSBundle bundleURL]: a file URL of the bundle's folder.
extern "C" fn bundle_url(this: Id, _: Sel) -> Id {
    let path = string::make(&bundle_path_of(this));
    runtime::send(runtime::class_named("NSURL"), "fileURLWithPath:", &[path])
}

/// CFBundleGetMainBundle: the same object as +[NSBundle mainBundle].
extern "C" fn cf_bundle_get_main_bundle() -> Id {
    main_bundle(0, 0)
}

/// CFBundleCopyBundleURL: a file URL of the bundle's folder (+1).
extern "C" fn cf_bundle_copy_bundle_url(bundle: Id) -> Id {
    if bundle == 0 {
        return 0;
    }
    runtime::send(bundle, "bundleURL", &[])
}

extern "C" fn executable_path(_this: Id, _: Sel) -> Id {
    string::make(&crate::loader::program_path())
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

/// The languages a folder has ("<name>.lproj" folders in it), read once
/// per folder (a bundle doesn't change while the app runs).
fn language_folders(place: &str) -> std::sync::Arc<Vec<String>> {
    static FOUND: OnceLock<Mutex<std::collections::HashMap<String, std::sync::Arc<Vec<String>>>>> = OnceLock::new();
    let found = FOUND.get_or_init(Default::default);
    if let Some(languages) = found.lock().unwrap().get(place) {
        return languages.clone();
    }
    let languages: Vec<String> = std::fs::read_dir(place)
        .map(|entries| entries.filter_map(|e| e.ok()?.file_name().to_str()?.strip_suffix(".lproj").map(String::from)).collect())
        .unwrap_or_default();
    let languages = std::sync::Arc::new(languages);
    found.lock().unwrap().insert(place.to_string(), languages.clone());
    languages
}

/// CFBundle's search: in the folder, then in its language folder (the
/// picked localization, then the development ones); in each place the
/// device's own variant ("name~iphone.png") before the plain file.
fn find_resource(this: Id, name: Id, kind: Id, folder: Id) -> Option<String> {
    let stem = string::get(name);
    let kind = string::get(kind);
    let kind = kind.trim_start_matches('.');
    let (stem, kind) = if kind.is_empty() {
        // "hero.png" with no type: the variant goes before the extension.
        match stem.rsplit_once('.') {
            Some((s, k)) if !s.is_empty() => (s.to_string(), k.to_string()),
            _ => (stem.clone(), String::new()),
        }
    } else {
        (stem, kind.to_string())
    };
    if stem.is_empty() && kind.is_empty() {
        return None;
    }
    let device = if crate::libsystem::io::device_model().starts_with("iPad") { "ipad" } else { "iphone" };
    let file = |suffix: &str| if kind.is_empty() { format!("{stem}{suffix}") } else { format!("{stem}{suffix}.{kind}") };
    let files = [file(&format!("~{device}")), file("")];
    let base = bundle_path_of(this);
    let folder = string::get(folder);
    let place = if folder.is_empty() { base.clone() } else { format!("{base}/{folder}") };
    let languages = language_folders(&place);
    let mut places = vec![place.clone()];
    for language in pick_localization(&languages).into_iter().chain(["English".into(), "en".into(), "Base".into()]) {
        let lproj = format!("{place}/{language}.lproj");
        if !places.contains(&lproj) {
            places.push(lproj);
        }
    }
    places.iter().flat_map(|p| files.iter().map(move |f| format!("{p}/{f}"))).find(|path| std::path::Path::new(path).exists())
}

extern "C" fn path_for_resource(this: Id, _: Sel, name: Id, kind: Id) -> Id {
    find_resource(this, name, kind, 0).map(|p| string::make(&p)).unwrap_or(0)
}

extern "C" fn path_for_resource_in(this: Id, _: Sel, name: Id, kind: Id, folder: Id) -> Id {
    find_resource(this, name, kind, folder).map(|p| string::make(&p)).unwrap_or(0)
}

/// A strings table of a bundle, read once: `<table>.strings` from the picked
/// language folder, then Base.lproj, then the bundle's own folder.
fn strings_table(base: &str, table: &str) -> std::sync::Arc<std::collections::HashMap<String, String>> {
    type Tables = std::collections::HashMap<(String, String), std::sync::Arc<std::collections::HashMap<String, String>>>;
    static TABLES: OnceLock<Mutex<Tables>> = OnceLock::new();
    let tables = TABLES.get_or_init(Default::default);
    let wanted = (base.to_string(), table.to_string());
    if let Some(found) = tables.lock().unwrap().get(&wanted) {
        return found.clone();
    }
    let languages: Vec<String> = std::fs::read_dir(base)
        .map(|entries| entries.filter_map(|e| e.ok()?.file_name().to_str()?.strip_suffix(".lproj").map(String::from)).collect())
        .unwrap_or_default();
    let folders = pick_localization(&languages).map(|l| format!("{base}/{l}.lproj")).into_iter().chain([format!("{base}/Base.lproj"), base.to_string()]);
    let mut words = std::collections::HashMap::new();
    if let Some(plist::Value::Dict(entries)) = folders.filter_map(|f| std::fs::read(format!("{f}/{table}.strings")).ok()).find_map(|bytes| plist::parse_strings(&bytes)) {
        words.extend(entries.into_iter().filter_map(|(k, v)| match v {
            plist::Value::String(s) => Some((k, s)),
            _ => None,
        }));
    }
    let words = std::sync::Arc::new(words);
    tables.lock().unwrap().insert(wanted, words.clone());
    words
}

/// The key's text in the table (nil or "" means Localizable); a missing key
/// gives the value, or the key when the value is nil or empty.
extern "C" fn localized_string(this: Id, _: Sel, key: Id, value: Id, table: Id) -> Id {
    let name = Some(string::get(table)).filter(|t| !t.is_empty()).unwrap_or_else(|| "Localizable".into());
    if key != 0 {
        if let Some(text) = strings_table(&bundle_path_of(this), &name).get(&string::get(key)) {
            return string::make(text);
        }
    }
    if value != 0 && !string::get(value).is_empty() { value } else { key }
}

extern "C" fn preferred_localizations(_this: Id, _: Sel) -> Id {
    collections::array_from(&[string::make("en")])
}

/// The bundle's languages: its "<name>.lproj" folders.
extern "C" fn localizations(this: Id, _: Sel) -> Id {
    let mut names: Vec<String> = std::fs::read_dir(bundle_path_of(this))
        .map(|entries| entries.filter_map(|e| e.ok()?.file_name().to_str()?.strip_suffix(".lproj").map(String::from)).collect())
        .unwrap_or_default();
    names.sort();
    collections::array_from(&names.iter().map(|n| string::make(n)).collect::<Vec<_>>())
}

/// Which of `offered` to use for the phone's languages (English here):
/// the first match, else the development language's, else the first one.
pub fn pick_localization(offered: &[String]) -> Option<String> {
    let wanted = ["en", "English", "en_US", "en-US", "Base"];
    wanted
        .iter()
        .find_map(|w| offered.iter().find(|o| o.as_str() == *w))
        .or_else(|| offered.iter().find(|o| o.starts_with("en")))
        .or_else(|| offered.first())
        .cloned()
}

extern "C" fn preferred_localizations_from(_: Class, _: Sel, offered: Id) -> Id {
    let offered: Vec<String> = collections::array_items(offered).into_iter().map(string::get).collect();
    let picked: Vec<Id> = pick_localization(&offered).into_iter().map(|p| string::make(&p)).collect();
    collections::array_from(&picked)
}

extern "C" fn preferred_localizations_for(class: Class, s: Sel, offered: Id, _preferences: Id) -> Id {
    preferred_localizations_from(class, s, offered)
}

/// Every file of a type ("png", or all for nil/"") in a folder of the bundle
/// and in its language folder, full paths, the folder's own first.
extern "C" fn paths_for_resources(this: Id, _: Sel, kind: Id, folder: Id) -> Id {
    let base = bundle_path_of(this);
    let folder = string::get(folder);
    let kind = string::get(kind);
    let kind = kind.trim_start_matches('.');
    let place = |root: String| if folder.is_empty() { root } else { format!("{root}/{folder}") };
    let mut places = vec![place(base.clone())];
    let languages: Vec<String> = collections::array_items(localizations(this, 0)).into_iter().map(string::get).collect();
    if let Some(language) = pick_localization(&languages) {
        places.push(place(format!("{base}/{language}.lproj")));
    }
    let mut found = Vec::new();
    for place in places {
        let Ok(entries) = std::fs::read_dir(&place) else { continue };
        let mut names: Vec<String> = entries.filter_map(|e| e.ok()?.file_name().to_str().map(String::from)).collect();
        names.sort();
        for name in names {
            // A type of several parts ("uispec.plist") matches the whole end
            // of the name, as CFBundle's does.
            let matches = kind.is_empty() || name.strip_suffix(kind).and_then(|stem| stem.strip_suffix('.')).is_some_and(|stem| !stem.is_empty());
            if matches && !name.ends_with(".lproj") {
                found.push(string::make(&format!("{place}/{name}")));
            }
        }
    }
    collections::array_from(&found)
}

// ----- NSUserDefaults: values kept in memory and saved as a property list -----

struct Defaults {
    values: Id,
    registered: Id,
}

static DEFAULTS: Mutex<Option<Defaults>> = Mutex::new(None);
static DEFAULTS_OBJECT: OnceLock<Id> = OnceLock::new();

/// Where tests keep the settings file (the app's home isn't theirs to write).
#[cfg(test)]
static TEST_DEFAULTS_PATH: Mutex<Option<String>> = Mutex::new(None);

fn defaults_path() -> String {
    #[cfg(test)]
    if let Some(path) = TEST_DEFAULTS_PATH.lock().unwrap().clone() {
        return path;
    }
    format!("{}/Library/Preferences/{}.plist", home(), bundle_identifier())
}

fn with_defaults<R>(f: impl FnOnce(&mut Defaults) -> R) -> R {
    let mut guard = DEFAULTS.lock().unwrap();
    if guard.is_none() {
        // The values themselves come back immutable, as Apple's do.
        let values = std::fs::read(defaults_path())
            .ok()
            .and_then(|bytes| plist::parse(&bytes))
            .map(|v| plist::to_immutable_object(&v))
            .filter(|&o| collections::is_dictionary(o))
            .map(|o| runtime::send(o, "mutableCopy", &[]))
            .unwrap_or_else(|| runtime::send(collections::dictionary_from(&[]), "mutableCopy", &[]));
        let registered = runtime::send(collections::dictionary_from(&[]), "mutableCopy", &[]);
        *guard = Some(Defaults { values, registered });
    }
    f(guard.as_mut().unwrap())
}

/// One lock around every read, change and save of the defaults: a save (the
/// timed one on the main thread, or a game thread's -synchronize) reads
/// the whole dictionary and writes the file while no other thread changes
/// either. The same thread may take it again (a key's -isEqual: may read
/// the defaults).
static DEFAULTS_LOCK: crate::libsystem::libc::parking::Reentrant = crate::libsystem::libc::parking::Reentrant::new();

fn locked_defaults<R>(work: impl FnOnce() -> R) -> R {
    DEFAULTS_LOCK.enter();
    let result = work();
    DEFAULTS_LOCK.exit();
    result
}

fn defaults_get(key: Id) -> Id {
    locked_defaults(|| {
        let (values, registered) = with_defaults(|d| (d.values, d.registered));
        let value = collections::dictionary_get(values, key);
        if value != 0 { value } else { collections::dictionary_get(registered, key) }
    })
}

/// Settings changed since they were last saved.
static DEFAULTS_CHANGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Seconds after a change that the settings save on their own (CFPreferences
/// saves by itself too, without the app calling -synchronize).
const DEFAULTS_SAVE_DELAY: f64 = 5.0;

/// Keeps an immutable copy of `value` (Apple's -setObject:forKey: copies a
/// property list deeply and refuses anything else).
fn defaults_set(key: Id, value: Id) {
    let copy = if value == 0 {
        0
    } else {
        match plist::from_object(value) {
            Some(list) => plist::to_immutable_object(&list),
            None => {
                crate::log!("NSUserDefaults: {} for key {} is not a property list; not stored", string::describe(value), string::describe(key));
                return;
            }
        }
    };
    locked_defaults(|| {
        let values = with_defaults(|d| d.values);
        if copy == 0 {
            runtime::send(values, "removeObjectForKey:", &[key]);
        } else {
            runtime::send(values, "setObject:forKey:", &[copy, key]);
        }
        defaults_changed();
    });
}

fn defaults_changed() {
    if !DEFAULTS_CHANGED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        save_later();
    }
}

fn save_later() {
    crate::fw::runloop::post_after(DEFAULTS_SAVE_DELAY, Box::new(save_changed_defaults));
}

/// Saves the settings if they changed since the last save.
pub fn save_changed_defaults() {
    if DEFAULTS_CHANGED.load(std::sync::atomic::Ordering::SeqCst) {
        defaults_synchronize(0, 0);
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
    locked_defaults(|| {
        let registered = with_defaults(|d| d.registered);
        runtime::send(registered, "addEntriesFromDictionary:", &[values]);
    });
}

/// Writes the settings file (whole: a temporary file renamed over it). The
/// changes count as saved only once the file is written; a failed save is
/// tried again later.
extern "C" fn defaults_synchronize(_this: Id, _: Sel) -> bool {
    locked_defaults(|| {
        let values = with_defaults(|d| d.values);
        let written = match plist::from_object(values) {
            Some(value) => {
                let path = defaults_path();
                let temporary = format!("{path}.part");
                std::fs::write(&temporary, plist::to_xml(&value)).and_then(|_| std::fs::rename(&temporary, &path)).is_ok()
            }
            None => false,
        };
        if written {
            DEFAULTS_CHANGED.store(false, std::sync::atomic::Ordering::SeqCst);
        } else if DEFAULTS_CHANGED.load(std::sync::atomic::Ordering::SeqCst) {
            save_later();
        }
        written
    })
}

extern "C" fn defaults_dictionary(_this: Id, _: Sel) -> Id {
    locked_defaults(|| {
        let (values, registered) = with_defaults(|d| (d.values, d.registered));
        let merged = runtime::send(registered, "mutableCopy", &[]);
        runtime::send(merged, "addEntriesFromDictionary:", &[values]);
        merged
    })
}

extern "C" fn defaults_persistent_domain(_this: Id, _: Sel, name: Id) -> Id {
    if string::get(name) != bundle_identifier() {
        return 0;
    }
    locked_defaults(|| {
        let values = with_defaults(|d| d.values);
        runtime::send(values, "copy", &[])
    })
}

extern "C" fn defaults_set_persistent_domain(_this: Id, _: Sel, domain: Id, name: Id) {
    if string::get(name) != bundle_identifier() {
        crate::log!("settings for another app ({}) were not saved", string::get(name));
        return;
    }
    let copy = runtime::send(domain, "mutableCopy", &[]);
    locked_defaults(|| {
        with_defaults(|d| d.values = copy);
        defaults_changed();
    });
}

extern "C" fn defaults_remove_persistent_domain(this: Id, s: Sel, name: Id) {
    defaults_set_persistent_domain(this, s, collections::dictionary_from(&[]), name);
}

// ----- NSNotificationCenter -----

#[derive(Clone)]
struct Observer {
    /// Each entry's own number: a post calls an entry only while it is
    /// still registered (Apple re-checks its record's token before each call).
    token: u64,
    target: Id,
    selector: Sel,
    name: Option<String>,
    sender: Id,
    block: usize,
    /// The NSOperationQueue a block observer runs on (0: the posting thread).
    queue: Id,
}

static OBSERVERS: Mutex<Vec<Observer>> = Mutex::new(Vec::new());
static NEXT_TOKEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
static CENTER: OnceLock<Id> = OnceLock::new();

extern "C" fn default_center(_: Class, _: Sel) -> Id {
    *CENTER.get_or_init(|| runtime::alloc_instance(runtime::class_named("NSNotificationCenter")))
}

fn name_of(name: Id) -> Option<String> {
    if name == 0 { None } else { Some(string::get(name)) }
}

fn register_observer(target: Id, selector: Sel, name: Id, sender: Id, block: usize, queue: Id) {
    let token = NEXT_TOKEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    OBSERVERS.lock().unwrap().push(Observer { token, target, selector, name: name_of(name), sender, block, queue });
}

extern "C" fn add_observer(_this: Id, _: Sel, target: Id, selector: Sel, name: Id, sender: Id) {
    register_observer(target, selector, name, sender, 0, 0);
}

extern "C" fn add_observer_block(_this: Id, _: Sel, name: Id, sender: Id, queue: Id, block: usize) -> Id {
    let block = crate::libsystem::mach::block_copy(block as *mut _) as usize;
    let token = runtime::alloc_instance(runtime::class_named("NSObject"));
    register_observer(token, 0, name, sender, block, queue);
    token
}

/// How many notification registrations name `object`, as observer or as
/// sender (for the "would dealloc" log, counted.rs).
pub fn notification_registrations_naming(object: Id) -> usize {
    OBSERVERS.lock().unwrap().iter().filter(|o| o.target == object || o.sender == object).count()
}

/// Removes the entries `gone` picks; their blocks are let go of (outside
/// the lock: a block's dispose may run game code).
fn remove_observers_where(gone: impl Fn(&Observer) -> bool) {
    let mut blocks = Vec::new();
    OBSERVERS.lock().unwrap().retain(|o| {
        if gone(o) {
            blocks.push(o.block);
        }
        !gone(o)
    });
    for block in blocks.into_iter().filter(|&b| b != 0) {
        crate::libsystem::mach::block_release(block as *mut _);
    }
}

extern "C" fn remove_observer(_this: Id, _: Sel, target: Id) {
    remove_observers_where(|o| o.target == target);
}

/// Removes `target`'s entries for that name (nil: any) and sender (nil: any).
extern "C" fn remove_observer_named(_this: Id, _: Sel, target: Id, name: Id, sender: Id) {
    let name = name_of(name);
    remove_observers_where(|o| o.target == target && (name.is_none() || o.name == name) && (sender == 0 || o.sender == sender));
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
        // An earlier observer may have removed this one (and freed it). A
        // block is held while it runs: removing its observer lets go of it.
        let held = {
            let observers = OBSERVERS.lock().unwrap();
            if !observers.iter().any(|o| o.token == observer.token) {
                continue;
            }
            (observer.block != 0).then(|| crate::libsystem::mach::block_copy(observer.block as *mut _) as usize)
        };
        if let Some(block) = held {
            call_block_observer(block, observer.queue, note);
            crate::libsystem::mach::block_release(block as *mut _);
        } else {
            let selector_name = lock().sel_name(observer.selector);
            runtime::send(observer.target, &selector_name, &[note]);
        }
    }
    // CFPreferences saves changed settings when the app leaves the screen
    // or ends, after the app's own handlers have run (however it was posted).
    if matches!(name.as_str(), "UIApplicationDidEnterBackgroundNotification" | "UIApplicationWillTerminateNotification") {
        save_changed_defaults();
    }
}

/// A block observer's block with its notification, made into a block for
/// an NSBlockOperation.
#[repr(C)]
struct QueuedNote {
    header: crate::libsystem::mach::BlockHeader,
    block: usize,
    note: Id,
}

extern "C" fn run_queued_note(this: usize) {
    let queued = unsafe { &*(this as *const QueuedNote) };
    invoke_note_block(queued.block, queued.note);
}

fn invoke_note_block(block: usize, note: Id) {
    let header = block as *const crate::libsystem::mach::BlockHeader;
    let invoke: extern "C" fn(usize, Id) = unsafe { std::mem::transmute((*header).invoke) };
    invoke(block, note);
}

/// Calls a block observer as Apple's -[__NSObserver _doit:] does: at once
/// for no queue, when the post is on that queue already, or for the main
/// queue on the main thread; else as an NSBlockOperation added to the queue,
/// the post waiting until it has run there.
///
/// As on Apple, a queue that can't run the operation (suspended) makes the
/// post wait until it can. The main-queue case can't deadlock our run loop
/// the way a private queue can: a post on the main thread runs its block at
/// once, as Apple's does, and one from a worker is run by the main loop.
fn call_block_observer(block: usize, queue: Id, note: Id) {
    let queues = runtime::class_named("NSOperationQueue");
    let on_main_queue = || crate::fw::runloop::is_main_thread() && runtime::send(queues, "mainQueue", &[]) == queue;
    if queue == 0 || runtime::send(queues, "currentQueue", &[]) == queue || on_main_queue() {
        invoke_note_block(block, note);
        return;
    }
    // A global block (block_copy keeps it as it is).
    let header = crate::libsystem::mach::BlockHeader { isa: 0, flags: 1 << 28, reserved: 0, invoke: run_queued_note as usize, descriptor: std::ptr::null() };
    let queued = Box::into_raw(Box::new(QueuedNote { header, block, note }));
    let operation = runtime::send(runtime::class_named("NSBlockOperation"), "new", &[]);
    runtime::send(operation, "addExecutionBlock:", &[queued as usize]);
    runtime::send(queue, "addOperation:", &[operation]);
    runtime::send(operation, "waitUntilFinished", &[]);
    // The operation ran the block (or was cancelled, which Apple's wait also
    // returns from); the block it held isn't needed any more.
    drop(unsafe { Box::from_raw(queued) });
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
    collections::array_from(&[string::make(&crate::loader::program_path())])
}

extern "C" fn process_environment(_this: Id, _: Sel) -> Id {
    collections::dictionary_from(&[])
}

extern "C" fn process_name(_this: Id, _: Sel) -> Id {
    let path = &crate::loader::program_path();
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

/// NSMACHOperatingSystem, what iOS answers.
extern "C" fn process_operating_system(_this: Id, _: Sel) -> usize {
    5
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
    r.function("CFBundleGetMainBundle", cf_bundle_get_main_bundle as usize);
    r.function("CFBundleCopyBundleURL", cf_bundle_copy_bundle_url as usize);
    r.function("NSSetUncaughtExceptionHandler", set_uncaught_exception_handler as usize);
    r.function("NSGetUncaughtExceptionHandler", get_uncaught_exception_handler as usize);

    let bundle = define_class("NSBundle", "NSObject", 16);
    class_methods(
        bundle,
        &[
            ("mainBundle", main_bundle as usize),
            ("bundleWithPath:", bundle_with_path as usize),
            ("preferredLocalizationsFromArray:", preferred_localizations_from as usize),
            ("preferredLocalizationsFromArray:forPreferences:", preferred_localizations_for as usize),
        ],
    );
    methods(
        bundle,
        &[
            ("initWithPath:", bundle_init_with_path as usize),
            ("bundlePath", bundle_path as usize),
            ("bundleURL", bundle_url as usize),
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
            ("localizations", localizations as usize),
            ("pathsForResourcesOfType:inDirectory:", paths_for_resources as usize),
        ],
    );

    let defaults = define_class("NSUserDefaults", "NSObject", 8);
    class_methods(defaults, &[("standardUserDefaults", standard_defaults as usize)]);
    methods(
        defaults,
        &[
            ("objectForKey:", defaults_object_for_key as usize),
            ("valueForKey:", defaults_object_for_key as usize),
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
            ("operatingSystem", process_operating_system as usize),
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
        "UIApplicationDidReceiveMemoryWarningNotification",
        "UIApplicationWillChangeStatusBarOrientationNotification",
        "UIApplicationDidChangeStatusBarOrientationNotification",
        "UIApplicationDidChangeStatusBarFrameNotification",
        "UIApplicationStatusBarOrientationUserInfoKey",
        "UILocalNotificationDefaultSoundName",
        "UITrackingRunLoopMode",
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
        "UIKeyboardWillShowNotification",
        "UIKeyboardDidShowNotification",
        "UIKeyboardWillHideNotification",
        "UIKeyboardDidHideNotification",
        "UIKeyboardWillChangeFrameNotification",
        "UIKeyboardDidChangeFrameNotification",
        "UIKeyboardFrameBeginUserInfoKey",
        "UIKeyboardFrameEndUserInfoKey",
        "UIKeyboardAnimationDurationUserInfoKey",
        "UIKeyboardAnimationCurveUserInfoKey",
        "NSMetadataQueryDidFinishGatheringNotification",
        "NSMetadataQueryDidUpdateNotification",
        "NSMetadataQueryUbiquitousDocumentsScope",
        "NSMetadataQueryUbiquitousDataScope",
        "NSURLUbiquitousItemIsDownloadedKey",
    ]
    .into_iter()
    .chain(crate::fw::typing::NOTIFICATIONS)
    {
        named_constant(r, name);
    }
    for (name, value) in [
        ("NSMetadataItemFSNameKey", "kMDItemFSName"),
        ("NSMetadataItemFSCreationDateKey", "kMDItemFSCreationDate"),
        ("NSMetadataItemURLKey", "kMDItemURL"),
        ("NSMetadataItemPathKey", "kMDItemPath"),
        ("ACAccountTypeIdentifierTwitter", "com.apple.twitter"),
        ("ACAccountTypeIdentifierFacebook", "com.apple.facebook"),
        ("ACFacebookAppIdKey", "ACFacebookAppIdKey"),
        ("ACFacebookPermissionsKey", "ACFacebookPermissionsKey"),
        ("ACFacebookAudienceKey", "ACFacebookAudienceKey"),
        ("ACFacebookAudienceEveryone", "everyone"),
        ("ACFacebookAudienceFriends", "friends"),
        ("ACFacebookAudienceOnlyMe", "me"),
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
    r.data("NSLocaleIdentifier", leak(string::make("kCFLocaleIdentifierKey")));
    r.data("NSLocaleScriptCode", leak(string::make("kCFLocaleScriptCodeKey")));
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
        // A type of several parts matches whole names ending in it.
        std::fs::write(folder.join("Art.bundle/Deck.uispec.plist"), b"plist").unwrap();
        std::fs::write(folder.join("Art.bundle/Info.plist"), b"plist").unwrap();
        let listed = |kind: &str| -> Vec<String> {
            let paths = runtime::send(bundle, "pathsForResourcesOfType:inDirectory:", &[string::make(kind), 0]);
            crate::fw::collections::array_items(paths).into_iter().map(string::get).collect()
        };
        assert_eq!(listed("uispec.plist"), [format!("{path}/Deck.uispec.plist")]);
        assert_eq!(listed("plist"), [format!("{path}/Deck.uispec.plist"), format!("{path}/Info.plist")]);
        // The same folder gives the same bundle; a missing one or a file gives nil.
        assert_eq!(runtime::send(class, "bundleWithPath:", &[string::make(&format!("{path}/"))]), bundle);
        assert_eq!(runtime::send(class, "bundleWithPath:", &[string::make(&format!("{path}/nothing"))]), 0);
        assert_eq!(runtime::send(class, "bundleWithPath:", &[string::make(&format!("{path}/hero.png"))]), 0);
        let fresh = runtime::send(runtime::alloc_instance(class), "initWithPath:", &[string::make(&format!("{path}/nothing"))]);
        assert_eq!(fresh, 0);
        std::fs::remove_dir_all(&folder).unwrap();
    }

    /// Localized strings come from the bundle's tables: text .strings files
    /// (UTF-16 or UTF-8, with comments and escapes) and binary ones; a
    /// missing key gives the value, else the key itself.
    #[test]
    fn localized_strings_come_from_the_bundles_tables() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let folder = std::env::temp_dir().join(format!("gamebridge-strings-{}", std::process::id()));
        let lproj = folder.join("Words.bundle/en.lproj");
        std::fs::create_dir_all(&lproj).unwrap();
        let text = "/* the room */\n\"Llb_Dormitory\" = \"Dormitory\";\n// a line comment\nHello = \"Hi \\\"you\\\"\\n\\U00e9\";\n";
        let mut utf16 = vec![0xFF, 0xFE];
        utf16.extend(text.encode_utf16().flat_map(|u| u.to_le_bytes()));
        std::fs::write(lproj.join("Localizable.strings"), utf16).unwrap();
        let table = crate::fw::plist::Value::Dict(vec![("Start".into(), crate::fw::plist::Value::String("Begin".into()))]);
        std::fs::write(lproj.join("Menu.strings"), crate::fw::plist::to_binary(&table)).unwrap();
        let path = folder.join("Words.bundle").to_string_lossy().into_owned();
        let bundle = runtime::send(runtime::class_named("NSBundle"), "bundleWithPath:", &[string::make(&path)]);
        let look = |key: &str, value: Id, table: Id| {
            string::get(runtime::send(bundle, "localizedStringForKey:value:table:", &[string::make(key), value, table]))
        };
        assert_eq!(look("Llb_Dormitory", 0, 0), "Dormitory");
        assert_eq!(look("Hello", 0, string::make("")), "Hi \"you\"\n\u{e9}");
        assert_eq!(look("Start", 0, string::make("Menu")), "Begin");
        assert_eq!(look("Start", 0, 0), "Start");
        assert_eq!(look("Missing", string::make("Fallback"), string::make("Menu")), "Fallback");
        assert_eq!(look("Missing", 0, string::make("NoSuchTable")), "Missing");
        std::fs::remove_dir_all(&folder).unwrap();
    }

    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

    fn center() -> Id {
        runtime::send(runtime::class_named("NSNotificationCenter"), "defaultCenter", &[])
    }

    static VICTIM: AtomicUsize = AtomicUsize::new(0);
    static VICTIM_CALLS: AtomicUsize = AtomicUsize::new(0);
    static HEARD_FROM: Mutex<Vec<Id>> = Mutex::new(Vec::new());

    extern "C" fn remove_the_victim(_this: Id, _: Sel, _note: Id) {
        runtime::send(center(), "removeObserver:", &[VICTIM.load(SeqCst)]);
    }
    extern "C" fn victim_heard(_this: Id, _: Sel, _note: Id) {
        VICTIM_CALLS.fetch_add(1, SeqCst);
    }
    extern "C" fn note_sender(_this: Id, _: Sel, note: Id) {
        HEARD_FROM.lock().unwrap().push(runtime::send(note, "object", &[]));
    }

    /// An observer removed while a notification is being posted (by an
    /// earlier observer, say in its dealloc) isn't called, as on Apple.
    #[test]
    fn apple_an_observer_removed_during_a_post_is_not_called() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let class = define_class("TestRemovingListener", "NSObject", 8);
        methods(class, &[("removeVictim:", remove_the_victim as usize), ("victimHeard:", victim_heard as usize)]);
        let (remover, victim) = (runtime::alloc_instance(class), runtime::alloc_instance(class));
        VICTIM.store(victim, SeqCst);
        let name = string::make("TestRemovedDuringPost");
        runtime::send(center(), "addObserver:selector:name:object:", &[remover, runtime::sel("removeVictim:") as usize, name, 0]);
        runtime::send(center(), "addObserver:selector:name:object:", &[victim, runtime::sel("victimHeard:") as usize, name, 0]);
        runtime::send(center(), "postNotificationName:object:", &[name, 0]);
        assert_eq!(VICTIM_CALLS.load(SeqCst), 0);
    }

    /// -removeObserver:name:object: removes only the entries for that sender.
    #[test]
    fn apple_removing_an_observer_for_one_sender_keeps_the_others() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let class = define_class("TestSenderListener", "NSObject", 8);
        methods(class, &[("heard:", note_sender as usize)]);
        let listener = runtime::alloc_instance(class);
        let (one, two) = (runtime::alloc_instance(class), runtime::alloc_instance(class));
        let name = string::make("TestPerSender");
        for sender in [one, two] {
            runtime::send(center(), "addObserver:selector:name:object:", &[listener, runtime::sel("heard:") as usize, name, sender]);
        }
        runtime::send(center(), "removeObserver:name:object:", &[listener, name, one]);
        runtime::send(center(), "postNotificationName:object:", &[name, one]);
        runtime::send(center(), "postNotificationName:object:", &[name, two]);
        assert_eq!(*HEARD_FROM.lock().unwrap(), [two]);
    }

    /// The defaults tests share one settings file; they take turns.
    static DEFAULTS_TESTS: Mutex<()> = Mutex::new(());

    fn defaults_turn() -> std::sync::MutexGuard<'static, ()> {
        DEFAULTS_TESTS.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn test_defaults() -> Id {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            crate::run::setup();
            let path = std::env::temp_dir().join(format!("gamebridge-defaults-{}.plist", std::process::id()));
            let _ = std::fs::remove_file(&path);
            *TEST_DEFAULTS_PATH.lock().unwrap() = Some(path.to_string_lossy().into_owned());
        });
        runtime::send(runtime::class_named("NSUserDefaults"), "standardUserDefaults", &[])
    }

    /// NSUserDefaults keeps an immutable copy of what is set, and refuses
    /// what isn't a property list.
    #[test]
    fn apple_defaults_keep_a_copy() {
        let _turn = defaults_turn();
        let defaults = test_defaults();
        let list = runtime::send(collections::array_from(&[string::make("a")]), "mutableCopy", &[]);
        runtime::send(defaults, "setObject:forKey:", &[list, string::make("TestCopiedList")]);
        runtime::send(list, "addObject:", &[string::make("b")]);
        let saved = runtime::send(defaults, "objectForKey:", &[string::make("TestCopiedList")]);
        assert_eq!(collections::array_items(saved).len(), 1, "the later change isn't in the defaults");
        assert_eq!(runtime::send(saved, "isKindOfClass:", &[runtime::class_named("NSMutableArray")]) & 0xFF, 0);
        let odd = runtime::alloc_instance(runtime::class_named("NSObject"));
        runtime::send(defaults, "setObject:forKey:", &[odd, string::make("TestNotAList")]);
        assert_eq!(runtime::send(defaults, "objectForKey:", &[string::make("TestNotAList")]), 0);
    }

    /// The defaults are saved when the app goes to the background, without
    /// the game calling -synchronize (CFPreferences saves on its own).
    #[test]
    fn apple_defaults_save_on_entering_the_background() {
        let _turn = defaults_turn();
        let defaults = test_defaults();
        runtime::send(defaults, "setObject:forKey:", &[string::make("kept"), string::make("TestSavedLater")]);
        post("UIApplicationDidEnterBackgroundNotification", 0, 0);
        let path = TEST_DEFAULTS_PATH.lock().unwrap().clone().unwrap();
        let saved = std::fs::read(&path).ok().and_then(|bytes| plist::parse(&bytes));
        assert!(matches!(saved.as_ref().and_then(|v| v.get("TestSavedLater")), Some(plist::Value::String(s)) if s == "kept"), "{saved:?}");
        // Posted through NSNotificationCenter (by UIKit or the game) alike.
        runtime::send(defaults, "setObject:forKey:", &[string::make("posted"), string::make("TestSavedLater")]);
        let center = runtime::send(runtime::class_named("NSNotificationCenter"), "defaultCenter", &[]);
        runtime::send(center, "postNotificationName:object:", &[string::make("UIApplicationWillTerminateNotification"), 0]);
        let saved = std::fs::read(&path).ok().and_then(|bytes| plist::parse(&bytes));
        assert!(matches!(saved.as_ref().and_then(|v| v.get("TestSavedLater")), Some(plist::Value::String(s)) if s == "posted"), "{saved:?}");
    }

    /// A save that fails keeps the change, so the next save writes it.
    #[test]
    fn apple_defaults_keep_a_change_whose_save_failed() {
        let _turn = defaults_turn();
        let defaults = test_defaults();
        let path = TEST_DEFAULTS_PATH.lock().unwrap().clone().unwrap();
        *TEST_DEFAULTS_PATH.lock().unwrap() = Some("/nonexistent-gamebridge-folder/settings.plist".into());
        runtime::send(defaults, "setObject:forKey:", &[string::make("late"), string::make("TestSavedAfterFailure")]);
        let saved = runtime::send(defaults, "synchronize", &[]) & 0xFF;
        *TEST_DEFAULTS_PATH.lock().unwrap() = Some(path.clone());
        assert_eq!(saved, 0, "the first save can't be written");
        save_changed_defaults();
        let file = std::fs::read(&path).ok().and_then(|bytes| plist::parse(&bytes));
        assert!(matches!(file.as_ref().and_then(|v| v.get("TestSavedAfterFailure")), Some(plist::Value::String(s)) if s == "late"), "{file:?}");
    }

    /// Two threads changing and saving the defaults at once leave a whole,
    /// readable settings file (saves used to share one temporary file).
    #[test]
    fn apple_defaults_saved_from_two_threads_stay_whole() {
        let _turn = defaults_turn();
        let defaults = test_defaults();
        let workers: Vec<_> = (0..2)
            .map(|worker| {
                std::thread::spawn(move || {
                    for round in 0..60 {
                        let key = string::make(&format!("TestThread{worker}"));
                        runtime::send(defaults, "setInteger:forKey:", &[round, key]);
                        runtime::send(defaults, "synchronize", &[]);
                    }
                })
            })
            .collect();
        workers.into_iter().for_each(|w| w.join().unwrap());
        let path = TEST_DEFAULTS_PATH.lock().unwrap().clone().unwrap();
        let file = std::fs::read(&path).ok().and_then(|bytes| plist::parse(&bytes));
        for worker in 0..2 {
            assert!(matches!(file.as_ref().and_then(|v| v.get(&format!("TestThread{worker}"))), Some(plist::Value::Integer(59))), "{file:?}");
        }
    }

    #[repr(C)]
    struct HeardBlock {
        header: crate::libsystem::mach::BlockHeader,
        heard: Mutex<Vec<std::thread::ThreadId>>,
    }

    extern "C" fn heard_on(block: usize, _note: Id) {
        unsafe { &*(block as *const HeardBlock) }.heard.lock().unwrap().push(std::thread::current().id());
    }

    fn heard_block() -> &'static HeardBlock {
        let header = crate::libsystem::mach::BlockHeader { isa: 0, flags: 1 << 28, reserved: 0, invoke: heard_on as usize, descriptor: std::ptr::null() };
        Box::leak(Box::new(HeardBlock { header, heard: Mutex::new(Vec::new()) }))
    }

    /// A block observer with a queue runs on that queue and the post waits
    /// until it has run there (Apple's -[__NSObserver _doit:] adds a block
    /// operation and waits for it, even when the queue is suspended).
    #[test]
    fn apple_queued_block_observers_run_on_their_queue_before_the_post_returns() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let queues = runtime::class_named("NSOperationQueue");
        let new_queue = || runtime::send(runtime::send(queues, "alloc", &[]), "init", &[]);
        let observe = |name: Id, queue: Id, block: &'static HeardBlock| {
            runtime::send(center(), "addObserverForName:object:queue:usingBlock:", &[name, 0, queue, block as *const HeardBlock as usize]);
        };
        let name = string::make("TestQueuedNote");
        let block = heard_block();
        observe(name, new_queue(), block);
        runtime::send(center(), "postNotificationName:object:", &[name, 0]);
        let heard = block.heard.lock().unwrap().clone();
        assert_eq!(heard.len(), 1, "the post waited for the block");
        assert_ne!(heard[0], std::thread::current().id(), "it ran on its queue");

        // On the main queue, on the main thread (here every test thread is),
        // the block runs at once: Apple's fast path, and no deadlock.
        let name = string::make("TestMainQueueNote");
        let block = heard_block();
        observe(name, runtime::send(queues, "mainQueue", &[]), block);
        runtime::send(center(), "postNotificationName:object:", &[name, 0]);
        assert_eq!(block.heard.lock().unwrap().len(), 1, "the main queue's own thread runs it without waiting");

        // A suspended queue can't run the operation: Apple's post waits
        // until it is resumed (ours did call the block on the poster).
        let stuck = new_queue();
        runtime::send(stuck, "setSuspended:", &[1]);
        let name = string::make("TestSuspendedQueueNote");
        let block = heard_block();
        observe(name, stuck, block);
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            runtime::send(center(), "postNotificationName:object:", &[name, 0]);
            let _ = done.send(());
        });
        assert!(finished.recv_timeout(std::time::Duration::from_millis(200)).is_err(), "the post returned before its queue could run the block");
        assert!(block.heard.lock().unwrap().is_empty(), "the block ran on the poster instead");
        runtime::send(stuck, "setSuspended:", &[0]);
        assert!(finished.recv_timeout(std::time::Duration::from_millis(500)).is_ok(), "resuming the queue lets the post finish");
        assert_eq!(block.heard.lock().unwrap().len(), 1, "the block ran once, on the queue");
    }

    #[repr(C)]
    struct LockBlock {
        header: crate::libsystem::mach::BlockHeader,
        got: std::sync::atomic::AtomicBool,
    }

    /// A queued block that needs the guest lock, as a 32-bit game's block
    /// does when its observer runs guest code.
    extern "C" fn needs_guest_lock(block: usize) {
        let block = unsafe { &*(block as *const LockBlock) };
        let got = arm32_bridge::runtime::try_hold_gil(std::time::Duration::from_millis(500));
        block.got.store(got.is_some(), std::sync::atomic::Ordering::SeqCst);
    }

    /// The post must give up the guest lock while it waits: with one thread
    /// running game code at a time (ARM32_PARALLEL=0) the observer's block
    /// otherwise never gets it and the post waits for ever (fw/operations.rs).
    #[test]
    #[ignore = "GIL mode: test-pc.sh runs it with ARM32_PARALLEL=0"]
    fn a_queued_block_gets_the_guest_lock_while_the_post_waits() {
        assert!(!arm32_bridge::runtime::parallel(), "run with ARM32_PARALLEL=0 (test-pc.sh does)");
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            crate::run::setup();
            // The guest lock's waiter bookkeeping (shared state) needs it.
            arm32_bridge::runtime::init(false);
        });
        let block = Box::leak(Box::new(LockBlock {
            header: crate::libsystem::mach::BlockHeader { isa: 0, flags: 1 << 28, reserved: 0, invoke: needs_guest_lock as usize, descriptor: std::ptr::null() },
            got: std::sync::atomic::AtomicBool::new(false),
        }));
        let queue = runtime::send(runtime::send(runtime::class_named("NSOperationQueue"), "alloc", &[]), "init", &[]);
        let name = string::make("TestGuestLockNote");
        runtime::send(center(), "addObserverForName:object:queue:usingBlock:", &[name, 0, queue, block as *const LockBlock as usize]);
        let _gil = arm32_bridge::runtime::hold_gil();
        runtime::send(center(), "postNotificationName:object:", &[name, 0]);
        assert!(block.got.load(std::sync::atomic::Ordering::SeqCst), "the observer's block never got the guest lock");
    }

    /// pathForResource: prefers the device's own variant (name~iphone.png)
    /// and looks in language folders inside the given folder.
    #[test]
    fn apple_resources_prefer_device_variants_and_folder_languages() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let folder = std::env::temp_dir().join(format!("gamebridge-variants-{}", std::process::id()));
        let root = folder.join("Game.bundle");
        std::fs::create_dir_all(root.join("Art/en.lproj")).unwrap();
        let device = if crate::libsystem::io::device_model().starts_with("iPad") { "ipad" } else { "iphone" };
        for name in ["hero.png".to_string(), format!("hero~{device}.png"), "Art/en.lproj/words.plist".to_string()] {
            std::fs::write(root.join(name), b"x").unwrap();
        }
        let path = root.to_string_lossy().into_owned();
        let bundle = runtime::send(runtime::class_named("NSBundle"), "bundleWithPath:", &[string::make(&path)]);
        let found = runtime::send(bundle, "pathForResource:ofType:", &[string::make("hero"), string::make("png")]);
        assert_eq!(string::get(found), format!("{path}/hero~{device}.png"));
        let found = runtime::send(bundle, "pathForResource:ofType:inDirectory:", &[string::make("words"), string::make("plist"), string::make("Art")]);
        assert_eq!(string::get(found), format!("{path}/Art/en.lproj/words.plist"));
        std::fs::remove_dir_all(&folder).unwrap();
    }

    /// Foundation methods games call that were missing.
    #[test]
    fn apple_small_foundation_methods_answer() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let info = runtime::send(runtime::class_named("NSProcessInfo"), "processInfo", &[]);
        let date = crate::fw::misc::date(100.0);
        let zone = runtime::alloc_instance(runtime::class_named("NSTimeZone"));
        let data = runtime::send(runtime::class_named("NSMutableData"), "data", &[]);
        let object = runtime::alloc_instance(runtime::class_named("NSObject"));
        let missing: Vec<&str> = [
            (info, "operatingSystem"),
            (runtime::class_named("NSObject"), "version"),
            (object, "classForCoder"),
            (object, "classForKeyedArchiver"),
            (date, "addTimeInterval:"),
            (zone, "initWithName:"),
            (data, "setData:"),
        ]
        .into_iter()
        .filter(|&(receiver, name)| !runtime::responds(receiver, name))
        .map(|(_, name)| name)
        .collect();
        assert!(missing.is_empty(), "missing: {missing:?}");
        assert_eq!(runtime::send(info, "operatingSystem", &[]), 5, "NSMACHOperatingSystem");
        assert_eq!(runtime::send(runtime::class_named("NSObject"), "version", &[]), 0);
        assert_eq!(runtime::send(object, "classForCoder", &[]), runtime::class_named("NSObject"));
        assert_eq!(runtime::send(object, "classForKeyedArchiver", &[]), runtime::class_named("NSObject"));
        let add: extern "C" fn(Id, Sel, f64) -> Id = unsafe { std::mem::transmute(runtime::imp_for(date, runtime::sel("addTimeInterval:")).expect("addTimeInterval:")) };
        assert_eq!(crate::fw::misc::date_value(add(date, 0, 5.0)), 105.0);
        let zone = runtime::send(zone, "initWithName:", &[string::make("UTC")]);
        assert_eq!(runtime::send(zone, "secondsFromGMT", &[]), 0);
        runtime::send(data, "setData:", &[crate::fw::data::make(b"abc")]);
        assert_eq!(crate::fw::data::bytes(data), b"abc");
        assert!(runtime::responds(runtime::alloc_instance(runtime::class_named("NSURLConnection")), "currentRequest"));
    }

    /// Key-value coding on NSUserDefaults reads the defaults themselves.
    #[test]
    fn defaults_answer_value_for_key() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let defaults = runtime::send(runtime::class_named("NSUserDefaults"), "standardUserDefaults", &[]);
        assert!(runtime::responds(defaults, "valueForKey:"), "-[NSUserDefaults valueForKey:] is missing");
        assert!(runtime::responds(defaults, "setValue:forKey:"));
    }
}
