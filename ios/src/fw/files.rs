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

// ----- NSFileHandle: { isa, *mut Handle } -----

/// An open file (None once -closeFile ran, as Apple's descriptor is -1 then).
struct Handle {
    file: Option<std::fs::File>,
    /// Whether -dealloc closes it: the +fileHandleFor... handles own their
    /// descriptor (Apple 0x89749); -initWithFileDescriptor: does not.
    close_on_dealloc: bool,
}

fn handle(object: Id) -> &'static mut Handle {
    unsafe {
        let slot = (object + 8) as *mut *mut Handle;
        if (*slot).is_null() {
            *slot = Box::into_raw(Box::new(Handle { file: None, close_on_dealloc: false }));
        }
        &mut **slot
    }
}

/// The handle's open file; a closed one raises NSFileHandleOperationException,
/// as Apple's operations do.
fn open_file(this: Id) -> Option<&'static mut std::fs::File> {
    let handle = handle(this);
    if handle.file.is_none() {
        crate::fw::collections::raise_named("NSFileHandleOperationException", "*** -[NSFileHandle]: the file handle is closed".into());
    }
    handle.file.as_mut()
}

fn operation_failed(what: &str, error: std::io::Error) {
    crate::fw::collections::raise_named("NSFileHandleOperationException", format!("*** -[NSFileHandle {what}]: {error}"));
}

fn with_file(path: Id, options: std::fs::OpenOptions) -> Id {
    let object = runtime::alloc_instance(runtime::class_named("NSFileHandle"));
    match options.open(path_of(path)) {
        Ok(file) => {
            let handle = handle(object);
            handle.file = Some(file);
            handle.close_on_dealloc = true;
        }
        Err(_) => return 0,
    }
    object
}

extern "C" fn file_handle_for_reading(_: Class, _: Sel, path: Id) -> Id {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    with_file(path, options)
}

extern "C" fn file_handle_for_writing(_: Class, _: Sel, path: Id) -> Id {
    let mut options = std::fs::OpenOptions::new();
    options.write(true);
    with_file(path, options)
}

extern "C" fn file_handle_for_updating(_: Class, _: Sel, path: Id) -> Id {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true);
    with_file(path, options)
}

/// The one handle for a standard stream (Apple's is a handle on the stream,
/// not a new duplicate of it each time a game asks).
static STANDARD_INPUT: OnceLock<Id> = OnceLock::new();
static STANDARD_OUTPUT: OnceLock<Id> = OnceLock::new();
static STANDARD_ERROR: OnceLock<Id> = OnceLock::new();

fn standard_handle(fd: i32, slot: &'static OnceLock<Id>) -> Id {
    *slot.get_or_init(|| {
        let object = runtime::alloc_instance(runtime::class_named("NSFileHandle"));
        let copy = unsafe { libc::dup(fd) };
        if copy >= 0 {
            use std::os::unix::io::FromRawFd;
            handle(object).file = Some(unsafe { std::fs::File::from_raw_fd(copy) });
        }
        object
    })
}

extern "C" fn file_handle_standard_input(_: Class, _: Sel) -> Id {
    standard_handle(0, &STANDARD_INPUT)
}

extern "C" fn file_handle_standard_output(_: Class, _: Sel) -> Id {
    standard_handle(1, &STANDARD_OUTPUT)
}

extern "C" fn file_handle_standard_error(_: Class, _: Sel) -> Id {
    standard_handle(2, &STANDARD_ERROR)
}

extern "C" fn file_handle_null_device(_: Class, _: Sel) -> Id {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true);
    with_file(string::make("/dev/null"), options)
}

extern "C" fn file_handle_init(this: Id, _: Sel) -> Id {
    handle(this);
    this
}

extern "C" fn file_handle_init_descriptor(this: Id, _: Sel, descriptor: i32) -> Id {
    file_handle_init_descriptor_close(this, 0, descriptor, 0)
}

/// -initWithFileDescriptor:closeOnDealloc: as Apple's: the handle uses the
/// descriptor, and -dealloc closes it only when told to.
extern "C" fn file_handle_init_descriptor_close(this: Id, _: Sel, descriptor: i32, close: u8) -> Id {
    use std::os::unix::io::FromRawFd;
    let handle = handle(this);
    if descriptor >= 0 {
        handle.file = Some(unsafe { std::fs::File::from_raw_fd(descriptor) });
    }
    handle.close_on_dealloc = close != 0;
    this
}

