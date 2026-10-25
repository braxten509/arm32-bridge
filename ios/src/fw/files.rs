//! NSFileManager and NSLocale.

use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::{collections, string};
use crate::objc::runtime::{self, Class, Id, Sel};
use crate::symbols::Registry;
use std::sync::OnceLock;

static MANAGER: OnceLock<Id> = OnceLock::new();

extern "C" fn default_manager(_: Class, _: Sel) -> Id {
    *MANAGER.get_or_init(|| runtime::alloc_instance(runtime::class_named("NSFileManager")))
}

fn path_of(object: Id) -> String {
    string::get(object)
}

fn set_error(error: *mut Id, message: String) {
    crate::log!("file manager: {message}");
    if !error.is_null() {
        unsafe { *error = crate::fw::misc::error("NSCocoaErrorDomain", 4, &message) };
    }
}

extern "C" fn file_exists(_this: Id, _: Sel, path: Id) -> bool {
    path != 0 && std::path::Path::new(&path_of(path)).exists()
}

extern "C" fn file_exists_is_dir(_this: Id, _: Sel, path: Id, is_dir: *mut u8) -> bool {
    if path == 0 {
        return false;
    }
    match std::fs::metadata(path_of(path)) {
        Ok(meta) => {
            if !is_dir.is_null() {
                unsafe { *is_dir = meta.is_dir() as u8 };
            }
            true
        }
        Err(_) => false,
    }
}

extern "C" fn create_directory(_this: Id, _: Sel, path: Id, intermediate: u8, _attributes: Id, error: *mut Id) -> bool {
    let path = path_of(path);
    let result = if intermediate != 0 { std::fs::create_dir_all(&path) } else { std::fs::create_dir(&path) };
    match result {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && intermediate != 0 => true,
        Err(e) => {
            set_error(error, format!("can't make folder {path}: {e}"));
            false
        }
    }
}

extern "C" fn remove_item(_this: Id, _: Sel, path: Id, error: *mut Id) -> bool {
    let path = path_of(path);
    let result = match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(&path),
        Ok(_) => std::fs::remove_file(&path),
        Err(e) => Err(e),
    };
    match result {
        Ok(()) => true,
        Err(e) => {
            set_error(error, format!("can't remove {path}: {e}"));
            false
        }
    }
}

extern "C" fn move_item(_this: Id, _: Sel, from: Id, to: Id, error: *mut Id) -> bool {
    match std::fs::rename(path_of(from), path_of(to)) {
        Ok(()) => true,
        Err(e) => {
            set_error(error, format!("can't move {}: {e}", path_of(from)));
            false
        }
    }
}

extern "C" fn copy_item(_this: Id, _: Sel, from: Id, to: Id, error: *mut Id) -> bool {
    match std::fs::copy(path_of(from), path_of(to)) {
        Ok(_) => true,
        Err(e) => {
            set_error(error, format!("can't copy {}: {e}", path_of(from)));
            false
        }
    }
}

extern "C" fn contents_of_directory(_this: Id, _: Sel, path: Id, error: *mut Id) -> Id {
    match std::fs::read_dir(path_of(path)) {
        Ok(entries) => {
            let mut names: Vec<String> = entries.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
            names.sort();
            let list: Vec<Id> = names.iter().map(|n| string::make(n)).collect();
            collections::array_from(&list)
        }
        Err(e) => {
            set_error(error, format!("can't list {}: {e}", path_of(path)));
            0
        }
    }
}

extern "C" fn attributes_of_item(_this: Id, _: Sel, path: Id, error: *mut Id) -> Id {
    match std::fs::metadata(path_of(path)) {
        Ok(meta) => {
            use std::os::unix::fs::PermissionsExt;
            let kind = if meta.is_dir() { "NSFileTypeDirectory" } else { "NSFileTypeRegular" };
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0);
            let date = runtime::send(runtime::class_named("NSDate"), "alloc", &[]);
            unsafe { *((date + 8) as *mut f64) = modified - 978_307_200.0 };
            collections::dictionary_from(&[
                (string::make("NSFileSize"), collections::number_int(meta.len() as i64)),
                (string::make("NSFileType"), string::make(kind)),
                (string::make("NSFileModificationDate"), date),
                (string::make("NSFilePosixPermissions"), collections::number_int((meta.permissions().mode() & 0o7777) as i64)),
            ])
        }
        Err(e) => {
            set_error(error, format!("can't read {}: {e}", path_of(path)));
            0
        }
    }
}

extern "C" fn attributes_of_file_system(_this: Id, _: Sel, path: Id, _error: *mut Id) -> Id {
    let path = std::ffi::CString::new(path_of(path)).unwrap_or_default();
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    let (free, size) = if unsafe { libc::statvfs(path.as_ptr(), &mut stats) } == 0 {
        (stats.f_bavail as i64 * stats.f_frsize as i64, stats.f_blocks as i64 * stats.f_frsize as i64)
    } else {
        (8 << 30, 16 << 30)
    };
    collections::dictionary_from(&[
        (string::make("NSFileSystemFreeSize"), collections::number_int(free)),
        (string::make("NSFileSystemSize"), collections::number_int(size)),
    ])
}

