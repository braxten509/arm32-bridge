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
        6 => 0x0021,  // Mac Symbol
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
        CF_ISO_LATIN1 => 5,
        CF_UTF16 | CF_UTF16_LE => 0x9400_0100,
        CF_UTF32 | CF_UTF32_LE => 0x9c00_0100,
        CF_UTF16_BE | CF_UTF32_BE | CF_NON_LOSSY_ASCII => return None,
        _ => return None,
    })
}

/// The characters of bytes 0x80-0xFF in Mac Roman (Apple's mapping, with
/// the euro sign at 0xDB) and Windows Latin-1 (cp1252; 0 = no character).
const MAC_ROMAN_HIGH: [u16; 128] = [
    0x00C4, 0x00C5, 0x00C7, 0x00C9, 0x00D1, 0x00D6, 0x00DC, 0x00E1, 0x00E0, 0x00E2, 0x00E4, 0x00E3, 0x00E5, 0x00E7, 0x00E9, 0x00E8,
    0x00EA, 0x00EB, 0x00ED, 0x00EC, 0x00EE, 0x00EF, 0x00F1, 0x00F3, 0x00F2, 0x00F4, 0x00F6, 0x00F5, 0x00FA, 0x00F9, 0x00FB, 0x00FC,
    0x2020, 0x00B0, 0x00A2, 0x00A3, 0x00A7, 0x2022, 0x00B6, 0x00DF, 0x00AE, 0x00A9, 0x2122, 0x00B4, 0x00A8, 0x2260, 0x00C6, 0x00D8,
    0x221E, 0x00B1, 0x2264, 0x2265, 0x00A5, 0x00B5, 0x2202, 0x2211, 0x220F, 0x03C0, 0x222B, 0x00AA, 0x00BA, 0x03A9, 0x00E6, 0x00F8,
    0x00BF, 0x00A1, 0x00AC, 0x221A, 0x0192, 0x2248, 0x2206, 0x00AB, 0x00BB, 0x2026, 0x00A0, 0x00C0, 0x00C3, 0x00D5, 0x0152, 0x0153,
    0x2013, 0x2014, 0x201C, 0x201D, 0x2018, 0x2019, 0x00F7, 0x25CA, 0x00FF, 0x0178, 0x2044, 0x20AC, 0x2039, 0x203A, 0xFB01, 0xFB02,
    0x2021, 0x00B7, 0x201A, 0x201E, 0x2030, 0x00C2, 0x00CA, 0x00C1, 0x00CB, 0x00C8, 0x00CD, 0x00CE, 0x00CF, 0x00CC, 0x00D3, 0x00D4,
    0xF8FF, 0x00D2, 0x00DA, 0x00DB, 0x00D9, 0x0131, 0x02C6, 0x02DC, 0x00AF, 0x02D8, 0x02D9, 0x02DA, 0x00B8, 0x02DD, 0x02DB, 0x02C7,
];
const WINDOWS_LATIN1_HIGH: [u16; 128] = [
    0x20AC, 0, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0, 0x017D, 0,
    0, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0, 0x017E, 0x0178,
    0x00A0, 0x00A1, 0x00A2, 0x00A3, 0x00A4, 0x00A5, 0x00A6, 0x00A7, 0x00A8, 0x00A9, 0x00AA, 0x00AB, 0x00AC, 0x00AD, 0x00AE, 0x00AF,
    0x00B0, 0x00B1, 0x00B2, 0x00B3, 0x00B4, 0x00B5, 0x00B6, 0x00B7, 0x00B8, 0x00B9, 0x00BA, 0x00BB, 0x00BC, 0x00BD, 0x00BE, 0x00BF,
    0x00C0, 0x00C1, 0x00C2, 0x00C3, 0x00C4, 0x00C5, 0x00C6, 0x00C7, 0x00C8, 0x00C9, 0x00CA, 0x00CB, 0x00CC, 0x00CD, 0x00CE, 0x00CF,
    0x00D0, 0x00D1, 0x00D2, 0x00D3, 0x00D4, 0x00D5, 0x00D6, 0x00D7, 0x00D8, 0x00D9, 0x00DA, 0x00DB, 0x00DC, 0x00DD, 0x00DE, 0x00DF,
    0x00E0, 0x00E1, 0x00E2, 0x00E3, 0x00E4, 0x00E5, 0x00E6, 0x00E7, 0x00E8, 0x00E9, 0x00EA, 0x00EB, 0x00EC, 0x00ED, 0x00EE, 0x00EF,
    0x00F0, 0x00F1, 0x00F2, 0x00F3, 0x00F4, 0x00F5, 0x00F6, 0x00F7, 0x00F8, 0x00F9, 0x00FA, 0x00FB, 0x00FC, 0x00FD, 0x00FE, 0x00FF,
];