/// -dealloc closes a handle that owns its descriptor, as Apple's does (0x89749).
extern "C" fn file_handle_dealloc(this: Id, _: Sel) {
    let handle = handle(this);
    if handle.close_on_dealloc {
        handle.file = None;
    }
}

extern "C" fn file_handle_read_length(this: Id, _: Sel, length: usize) -> Id {
    use std::io::Read;
    let Some(file) = open_file(this) else { return 0 };
    let mut bytes = Vec::new();
    match file.take(length as u64).read_to_end(&mut bytes) {
        Ok(_) => crate::fw::counted::autorelease(crate::fw::data::make(&bytes)),
        Err(error) => {
            operation_failed("readDataOfLength:", error);
            0
        }
    }
}

extern "C" fn file_handle_read_to_end(this: Id, _: Sel) -> Id {
    use std::io::Read;
    let Some(file) = open_file(this) else { return 0 };
    let mut bytes = Vec::new();
    match file.read_to_end(&mut bytes) {
        Ok(_) => crate::fw::counted::autorelease(crate::fw::data::make(&bytes)),
        Err(error) => {
            operation_failed("readDataToEndOfFile", error);
            0
        }
    }
}

/// The rest of a regular file, or one read of what a pipe or socket holds now
/// (Apple's -availableData: waits for neither the writer nor 4 KiB).
extern "C" fn file_handle_available(this: Id, _: Sel) -> Id {
    use std::io::Read;
    use std::os::unix::io::AsRawFd;
    let Some(file) = open_file(this) else { return 0 };
    let mut bytes = Vec::new();
    if is_regular_file(file.as_raw_fd()) {
        if let Err(error) = file.read_to_end(&mut bytes) {
            operation_failed("availableData", error);
            return 0;
        }
    } else {
        let mut buffer = [0u8; 4096];
        match file.read(&mut buffer) {
            Ok(read) => bytes.extend_from_slice(&buffer[..read]),
            Err(error) => {
                operation_failed("availableData", error);
                return 0;
            }
        }
    }
    crate::fw::counted::autorelease(crate::fw::data::make(&bytes))
}

fn is_regular_file(descriptor: i32) -> bool {
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    unsafe { libc::fstat(descriptor, &mut stat) == 0 && (stat.st_mode & libc::S_IFMT) == libc::S_IFREG }
}

extern "C" fn file_handle_write(this: Id, _: Sel, data: Id) -> Id {
    use std::io::Write;
    let Some(file) = open_file(this) else { return 0 };
    if data == 0 {
        return 0;
    }
    if let Err(error) = file.write_all(&crate::fw::data::bytes(data)) {
        operation_failed("writeData:", error);
    }
    this
}

extern "C" fn file_handle_offset(this: Id, _: Sel) -> u64 {
    use std::io::Seek;
    let Some(file) = open_file(this) else { return 0 };
    match file.stream_position() {
        Ok(position) => position,
        Err(error) => {
            operation_failed("offsetInFile", error);
            0
        }
    }
}

extern "C" fn file_handle_seek(this: Id, _: Sel, offset: u64) {
    use std::io::Seek;
    let Some(file) = open_file(this) else { return };
    if let Err(error) = file.seek(std::io::SeekFrom::Start(offset)) {
        operation_failed("seekToFileOffset:", error);
    }
}

extern "C" fn file_handle_seek_end(this: Id, _: Sel) -> u64 {
    use std::io::Seek;
    let Some(file) = open_file(this) else { return 0 };
    match file.seek(std::io::SeekFrom::End(0)) {
        Ok(position) => position,
        Err(error) => {
            operation_failed("seekToEndOfFile", error);
            0
        }
    }
}

/// -truncateFileAtOffset: sets the file's length and leaves the offset there
/// (Apple 0xa4842).
extern "C" fn file_handle_truncate(this: Id, _: Sel, offset: u64) {
    use std::io::Seek;
    let Some(file) = open_file(this) else { return };
    if let Err(error) = file.set_len(offset) {
        operation_failed("truncateFileAtOffset:", error);
    } else if let Err(error) = file.seek(std::io::SeekFrom::Start(offset)) {
        operation_failed("truncateFileAtOffset:", error);
    }
}