extern "C" fn create_file(_this: Id, _: Sel, path: Id, contents: Id, _attributes: Id) -> bool {
    std::fs::write(path_of(path), crate::fw::data::bytes(contents)).is_ok()
}

extern "C" fn contents_at_path(_this: Id, _: Sel, path: Id) -> Id {
    match std::fs::read(path_of(path)) {
        Ok(bytes) => crate::fw::counted::autorelease(crate::fw::data::make(&bytes)),
        Err(_) => 0,
    }
}

extern "C" fn is_readable(_this: Id, _: Sel, path: Id) -> bool {
    let path = std::ffi::CString::new(path_of(path)).unwrap_or_default();
    unsafe { libc::access(path.as_ptr(), libc::R_OK) == 0 }
}

extern "C" fn is_writable(_this: Id, _: Sel, path: Id) -> bool {
    let path = std::ffi::CString::new(path_of(path)).unwrap_or_default();
    unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 }
}

extern "C" fn current_directory(_this: Id, _: Sel) -> Id {
    string::make(&std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default())
}

extern "C" fn ubiquity_container(_this: Id, _: Sel, _identifier: Id) -> Id {
    0
}

extern "C" fn enumerator_at_path(_this: Id, _: Sel, path: Id) -> Id {
    fn walk(base: &std::path::Path, here: &std::path::Path, out: &mut Vec<String>) {
        if let Ok(entries) = std::fs::read_dir(here) {
            let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                let full = entry.path();
                if let Ok(relative) = full.strip_prefix(base) {
                    out.push(relative.to_string_lossy().into_owned());
                }
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    walk(base, &full, out);
                }
            }
        }
    }
    let base = std::path::PathBuf::from(path_of(path));
    let mut names = Vec::new();
    walk(&base, &base, &mut names);
    collections::enumerator(names.iter().map(|n| string::make(n)).collect())
}

// ----- NSLocale -----

static LOCALE: OnceLock<Id> = OnceLock::new();

extern "C" fn current_locale(_: Class, _: Sel) -> Id {
    *LOCALE.get_or_init(|| runtime::alloc_instance(runtime::class_named("NSLocale")))
}

extern "C" fn preferred_languages(_: Class, _: Sel) -> Id {
    collections::array_from(&[string::make("en")])
}

/// The phone's locale is en_US; one made with an identifier keeps it.
fn identifier_of(locale: Id) -> String {
    match crate::fw::uikit::property_get(locale, "localeIdentifier") {
        Some(text) => string::get(text as Id),
        None => "en_US".into(),
    }
}

extern "C" fn locale_identifier(this: Id, _: Sel) -> Id {
    string::make(&identifier_of(this))
}

/// "de_DE", "en-GB", "pt_BR@currency=EUR": language, then the country
/// (a script such as zh_Hans_CN sits between them).
fn locale_parts(identifier: &str) -> (String, Option<String>, Option<String>) {
    let base = identifier.split('@').next().unwrap_or("");
    let mut parts = base.split(['_', '-']).filter(|p| !p.is_empty());
    let language = parts.next().unwrap_or("").to_lowercase();
    let mut country = None;
    let mut script = None;
    for part in parts {
        if part.len() == 4 && part.chars().all(|c| c.is_ascii_alphabetic()) && script.is_none() && country.is_none() {
            script = Some(part.to_string());
        } else if country.is_none() {
            country = Some(part.to_uppercase());
        }
    }
    (language, script, country)
}

/// The usual currency of a country (enough for prices; others USD).
fn currency_of(country: Option<&str>) -> (&'static str, &'static str) {
    match country.unwrap_or("US") {
        "GB" => ("GBP", "£"),
        "JP" => ("JPY", "¥"),
        "CN" => ("CNY", "¥"),
        "KR" => ("KRW", "₩"),
        "IN" => ("INR", "₹"),
        "RU" => ("RUB", "₽"),
        "BR" => ("BRL", "R$"),
        "CA" => ("CAD", "$"),
        "AU" => ("AUD", "$"),
        "MX" => ("MXN", "$"),
        "CH" => ("CHF", "CHF"),
        "SE" => ("SEK", "kr"),
        "NO" => ("NOK", "kr"),
        "DK" => ("DKK", "kr."),
        "DE" | "FR" | "ES" | "IT" | "NL" | "BE" | "AT" | "PT" | "FI" | "IE" | "GR" | "SK" | "SI" | "EE" | "LV" | "LT" | "LU" | "MT" | "CY" => ("EUR", "€"),
        _ => ("USD", "$"),
    }
}

