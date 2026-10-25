//! NSCharacterSet (and its mutable form), NSScanner, and the NSString
//! methods that take character sets.

use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::{collections, data, string};
use crate::objc::runtime::{self, lock, Class, Id, Sel};
use crate::symbols::Registry;
use std::sync::OnceLock;

// ----- sets of characters -----

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Builtin {
    Whitespace,
    WhitespaceAndNewline,
    Newline,
    DecimalDigit,
    Letter,
    Lowercase,
    Uppercase,
    Capitalized,
    Alphanumeric,
    Punctuation,
    Symbol,
    Control,
    NonBase,
    Decomposable,
    Illegal,
    UrlQuery,
    UrlPath,
    UrlHost,
    UrlFragment,
    UrlUser,
    UrlPassword,
}

const URL_UNRESERVED: &str = "-._~";

fn is_newline(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{0B}' | '\u{0C}' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

fn is_unicode_whitespace(c: char) -> bool {
    c == '\t' || matches!(c, ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}')
}

fn is_punctuation(c: char) -> bool {
    if c.is_ascii() {
        // Unicode puts $ + < = > ^ ` | ~ under symbols.
        return c.is_ascii_punctuation() && !"$+<=>^`|~".contains(c);
    }
    matches!(c,
        '\u{A1}' | '\u{A7}' | '\u{AB}' | '\u{B6}' | '\u{B7}' | '\u{BB}' | '\u{BF}' | '\u{2010}'..='\u{2027}'
        | '\u{2030}'..='\u{2043}' | '\u{2045}'..='\u{2051}' | '\u{2053}'..='\u{205E}' | '\u{3001}'..='\u{3003}'
        | '\u{3008}'..='\u{3011}' | '\u{3014}'..='\u{301F}' | '\u{FF01}'..='\u{FF03}' | '\u{FF05}'..='\u{FF0A}'
        | '\u{FF0C}'..='\u{FF0F}' | '\u{FF1A}' | '\u{FF1B}' | '\u{FF1F}' | '\u{FF20}')
}

fn is_symbol(c: char) -> bool {
    if c.is_ascii() {
        return "$+<=>^`|~".contains(c);
    }
    matches!(c,
        '\u{A2}'..='\u{A6}' | '\u{A8}' | '\u{A9}' | '\u{AC}' | '\u{AE}'..='\u{B1}' | '\u{B4}' | '\u{B8}' | '\u{D7}' | '\u{F7}'
        | '\u{2044}' | '\u{20A0}'..='\u{20CF}' | '\u{2100}'..='\u{214F}' | '\u{2190}'..='\u{23FF}' | '\u{2500}'..='\u{27BF}'
        | '\u{2900}'..='\u{2BFF}' | '\u{1F000}'..='\u{1FAFF}')
}

fn is_non_base(c: char) -> bool {
    matches!(c, '\u{300}'..='\u{36F}' | '\u{483}'..='\u{489}' | '\u{591}'..='\u{5BD}' | '\u{610}'..='\u{61A}'
        | '\u{64B}'..='\u{65F}' | '\u{1AB0}'..='\u{1AFF}' | '\u{1DC0}'..='\u{1DFF}' | '\u{20D0}'..='\u{20FF}'
        | '\u{FE20}'..='\u{FE2F}')
}

impl Builtin {
    fn contains(self, c: char) -> bool {
        let url = |extra: &str| c.is_ascii_alphanumeric() || URL_UNRESERVED.contains(c) || extra.contains(c);
        match self {
            Builtin::Whitespace => is_unicode_whitespace(c),
            Builtin::WhitespaceAndNewline => is_unicode_whitespace(c) || is_newline(c),
            Builtin::Newline => is_newline(c),
            Builtin::DecimalDigit => c.is_numeric() && c.to_digit(10).is_some() || (c.is_numeric() && !c.is_ascii() && !matches!(c, '\u{B2}' | '\u{B3}' | '\u{B9}' | '\u{BC}'..='\u{BE}')),
            Builtin::Letter => c.is_alphabetic() || is_non_base(c),
            Builtin::Lowercase => c.is_lowercase(),
            Builtin::Uppercase => c.is_uppercase(),
            Builtin::Capitalized => matches!(c, '\u{1C5}' | '\u{1C8}' | '\u{1CB}' | '\u{1F2}' | '\u{1F88}'..='\u{1F8F}' | '\u{1F98}'..='\u{1F9F}' | '\u{1FA8}'..='\u{1FAF}' | '\u{1FBC}' | '\u{1FCC}' | '\u{1FFC}'),
            Builtin::Alphanumeric => c.is_alphanumeric() || is_non_base(c),
            Builtin::Punctuation => is_punctuation(c),
            Builtin::Symbol => is_symbol(c),
            Builtin::Control => c.is_control() || matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}'),
            Builtin::NonBase => is_non_base(c),
            Builtin::Decomposable => matches!(c, '\u{C0}'..='\u{17F}') && c != '\u{D7}' && c != '\u{F7}' && !matches!(c, '\u{D0}' | '\u{DE}' | '\u{DF}' | '\u{F0}' | '\u{FE}' | '\u{110}' | '\u{111}' | '\u{126}' | '\u{127}' | '\u{131}' | '\u{138}' | '\u{141}' | '\u{142}' | '\u{149}' | '\u{14A}' | '\u{14B}' | '\u{152}' | '\u{153}' | '\u{166}' | '\u{167}' | '\u{17F}' | '\u{13F}' | '\u{140}' | '\u{132}' | '\u{133}'),
            Builtin::Illegal => false,
            Builtin::UrlQuery | Builtin::UrlFragment => url("!$&'()*+,;=:@/?"),
            Builtin::UrlPath => url("!$&'()*+,;=:@/"),
            Builtin::UrlHost => url("!$&'()*+,;=:[]"),
            Builtin::UrlUser => url("!$&'()*+,;="),
            Builtin::UrlPassword => url("!$&'()*+,;="),
        }
    }
}