/// One byte per character through a table for 0x80-0xFF; None when a
/// character has no byte.
fn single_byte(text: &str, high: &[u16; 128]) -> Option<Vec<u8>> {
    text.chars()
        .map(|c| match c as u32 {
            ascii @ 0..=0x7F => Some(ascii as u8),
            code => high.iter().position(|&h| h != 0 && h as u32 == code).map(|i| 0x80 + i as u8),
        })
        .collect()
}

/// A string's bytes in a CF encoding; None when the encoding can't hold it.
fn encoded(object: Id, cf: u32) -> Option<Vec<u8>> {
    let text = string::get(object);
    match cf {
        CF_ASCII if !text.is_ascii() => return None,
        CF_ISO_LATIN1 if text.chars().any(|c| c as u32 > 0xFF) => return None,
        CF_MAC_ROMAN => return single_byte(&text, &MAC_ROMAN_HIGH),
        CF_WINDOWS_LATIN1 => return single_byte(&text, &WINDOWS_LATIN1_HIGH),
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
                // In i128: max as f64 rounds i64::MAX up to 2^63.
                let whole = truncated as i128;
                (value, float.fract() == 0.0 && whole >= min as i128 && whole <= max as i128)
            }
        }
    };
    let as_float = || integer.map(|i| i as f64).unwrap_or(float);
    // Whether the integer survives the trip through the float type (compared
    // in i128, so values that round to 2^63 don't saturate back to equal).
    let integer_exact = |back: i128| integer.map(|i| back == i as i128);
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
            let v = match integer {
                Some(i) => i as f32,
                None => float as f32,
            };
            put(out, v);
            integer_exact(v as f64 as i128).unwrap_or((v as f64) == float || float.is_nan())
        }
        6 | 13 | 16 => {
            let v = as_float();
            put(out, v);
            integer_exact(v as i128).unwrap_or(true)
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
        // The number's own kind: a double stays a double even when whole.
        let float = collections::number_as_f64(number);
        ((!collections::number_is_float(number)).then(|| collections::number_as_i64(number)), float)
    } else {
        (None, dates::call_f64(number, "doubleValue"))
    };
    unsafe { number_get_value(integer, float, kind, out) }
}

/// Core Foundation and Foundation of iOS 8.2, the version UIDevice reports.
const CF_VERSION: f64 = 1142.16;
const FOUNDATION_VERSION: f64 = 1142.14;

/// CFStringCompare: the CFStringCompareFlags are NSStringCompareOptions' values.
extern "C" fn cf_string_compare(a: Id, b: Id, flags: usize) -> isize {
    crate::objc::runtime::send(a, "compare:options:", &[b, flags]) as isize
}

/// A new string (the caller's reference) or NULL when there is no extension.
extern "C" fn cf_url_copy_path_extension(url: Id) -> Id {
    let extension = crate::objc::runtime::send(url, "pathExtension", &[]);
    if extension == 0 || string::get(extension).is_empty() {
        return 0;
    }
    string::make(&string::get(extension))
}

extern "C" fn cf_string_append_characters(text: Id, characters: *const u16, count: isize) {
    if characters.is_null() || count <= 0 {
        return;
    }
    let units = unsafe { std::slice::from_raw_parts(characters, count as usize) };
    let more = string::make(&String::from_utf16_lossy(units));
    crate::objc::runtime::send(text, "appendString:", &[more]);
}

/// CFPropertyListCreateFromXMLData(allocator, data, options, &errorString):
/// any property list format, read by this layer's reader.
extern "C" fn cf_property_list_create_from_xml_data(_allocator: usize, data: Id, _options: usize, error: *mut Id) -> Id {
    let bytes = crate::fw::data::bytes(data);
    match crate::fw::plist::parse(&bytes) {
        Some(value) => crate::fw::plist::to_object(&value),
        None => {
            if !error.is_null() {
                unsafe { *error = string::make("not a property list") };
            }
            0
        }
    }
}

// ----- CFBitVector: bits numbered from the top bit of the first byte -----

static BIT_VECTOR: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

fn bit_vector(object: Id) -> Option<&'static std::sync::Mutex<Vec<bool>>> {
    let class = *BIT_VECTOR.get()?;
    if object == 0 || crate::objc::runtime::lock().class_of(object) != class {
        return None;
    }
    unsafe { (*((object + 8) as *const *const std::sync::Mutex<Vec<bool>>)).as_ref() }
}

fn make_bit_vector(bits: Vec<bool>) -> Id {
    let Some(&class) = BIT_VECTOR.get() else { return 0 };
    let object = crate::objc::runtime::alloc_instance(class);
    unsafe { *((object + 8) as *mut *mut std::sync::Mutex<Vec<bool>>) = Box::into_raw(Box::new(std::sync::Mutex::new(bits))) };
    object
}

