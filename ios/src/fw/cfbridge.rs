//! CoreFoundation calls on our NSString/NSDictionary/NSNumber objects (they
//! are toll-free bridged on Apple), version numbers, and Foundation string
//! constants.

use crate::fw::{collections, dates, string};
use crate::objc::runtime::Id;
use crate::symbols::{leak, Registry};

// CFStringEncoding values.
const CF_MAC_ROMAN: u32 = 0;
const CF_WINDOWS_LATIN1: u32 = 0x0500;
const CF_ISO_LATIN1: u32 = 0x0201;
const CF_ASCII: u32 = 0x0600;
const CF_UTF8: u32 = 0x0800_0100;
const CF_UTF16: u32 = 0x0100;
const CF_UTF16_BE: u32 = 0x1000_0100;
const CF_UTF16_LE: u32 = 0x1400_0100;
const CF_UTF32: u32 = 0x0c00_0100;
const CF_UTF32_BE: u32 = 0x1800_0100;
const CF_UTF32_LE: u32 = 0x1c00_0100;
const CF_NON_LOSSY_ASCII: u32 = 0x0BFF;
const CF_INVALID: u32 = 0xFFFF_FFFF;

/// NSStringEncoding -> CFStringEncoding (Apple's table).
pub fn cf_encoding(ns: u64) -> u32 {
    if ns & 0x8000_0000 != 0 {
        // The high-bit NS values are CF values with that bit set.
        return (ns & 0x7FFF_FFFF) as u32;
    }
    match ns {
        1 => CF_ASCII,
        2 => 0x0B01,  // NEXTSTEP
        3 => 0x0920,  // EUC-JP
        4 => CF_UTF8,
        5 => CF_ISO_LATIN1,
        6 => 0x0006,  // Mac Symbol
        7 => CF_NON_LOSSY_ASCII,
        8 => 0x0A01,  // Shift JIS (Windows)
        9 => 0x0202,  // ISO Latin 2
        10 => CF_UTF16,
        11 => 0x0502, // Windows Cyrillic
        12 => CF_WINDOWS_LATIN1,
        13 => 0x0503, // Windows Greek
        14 => 0x0504, // Windows Turkish
        15 => 0x0501, // Windows Latin 2
        21 => 0x0820, // ISO-2022-JP
        30 => CF_MAC_ROMAN,
        _ => CF_INVALID,
    }
}

/// CFStringEncoding -> the NSStringEncoding our string code reads and writes.
fn ns_encoding(cf: u32) -> Option<u64> {
    Some(match cf {
        CF_ASCII => 1,
        CF_UTF8 => 4,
        CF_ISO_LATIN1 | CF_WINDOWS_LATIN1 | CF_MAC_ROMAN => 5,
        CF_UTF16 | CF_UTF16_LE => 0x9400_0100,
        CF_UTF32 | CF_UTF32_LE => 0x9c00_0100,
        CF_UTF16_BE | CF_UTF32_BE | CF_NON_LOSSY_ASCII => return None,
        _ => return None,
    })
}

/// A string's bytes in a CF encoding; None when the encoding can't hold it.
fn encoded(object: Id, cf: u32) -> Option<Vec<u8>> {
    let text = string::get(object);
    match cf {
        CF_ASCII if !text.is_ascii() => return None,
        CF_ISO_LATIN1 | CF_WINDOWS_LATIN1 | CF_MAC_ROMAN if text.chars().any(|c| c as u32 > 0x7F && (cf != CF_ISO_LATIN1 || c as u32 > 0xFF)) => {
            // Only Latin-1 maps 0x80-0xFF one to one; the others agree on ASCII.
            return None;
        }
        CF_UTF16_BE => return Some(text.encode_utf16().flat_map(|u| u.to_be_bytes()).collect()),
        CF_UTF32_BE => return Some(text.chars().flat_map(|c| (c as u32).to_be_bytes()).collect()),
        _ => {}
    }
    let units: Vec<u16> = text.encode_utf16().collect();
    match ns_encoding(cf) {
        Some(ns) => Some(string::encode(&units, ns as usize)),
        None => {
            crate::log!("CFString encoding {cf:#x} not known");
            None
        }
    }
}