/// A set as a small formula over built-in classes and ranges.
#[derive(Clone, Debug)]
pub enum Set {
    Builtin(Builtin),
    /// Inclusive code point ranges.
    Ranges(Vec<(u32, u32)>),
    Not(Box<Set>),
    Union(Vec<Set>),
    Intersection(Vec<Set>),
}

impl Set {
    pub fn of_text(text: &str) -> Set {
        Set::Ranges(text.chars().map(|c| (c as u32, c as u32)).collect())
    }

    pub fn contains(&self, code: u32) -> bool {
        match self {
            Set::Builtin(b) => char::from_u32(code).map(|c| b.contains(c)).unwrap_or(false),
            Set::Ranges(ranges) => ranges.iter().any(|&(a, b)| code >= a && code <= b),
            Set::Not(inner) => !inner.contains(code),
            Set::Union(list) => list.iter().any(|s| s.contains(code)),
            Set::Intersection(list) => list.iter().all(|s| s.contains(code)),
        }
    }

    /// UTF-16 unit membership (surrogate halves are never members, as on Apple).
    pub fn contains_unit(&self, unit: u16) -> bool {
        !(0xD800..0xE000).contains(&unit) && self.contains(unit as u32)
    }
}

// ----- NSCharacterSet: { isa, *mut Set } -----

static CLASSES: OnceLock<(Class, Class)> = OnceLock::new();

fn set_of(object: Id) -> &'static mut Set {
    unsafe {
        let slot = (object + 8) as *mut *mut Set;
        if (*slot).is_null() {
            *slot = Box::into_raw(Box::new(Set::Ranges(Vec::new())));
        }
        &mut **slot
    }
}

/// The set behind an NSCharacterSet (empty for nil or other objects).
pub fn set(object: Id) -> Set {
    if object == 0 {
        return Set::Ranges(Vec::new());
    }
    let (base, _) = *CLASSES.get().unwrap();
    let is_set = {
        let rt = lock();
        rt.is_subclass(rt.class_of(object), base)
    };
    if is_set {
        set_of(object).clone()
    } else {
        Set::Ranges(Vec::new())
    }
}

fn make_set(class: Class, value: Set) -> Id {
    let object = runtime::alloc_instance(class);
    unsafe { *((object + 8) as *mut *mut Set) = Box::into_raw(Box::new(value)) };
    object
}

fn class_for(class: Class) -> Class {
    let (base, mutable) = *CLASSES.get().unwrap();
    let rt = lock();
    if rt.is_subclass(class, mutable) {
        class
    } else {
        base
    }
}

macro_rules! builtin_set {
    ($name:ident, $kind:expr) => {
        extern "C" fn $name(class: Class, _: Sel) -> Id {
            make_set(class_for(class), Set::Builtin($kind))
        }
    };
}

builtin_set!(cs_whitespace, Builtin::Whitespace);
builtin_set!(cs_whitespace_newline, Builtin::WhitespaceAndNewline);
builtin_set!(cs_newline, Builtin::Newline);
builtin_set!(cs_digits, Builtin::DecimalDigit);
builtin_set!(cs_letters, Builtin::Letter);
builtin_set!(cs_lowercase, Builtin::Lowercase);
builtin_set!(cs_uppercase, Builtin::Uppercase);
builtin_set!(cs_capitalized, Builtin::Capitalized);
builtin_set!(cs_alphanumeric, Builtin::Alphanumeric);
builtin_set!(cs_punctuation, Builtin::Punctuation);
builtin_set!(cs_symbols, Builtin::Symbol);
builtin_set!(cs_control, Builtin::Control);
builtin_set!(cs_non_base, Builtin::NonBase);
builtin_set!(cs_decomposable, Builtin::Decomposable);
builtin_set!(cs_illegal, Builtin::Illegal);
builtin_set!(cs_url_query, Builtin::UrlQuery);
builtin_set!(cs_url_path, Builtin::UrlPath);
builtin_set!(cs_url_host, Builtin::UrlHost);
builtin_set!(cs_url_fragment, Builtin::UrlFragment);
builtin_set!(cs_url_user, Builtin::UrlUser);
builtin_set!(cs_url_password, Builtin::UrlPassword);

