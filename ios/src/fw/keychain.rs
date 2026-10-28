//! The keychain (Apple's password store) kept in a property-list file in the
//! game's home folder, plus the Core Foundation type functions it needs.

use crate::fw::{collections, data, plist, string};
use crate::objc::runtime::{self, lock, Id};
use crate::symbols::{leak, Registry};
use std::sync::Mutex;

const SUCCESS: i32 = 0;
const NOT_FOUND: i32 = -25300;
const DUPLICATE: i32 = -25299;
const BAD_PARAMETER: i32 = -50;
const IO_ERROR: i32 = -36;

const CONTROL_KEYS: &[&str] = &["r_Data", "r_Attributes", "r_Ref", "r_PersistentRef", "m_Limit", "v_Data", "v_PersistentRef", "u_AuthUI", "u_OpPrompt"];

static LOCK: Mutex<()> = Mutex::new(());

fn path() -> String {
    format!("{}/Library/keychain.plist", crate::fw::foundation::home())
}

type Item = Vec<(String, plist::Value)>;

/// Reads the stored items. A missing file is an empty keychain; a file that
/// can't be read is an error, so nothing gets saved over it.
fn load() -> Result<Vec<Item>, i32> {
    let path = path();
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            crate::log!("keychain: can't read {path}: {e}; leaving it untouched");
            return Err(IO_ERROR);
        }
    };
    match plist::parse(&bytes) {
        Some(plist::Value::Array(items)) => Ok(items
            .into_iter()
            .filter_map(|v| match v {
                plist::Value::Dict(entries) => Some(entries),
                _ => None,
            })
            .collect()),
        _ => {
            crate::log!("keychain: {path} is damaged; leaving it untouched");
            Err(IO_ERROR)
        }
    }
}

/// Attributes that tell two items apart (Apple's primary keys per item class).
fn identity_keys(class: &str) -> &'static [&'static str] {
    match class {
        "inet" => &["class", "acct", "sdmn", "srvr", "ptcl", "atyp", "port", "path", "agrp", "sync"],
        "keys" => &["class", "kcls", "klbl", "atag", "type", "bsiz", "esiz", "agrp", "sync"],
        _ => &["class", "acct", "svce", "agrp", "sync"],
    }
}

fn item_class(item: &Item) -> String {
    match item.iter().find(|(k, _)| k == "class").map(|(_, v)| v) {
        Some(plist::Value::String(class)) => class.clone(),
        _ => String::new(),
    }
}

fn identity(item: &Item) -> Vec<(String, String)> {
    let class = item_class(item);
    identity_keys(&class)
        .iter()
        .map(|key| {
            let value = item.iter().find(|(k, _)| k == key).map(|(_, v)| format!("{v:?}")).unwrap_or_default();
            (key.to_string(), value)
        })
        .collect()
}

fn save(items: &[Item]) -> bool {
    let value = plist::Value::Array(items.iter().cloned().map(plist::Value::Dict).collect());
    let path = path();
    let temporary = format!("{path}.part");
    std::fs::write(&temporary, plist::to_xml(&value)).and_then(|_| std::fs::rename(&temporary, &path)).is_ok()
}

fn query_pairs(query: Id) -> Vec<(String, Id)> {
    collections::dictionary_pairs(query).into_iter().map(|(k, v)| (string::get(k), v)).collect()
}

fn truthy(value: Id) -> bool {
    value != 0 && collections::is_number(value) && collections::number_as_f64(value) != 0.0
}

fn same_value(stored: &plist::Value, wanted: Id) -> bool {
    match plist::from_object(wanted) {
        Some(w) => format!("{stored:?}") == format!("{w:?}"),
        None => false,
    }
}

fn matches(item: &Item, query: &[(String, Id)]) -> bool {
    let by_reference = query.iter().filter(|(k, _)| k == "v_PersistentRef").all(|(_, wanted)| data::is_data(*wanted) && data::bytes(*wanted) == persistent_ref(item));
    by_reference
        && query.iter().filter(|(k, _)| !CONTROL_KEYS.contains(&k.as_str())).all(|(key, wanted)| item.iter().any(|(k, v)| k == key && same_value(v, *wanted)))
}