extern "C" fn locale_object_for_key(this: Id, _: Sel, key: Id) -> Id {
    let key = string::get(key);
    let identifier = identifier_of(this);
    let (language, script, country) = locale_parts(&identifier);
    let value = match key.as_str() {
        "kCFLocaleCountryCodeKey" => match country {
            Some(country) => country,
            None => return 0,
        },
        "kCFLocaleLanguageCodeKey" => language,
        "kCFLocaleScriptCodeKey" => match script {
            Some(script) => script,
            None => return 0,
        },
        "currency" | "kCFLocaleCurrencyCodeKey" => currency_of(country.as_deref()).0.to_string(),
        "kCFLocaleCurrencySymbolKey" => currency_of(country.as_deref()).1.to_string(),
        "kCFLocaleIdentifierKey" => identifier,
        "kCFLocaleUsesMetricSystemKey" => return collections::number_bool(!matches!(country.as_deref(), Some("US" | "LR" | "MM"))),
        "kCFLocaleDecimalSeparatorKey" => if matches!(language.as_str(), "de" | "fr" | "es" | "it" | "pt" | "ru" | "nl" | "sv" | "da" | "fi" | "nb" | "pl" | "tr") { ",".into() } else { ".".into() },
        "kCFLocaleVariantCodeKey" => return 0,
        _ => {
            crate::log!("NSLocale objectForKey {key}: not known");
            return 0;
        }
    };
    string::make(&value)
}

extern "C" fn locale_init_with_identifier(this: Id, _: Sel, identifier: Id) -> Id {
    let identifier = if identifier == 0 { string::make("") } else { runtime::send(identifier, "copy", &[]) };
    crate::fw::uikit::property_set_object(this, "localeIdentifier", identifier);
    this
}

extern "C" fn locale_with_identifier(class: Class, s: Sel, identifier: Id) -> Id {
    locale_init_with_identifier(runtime::alloc_instance(class), s, identifier)
}

/// A path's bytes (UTF-8 here, as on iPhone) to a string.
extern "C" fn string_with_file_system_representation(_this: Id, _: Sel, bytes: *const u8, length: usize) -> Id {
    if bytes.is_null() {
        return 0;
    }
    let raw = unsafe { std::slice::from_raw_parts(bytes, length) };
    string::make(&String::from_utf8_lossy(raw))
}

extern "C" fn file_system_representation_with_path(_this: Id, _: Sel, path: Id) -> usize {
    runtime::send(path, "fileSystemRepresentation", &[])
}

pub fn register(r: &mut Registry) {
    // The attribute key for a file's modification date.
    r.data("NSFileModificationDate", crate::symbols::leak(string::make("NSFileModificationDate")));
    let manager = define_class("NSFileManager", "NSObject", 8);
    class_methods(manager, &[("defaultManager", default_manager as usize)]);
    methods(
        manager,
        &[
            ("fileExistsAtPath:", file_exists as usize),
            ("fileExistsAtPath:isDirectory:", file_exists_is_dir as usize),
            ("createDirectoryAtPath:withIntermediateDirectories:attributes:error:", create_directory as usize),
            ("removeItemAtPath:error:", remove_item as usize),
            ("moveItemAtPath:toPath:error:", move_item as usize),
            ("copyItemAtPath:toPath:error:", copy_item as usize),
            ("contentsOfDirectoryAtPath:error:", contents_of_directory as usize),
            ("attributesOfItemAtPath:error:", attributes_of_item as usize),
            ("attributesOfFileSystemForPath:error:", attributes_of_file_system as usize),
            ("createFileAtPath:contents:attributes:", create_file as usize),
            ("contentsAtPath:", contents_at_path as usize),
            ("isReadableFileAtPath:", is_readable as usize),
            ("isWritableFileAtPath:", is_writable as usize),
            ("isDeletableFileAtPath:", is_writable as usize),
            ("currentDirectoryPath", current_directory as usize),
            ("URLForUbiquityContainerIdentifier:", ubiquity_container as usize),
            ("enumeratorAtPath:", enumerator_at_path as usize),
            ("stringWithFileSystemRepresentation:length:", string_with_file_system_representation as usize),
            ("fileSystemRepresentationWithPath:", file_system_representation_with_path as usize),
        ],
    );
    let locale = define_class("NSLocale", "NSObject", 8);
    class_methods(
        locale,
        &[
            ("currentLocale", current_locale as usize),
            ("autoupdatingCurrentLocale", current_locale as usize),
            ("systemLocale", current_locale as usize),
            ("preferredLanguages", preferred_languages as usize),
            ("localeWithLocaleIdentifier:", locale_with_identifier as usize),
        ],
    );
    methods(
        locale,
        &[
            ("localeIdentifier", locale_identifier as usize),
            ("objectForKey:", locale_object_for_key as usize),
            ("initWithLocaleIdentifier:", locale_init_with_identifier as usize),
        ],
    );
}