extern "C" fn cs_with_string(class: Class, _: Sel, text: Id) -> Id {
    make_set(class_for(class), Set::of_text(&string::get(text)))
}

fn range_set(range: string::Range) -> Set {
    if range.length == 0 {
        return Set::Ranges(Vec::new());
    }
    Set::Ranges(vec![(range.location as u32, (range.location + range.length - 1) as u32)])
}

extern "C" fn cs_with_range(class: Class, _: Sel, range: string::Range) -> Id {
    make_set(class_for(class), range_set(range))
}

extern "C" fn cs_with_bitmap(class: Class, _: Sel, bitmap: Id) -> Id {
    let bytes = data::bytes(bitmap);
    let mut ranges = Vec::new();
    for (index, byte) in bytes.iter().enumerate().take(8192) {
        for bit in 0..8 {
            if byte & (1 << bit) != 0 {
                let code = (index * 8 + bit) as u32;
                ranges.push((code, code));
            }
        }
    }
    make_set(class_for(class), Set::Ranges(ranges))
}

extern "C" fn cs_init(this: Id, _: Sel) -> Id {
    set_of(this);
    this
}

extern "C" fn cs_is_member(this: Id, _: Sel, unit: u16) -> bool {
    set_of(this).contains_unit(unit)
}

extern "C" fn cs_long_is_member(this: Id, _: Sel, code: u32) -> bool {
    set_of(this).contains(code)
}

extern "C" fn cs_inverted(this: Id, _: Sel) -> Id {
    make_set(CLASSES.get().unwrap().0, Set::Not(Box::new(set_of(this).clone())))
}

extern "C" fn cs_is_superset(this: Id, _: Sel, other: Id) -> bool {
    let (mine, theirs) = (set_of(this).clone(), set(other));
    (0..=0xFFFFu32).chain(0x1_0000..0x2_0000).all(|c| !theirs.contains(c) || mine.contains(c))
}

extern "C" fn cs_has_member_in_plane(this: Id, _: Sel, plane: u8) -> bool {
    let start = plane as u32 * 0x1_0000;
    let s = set_of(this).clone();
    (start..start + 0x1_0000).any(|c| s.contains(c))
}

extern "C" fn cs_bitmap(this: Id, _: Sel) -> Id {
    let s = set_of(this).clone();
    let mut bytes = vec![0u8; 8192];
    for code in 0..0x1_0000u32 {
        if s.contains(code) {
            bytes[(code / 8) as usize] |= 1 << (code % 8);
        }
    }
    crate::fw::counted::autorelease(data::make(&bytes))
}

extern "C" fn cs_copy(this: Id, _: Sel, _zone: usize) -> Id {
    make_set(CLASSES.get().unwrap().0, set_of(this).clone())
}

extern "C" fn cs_mutable_copy(this: Id, _: Sel, _zone: usize) -> Id {
    make_set(CLASSES.get().unwrap().1, set_of(this).clone())
}

fn change(this: Id, f: impl FnOnce(Set) -> Set) {
    let current = std::mem::replace(set_of(this), Set::Ranges(Vec::new()));
    *set_of(this) = f(current);
}

extern "C" fn ms_add_string(this: Id, _: Sel, text: Id) {
    let extra = Set::of_text(&string::get(text));
    change(this, |s| Set::Union(vec![s, extra]));
}

extern "C" fn ms_remove_string(this: Id, _: Sel, text: Id) {
    let gone = Set::of_text(&string::get(text));
    change(this, |s| Set::Intersection(vec![s, Set::Not(Box::new(gone))]));
}

extern "C" fn ms_add_range(this: Id, _: Sel, range: string::Range) {
    let extra = range_set(range);
    change(this, |s| Set::Union(vec![s, extra]));
}

extern "C" fn ms_remove_range(this: Id, _: Sel, range: string::Range) {
    let gone = range_set(range);
    change(this, |s| Set::Intersection(vec![s, Set::Not(Box::new(gone))]));
}

extern "C" fn ms_union(this: Id, _: Sel, other: Id) {
    let other = set(other);
    change(this, |s| Set::Union(vec![s, other]));
}

extern "C" fn ms_intersection(this: Id, _: Sel, other: Id) {
    let other = set(other);
    change(this, |s| Set::Intersection(vec![s, other]));
}

extern "C" fn ms_invert(this: Id, _: Sel) {
    change(this, |s| Set::Not(Box::new(s)));
}

// ----- NSString methods with character sets -----