/// Each item's own number, saved with it (Apple's keychain rows have one);
/// not an attribute, so never handed out or matched.
const ROW_KEY: &str = "gb_row";

/// An item's persistent reference (kSecValuePersistentRef): bytes naming it
/// by its saved row number, so it finds the same item in a later run, even
/// after an update. Items saved before rows were numbered: all their
/// attributes but the secret.
fn persistent_ref(item: &Item) -> Vec<u8> {
    let mut bytes = b"gbkc".to_vec();
    match item.iter().find(|(k, _)| k == ROW_KEY) {
        Some((_, row)) => bytes.extend_from_slice(format!("{row:?}").as_bytes()),
        None => {
            for (key, value) in item.iter().filter(|(k, _)| k != "v_Data") {
                bytes.extend_from_slice(format!("{key}={value:?}\0").as_bytes());
            }
        }
    }
    bytes
}

/// A new row number, never given out before (like Apple's rowids, a deleted
/// item's number stays unused): past every saved row, every number handed
/// out in this run, and the clock in microseconds, so a later run does not
/// reuse a number whose item was deleted.
fn next_row(items: &[Item]) -> i64 {
    static LAST: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    let rows = items.iter().filter_map(|item| match item.iter().find(|(k, _)| k == ROW_KEY) {
        Some((_, plist::Value::Integer(row))) => Some(*row),
        _ => None,
    });
    let saved = rows.max().unwrap_or(0);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_micros() as i64);
    let mut row = 0;
    let _ = LAST.fetch_update(std::sync::atomic::Ordering::SeqCst, std::sync::atomic::Ordering::SeqCst, |last| {
        row = (last + 1).max(saved + 1).max(now);
        Some(row)
    });
    row
}

/// A key item's SecKeyRef: made from its stored bytes (public RSA keys);
/// other items have none here.
fn key_ref(item: &Item) -> Id {
    match item.iter().find(|(k, _)| k == "v_Data").map(|(_, v)| v) {
        Some(plist::Value::Data(bytes)) if item_class(item) == "keys" => crate::fw::seckey::key_object(bytes).unwrap_or(0),
        _ => 0,
    }
}

fn result_for(item: &Item, query: &[(String, Id)]) -> Id {
    let get = |name: &str| query.iter().find(|(k, _)| k == name).map(|(_, v)| *v).unwrap_or(0);
    let want_data = truthy(get("r_Data"));
    let want_attributes = truthy(get("r_Attributes"));
    let want_persistent = truthy(get("r_PersistentRef"));
    let want_ref = truthy(get("r_Ref"));
    let kinds = [want_data, want_attributes, want_persistent, want_ref].iter().filter(|&&w| w).count();
    if want_ref && kinds == 1 {
        return key_ref(item);
    }
    let stored_data = || match item.iter().find(|(k, _)| k == "v_Data").map(|(_, v)| v) {
        Some(plist::Value::Data(bytes)) => data::make(bytes),
        _ => data::make(&[]),
    };
    if want_persistent && kinds == 1 {
        return data::make(&persistent_ref(item));
    }
    // More than one kind asked for: a dictionary of them (the attributes too when asked).
    if want_attributes || kinds > 1 {
        let mut entries: Vec<(String, plist::Value)> = if want_attributes {
            item.iter().filter(|(k, _)| k != "v_Data" && k != ROW_KEY).cloned().collect()
        } else {
            Vec::new()
        };
        if want_data {
            if let Some(v) = item.iter().find(|(k, _)| k == "v_Data") {
                entries.push(v.clone());
            }
        }
        if want_persistent {
            entries.push(("v_PersistentRef".into(), plist::Value::Data(persistent_ref(item))));
        }
        let dictionary = plist::to_object(&plist::Value::Dict(entries));
        let key = if want_ref { key_ref(item) } else { 0 };
        if key == 0 {
            return dictionary;
        }
        let mut pairs = collections::dictionary_pairs(dictionary);
        pairs.push((string::make("v_Ref"), key));
        return collections::dictionary_from(&pairs);
    }
    if want_data {
        return stored_data();
    }
    0
}