extern "C" fn cf_bit_vector_create(_allocator: usize, bytes: *const u8, count: isize) -> Id {
    let count = count.max(0) as usize;
    let bits = (0..count).map(|i| !bytes.is_null() && unsafe { *bytes.add(i / 8) } & (0x80 >> (i % 8)) != 0).collect();
    make_bit_vector(bits)
}
extern "C" fn cf_bit_vector_create_copy(_allocator: usize, vector: Id) -> Id {
    bit_vector(vector).map_or(0, |v| make_bit_vector(v.lock().unwrap_or_else(|e| e.into_inner()).clone()))
}
extern "C" fn cf_bit_vector_create_mutable_copy(allocator: usize, _capacity: isize, vector: Id) -> Id {
    cf_bit_vector_create_copy(allocator, vector)
}
extern "C" fn cf_bit_vector_create_mutable(_allocator: usize, _capacity: isize) -> Id {
    make_bit_vector(Vec::new())
}
extern "C" fn cf_bit_vector_get_count(vector: Id) -> isize {
    bit_vector(vector).map_or(0, |v| v.lock().unwrap_or_else(|e| e.into_inner()).len() as isize)
}
extern "C" fn cf_bit_vector_set_count(vector: Id, count: isize) {
    if let Some(v) = bit_vector(vector) {
        v.lock().unwrap_or_else(|e| e.into_inner()).resize(count.max(0) as usize, false);
    }
}
extern "C" fn cf_bit_vector_get_bit_at_index(vector: Id, index: isize) -> u32 {
    bit_vector(vector).map_or(0, |v| v.lock().unwrap_or_else(|e| e.into_inner()).get(index as usize).copied().unwrap_or(false) as u32)
}
extern "C" fn cf_bit_vector_set_bit_at_index(vector: Id, index: isize, value: u32) {
    if let Some(v) = bit_vector(vector) {
        if let Some(bit) = v.lock().unwrap_or_else(|e| e.into_inner()).get_mut(index as usize) {
            *bit = value != 0;
        }
    }
}

pub fn register(r: &mut Registry) {
    let class = crate::fw::object::define_class("__CFBitVector", "NSObject", 16);
    let _ = BIT_VECTOR.set(class);
    r.function("CFStringCompare", cf_string_compare as usize);
    r.function("CFURLCopyPathExtension", cf_url_copy_path_extension as usize);
    r.function("CFStringAppendCharacters", cf_string_append_characters as usize);
    r.function("CFPropertyListCreateFromXMLData", cf_property_list_create_from_xml_data as usize);
    r.function("CFBitVectorCreate", cf_bit_vector_create as usize);
    r.function("CFBitVectorCreateCopy", cf_bit_vector_create_copy as usize);
    r.function("CFBitVectorCreateMutable", cf_bit_vector_create_mutable as usize);
    r.function("CFBitVectorCreateMutableCopy", cf_bit_vector_create_mutable_copy as usize);
    r.function("CFBitVectorGetCount", cf_bit_vector_get_count as usize);
    r.function("CFBitVectorSetCount", cf_bit_vector_set_count as usize);
    r.function("CFBitVectorGetBitAtIndex", cf_bit_vector_get_bit_at_index as usize);
    r.function("CFBitVectorSetBitAtIndex", cf_bit_vector_set_bit_at_index as usize);
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
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
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
            // 2^53+1 has no exact double or float: the check uses the integer itself.
            let odd = (1i64 << 53) + 1;
            assert!(!number_get_value(Some(odd), odd as f64, 5, out.as_mut_ptr()));
            assert!(!number_get_value(Some(odd), odd as f64, 6, out.as_mut_ptr()));
            assert!(!number_get_value(Some(i64::MAX), i64::MAX as f64, 6, out.as_mut_ptr()));
            assert!(number_get_value(Some(1 << 40), (1i64 << 40) as f64, 5, out.as_mut_ptr()));
        }
        // A double of 2^63 doesn't fit a 64-bit integer: lossy.
        let mut wide = [0u8; 8];
        assert!(!cf_number_get_value(collections::number_float(9_223_372_036_854_775_808.0), 4, wide.as_mut_ptr()));
        assert!(cf_number_get_value(collections::number_int(i64::MAX), 4, wide.as_mut_ptr()));
        let e = string::make("\u{e9}\u{20ac}");
        assert_eq!(encoded(e, CF_MAC_ROMAN), Some(vec![0x8E, 0xDB]));
        assert_eq!(encoded(e, CF_WINDOWS_LATIN1), Some(vec![0xE9, 0x80]));
        assert_eq!(encoded(e, CF_ISO_LATIN1), None);
        assert_eq!(cf_encoding(6), 0x21);
        assert_eq!(cf_encoding(4), CF_UTF8);
        assert_eq!(cf_encoding(0x9400_0100), CF_UTF16_LE);
        assert_eq!(cf_encoding(30), CF_MAC_ROMAN);
        assert_eq!(cf_string_convert_encoding_to_ns(CF_UTF8), 4);
    }
}