fn units_of(text: Id) -> Vec<u16> {
    string::get(text).encode_utf16().collect()
}

/// Is the character (or surrogate pair) starting at `index` in the set?
fn member_at(s: &Set, units: &[u16], index: usize) -> (bool, usize) {
    let unit = units[index];
    if (0xD800..0xDC00).contains(&unit) && index + 1 < units.len() && (0xDC00..0xE000).contains(&units[index + 1]) {
        let code = 0x1_0000 + (((unit as u32) - 0xD800) << 10) + (units[index + 1] as u32 - 0xDC00);
        return (s.contains(code), 2);
    }
    (s.contains_unit(unit), 1)
}

pub fn trim(units: &[u16], s: &Set) -> Vec<u16> {
    let mut start = 0;
    while start < units.len() {
        let (inside, width) = member_at(s, units, start);
        if !inside {
            break;
        }
        start += width;
    }
    let mut end = units.len();
    while end > start {
        let low_surrogate = end >= 2 && (0xDC00..0xE000).contains(&units[end - 1]) && (0xD800..0xDC00).contains(&units[end - 2]);
        let begin = if low_surrogate { end - 2 } else { end - 1 };
        if !member_at(s, units, begin).0 {
            break;
        }
        end = begin;
    }
    units[start..end].to_vec()
}

extern "C" fn by_trimming(this: Id, _: Sel, characters: Id) -> Id {
    let trimmed = trim(&units_of(this), &set(characters));
    string::make(&String::from_utf16_lossy(&trimmed))
}

const BACKWARDS: usize = 4;

extern "C" fn range_of_character_options(this: Id, _: Sel, characters: Id, options: usize) -> string::Range {
    let units = units_of(this);
    let full = string::Range { location: 0, length: units.len() };
    range_of_character_in(this, 0, characters, options, full)
}

extern "C" fn range_of_character(this: Id, s: Sel, characters: Id) -> string::Range {
    range_of_character_options(this, s, characters, 0)
}

extern "C" fn range_of_character_in(this: Id, _: Sel, characters: Id, options: usize, range: string::Range) -> string::Range {
    let units = units_of(this);
    let s = set(characters);
    let end = (range.location + range.length).min(units.len());
    let start = range.location.min(end);
    let positions: Vec<usize> = if options & BACKWARDS != 0 { (start..end).rev().collect() } else { (start..end).collect() };
    for index in positions {
        if (0xDC00..0xE000).contains(&units[index]) && index > start && (0xD800..0xDC00).contains(&units[index - 1]) {
            continue;
        }
        let (inside, width) = member_at(&s, &units, index);
        if inside {
            return string::Range { location: index, length: width };
        }
    }
    string::Range { location: i64::MAX as usize, length: 0 }
}

extern "C" fn components_separated_by_set(this: Id, _: Sel, characters: Id) -> Id {
    let units = units_of(this);
    let s = set(characters);
    let mut parts = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index < units.len() {
        let (inside, width) = member_at(&s, &units, index);
        if inside {
            parts.push(string::make(&String::from_utf16_lossy(&units[start..index])));
            start = index + width;
        }
        index += width;
    }
    parts.push(string::make(&String::from_utf16_lossy(&units[start..])));
    collections::array_from(&parts)
}