extern "C" fn cf_string_get_c_string(object: Id, buffer: *mut u8, size: isize, encoding: u32) -> bool {
    if object == 0 || buffer.is_null() || size <= 0 {
        return false;
    }
    let Some(bytes) = encoded(object, encoding) else {
        return false;
    };
    if bytes.len() + 1 > size as usize {
        return false;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer, bytes.len());
        *buffer.add(bytes.len()) = 0;
    }
    true
}

/// A pointer to the string's own 8-bit bytes when it has them for this
/// encoding, else NULL (callers then use CFStringGetCString, as on Apple).
extern "C" fn cf_string_get_c_string_ptr(object: Id, encoding: u32) -> usize {
    if object == 0 {
        return 0;
    }
    let ascii_compatible = matches!(encoding, CF_ASCII | CF_ISO_LATIN1 | CF_WINDOWS_LATIN1 | CF_MAC_ROMAN);
    if encoding == CF_UTF8 || (ascii_compatible && string::get(object).is_ascii()) {
        return crate::objc::runtime::send(object, "UTF8String", &[]);
    }
    0
}

extern "C" fn cf_string_create_with_characters(_allocator: usize, characters: *const u16, count: isize) -> Id {
    if characters.is_null() || count <= 0 {
        return string::make("");
    }
    let units = unsafe { std::slice::from_raw_parts(characters, count as usize) };
    string::make(&String::from_utf16_lossy(units))
}

extern "C" fn cf_string_convert_ns_encoding(encoding: u64) -> u32 {
    cf_encoding(encoding)
}

extern "C" fn cf_string_convert_encoding_to_ns(encoding: u32) -> u64 {
    match encoding {
        CF_UTF16_BE => 0x9000_0100,
        CF_UTF32_BE => 0x9800_0100,
        CF_UTF16_LE => 0x9400_0100,
        CF_UTF32_LE => 0x9c00_0100,
        CF_UTF32 => 0x8c00_0100,
        CF_UTF16 => 10,
        _ => (1..=30).find(|&ns| cf_encoding(ns) == encoding).unwrap_or(0),
    }
}

extern "C" fn cf_dictionary_get_value(dictionary: Id, key: Id) -> Id {
    if dictionary == 0 || !collections::is_dictionary(dictionary) {
        return 0;
    }
    collections::dictionary_get(dictionary, key)
}

/// CFNumberGetValue: writes the number in the asked type; true when that
/// type holds it exactly.
pub unsafe fn number_get_value(integer: Option<i64>, float: f64, kind: isize, out: *mut u8) -> bool {
    fn put<T>(out: *mut u8, value: T) {
        unsafe { (out as *mut T).write_unaligned(value) }
    }
    let as_int = |min: i64, max: i64| -> (i64, bool) {
        match integer {
            Some(i) => (i.clamp(min, max), i >= min && i <= max),
            None => {
                let truncated = float.trunc();
                let value = if float.is_nan() { 0 } else { truncated.clamp(min as f64, max as f64) as i64 };
                (value, float.fract() == 0.0 && truncated >= min as f64 && truncated <= max as f64)
            }
        }
    };
    let as_float = || integer.map(|i| i as f64).unwrap_or(float);
    match kind {
        1 | 7 => {
            let (v, exact) = as_int(i8::MIN as i64, i8::MAX as i64);
            put(out, v as i8);
            exact
        }
        2 | 8 => {
            let (v, exact) = as_int(i16::MIN as i64, i16::MAX as i64);
            put(out, v as i16);
            exact
        }
        3 | 9 => {
            let (v, exact) = as_int(i32::MIN as i64, i32::MAX as i64);
            put(out, v as i32);
            exact
        }
        4 | 10 | 11 | 14 | 15 => {
            let (v, exact) = as_int(i64::MIN, i64::MAX);
            put(out, v);
            exact
        }
        5 | 12 => {
            let v = as_float();
            put(out, v as f32);
            (v as f32) as f64 == v || v.is_nan()
        }
        6 | 13 | 16 => {
            let v = as_float();
            put(out, v);
            integer.map(|i| (i as f64) as i64 == i).unwrap_or(true)
        }
        _ => {
            crate::log!("CFNumberGetValue type {kind} not known");
            false
        }
    }
}

extern "C" fn cf_number_get_value(number: Id, kind: isize, out: *mut u8) -> bool {
    if number == 0 || out.is_null() {
        return false;
    }
    let (integer, float) = if collections::is_number(number) {
        let (i, f) = (collections::number_as_i64(number), collections::number_as_f64(number));
        // Whole numbers are kept as integers; others as doubles.
        (if (i as f64) == f { Some(i) } else { None }, f)
    } else {
        (None, dates::call_f64(number, "doubleValue"))
    };
    unsafe { number_get_value(integer, float, kind, out) }
}