extern "C" fn copy_matching(query: Id, result: *mut Id) -> i32 {
    let _guard = LOCK.lock().unwrap();
    let query = query_pairs(query);
    let all = query.iter().any(|(k, v)| k == "m_Limit" && string::get(*v) == "m_LimitAll");
    let items = match load() {
        Ok(items) => items,
        Err(code) => return code,
    };
    let found: Vec<Item> = items.into_iter().filter(|item| matches(item, &query)).collect();
    if found.is_empty() {
        return NOT_FOUND;
    }
    if !result.is_null() {
        let value = if all {
            let list: Vec<Id> = found.iter().map(|item| result_for(item, &query)).collect();
            let array = collections::array_from(&list);
            // The array holds them now: the results' own references go.
            list.iter().for_each(|&item| crate::fw::counted::release(item));
            array
        } else {
            result_for(&found[0], &query)
        };
        unsafe { *result = value };
    }
    SUCCESS
}

fn item_from(attributes: &[(String, Id)]) -> Item {
    attributes
        .iter()
        .filter(|(k, _)| !CONTROL_KEYS.contains(&k.as_str()) || k == "v_Data")
        .filter_map(|(k, v)| Some((k.clone(), plist::from_object(*v)?)))
        .collect()
}

extern "C" fn add(attributes: Id, result: *mut Id) -> i32 {
    let _guard = LOCK.lock().unwrap();
    let attributes = query_pairs(attributes);
    let mut items = match load() {
        Ok(items) => items,
        Err(code) => return code,
    };
    let mut new_item = item_from(&attributes);
    let new_identity = identity(&new_item);
    if items.iter().any(|item| identity(item) == new_identity) {
        return DUPLICATE;
    }
    new_item.push((ROW_KEY.into(), plist::Value::Integer(next_row(&items))));
    if !result.is_null() {
        // What was asked for (a key's reference, its persistent reference...).
        unsafe { *result = result_for(&new_item, &attributes) };
    }
    items.push(new_item);
    if save(&items) { SUCCESS } else { BAD_PARAMETER }
}

extern "C" fn update(query: Id, changes: Id) -> i32 {
    let _guard = LOCK.lock().unwrap();
    let query = query_pairs(query);
    let changes = item_from(&query_pairs(changes));
    let mut items = match load() {
        Ok(items) => items,
        Err(code) => return code,
    };
    let mut found = false;
    for item in items.iter_mut().filter(|item| matches(item, &query)) {
        found = true;
        for (key, value) in &changes {
            match item.iter_mut().find(|(k, _)| k == key) {
                Some(entry) => entry.1 = value.clone(),
                None => item.push((key.clone(), value.clone())),
            }
        }
    }
    if !found {
        return NOT_FOUND;
    }
    // An update may not make two items the same.
    let identities: Vec<_> = items.iter().map(identity).collect();
    if (1..identities.len()).any(|i| identities[..i].contains(&identities[i])) {
        return DUPLICATE;
    }
    if save(&items) { SUCCESS } else { BAD_PARAMETER }
}

extern "C" fn delete(query: Id) -> i32 {
    let _guard = LOCK.lock().unwrap();
    let query = query_pairs(query);
    let mut items = match load() {
        Ok(items) => items,
        Err(code) => return code,
    };
    let before = items.len();
    items.retain(|item| !matches(item, &query));
    if items.len() == before {
        return NOT_FOUND;
    }
    if save(&items) { SUCCESS } else { BAD_PARAMETER }
}

// ----- Core Foundation type functions -----

const TYPE_STRING: usize = 7;
const TYPE_DICTIONARY: usize = 18;
const TYPE_ARRAY: usize = 19;
const TYPE_DATA: usize = 20;
const TYPE_BOOLEAN: usize = 21;
const TYPE_NUMBER: usize = 22;