extern "C" fn by_adding_percent_encoding(this: Id, _: Sel, allowed: Id) -> Id {
    let s = set(allowed);
    let mut out = String::new();
    for c in string::get(this).chars() {
        if s.contains(c as u32) {
            out.push(c);
        } else {
            let mut buffer = [0u8; 4];
            for byte in c.encode_utf8(&mut buffer).bytes() {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    string::make(&out)
}

// ----- NSScanner -----

/// Scanning state; `skip` is applied before every scan.
pub struct Scanner {
    pub units: Vec<u16>,
    pub location: usize,
    pub skip: Option<Set>,
    pub case_sensitive: bool,
    pub decimal: u16,
}

impl Scanner {
    pub fn new(text: &str) -> Scanner {
        Scanner {
            units: text.encode_utf16().collect(),
            location: 0,
            skip: Some(Set::Builtin(Builtin::WhitespaceAndNewline)),
            case_sensitive: false,
            decimal: b'.' as u16,
        }
    }

    fn skipped(&self) -> usize {
        let mut at = self.location.min(self.units.len());
        if let Some(skip) = &self.skip {
            while at < self.units.len() && skip.contains_unit(self.units[at]) {
                at += 1;
            }
        }
        at
    }

    pub fn at_end(&self) -> bool {
        self.skipped() >= self.units.len()
    }

    fn digits(&self, mut at: usize, radix: u32) -> usize {
        while at < self.units.len() && char::from_u32(self.units[at] as u32).and_then(|c| c.to_digit(radix)).is_some() {
            at += 1;
        }
        at
    }

    /// A whole number (clamped to the type's range by the caller).
    pub fn scan_integer(&mut self) -> Option<i128> {
        let mut at = self.skipped();
        let negative = match self.units.get(at) {
            Some(&u) if u == b'-' as u16 => {
                at += 1;
                true
            }
            Some(&u) if u == b'+' as u16 => {
                at += 1;
                false
            }
            _ => false,
        };
        let end = self.digits(at, 10);
        if end == at {
            return None;
        }
        let mut value: i128 = 0;
        for &u in &self.units[at..end] {
            value = (value * 10 + (u - b'0' as u16) as i128).min(i128::MAX / 100);
        }
        self.location = end;
        Some(if negative { -value } else { value })
    }

    pub fn scan_hex(&mut self) -> Option<u128> {
        let mut at = self.skipped();
        let zero_x = self.units.get(at) == Some(&(b'0' as u16))
            && matches!(self.units.get(at + 1), Some(&u) if u == b'x' as u16 || u == b'X' as u16)
            && self.digits(at + 2, 16) > at + 2;
        if zero_x {
            at += 2;
        }
        let end = self.digits(at, 16);
        if end == at {
            return None;
        }
        let mut value: u128 = 0;
        for &u in &self.units[at..end] {
            let digit = char::from_u32(u as u32).and_then(|c| c.to_digit(16)).unwrap_or(0) as u128;
            value = (value * 16 + digit).min(u128::MAX / 32);
        }
        self.location = end;
        Some(value)
    }

    pub fn scan_double(&mut self) -> Option<f64> {
        let start = self.skipped();
        let mut at = start;
        let mut text = String::new();
        if let Some(&u) = self.units.get(at) {
            if u == b'-' as u16 || u == b'+' as u16 {
                text.push(u as u8 as char);
                at += 1;
            }
        }
        let integer_end = self.digits(at, 10);
        let mut any_digits = integer_end > at;
        text.extend(self.units[at..integer_end].iter().map(|&u| u as u8 as char));
        at = integer_end;
        if self.units.get(at) == Some(&self.decimal) {
            let fraction_end = self.digits(at + 1, 10);
            if fraction_end > at + 1 || any_digits {
                text.push('.');
                text.extend(self.units[at + 1..fraction_end].iter().map(|&u| u as u8 as char));
                any_digits |= fraction_end > at + 1;
                at = fraction_end;
            }
        }
        if !any_digits {
            return None;
        }
        if matches!(self.units.get(at), Some(&u) if u == b'e' as u16 || u == b'E' as u16) {
            let mut exponent_at = at + 1;
            let mut exponent = String::from("e");
            if let Some(&u) = self.units.get(exponent_at) {
                if u == b'-' as u16 || u == b'+' as u16 {
                    exponent.push(u as u8 as char);
                    exponent_at += 1;
                }
            }
            let end = self.digits(exponent_at, 10);
            if end > exponent_at {
                exponent.extend(self.units[exponent_at..end].iter().map(|&u| u as u8 as char));
                text.push_str(&exponent);
                at = end;
            }
        }
        let value = text.parse::<f64>().ok()?;
        self.location = at;
        Some(value)
    }

    fn matches_at(&self, at: usize, wanted: &[u16]) -> bool {
        if at + wanted.len() > self.units.len() {
            return false;
        }
        let here = &self.units[at..at + wanted.len()];
        if self.case_sensitive {
            here == wanted
        } else {
            String::from_utf16_lossy(here).to_lowercase() == String::from_utf16_lossy(wanted).to_lowercase()
        }
    }

    pub fn scan_string(&mut self, wanted: &str) -> Option<String> {
        let wanted: Vec<u16> = wanted.encode_utf16().collect();
        let at = self.skipped();
        if wanted.is_empty() || !self.matches_at(at, &wanted) {
            return None;
        }
        self.location = at + wanted.len();
        Some(String::from_utf16_lossy(&self.units[at..self.location]))
    }

    pub fn scan_up_to_string(&mut self, stop: &str) -> Option<String> {
        let stop: Vec<u16> = stop.encode_utf16().collect();
        let start = self.skipped();
        let mut at = start;
        while at < self.units.len() && !(!stop.is_empty() && self.matches_at(at, &stop)) {
            at += 1;
        }
        if at == start {
            return None;
        }
        self.location = at;
        Some(String::from_utf16_lossy(&self.units[start..at]))
    }

    pub fn scan_characters(&mut self, s: &Set, inside: bool) -> Option<String> {
        let start = self.skipped();
        let mut at = start;
        while at < self.units.len() {
            let (member, width) = member_at(s, &self.units, at);
            if member != inside {
                break;
            }
            at += width;
        }
        if at == start {
            return None;
        }
        self.location = at;
        Some(String::from_utf16_lossy(&self.units[start..at]))
    }
}

/// NSScanner: { isa, *mut Scanner }.
fn scanner(this: Id) -> &'static mut Scanner {
    unsafe {
        let slot = (this + 8) as *mut *mut Scanner;
        if (*slot).is_null() {
            *slot = Box::into_raw(Box::new(Scanner::new("")));
        }
        &mut **slot
    }
}

/// The skip set object the game gave (kept so the getter returns it).
static SKIP_OBJECTS: std::sync::Mutex<Option<std::collections::HashMap<Id, Id>>> = std::sync::Mutex::new(None);

extern "C" fn sc_init_with_string(this: Id, _: Sel, text: Id) -> Id {
    unsafe { *((this + 8) as *mut *mut Scanner) = Box::into_raw(Box::new(Scanner::new(&string::get(text)))) };
    this
}

extern "C" fn sc_with_string(class: Class, s: Sel, text: Id) -> Id {
    sc_init_with_string(runtime::alloc_instance(class), s, text)
}

extern "C" fn sc_localized_with_string(class: Class, s: Sel, text: Id) -> Id {
    let object = sc_with_string(class, s, text);
    let identifier = string::get(runtime::send(runtime::send(runtime::class_named("NSLocale"), "currentLocale", &[]), "localeIdentifier", &[]));
    let decimal = crate::fw::numbers::locale_info(&identifier).decimal;
    scanner(object).decimal = decimal.encode_utf16().next().unwrap_or(b'.' as u16);
    object
}

extern "C" fn sc_string(this: Id, _: Sel) -> Id {
    string::make(&String::from_utf16_lossy(&scanner(this).units))
}

extern "C" fn sc_location(this: Id, _: Sel) -> usize {
    scanner(this).location
}

extern "C" fn sc_set_location(this: Id, _: Sel, location: usize) {
    let s = scanner(this);
    if location > s.units.len() {
        crate::log!("NSScanner setScanLocation: {location} is past the end ({}); NSRangeException on iOS", s.units.len());
    }
    s.location = location.min(s.units.len());
}

extern "C" fn sc_at_end(this: Id, _: Sel) -> bool {
    scanner(this).at_end()
}

extern "C" fn sc_case_sensitive(this: Id, _: Sel) -> bool {
    scanner(this).case_sensitive
}

extern "C" fn sc_set_case_sensitive(this: Id, _: Sel, value: u8) {
    scanner(this).case_sensitive = value != 0;
}

extern "C" fn sc_skipped(this: Id, _: Sel) -> Id {
    if let Some(&object) = SKIP_OBJECTS.lock().unwrap().as_ref().and_then(|m| m.get(&this)) {
        return object;
    }
    match &scanner(this).skip {
        Some(s) => make_set(CLASSES.get().unwrap().0, s.clone()),
        None => 0,
    }
}

extern "C" fn sc_set_skipped(this: Id, _: Sel, characters: Id) {
    scanner(this).skip = if characters == 0 { None } else { Some(set(characters)) };
    SKIP_OBJECTS.lock().unwrap().get_or_insert_with(Default::default).insert(this, characters);
}

extern "C" fn sc_locale(_this: Id, _: Sel) -> Id {
    0
}

extern "C" fn sc_set_locale(this: Id, _: Sel, locale: Id) {
    if locale == 0 {
        scanner(this).decimal = b'.' as u16;
        return;
    }
    let identifier = string::get(runtime::send(locale, "localeIdentifier", &[]));
    let decimal = crate::fw::numbers::locale_info(&identifier).decimal;
    scanner(this).decimal = decimal.encode_utf16().next().unwrap_or(b'.' as u16);
}

fn store<T>(target: *mut T, value: T) {
    if !target.is_null() {
        unsafe { target.write_unaligned(value) };
    }
}

extern "C" fn sc_scan_int(this: Id, _: Sel, result: *mut i32) -> bool {
    scanner(this).scan_integer().map(|v| store(result, v.clamp(i32::MIN as i128, i32::MAX as i128) as i32)).is_some()
}

extern "C" fn sc_scan_long_long(this: Id, _: Sel, result: *mut i64) -> bool {
    scanner(this).scan_integer().map(|v| store(result, v.clamp(i64::MIN as i128, i64::MAX as i128) as i64)).is_some()
}

extern "C" fn sc_scan_unsigned_long_long(this: Id, _: Sel, result: *mut u64) -> bool {
    scanner(this).scan_integer().map(|v| store(result, v.clamp(0, u64::MAX as i128) as u64)).is_some()
}

extern "C" fn sc_scan_hex_int(this: Id, _: Sel, result: *mut u32) -> bool {
    scanner(this).scan_hex().map(|v| store(result, v.min(u32::MAX as u128) as u32)).is_some()
}

extern "C" fn sc_scan_hex_long_long(this: Id, _: Sel, result: *mut u64) -> bool {
    scanner(this).scan_hex().map(|v| store(result, v.min(u64::MAX as u128) as u64)).is_some()
}

extern "C" fn sc_scan_float(this: Id, _: Sel, result: *mut f32) -> bool {
    scanner(this).scan_double().map(|v| store(result, v as f32)).is_some()
}

extern "C" fn sc_scan_double(this: Id, _: Sel, result: *mut f64) -> bool {
    scanner(this).scan_double().map(|v| store(result, v)).is_some()
}

fn give_text(result: *mut Id, text: Option<String>) -> bool {
    match text {
        Some(text) => {
            store(result, string::make(&text));
            true
        }
        None => false,
    }
}

extern "C" fn sc_scan_string(this: Id, _: Sel, wanted: Id, result: *mut Id) -> bool {
    let text = scanner(this).scan_string(&string::get(wanted));
    give_text(result, text)
}

extern "C" fn sc_scan_up_to_string(this: Id, _: Sel, stop: Id, result: *mut Id) -> bool {
    let text = scanner(this).scan_up_to_string(&string::get(stop));
    give_text(result, text)
}

extern "C" fn sc_scan_characters(this: Id, _: Sel, characters: Id, result: *mut Id) -> bool {
    let text = scanner(this).scan_characters(&set(characters), true);
    give_text(result, text)
}

extern "C" fn sc_scan_up_to_characters(this: Id, _: Sel, characters: Id, result: *mut Id) -> bool {
    let text = scanner(this).scan_characters(&set(characters), false);
    give_text(result, text)
}

extern "C" fn sc_copy(this: Id, _: Sel, _zone: usize) -> Id {
    let s = scanner(this);
    let copy = runtime::alloc_instance(runtime::class_named("NSScanner"));
    let t = scanner(copy);
    t.units = s.units.clone();
    t.location = s.location;
    t.skip = s.skip.clone();
    t.case_sensitive = s.case_sensitive;
    t.decimal = s.decimal;
    copy
}

pub fn register(_: &mut Registry) {
    let base = define_class("NSCharacterSet", "NSObject", 16);
    let mutable = define_class("NSMutableCharacterSet", "NSCharacterSet", 16);
    CLASSES.set((base, mutable)).ok();
    class_methods(
        base,
        &[
            ("whitespaceCharacterSet", cs_whitespace as usize),
            ("whitespaceAndNewlineCharacterSet", cs_whitespace_newline as usize),
            ("newlineCharacterSet", cs_newline as usize),
            ("decimalDigitCharacterSet", cs_digits as usize),
            ("letterCharacterSet", cs_letters as usize),
            ("lowercaseLetterCharacterSet", cs_lowercase as usize),
            ("uppercaseLetterCharacterSet", cs_uppercase as usize),
            ("capitalizedLetterCharacterSet", cs_capitalized as usize),
            ("alphanumericCharacterSet", cs_alphanumeric as usize),
            ("punctuationCharacterSet", cs_punctuation as usize),
            ("symbolCharacterSet", cs_symbols as usize),
            ("controlCharacterSet", cs_control as usize),
            ("nonBaseCharacterSet", cs_non_base as usize),
            ("decomposableCharacterSet", cs_decomposable as usize),
            ("illegalCharacterSet", cs_illegal as usize),
            ("URLQueryAllowedCharacterSet", cs_url_query as usize),
            ("URLPathAllowedCharacterSet", cs_url_path as usize),
            ("URLHostAllowedCharacterSet", cs_url_host as usize),
            ("URLFragmentAllowedCharacterSet", cs_url_fragment as usize),
            ("URLUserAllowedCharacterSet", cs_url_user as usize),
            ("URLPasswordAllowedCharacterSet", cs_url_password as usize),
            ("characterSetWithCharactersInString:", cs_with_string as usize),
            ("characterSetWithRange:", cs_with_range as usize),
            ("characterSetWithBitmapRepresentation:", cs_with_bitmap as usize),
        ],
    );
    methods(
        base,
        &[
            ("init", cs_init as usize),
            ("characterIsMember:", cs_is_member as usize),
            ("longCharacterIsMember:", cs_long_is_member as usize),
            ("invertedSet", cs_inverted as usize),
            ("isSupersetOfSet:", cs_is_superset as usize),
            ("hasMemberInPlane:", cs_has_member_in_plane as usize),
            ("bitmapRepresentation", cs_bitmap as usize),
            ("copyWithZone:", cs_copy as usize),
            ("mutableCopyWithZone:", cs_mutable_copy as usize),
        ],
    );
    methods(
        mutable,
        &[
            ("addCharactersInString:", ms_add_string as usize),
            ("removeCharactersInString:", ms_remove_string as usize),
            ("addCharactersInRange:", ms_add_range as usize),
            ("removeCharactersInRange:", ms_remove_range as usize),
            ("formUnionWithCharacterSet:", ms_union as usize),
            ("formIntersectionWithCharacterSet:", ms_intersection as usize),
            ("invert", ms_invert as usize),
        ],
    );

    let string_class = runtime::class_named("NSString");
    methods(
        string_class,
        &[
            ("stringByTrimmingCharactersInSet:", by_trimming as usize),
            ("rangeOfCharacterFromSet:", range_of_character as usize),
            ("rangeOfCharacterFromSet:options:", range_of_character_options as usize),
            ("rangeOfCharacterFromSet:options:range:", range_of_character_in as usize),
            ("componentsSeparatedByCharactersInSet:", components_separated_by_set as usize),
            ("stringByAddingPercentEncodingWithAllowedCharacters:", by_adding_percent_encoding as usize),
        ],
    );

    let scanner_class = define_class("NSScanner", "NSObject", 16);
    class_methods(
        scanner_class,
        &[
            ("scannerWithString:", sc_with_string as usize),
            ("localizedScannerWithString:", sc_localized_with_string as usize),
        ],
    );
    methods(
        scanner_class,
        &[
            ("initWithString:", sc_init_with_string as usize),
            ("string", sc_string as usize),
            ("scanLocation", sc_location as usize),
            ("setScanLocation:", sc_set_location as usize),
            ("isAtEnd", sc_at_end as usize),
            ("caseSensitive", sc_case_sensitive as usize),
            ("setCaseSensitive:", sc_set_case_sensitive as usize),
            ("charactersToBeSkipped", sc_skipped as usize),
            ("setCharactersToBeSkipped:", sc_set_skipped as usize),
            ("locale", sc_locale as usize),
            ("setLocale:", sc_set_locale as usize),
            ("scanInt:", sc_scan_int as usize),
            ("scanInteger:", sc_scan_long_long as usize),
            ("scanLongLong:", sc_scan_long_long as usize),
            ("scanUnsignedLongLong:", sc_scan_unsigned_long_long as usize),
            ("scanHexInt:", sc_scan_hex_int as usize),
            ("scanHexLongLong:", sc_scan_hex_long_long as usize),
            ("scanFloat:", sc_scan_float as usize),
            ("scanDouble:", sc_scan_double as usize),
            ("scanString:intoString:", sc_scan_string as usize),
            ("scanUpToString:intoString:", sc_scan_up_to_string as usize),
            ("scanCharactersFromSet:intoString:", sc_scan_characters as usize),
            ("scanUpToCharactersFromSet:intoString:", sc_scan_up_to_characters as usize),
            ("copyWithZone:", sc_copy as usize),
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scanner_reads_like_ios() {
        // The receipt-check pattern: skip to a key, read past it, read a number.
        let mut s = Scanner::new("{\"receipt\":{}, \"status\": 21007}");
        assert_eq!(s.scan_up_to_string("\"status\":").as_deref(), Some("{\"receipt\":{}, "));
        assert_eq!(s.scan_string("\"STATUS\":").as_deref(), Some("\"status\":"));
        assert_eq!(s.scan_integer(), Some(21007));
        assert_eq!(s.scan_string("}").as_deref(), Some("}"));
        assert!(s.at_end());
        // Nothing to scan up to: NO, and the place stays.
        let mut s = Scanner::new("key=value");
        assert_eq!(s.scan_up_to_string("key"), None);
        assert_eq!(s.location, 0);
        assert_eq!(s.scan_integer(), None);
        let mut s = Scanner::new("  -12.5e2x 0x1F ff");
        assert_eq!(s.scan_double(), Some(-1250.0));
        assert_eq!(s.scan_string("x").as_deref(), Some("x"));
        assert_eq!(s.scan_hex(), Some(0x1F));
        assert_eq!(s.scan_hex(), Some(0xFF));
        let mut s = Scanner::new("abc123def");
        let digits = Set::Builtin(Builtin::DecimalDigit);
        assert_eq!(s.scan_characters(&digits, false).as_deref(), Some("abc"));
        assert_eq!(s.scan_characters(&digits, true).as_deref(), Some("123"));
    }

    #[test]
    fn character_sets_trim_and_match() {
        let units: Vec<u16> = "<ab cd>".encode_utf16().collect();
        assert_eq!(String::from_utf16_lossy(&trim(&units, &Set::of_text("<>"))), "ab cd");
        let whitespace = Set::Builtin(Builtin::WhitespaceAndNewline);
        let units: Vec<u16> = " \t\nhi\u{3000}".encode_utf16().collect();
        assert_eq!(String::from_utf16_lossy(&trim(&units, &whitespace)), "hi");
        let letters_but_x = Set::Intersection(vec![Set::Builtin(Builtin::Letter), Set::Not(Box::new(Set::of_text("x")))]);
        assert!(letters_but_x.contains('é' as u32) && !letters_but_x.contains('x' as u32) && !letters_but_x.contains('1' as u32));
        assert!(Set::Builtin(Builtin::Punctuation).contains('!' as u32) && !Set::Builtin(Builtin::Punctuation).contains('+' as u32));
    }
}