/// Core Foundation and Foundation of iOS 8.2, the version UIDevice reports.
const CF_VERSION: f64 = 1142.16;
const FOUNDATION_VERSION: f64 = 1142.14;

pub fn register(r: &mut Registry) {
    r.function("CFStringGetCString", cf_string_get_c_string as usize);
    r.function("CFStringGetCStringPtr", cf_string_get_c_string_ptr as usize);
    r.function("CFStringCreateWithCharacters", cf_string_create_with_characters as usize);
    r.function("CFStringConvertNSStringEncodingToEncoding", cf_string_convert_ns_encoding as usize);
    r.function("CFStringConvertEncodingToNSStringEncoding", cf_string_convert_encoding_to_ns as usize);
    r.function("CFDictionaryGetValue", cf_dictionary_get_value as usize);
    r.function("CFNumberGetValue", cf_number_get_value as usize);
    r.data("kCFCoreFoundationVersionNumber", leak(CF_VERSION));
    r.data("NSFoundationVersionNumber", leak(FOUNDATION_VERSION));

    // The run loop runs every mode alike, so the common modes are simply a
    // name of their own.
    let common = leak(string::make("kCFRunLoopCommonModes"));
    r.data("NSRunLoopCommonModes", common);
    r.data("kCFRunLoopCommonModes", common);
    let currency_symbol = leak(string::make("kCFLocaleCurrencySymbolKey"));
    r.data("NSLocaleCurrencySymbol", currency_symbol);
    r.data("kCFLocaleCurrencySymbol", currency_symbol);
    r.data("NSURLAuthenticationMethodServerTrust", leak(string::make("NSURLAuthenticationMethodServerTrust")));
    for (old, new, identifier) in [
        ("NSGregorianCalendar", "NSCalendarIdentifierGregorian", "gregorian"),
        ("NSBuddhistCalendar", "NSCalendarIdentifierBuddhist", "buddhist"),
        ("NSChineseCalendar", "NSCalendarIdentifierChinese", "chinese"),
        ("NSHebrewCalendar", "NSCalendarIdentifierHebrew", "hebrew"),
        ("NSISO8601Calendar", "NSCalendarIdentifierISO8601", "iso8601"),
        ("NSIndianCalendar", "NSCalendarIdentifierIndian", "indian"),
        ("NSIslamicCalendar", "NSCalendarIdentifierIslamic", "islamic"),
        ("NSIslamicCivilCalendar", "NSCalendarIdentifierIslamicCivil", "islamic-civil"),
        ("NSJapaneseCalendar", "NSCalendarIdentifierJapanese", "japanese"),
        ("NSPersianCalendar", "NSCalendarIdentifierPersian", "persian"),
        ("NSRepublicOfChinaCalendar", "NSCalendarIdentifierRepublicOfChina", "roc"),
    ] {
        let value = leak(string::make(identifier));
        r.data(old, value);
        r.data(new, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cf_numbers_and_encodings_convert_like_apple() {
        let mut out = [0u8; 8];
        unsafe {
            assert!(number_get_value(Some(300), 300.0, 9, out.as_mut_ptr()));
            assert_eq!(i32::from_ne_bytes(out[..4].try_into().unwrap()), 300);
            // 300 doesn't fit a signed byte: clamped, and reported as lossy.
            assert!(!number_get_value(Some(300), 300.0, 1, out.as_mut_ptr()));
            assert_eq!(out[0] as i8, 127);
            assert!(!number_get_value(None, 2.5, 4, out.as_mut_ptr()));
            assert_eq!(i64::from_ne_bytes(out), 2);
            assert!(number_get_value(None, 0.5, 5, out.as_mut_ptr()));
            assert_eq!(f32::from_ne_bytes(out[..4].try_into().unwrap()), 0.5);
        }
        assert_eq!(cf_encoding(4), CF_UTF8);
        assert_eq!(cf_encoding(0x9400_0100), CF_UTF16_LE);
        assert_eq!(cf_encoding(30), CF_MAC_ROMAN);
        assert_eq!(cf_string_convert_encoding_to_ns(CF_UTF8), 4);
    }
}