extern "C" fn get_type_id(object: Id) -> usize {
    if string::is_string(object) {
        TYPE_STRING
    } else if collections::is_dictionary(object) {
        TYPE_DICTIONARY
    } else if collections::is_array(object) {
        TYPE_ARRAY
    } else if data::is_data(object) {
        TYPE_DATA
    } else if collections::is_number(object) {
        if collections::number_is_bool(object) { TYPE_BOOLEAN } else { TYPE_NUMBER }
    } else {
        let rt = lock();
        let class = rt.class_of(object);
        crate::log!("CFGetTypeID of a {}: not known", rt.class_name(class));
        0
    }
}

extern "C" fn data_type_id() -> usize {
    TYPE_DATA
}
extern "C" fn string_type_id() -> usize {
    TYPE_STRING
}
extern "C" fn dictionary_type_id() -> usize {
    TYPE_DICTIONARY
}
extern "C" fn array_type_id() -> usize {
    TYPE_ARRAY
}
extern "C" fn number_type_id() -> usize {
    TYPE_NUMBER
}
extern "C" fn boolean_type_id() -> usize {
    TYPE_BOOLEAN
}

extern "C" fn cf_retain(object: Id) -> Id {
    // Data providers keep their own count: CFRetain is a retain like any other.
    if !crate::fw::graphics::cf_retain_provider(object) {
        crate::fw::corevideo::retained(object);
        crate::fw::counted::retain(object);
    }
    object
}

extern "C" fn cf_release(object: Id) {
    // A data provider's release callback runs at its last reference.
    if !crate::fw::graphics::cf_release_provider(object) {
        crate::fw::corevideo::released(object);
        crate::fw::counted::release(object);
    }
}