extern "C" fn file_handle_synchronize(this: Id, _: Sel) {
    let Some(file) = open_file(this) else { return };
    if let Err(error) = file.sync_all() {
        operation_failed("synchronizeFile", error);
    }
}

extern "C" fn file_handle_descriptor(this: Id, _: Sel) -> isize {
    use std::os::unix::io::AsRawFd;
    handle(this).file.as_ref().map_or(-1, |file| file.as_raw_fd() as isize)
}

extern "C" fn file_handle_close(this: Id, _: Sel) {
    // Dropping the File closes its descriptor, as -closeFile does.
    handle(this).file = None;
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

/// (code, English name, its own name): the languages games offer. The
/// iPhone's full lists are Apple's locale data, which isn't here; a code
/// not listed is named nil, as Apple names an unknown one.
const LANGUAGE_NAMES: &[(&str, &str, &str)] = &[
    ("ar", "Arabic", "العربية"), ("ca", "Catalan", "català"), ("cs", "Czech", "čeština"), ("da", "Danish", "dansk"),
    ("de", "German", "Deutsch"), ("el", "Greek", "Ελληνικά"), ("en", "English", "English"), ("es", "Spanish", "español"),
    ("fi", "Finnish", "suomi"), ("fr", "French", "français"), ("he", "Hebrew", "עברית"), ("hi", "Hindi", "हिन्दी"),
    ("hr", "Croatian", "hrvatski"), ("hu", "Hungarian", "magyar"), ("id", "Indonesian", "Bahasa Indonesia"),
    ("it", "Italian", "italiano"), ("ja", "Japanese", "日本語"), ("ko", "Korean", "한국어"), ("ms", "Malay", "Bahasa Melayu"),
    ("nb", "Norwegian Bokmål", "norsk bokmål"), ("nl", "Dutch", "Nederlands"), ("no", "Norwegian", "norsk"),
    ("pl", "Polish", "polski"), ("pt", "Portuguese", "português"), ("ro", "Romanian", "română"), ("ru", "Russian", "русский"),
    ("sk", "Slovak", "slovenčina"), ("sv", "Swedish", "svenska"), ("th", "Thai", "ไทย"), ("tr", "Turkish", "Türkçe"),
    ("uk", "Ukrainian", "українська"), ("vi", "Vietnamese", "Tiếng Việt"), ("zh", "Chinese", "中文"),
];

const COUNTRY_NAMES: &[(&str, &str)] = &[
    ("AE", "United Arab Emirates"), ("AR", "Argentina"), ("AT", "Austria"), ("AU", "Australia"), ("BE", "Belgium"),
    ("BR", "Brazil"), ("CA", "Canada"), ("CH", "Switzerland"), ("CL", "Chile"), ("CN", "China"), ("CO", "Colombia"),
    ("CZ", "Czech Republic"), ("DE", "Germany"), ("DK", "Denmark"), ("EG", "Egypt"), ("ES", "Spain"), ("FI", "Finland"),
    ("FR", "France"), ("GB", "United Kingdom"), ("GR", "Greece"), ("HK", "Hong Kong SAR China"), ("HU", "Hungary"),
    ("ID", "Indonesia"), ("IE", "Ireland"), ("IL", "Israel"), ("IN", "India"), ("IT", "Italy"), ("JP", "Japan"),
    ("KR", "South Korea"), ("MX", "Mexico"), ("MY", "Malaysia"), ("NL", "Netherlands"), ("NO", "Norway"),
    ("NZ", "New Zealand"), ("PH", "Philippines"), ("PL", "Poland"), ("PT", "Portugal"), ("RO", "Romania"), ("RU", "Russia"),
    ("SA", "Saudi Arabia"), ("SE", "Sweden"), ("SG", "Singapore"), ("TH", "Thailand"), ("TR", "Turkey"), ("TW", "Taiwan"),
    ("UA", "Ukraine"), ("US", "United States"), ("VN", "Vietnam"), ("ZA", "South Africa"),
];

/// (code, English name, symbol in an English locale).
const CURRENCY_NAMES: &[(&str, &str, &str)] = &[
    ("AUD", "Australian Dollar", "A$"), ("BRL", "Brazilian Real", "R$"), ("CAD", "Canadian Dollar", "CA$"),
    ("CHF", "Swiss Franc", "CHF"), ("CNY", "Chinese Yuan", "CN¥"), ("DKK", "Danish Krone", "DKK"), ("EUR", "Euro", "€"),
    ("GBP", "British Pound", "£"), ("INR", "Indian Rupee", "₹"), ("JPY", "Japanese Yen", "¥"), ("KRW", "South Korean Won", "₩"),
    ("MXN", "Mexican Peso", "MX$"), ("NOK", "Norwegian Krone", "NOK"), ("RUB", "Russian Ruble", "RUB"),
    ("SEK", "Swedish Krona", "SEK"), ("USD", "US Dollar", "$"),
];

/// A language's name: its own name to a locale of that language, else English.
fn language_name(code: &str, viewer_language: &str) -> Option<String> {
    let &(_, english, own) = LANGUAGE_NAMES.iter().find(|(c, _, _)| *c == code)?;
    Some(if code == viewer_language { own } else { english }.to_string())
}

fn locale_display_name(viewer: &str, key: &str, value: &str) -> Option<String> {
    let (viewer_language, _, _) = locale_parts(viewer);
    let currency = || CURRENCY_NAMES.iter().find(|(c, _, _)| *c == value.to_uppercase());
    match key {
        "kCFLocaleLanguageCodeKey" => language_name(&value.to_lowercase(), &viewer_language),
        "kCFLocaleCountryCodeKey" => COUNTRY_NAMES.iter().find(|(c, _)| *c == value.to_uppercase()).map(|(_, n)| n.to_string()),
        "currency" | "kCFLocaleCurrencyCodeKey" => currency().map(|(_, name, _)| name.to_string()),
        "kCFLocaleCurrencySymbolKey" => currency().map(|(_, _, symbol)| symbol.to_string()),
        "kCFLocaleScriptCodeKey" => match value {
            "Hans" => Some("Simplified".into()),
            "Hant" => Some("Traditional".into()),
            _ => None,
        },
        // "French (France)", "Chinese (Simplified, China)".
        "kCFLocaleIdentifierKey" => {
            let (language, script, country) = locale_parts(value);
            let mut name = language_name(&language, &viewer_language)?;
            let extras: Vec<String> = [
                script.map(|s| locale_display_name(viewer, "kCFLocaleScriptCodeKey", &s).unwrap_or(s)),
                country.map(|c| locale_display_name(viewer, "kCFLocaleCountryCodeKey", &c).unwrap_or(c)),
            ]
            .into_iter()
            .flatten()
            .collect();
            if !extras.is_empty() {
                name += &format!(" ({})", extras.join(", "));
            }
            Some(name)
        }
        _ => None,
    }
}

extern "C" fn locale_display_name_for_key(this: Id, _: Sel, key: Id, value: Id) -> Id {
    if key == 0 || value == 0 {
        return 0;
    }
    match locale_display_name(&identifier_of(this), &string::get(key), &string::get(value)) {
        Some(name) => string::make(&name),
        None => 0,
    }
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
    let file_handle = define_class("NSFileHandle", "NSObject", 16);
    class_methods(
        file_handle,
        &[
            ("fileHandleForReadingAtPath:", file_handle_for_reading as usize),
            ("fileHandleForWritingAtPath:", file_handle_for_writing as usize),
            ("fileHandleForUpdatingAtPath:", file_handle_for_updating as usize),
            ("fileHandleWithStandardInput", file_handle_standard_input as usize),
            ("fileHandleWithStandardOutput", file_handle_standard_output as usize),
            ("fileHandleWithStandardError", file_handle_standard_error as usize),
            ("fileHandleWithNullDevice", file_handle_null_device as usize),
        ],
    );
    methods(
        file_handle,
        &[
            ("init", file_handle_init as usize),
            ("initWithFileDescriptor:", file_handle_init_descriptor as usize),
            ("initWithFileDescriptor:closeOnDealloc:", file_handle_init_descriptor_close as usize),
            ("dealloc", file_handle_dealloc as usize),
            ("readDataOfLength:", file_handle_read_length as usize),
            ("readDataToEndOfFile", file_handle_read_to_end as usize),
            ("availableData", file_handle_available as usize),
            ("writeData:", file_handle_write as usize),
            ("offsetInFile", file_handle_offset as usize),
            ("seekToFileOffset:", file_handle_seek as usize),
            ("seekToEndOfFile", file_handle_seek_end as usize),
            ("truncateFileAtOffset:", file_handle_truncate as usize),
            ("synchronizeFile", file_handle_synchronize as usize),
            ("fileDescriptor", file_handle_descriptor as usize),
            ("closeFile", file_handle_close as usize),
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
            ("displayNameForKey:value:", locale_display_name_for_key as usize),
            ("initWithLocaleIdentifier:", locale_init_with_identifier as usize),
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// -displayNameForKey:value: names codes as the iPhone does in English;
    /// a language is named in itself by its own locale; unknown keys and
    /// codes are nil.
    #[test]
    fn locales_name_languages_countries_and_currencies() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let class = runtime::class_named("NSLocale");
        let english = runtime::send(class, "localeWithLocaleIdentifier:", &[string::make("en_US")]);
        let name = |locale: Id, key: &str, value: &str| {
            let result = runtime::send(locale, "displayNameForKey:value:", &[string::make(key), string::make(value)]);
            if result == 0 { None } else { Some(string::get(result)) }
        };
        let some = |text: &str| Some(text.to_string());
        assert_eq!(name(english, "kCFLocaleLanguageCodeKey", "de"), some("German"));
        assert_eq!(name(english, "kCFLocaleCountryCodeKey", "JP"), some("Japan"));
        assert_eq!(name(english, "kCFLocaleIdentifierKey", "fr_FR"), some("French (France)"));
        assert_eq!(name(english, "kCFLocaleIdentifierKey", "zh-Hans"), some("Chinese (Simplified)"));
        assert_eq!(name(english, "currency", "EUR"), some("Euro"));
        assert_eq!(name(english, "kCFLocaleCurrencySymbolKey", "GBP"), some("£"));
        assert_eq!(name(english, "kCFLocaleLanguageCodeKey", "xx"), None, "unknown code");
        assert_eq!(name(english, "someOtherKey", "en"), None, "unknown key");
        let french = runtime::send(class, "localeWithLocaleIdentifier:", &[string::make("fr")]);
        assert_eq!(name(french, "kCFLocaleIdentifierKey", "fr"), some("français"));
        let japanese = runtime::send(class, "localeWithLocaleIdentifier:", &[string::make("ja_JP")]);
        assert_eq!(name(japanese, "kCFLocaleLanguageCodeKey", "ja"), Some("日本語".to_string()));
    }

    /// NSFileHandle reads, writes, seeks and closes as Apple's does:
    /// +fileHandleForReadingAtPath:/+fileHandleForWritingAtPath:, the read
    /// methods, -writeData:, -offsetInFile, -seekToFileOffset:,
    /// -seekToEndOfFile, -fileDescriptor (closed: -1) and -closeFile.
    #[test]
    fn file_handles_read_write_seek_and_close() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let folder = std::env::temp_dir().join(format!("gamebridge-filehandle-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let class = runtime::class_named("NSFileHandle");
        let reading = folder.join("reading.txt");
        std::fs::write(&reading, b"hello world").unwrap();
        let path = string::make(&reading.to_string_lossy());
        let handle = runtime::send(class, "fileHandleForReadingAtPath:", &[path]);
        assert_ne!(handle, 0, "a readable file gives a handle");
        let first = runtime::send(handle, "readDataOfLength:", &[5usize]);
        assert_eq!(crate::fw::data::bytes(first), b"hello");
        assert_eq!(runtime::send(handle, "offsetInFile", &[]), 5, "the read moved the offset");
        runtime::send(handle, "seekToFileOffset:", &[6usize]);
        let rest = runtime::send(handle, "readDataToEndOfFile", &[]);
        assert_eq!(crate::fw::data::bytes(rest), b"world");
        assert_eq!(runtime::send(handle, "seekToEndOfFile", &[]), 11, "the end of the file");
        assert!(runtime::send(handle, "fileDescriptor", &[]) as isize >= 0);
        runtime::send(handle, "closeFile", &[]);
        assert_eq!(runtime::send(handle, "fileDescriptor", &[]) as isize, -1, "a closed handle has no descriptor");
        assert_eq!(runtime::send(class, "fileHandleForReadingAtPath:", &[string::make(&folder.join("missing.txt").to_string_lossy())]), 0, "no file, no handle");

        let writing = folder.join("writing.txt");
        std::fs::write(&writing, b"").unwrap();
        let writer = runtime::send(class, "fileHandleForWritingAtPath:", &[string::make(&writing.to_string_lossy())]);
        assert_ne!(writer, 0, "an existing file opens for writing");
        runtime::send(writer, "writeData:", &[crate::fw::data::make(b"abc")]);
        runtime::send(writer, "synchronizeFile", &[]);
        runtime::send(writer, "closeFile", &[]);
        assert_eq!(std::fs::read(&writing).unwrap(), b"abc");

        // -truncateFileAtOffset: leaves the offset at the new end too (Apple
        // 0xa4842).
        let editing = folder.join("truncate.txt");
        std::fs::write(&editing, b"abcdef").unwrap();
        let editor = runtime::send(class, "fileHandleForUpdatingAtPath:", &[string::make(&editing.to_string_lossy())]);
        runtime::send(editor, "seekToFileOffset:", &[4usize]);
        runtime::send(editor, "truncateFileAtOffset:", &[3usize]);
        assert_eq!(runtime::send(editor, "offsetInFile", &[]) as u64, 3, "the offset moved to the end");
        assert_eq!(std::fs::read(&editing).unwrap(), b"abc");
        runtime::send(editor, "closeFile", &[]);

        // The standard handles are one object each (one descriptor), and a
        // handle closes its file when it is deallocated.
        let first = runtime::send(class, "fileHandleWithStandardOutput", &[]);
        let second = runtime::send(class, "fileHandleWithStandardOutput", &[]);
        assert_eq!(first, second, "standard output is one handle, not a new dup each call");
        let doomed = runtime::send(class, "fileHandleForReadingAtPath:", &[string::make(&reading.to_string_lossy())]);
        assert_ne!(runtime::send(doomed, "fileDescriptor", &[]) as isize, -1);
        runtime::send(doomed, "dealloc", &[]);
        assert_eq!(runtime::send(doomed, "fileDescriptor", &[]) as isize, -1, "dealloc closed the handle's file");
        std::fs::remove_dir_all(&folder).unwrap();
    }

    /// -availableData returns what a pipe holds after one read (it must not
    /// wait for the writer to close) and the rest of a regular file.
    #[test]
    fn available_data_reads_a_pipe_once_and_a_file_to_the_end() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "a pipe");
        let class = runtime::class_named("NSFileHandle");
        let handle = runtime::send(
            runtime::send(class, "alloc", &[]),
            "initWithFileDescriptor:closeOnDealloc:",
            &[fds[0] as usize, 1],
        );
        assert_ne!(handle, 0, "-initWithFileDescriptor:closeOnDealloc:");
        let wrote = unsafe { libc::write(fds[1], b"abc".as_ptr() as *const libc::c_void, 3) };
        assert_eq!(wrote, 3);
        let (done, got) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let data = runtime::send(handle, "availableData", &[]);
            let _ = done.send(crate::fw::data::bytes(data));
        });
        let bytes = got.recv_timeout(std::time::Duration::from_millis(500)).expect("availableData waited for the pipe to close");
        assert_eq!(bytes, b"abc", "one read gives what the pipe holds");
        unsafe { libc::close(fds[1]) };

        let folder = std::env::temp_dir().join(format!("gamebridge-available-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("large.txt");
        let contents: Vec<u8> = (0..5000).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &contents).unwrap();
        let reader = runtime::send(class, "fileHandleForReadingAtPath:", &[string::make(&path.to_string_lossy())]);
        let first = runtime::send(reader, "readDataOfLength:", &[100usize]);
        assert_eq!(crate::fw::data::bytes(first), contents[..100], "the first hundred bytes");
        let rest = runtime::send(reader, "availableData", &[]);
        assert_eq!(crate::fw::data::bytes(rest), contents[100..], "the rest of the file, not just 4 KiB");
        runtime::send(reader, "closeFile", &[]);
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