pub fn register(r: &mut Registry) {
    r.function("SecItemCopyMatching", copy_matching as usize);
    r.function("SecItemAdd", add as usize);
    r.function("SecItemUpdate", update as usize);
    r.function("SecItemDelete", delete as usize);
    for (name, value) in [
        ("kSecClass", "class"),
        ("kSecClassGenericPassword", "genp"),
        ("kSecClassInternetPassword", "inet"),
        ("kSecAttrService", "svce"),
        ("kSecAttrAccount", "acct"),
        ("kSecAttrAccessGroup", "agrp"),
        ("kSecAttrSynchronizable", "sync"),
        ("kSecAttrServer", "srvr"),
        ("kSecAttrSecurityDomain", "sdmn"),
        ("kSecAttrProtocol", "ptcl"),
        ("kSecAttrAuthenticationType", "atyp"),
        ("kSecAttrPort", "port"),
        ("kSecAttrPath", "path"),
        ("kSecAttrAccessible", "pdmn"),
        ("kSecAttrAccessibleWhenUnlocked", "ak"),
        ("kSecAttrAccessibleAfterFirstUnlock", "ck"),
        ("kSecAttrAccessibleAlways", "dk"),
        ("kSecAttrGeneric", "gena"),
        ("kSecAttrLabel", "labl"),
        ("kSecAttrDescription", "desc"),
        ("kSecAttrComment", "icmt"),
        ("kSecValueData", "v_Data"),
        ("kSecValuePersistentRef", "v_PersistentRef"),
        ("kSecReturnData", "r_Data"),
        ("kSecReturnAttributes", "r_Attributes"),
        ("kSecReturnPersistentRef", "r_PersistentRef"),
        ("kSecMatchLimit", "m_Limit"),
        ("kSecMatchLimitOne", "m_LimitOne"),
        ("kSecMatchLimitAll", "m_LimitAll"),
        ("kSecTrustInfoExtendedValidationKey", "ExtendedValidation"),
        ("kSecClassKey", "keys"),
        ("kSecAttrKeyClass", "kcls"),
        ("kSecAttrKeyClassPublic", "0"),
        ("kSecAttrKeyClassPrivate", "1"),
        ("kSecAttrApplicationLabel", "klbl"),
        ("kSecAttrApplicationTag", "atag"),
        ("kSecAttrKeyType", "type"),
        ("kSecAttrKeyTypeRSA", "42"),
        ("kSecAttrKeySizeInBits", "bsiz"),
        ("kSecReturnRef", "r_Ref"),
    ] {
        r.data(name, leak(string::make(value)));
    }
    r.function("CFGetTypeID", get_type_id as usize);
    r.function("CFDataGetTypeID", data_type_id as usize);
    r.function("CFStringGetTypeID", string_type_id as usize);
    r.function("CFDictionaryGetTypeID", dictionary_type_id as usize);
    r.function("CFArrayGetTypeID", array_type_id as usize);
    r.function("CFNumberGetTypeID", number_type_id as usize);
    r.function("CFBooleanGetTypeID", boolean_type_id as usize);
    r.function("CFRetain", cf_retain as usize);
    r.function("CFRelease", cf_release as usize);
    let _ = runtime::sel;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// kSecReturnPersistentRef hands back data naming the item, which finds
    /// it again as kSecValuePersistentRef (and no other item).
    #[test]
    fn a_persistent_ref_finds_its_item_again() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let item = |account: &str| -> Item {
            vec![
                ("class".into(), plist::Value::String("genp".into())),
                ("acct".into(), plist::Value::String(account.into())),
                ("svce".into(), plist::Value::String("game".into())),
                ("v_Data".into(), plist::Value::Data(b"secret".to_vec())),
            ]
        };
        let (mine, other) = (item("me"), item("you"));
        let reference = result_for(&mine, &[("r_PersistentRef".into(), collections::number_bool(true))]);
        assert!(reference != 0 && data::is_data(reference) && !data::bytes(reference).is_empty());
        let query = [("v_PersistentRef".to_string(), reference)];
        assert!(matches(&mine, &query));
        assert!(!matches(&other, &query));
        // Asked with the attributes, it is one of them.
        let attributes = result_for(&mine, &[("r_PersistentRef".into(), collections::number_bool(true)), ("r_Attributes".into(), collections::number_bool(true))]);
        let found = collections::dictionary_get(attributes, string::make("v_PersistentRef"));
        assert_eq!(data::bytes(found), data::bytes(reference));

        // Each item has its own: two with no account or service (told apart
        // by label here), or the same attributes under two saved row numbers.
        let labelled = |label: &str| -> Item { vec![("class".into(), plist::Value::String("genp".into())), ("labl".into(), plist::Value::String(label.into()))] };
        let pref = |item: &Item| data::bytes(result_for(item, &[("r_PersistentRef".into(), collections::number_bool(true))]));
        assert_ne!(pref(&labelled("a")), pref(&labelled("b")));
        let numbered = |row: i64| -> Item {
            let mut item = labelled("a");
            item.push((ROW_KEY.into(), plist::Value::Integer(row)));
            item
        };
        assert_ne!(pref(&numbered(1)), pref(&numbered(2)));
        let attributes = result_for(&numbered(1), &[("r_Attributes".into(), collections::number_bool(true))]);
        assert_eq!(collections::dictionary_get(attributes, string::make(ROW_KEY)), 0, "the row number is ours, not an attribute");
        // A row number is never given out again, even once its item is
        // deleted (a saved reference must not find a later item).
        let first = next_row(&[]);
        assert!(next_row(&[]) > first, "the deleted item's number stays unused");
        assert!(next_row(&[numbered(i64::MAX / 2)]) > i64::MAX / 2);
        // The reference and the persistent reference together: a dictionary.
        let both = result_for(&mine, &[("r_Ref".into(), collections::number_bool(true)), ("r_PersistentRef".into(), collections::number_bool(true))]);
        assert!(collections::is_dictionary(both));
        assert_eq!(data::bytes(collections::dictionary_get(both, string::make("v_PersistentRef"))), data::bytes(reference));
    }
}
