//! NSNumberFormatter, with each locale's separators and currency signs.

use crate::fw::dates::call_f64;
use crate::fw::object::{class_methods, define_class, methods};
use crate::fw::{collections, string};
use crate::objc::runtime::{self, Class, Id, Sel};
use crate::symbols::Registry;

// ----- locale facts -----

/// How numbers look in one locale.
#[derive(Clone, Debug, PartialEq)]
pub struct LocaleInfo {
    pub language: String,
    pub decimal: String,
    pub group: String,
    pub currency_code: String,
    /// "1,99 €" rather than "€1.99".
    pub symbol_after: bool,
    /// A space between the sign and the number ("€ 1,99", "1,99 €", "50 %").
    pub symbol_space: bool,
}

const EURO_COUNTRIES: [&str; 20] =
    ["AT", "BE", "CY", "DE", "EE", "ES", "FI", "FR", "GR", "IE", "IT", "LT", "LU", "LV", "MT", "NL", "PT", "SI", "SK", "HR"];

fn country_currency(country: &str) -> &'static str {
    if EURO_COUNTRIES.contains(&country) {
        return "EUR";
    }
    match country {
        "US" | "EC" | "SV" | "PR" => "USD",
        "GB" => "GBP",
        "JP" => "JPY",
        "CN" => "CNY",
        "CA" => "CAD",
        "AU" => "AUD",
        "NZ" => "NZD",
        "HK" => "HKD",
        "SG" => "SGD",
        "IN" => "INR",
        "KR" => "KRW",
        "BR" => "BRL",
        "MX" => "MXN",
        "CH" | "LI" => "CHF",
        "SE" => "SEK",
        "NO" => "NOK",
        "DK" => "DKK",
        "RU" => "RUB",
        "PL" => "PLN",
        "CZ" => "CZK",
        "HU" => "HUF",
        "TR" => "TRY",
        "TW" => "TWD",
        "IL" => "ILS",
        "VN" => "VND",
        "TH" => "THB",
        "ID" => "IDR",
        "MY" => "MYR",
        "PH" => "PHP",
        "ZA" => "ZAR",
        "AR" => "ARS",
        "CL" => "CLP",
        "CO" => "COP",
        "SA" => "SAR",
        "AE" => "AED",
        "UA" => "UAH",
        _ => "XXX",
    }
}

/// English sign and fraction digits for a currency.
fn currency_facts(code: &str) -> (&'static str, usize) {
    match code {
        "USD" => ("$", 2),
        "EUR" => ("€", 2),
        "GBP" => ("£", 2),
        "JPY" => ("¥", 0),
        "CNY" => ("CN¥", 2),
        "CAD" => ("CA$", 2),
        "AUD" => ("A$", 2),
        "NZD" => ("NZ$", 2),
        "HKD" => ("HK$", 2),
        "SGD" => ("SGD", 2),
        "INR" => ("₹", 2),
        "KRW" => ("₩", 0),
        "BRL" => ("R$", 2),
        "MXN" => ("MX$", 2),
        "TWD" => ("NT$", 2),
        "ILS" => ("₪", 2),
        "VND" => ("₫", 0),
        "THB" => ("THB", 2),
        "PHP" => ("₱", 2),
        "CLP" => ("CLP", 0),
        "IDR" => ("IDR", 2),
        "HUF" => ("HUF", 2),
        "XXX" => ("¤", 2),
        _ => ("", 2),
    }
}

/// The sign a locale uses for its own or another currency.
pub fn currency_symbol(info: &LocaleInfo, code: &str, country_code: &str) -> String {
    let local = match (info.language.as_str(), code) {
        ("sv", "SEK") | ("nb", "NOK") | ("nn", "NOK") | ("no", "NOK") => Some("kr"),
        ("da", "DKK") => Some("kr."),
        ("ja", "JPY") | ("zh", "CNY") => Some("￥"),
        ("pl", "PLN") => Some("zł"),
        ("ru", "RUB") => Some("руб."),
        ("cs", "CZK") => Some("Kč"),
        ("hu", "HUF") => Some("Ft"),
        ("tr", "TRY") => Some("₺"),
        ("th", "THB") => Some("฿"),
        ("uk", "UAH") => Some("₴"),
        ("id", "IDR") => Some("Rp"),
        _ => None,
    };
    if let Some(local) = local {
        return local.to_string();
    }
    let (symbol, _) = currency_facts(code);
    if symbol.is_empty() {
        return code.to_string();
    }
    // A country's own dollar is plain "$" there (en_CA: "$", elsewhere "CA$").
    if symbol.ends_with('$') && country_currency(country_code) == code {
        return "$".into();
    }
    symbol.to_string()
}

pub fn currency_digits(code: &str) -> usize {
    currency_facts(code).1
}

/// (language, country, currency keyword) of "de_DE@currency=EUR" or "en-US".
fn split_identifier(identifier: &str) -> (String, String, Option<String>) {
    let (base, keywords) = identifier.split_once('@').unwrap_or((identifier, ""));
    let currency = keywords
        .split(';')
        .find_map(|k| k.strip_prefix("currency="))
        .map(|c| c.to_ascii_uppercase());
    let mut parts = base.split(['_', '-']);
    let language = parts.next().unwrap_or("").to_ascii_lowercase();
    let country = parts.find(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_alphabetic())).unwrap_or("").to_ascii_uppercase();
    (language, country, currency)
}

pub fn locale_info(identifier: &str) -> LocaleInfo {
    let (language, country, currency) = split_identifier(identifier);
    let nbsp = "\u{a0}";
    let (decimal, group, symbol_after, symbol_space) = match language.as_str() {
        "de" if country == "CH" || country == "LI" => (".", "'", false, true),
        "de" | "es" | "it" | "da" | "tr" | "id" | "el" | "ro" | "hr" | "sl" | "sr" | "vi" => (",", ".", true, true),
        "nl" => (",", ".", false, true),
        "pt" if country == "PT" => (",", nbsp, true, true),
        "pt" => (",", ".", false, true),
        "fr" | "ru" | "sv" | "nb" | "nn" | "no" | "fi" | "pl" | "cs" | "sk" | "uk" | "hu" | "bg" | "lt" | "lv" | "et" => {
            (",", nbsp, true, true)
        }
        _ => (".", ",", false, false),
    };
    let currency_code = currency.unwrap_or_else(|| country_currency(&country).to_string());
    LocaleInfo {
        language,
        decimal: decimal.into(),
        group: group.into(),
        currency_code,
        symbol_after,
        symbol_space,
    }
}

fn locale_identifier(locale: Id) -> String {
    let locale = if locale == 0 { runtime::send(runtime::class_named("NSLocale"), "currentLocale", &[]) } else { locale };
    let identifier = string::get(runtime::send(locale, "localeIdentifier", &[]));
    if identifier.is_empty() {
        "en_US".into()
    } else {
        identifier
    }
}

// ----- formatting -----

pub const STYLE_NONE: i64 = 0;
pub const STYLE_DECIMAL: i64 = 1;
pub const STYLE_CURRENCY: i64 = 2;
pub const STYLE_PERCENT: i64 = 3;
pub const STYLE_SCIENTIFIC: i64 = 4;
pub const STYLE_SPELL_OUT: i64 = 5;
pub const STYLE_ORDINAL: i64 = 6;
pub const STYLE_CURRENCY_ISO: i64 = 8;
pub const STYLE_CURRENCY_PLURAL: i64 = 9;
pub const STYLE_CURRENCY_ACCOUNTING: i64 = 10;

/// Everything stringFromNumber: needs, after the formatter's settings and
/// the locale are combined.
#[derive(Clone, Debug)]
pub struct Settings {
    pub style: i64,
    pub min_integer: usize,
    pub max_integer: usize,
    pub min_fraction: usize,
    pub max_fraction: usize,
    pub grouping: bool,
    pub group_size: usize,
    pub decimal: String,
    pub group: String,
    pub currency_symbol: String,
    pub currency_code: String,
    pub symbol_after: bool,
    pub symbol_space: bool,
    pub percent: String,
    pub minus: String,
    pub plus: String,
    /// NSNumberFormatterRoundingMode: ceiling, floor, down, up, half even, half down, half up.
    pub rounding: i64,
    pub multiplier: Option<f64>,
    pub positive_prefix: Option<String>,
    pub positive_suffix: Option<String>,
    pub negative_prefix: Option<String>,
    pub negative_suffix: Option<String>,
    pub zero_symbol: Option<String>,
    pub not_a_number: String,
    pub infinity: String,
}

impl Settings {
    pub fn for_style(style: i64, info: &LocaleInfo, currency_code: &str, symbol: &str) -> Settings {
        let digits = currency_digits(currency_code);
        let currency_like = matches!(style, STYLE_CURRENCY | STYLE_CURRENCY_ISO | STYLE_CURRENCY_PLURAL | STYLE_CURRENCY_ACCOUNTING);
        let (min_fraction, max_fraction, grouping) = match style {
            STYLE_DECIMAL => (0, 3, true),
            STYLE_PERCENT => (0, 0, true),
            STYLE_SCIENTIFIC => (0, 6, false),
            _ if currency_like => (digits, digits, true),
            _ => (0, 0, false),
        };
        Settings {
            style,
            min_integer: 1,
            max_integer: 42,
            min_fraction,
            max_fraction,
            grouping,
            group_size: 3,
            decimal: info.decimal.clone(),
            group: info.group.clone(),
            currency_symbol: symbol.to_string(),
            currency_code: currency_code.to_string(),
            symbol_after: info.symbol_after,
            symbol_space: info.symbol_space,
            percent: "%".into(),
            minus: "-".into(),
            plus: "+".into(),
            rounding: 4,
            multiplier: None,
            positive_prefix: None,
            positive_suffix: None,
            negative_prefix: None,
            negative_suffix: None,
            zero_symbol: None,
            not_a_number: "NaN".into(),
            infinity: "∞".into(),
        }
    }
}

/// A number to format or a number read: whole numbers stay exact integers
/// (a double has only 53 bits, so 2^63-1 would lose digits).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Amount {
    Integer(i128),
    Float(f64),
}

impl Amount {
    fn float(self) -> f64 {
        match self {
            Amount::Integer(i) => i as f64,
            Amount::Float(f) => f,
        }
    }

    fn negative(self) -> bool {
        match self {
            Amount::Integer(i) => i < 0,
            Amount::Float(f) => f < 0.0,
        }
    }

    /// Times a multiplier, staying exact when both are whole.
    fn times(self, multiplier: f64) -> Amount {
        match self {
            Amount::Integer(i) if multiplier.fract() == 0.0 && multiplier.abs() < 1e18 => {
                i.checked_mul(multiplier as i128).map(Amount::Integer).unwrap_or(Amount::Float(i as f64 * multiplier))
            }
            other => Amount::Float(other.float() * multiplier),
        }
    }
}

/// |amount| rounded to `places` fraction digits, as decimal text "123.4500".
fn amount_text(amount: Amount, places: usize, mode: i64) -> String {
    match amount {
        Amount::Integer(i) if places == 0 => i.unsigned_abs().to_string(),
        Amount::Integer(i) => format!("{}.{}", i.unsigned_abs(), "0".repeat(places)),
        Amount::Float(f) => rounded_text(f, places, mode),
    }
}

/// |value| rounded to `places` fraction digits, as decimal text "123.4500".
fn rounded_text(value: f64, places: usize, mode: i64) -> String {
    let value = value.abs();
    if mode == 4 {
        // Rust prints the exact binary value rounded half to even.
        return format!("{value:.places$}");
    }
    let scale = 10f64.powi(places as i32);
    let scaled = value * scale;
    let floor = scaled.floor();
    let fraction = scaled - floor;
    let whole = match mode {
        0 | 3 => scaled.ceil(), // ceiling and up (the value is positive here; sign handled by caller)
        1 | 2 => floor,
        5 => {
            if fraction > 0.5 {
                floor + 1.0
            } else {
                floor
            }
        }
        _ => {
            if fraction >= 0.5 {
                floor + 1.0
            } else {
                floor
            }
        }
    };
    format!("{:.places$}", whole / scale)
}

fn group_digits(digits: &str, separator: &str, size: usize) -> String {
    if size == 0 || digits.len() <= size {
        return digits.to_string();
    }
    let mut out = String::new();
    let first = digits.len() % size;
    for (index, c) in digits.chars().enumerate() {
        if index > 0 && (index + size - first) % size == 0 {
            out.push_str(separator);
        }
        out.push(c);
    }
    out
}

/// The digits of |value|: integer and fraction parts, grouped.
fn number_body(value: Amount, s: &Settings) -> (String, bool) {
    // Ceiling rounds negative numbers toward zero and floor away from it.
    let mode = match (s.rounding, value.negative()) {
        (0, true) => 2,
        (1, true) => 3,
        (mode, _) => mode,
    };
    let text = amount_text(value, s.max_fraction, mode);
    let (integer, fraction) = text.split_once('.').unwrap_or((&text, ""));
    let mut fraction = fraction.to_string();
    while fraction.len() > s.min_fraction && fraction.ends_with('0') {
        fraction.pop();
    }
    let mut integer = integer.trim_start_matches('0').to_string();
    if integer.len() > s.max_integer {
        integer = integer[integer.len() - s.max_integer..].to_string();
    }
    while integer.len() < s.min_integer {
        integer.insert(0, '0');
    }
    let is_zero = integer.chars().all(|c| c == '0') && fraction.chars().all(|c| c == '0');
    let integer = if s.grouping { group_digits(&integer, &s.group, s.group_size) } else { integer };
    let body = if fraction.is_empty() { integer } else { format!("{integer}{}{fraction}", s.decimal) };
    (body, is_zero)
}

const ONES: [&str; 20] = [
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve", "thirteen",
    "fourteen", "fifteen", "sixteen", "seventeen", "eighteen", "nineteen",
];
const TENS: [&str; 10] = ["", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety"];

fn spell_integer(n: u64) -> String {
    if n < 20 {
        return ONES[n as usize].into();
    }
    if n < 100 {
        let tens = TENS[(n / 10) as usize];
        return if n % 10 == 0 { tens.into() } else { format!("{tens}-{}", ONES[(n % 10) as usize]) };
    }
    if n < 1000 {
        let rest = if n % 100 == 0 { String::new() } else { format!(" {}", spell_integer(n % 100)) };
        return format!("{} hundred{rest}", ONES[(n / 100) as usize]);
    }
    for (size, name) in [(1_000_000_000_000_000u64, "quadrillion"), (1_000_000_000_000, "trillion"), (1_000_000_000, "billion"), (1_000_000, "million"), (1000, "thousand")] {
        if n >= size {
            let rest = if n % size == 0 { String::new() } else { format!(" {}", spell_integer(n % size)) };
            return format!("{} {name}{rest}", spell_integer(n / size));
        }
    }
    unreachable!()
}

fn ordinal_suffix(n: i64) -> &'static str {
    match (n.abs() % 10, n.abs() % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    }
}

pub fn format_number(value: f64, s: &Settings) -> String {
    format_amount(Amount::Float(value), s)
}

pub fn format_amount(amount: Amount, s: &Settings) -> String {
    if amount.float().is_nan() {
        return s.not_a_number.clone();
    }
    let amount = amount.times(s.multiplier.unwrap_or(if s.style == STYLE_PERCENT { 100.0 } else { 1.0 }));
    let value = amount.float();
    if value.is_infinite() {
        return if value < 0.0 { format!("{}{}", s.minus, s.infinity) } else { s.infinity.clone() };
    }
    match s.style {
        STYLE_SPELL_OUT => {
            let whole = value.abs().trunc();
            let mut text = match amount {
                Amount::Integer(i) => spell_integer(u64::try_from(i.unsigned_abs()).unwrap_or(u64::MAX)),
                Amount::Float(_) => spell_integer(whole as u64),
            };
            let fraction = rounded_text(value.abs() - whole, 6, 4);
            let digits = fraction.split_once('.').map(|(_, f)| f.trim_end_matches('0')).unwrap_or("");
            if !digits.is_empty() {
                text.push_str(" point");
                for d in digits.chars() {
                    text.push(' ');
                    text.push_str(ONES[d.to_digit(10).unwrap_or(0) as usize]);
                }
            }
            return if value < 0.0 { format!("minus {text}") } else { text };
        }
        STYLE_ORDINAL => {
            let n = match amount {
                Amount::Integer(i) => i,
                Amount::Float(f) => f.round() as i128,
            };
            return format!("{n}{}", ordinal_suffix((n % 100) as i64));
        }
        STYLE_SCIENTIFIC => {
            if value == 0.0 {
                return "0E0".into();
            }
            let exponent = value.abs().log10().floor() as i32;
            let mantissa = value / 10f64.powi(exponent);
            let (body, _) = number_body(Amount::Float(mantissa), s);
            let sign = if value < 0.0 { s.minus.as_str() } else { "" };
            return format!("{sign}{body}E{exponent}");
        }
        _ => {}
    }
    let (body, is_zero) = number_body(amount, s);
    if is_zero {
        if let Some(zero) = &s.zero_symbol {
            return zero.clone();
        }
    }
    let negative = amount.negative() && !is_zero;
    let space = if s.symbol_space { "\u{a0}" } else { "" };
    let decorated = match s.style {
        STYLE_CURRENCY | STYLE_CURRENCY_ACCOUNTING => {
            if s.symbol_after {
                format!("{body}{space}{}", s.currency_symbol)
            } else {
                format!("{}{space}{body}", s.currency_symbol)
            }
        }
        STYLE_CURRENCY_ISO => {
            if s.symbol_after {
                format!("{body}\u{a0}{}", s.currency_code)
            } else {
                format!("{}\u{a0}{body}", s.currency_code)
            }
        }
        STYLE_CURRENCY_PLURAL => format!("{body} {}", s.currency_code),
        STYLE_PERCENT => format!("{body}{space}{}", s.percent),
        _ => body,
    };
    if negative {
        if s.negative_prefix.is_some() || s.negative_suffix.is_some() {
            let (prefix, suffix) = (s.negative_prefix.clone().unwrap_or_default(), s.negative_suffix.clone().unwrap_or_default());
            return format!("{prefix}{}{suffix}", strip_affixes(&decorated, s));
        }
        if s.style == STYLE_CURRENCY_ACCOUNTING {
            return format!("({decorated})");
        }
        return format!("{}{decorated}", s.minus);
    }
    if s.positive_prefix.is_some() || s.positive_suffix.is_some() {
        let (prefix, suffix) = (s.positive_prefix.clone().unwrap_or_default(), s.positive_suffix.clone().unwrap_or_default());
        return format!("{prefix}{}{suffix}", strip_affixes(&decorated, s));
    }
    decorated
}

/// The number alone, for custom prefixes and suffixes.
fn strip_affixes(decorated: &str, s: &Settings) -> String {
    let mut text = decorated.to_string();
    for affix in [s.currency_symbol.as_str(), s.currency_code.as_str(), s.percent.as_str()] {
        if !affix.is_empty() && s.style != STYLE_DECIMAL && s.style != STYLE_NONE {
            text = text.replace(affix, "");
        }
    }
    text.trim_matches(|c: char| c == '\u{a0}' || c == ' ').to_string()
}

/// numberFromString:'s reading; None when the text isn't a number.
pub fn parse_number(text: &str, s: &Settings, lenient: bool) -> Option<f64> {
    parse_amount(text, s, lenient).map(Amount::float)
}

/// Like parse_number; whole numbers come back as exact integers.
pub fn parse_amount(text: &str, s: &Settings, lenient: bool) -> Option<Amount> {
    let mut t = text.trim().replace('\u{202f}', "\u{a0}");
    let mut negative = false;
    if t.starts_with('(') && t.ends_with(')') {
        negative = true;
        t = t[1..t.len() - 1].to_string();
    }
    for affix in [s.currency_symbol.as_str(), s.currency_code.as_str(), s.percent.as_str()] {
        if !affix.is_empty() {
            t = t.replace(affix, "");
        }
    }
    for affix in [&s.positive_prefix, &s.positive_suffix] {
        if let Some(affix) = affix.as_deref().filter(|a| !a.is_empty()) {
            t = t.replace(affix, "");
        }
    }
    let mut t = t.trim_matches(|c: char| c.is_whitespace() || c == '\u{a0}').to_string();
    if let Some(rest) = t.strip_prefix(s.minus.as_str()).or_else(|| t.strip_prefix('-')) {
        negative = !negative;
        t = rest.trim_start().to_string();
    } else if let Some(rest) = t.strip_prefix(s.plus.as_str()) {
        t = rest.to_string();
    }
    if t.is_empty() {
        return None;
    }
    let mut plain = String::new();
    let decimal: Vec<char> = s.decimal.chars().collect();
    let group: Vec<char> = s.group.chars().collect();
    let chars: Vec<char> = t.chars().collect();
    let mut i = 0;
    let mut seen_decimal = false;
    let mut seen_exponent = false;
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_digit() {
            plain.push(c);
        } else if !seen_decimal && !seen_exponent && chars[i..].starts_with(&decimal) {
            plain.push('.');
            seen_decimal = true;
            i += decimal.len();
            continue;
        } else if !seen_decimal && !seen_exponent && !group.is_empty() && chars[i..].starts_with(&group) && (s.grouping || lenient) {
            i += group.len();
            continue;
        } else if (c == 'E' || c == 'e') && !seen_exponent && !plain.is_empty() {
            plain.push('e');
            seen_exponent = true;
            if matches!(chars.get(i + 1), Some('-') | Some('+')) {
                plain.push(chars[i + 1]);
                i += 1;
            }
        } else {
            return None;
        }
        i += 1;
    }
    let divide = s.multiplier.unwrap_or(if s.style == STYLE_PERCENT { 100.0 } else { 1.0 });
    if !seen_decimal && !seen_exponent && divide == 1.0 {
        if let Ok(whole) = plain.parse::<i128>() {
            return Some(Amount::Integer(if negative { -whole } else { whole }));
        }
    }
    let mut value: f64 = plain.parse().ok()?;
    if negative {
        value = -value;
    }
    Some(Amount::Float(value / divide))
}

/// Reads "#,##0.00" style patterns into the settings: digit counts,
/// grouping and the text around the number.
fn apply_pattern(pattern: &str, s: &mut Settings, negative: bool) {
    let pattern = pattern.split(';').next().unwrap_or("");
    let is_number_char = |c: char| matches!(c, '#' | '0' | ',' | '.' | '@');
    let start = pattern.find(is_number_char).unwrap_or(pattern.len());
    let end = pattern.rfind(is_number_char).map(|i| i + 1).unwrap_or(start);
    let prefix = pattern[..start].replace('¤', &s.currency_symbol).replace('\'', "");
    let suffix = pattern[end..].replace('¤', &s.currency_symbol).replace('\'', "");
    if negative {
        s.negative_prefix = Some(prefix);
        s.negative_suffix = Some(suffix);
        return;
    }
    s.positive_prefix = Some(prefix);
    s.positive_suffix = Some(suffix);
    let body = &pattern[start..end];
    let (integer, fraction) = body.split_once('.').unwrap_or((body, ""));
    s.grouping = integer.contains(',');
    if let Some(last) = integer.rfind(',') {
        s.group_size = integer.len() - last - 1;
    }
    s.min_integer = integer.chars().filter(|&c| c == '0').count();
    s.min_fraction = fraction.chars().filter(|&c| c == '0').count();
    s.max_fraction = fraction.chars().filter(|&c| c == '0' || c == '#').count();
}

// ----- NSNumberFormatter: { isa, *mut Formatter } -----

#[derive(Default)]
struct Formatter {
    style: i64,
    behavior: i64,
    locale: Id,
    lenient: bool,
    allows_floats: Option<bool>,
    currency_code: Option<String>,
    currency_symbol: Option<String>,
    min_integer: Option<usize>,
    max_integer: Option<usize>,
    min_fraction: Option<usize>,
    max_fraction: Option<usize>,
    grouping: Option<bool>,
    group_size: Option<usize>,
    decimal: Option<String>,
    group: Option<String>,
    percent: Option<String>,
    minus: Option<String>,
    rounding: Option<i64>,
    multiplier: Option<f64>,
    positive_format: Option<String>,
    negative_format: Option<String>,
    positive_prefix: Option<String>,
    positive_suffix: Option<String>,
    negative_prefix: Option<String>,
    negative_suffix: Option<String>,
    zero_symbol: Option<String>,
    nil_symbol: Option<String>,
    generates_decimals: bool,
}

fn formatter(this: Id) -> &'static mut Formatter {
    unsafe {
        let slot = (this + 8) as *mut *mut Formatter;
        if (*slot).is_null() {
            *slot = Box::into_raw(Box::new(Formatter { behavior: 1040, ..Default::default() }));
        }
        &mut **slot
    }
}

fn settings(f: &Formatter) -> Settings {
    let identifier = locale_identifier(f.locale);
    let info = locale_info(&identifier);
    let country = split_identifier(&identifier).1;
    let code = f.currency_code.clone().unwrap_or_else(|| info.currency_code.clone());
    let symbol = f.currency_symbol.clone().unwrap_or_else(|| currency_symbol(&info, &code, &country));
    let mut s = Settings::for_style(f.style, &info, &code, &symbol);
    if let Some(format) = &f.positive_format {
        apply_pattern(format, &mut s, false);
    }
    if let Some(format) = &f.negative_format {
        apply_pattern(format, &mut s, true);
    }
    if let Some(v) = f.min_integer {
        s.min_integer = v;
    }
    if let Some(v) = f.max_integer {
        s.max_integer = v;
    }
    if let Some(v) = f.min_fraction {
        s.min_fraction = v;
        s.max_fraction = s.max_fraction.max(v);
    }
    if let Some(v) = f.max_fraction {
        s.max_fraction = v;
        s.min_fraction = s.min_fraction.min(v);
    }
    if let Some(v) = f.grouping {
        s.grouping = v;
    }
    if let Some(v) = f.group_size {
        s.group_size = v;
    }
    for (target, value) in [
        (&mut s.decimal, &f.decimal),
        (&mut s.group, &f.group),
        (&mut s.percent, &f.percent),
        (&mut s.minus, &f.minus),
    ] {
        if let Some(value) = value {
            *target = value.clone();
        }
    }
    for (target, value) in [
        (&mut s.positive_prefix, &f.positive_prefix),
        (&mut s.positive_suffix, &f.positive_suffix),
        (&mut s.negative_prefix, &f.negative_prefix),
        (&mut s.negative_suffix, &f.negative_suffix),
        (&mut s.zero_symbol, &f.zero_symbol),
    ] {
        if value.is_some() {
            *target = value.clone();
        }
    }
    if let Some(v) = f.rounding {
        s.rounding = v;
    }
    s.multiplier = f.multiplier;
    s
}

/// The value of an NSNumber or NSDecimalNumber-like object; integers exact.
fn number_value(number: Id) -> Amount {
    if collections::is_number(number) {
        let (integer, float) = (collections::number_as_i64(number), collections::number_as_f64(number));
        if integer as f64 == float { Amount::Integer(integer as i128) } else { Amount::Float(float) }
    } else {
        Amount::Float(call_f64(number, "doubleValue"))
    }
}

extern "C" fn nf_init(this: Id, _: Sel) -> Id {
    formatter(this);
    this
}

extern "C" fn nf_string_from_number(this: Id, _: Sel, number: Id) -> Id {
    if number == 0 {
        return 0;
    }
    string::make(&format_amount(number_value(number), &settings(formatter(this))))
}

extern "C" fn nf_string_for_object(this: Id, s: Sel, object: Id) -> Id {
    if object == 0 {
        return match &formatter(this).nil_symbol {
            Some(text) => string::make(text),
            None => 0,
        };
    }
    if !collections::is_number(object) && !runtime::responds(object, "doubleValue") {
        return 0;
    }
    nf_string_from_number(this, s, object)
}

extern "C" fn nf_number_from_string(this: Id, _: Sel, text: Id) -> Id {
    if text == 0 {
        return 0;
    }
    let f = formatter(this);
    let Some(amount) = parse_amount(&string::get(text), &settings(f), f.lenient) else {
        return 0;
    };
    // allowsFloats only limits reading: text with a fraction is rejected.
    match amount {
        Amount::Integer(i) => match i64::try_from(i) {
            Ok(i) => collections::number_int(i),
            Err(_) => collections::number_float(i as f64),
        },
        Amount::Float(value) if f.allows_floats == Some(false) && value.fract() != 0.0 => 0,
        Amount::Float(value) if value.fract() == 0.0 && value.abs() < 9.0e15 => collections::number_int(value as i64),
        Amount::Float(value) => collections::number_float(value),
    }
}

extern "C" fn nf_get_object_value(this: Id, s: Sel, result: *mut Id, text: Id, error: *mut Id) -> bool {
    let number = nf_number_from_string(this, s, text);
    if number == 0 {
        if !error.is_null() {
            unsafe { *error = string::make("Invalid number") };
        }
        return false;
    }
    if !result.is_null() {
        unsafe { *result = number };
    }
    true
}

macro_rules! int_property {
    ($get:ident, $set:ident, $field:ident, $default:expr) => {
        extern "C" fn $get(this: Id, _: Sel) -> usize {
            let f = formatter(this);
            f.$field.map(|v| v as usize).unwrap_or_else(|| $default(f))
        }
        extern "C" fn $set(this: Id, _: Sel, value: usize) {
            formatter(this).$field = Some(value as _);
        }
    };
}

macro_rules! text_property {
    ($get:ident, $set:ident, $field:ident, $default:expr) => {
        extern "C" fn $get(this: Id, _: Sel) -> Id {
            let f = formatter(this);
            match &f.$field {
                Some(text) => string::make(text),
                None => string::make(&$default(f)),
            }
        }
        extern "C" fn $set(this: Id, _: Sel, value: Id) {
            formatter(this).$field = if value == 0 { None } else { Some(string::get(value)) };
        }
    };
}

int_property!(nf_min_integer, nf_set_min_integer, min_integer, |f: &Formatter| settings(f).min_integer);
int_property!(nf_max_integer, nf_set_max_integer, max_integer, |f: &Formatter| settings(f).max_integer);
int_property!(nf_min_fraction, nf_set_min_fraction, min_fraction, |f: &Formatter| settings(f).min_fraction);
int_property!(nf_max_fraction, nf_set_max_fraction, max_fraction, |f: &Formatter| settings(f).max_fraction);
int_property!(nf_group_size, nf_set_group_size, group_size, |f: &Formatter| settings(f).group_size);
int_property!(nf_rounding, nf_set_rounding, rounding, |f: &Formatter| settings(f).rounding as usize);
text_property!(nf_currency_code, nf_set_currency_code, currency_code, |f: &Formatter| settings(f).currency_code);
text_property!(nf_currency_symbol, nf_set_currency_symbol, currency_symbol, |f: &Formatter| settings(f).currency_symbol);
text_property!(nf_decimal, nf_set_decimal, decimal, |f: &Formatter| settings(f).decimal);
text_property!(nf_group, nf_set_group, group, |f: &Formatter| settings(f).group);
text_property!(nf_percent, nf_set_percent, percent, |f: &Formatter| settings(f).percent);
text_property!(nf_minus, nf_set_minus, minus, |f: &Formatter| settings(f).minus);
text_property!(nf_positive_format, nf_set_positive_format, positive_format, |_: &Formatter| String::from("#,##0.###"));
text_property!(nf_negative_format, nf_set_negative_format, negative_format, |_: &Formatter| String::from("-#,##0.###"));
text_property!(nf_positive_prefix, nf_set_positive_prefix, positive_prefix, |_: &Formatter| String::new());
text_property!(nf_positive_suffix, nf_set_positive_suffix, positive_suffix, |_: &Formatter| String::new());
text_property!(nf_negative_prefix, nf_set_negative_prefix, negative_prefix, |f: &Formatter| settings(f).minus);
text_property!(nf_negative_suffix, nf_set_negative_suffix, negative_suffix, |_: &Formatter| String::new());
text_property!(nf_zero_symbol, nf_set_zero_symbol, zero_symbol, |_: &Formatter| String::new());
text_property!(nf_nil_symbol, nf_set_nil_symbol, nil_symbol, |_: &Formatter| String::new());

extern "C" fn nf_style(this: Id, _: Sel) -> usize {
    formatter(this).style as usize
}

extern "C" fn nf_set_style(this: Id, _: Sel, style: usize) {
    let f = formatter(this);
    f.style = style as i64;
    // A new style brings its own digits, as on iOS.
    f.min_fraction = None;
    f.max_fraction = None;
    f.grouping = None;
    if !matches!(f.style, 0..=6 | 8..=10) {
        crate::log!("NSNumberFormatter style {style}: not known; numbers shown plain");
        f.style = STYLE_NONE;
    }
}

extern "C" fn nf_behavior(this: Id, _: Sel) -> usize {
    formatter(this).behavior as usize
}

extern "C" fn nf_set_behavior(this: Id, _: Sel, value: usize) {
    formatter(this).behavior = value as i64;
}

extern "C" fn nf_locale(this: Id, _: Sel) -> Id {
    match formatter(this).locale {
        0 => runtime::send(runtime::class_named("NSLocale"), "currentLocale", &[]),
        locale => locale,
    }
}

extern "C" fn nf_set_locale(this: Id, _: Sel, locale: Id) {
    formatter(this).locale = locale;
}

extern "C" fn nf_grouping(this: Id, _: Sel) -> bool {
    let f = formatter(this);
    f.grouping.unwrap_or_else(|| settings(f).grouping)
}

extern "C" fn nf_set_grouping(this: Id, _: Sel, value: u8) {
    formatter(this).grouping = Some(value != 0);
}

extern "C" fn nf_lenient(this: Id, _: Sel) -> bool {
    formatter(this).lenient
}

extern "C" fn nf_set_lenient(this: Id, _: Sel, value: u8) {
    formatter(this).lenient = value != 0;
}

extern "C" fn nf_allows_floats(this: Id, _: Sel) -> bool {
    formatter(this).allows_floats.unwrap_or(true)
}

extern "C" fn nf_set_allows_floats(this: Id, _: Sel, value: u8) {
    formatter(this).allows_floats = Some(value != 0);
}

extern "C" fn nf_generates_decimals(this: Id, _: Sel) -> bool {
    formatter(this).generates_decimals
}

extern "C" fn nf_set_generates_decimals(this: Id, _: Sel, value: u8) {
    formatter(this).generates_decimals = value != 0;
}

extern "C" fn nf_multiplier(this: Id, _: Sel) -> Id {
    match formatter(this).multiplier {
        Some(v) => collections::number_float(v),
        None => 0,
    }
}

extern "C" fn nf_set_multiplier(this: Id, _: Sel, value: Id) {
    formatter(this).multiplier = if value == 0 { None } else { Some(number_value(value).float()) };
}

extern "C" fn nf_localized_string(class: Class, _: Sel, number: Id, style: usize) -> Id {
    let object = runtime::alloc_instance(class);
    nf_set_style(object, 0, style);
    nf_string_from_number(object, 0, number)
}

extern "C" fn nf_set_default_behavior(_: Class, _: Sel, _behavior: usize) {}

pub fn register(_: &mut Registry) {
    define_class("NSFormatter", "NSObject", 8);
    let class = define_class("NSNumberFormatter", "NSFormatter", 16);
    class_methods(
        class,
        &[
            ("localizedStringFromNumber:numberStyle:", nf_localized_string as usize),
            ("setDefaultFormatterBehavior:", nf_set_default_behavior as usize),
        ],
    );
    methods(
        class,
        &[
            ("init", nf_init as usize),
            ("stringFromNumber:", nf_string_from_number as usize),
            ("stringForObjectValue:", nf_string_for_object as usize),
            ("numberFromString:", nf_number_from_string as usize),
            ("getObjectValue:forString:errorDescription:", nf_get_object_value as usize),
            ("numberStyle", nf_style as usize),
            ("setNumberStyle:", nf_set_style as usize),
            ("formatterBehavior", nf_behavior as usize),
            ("setFormatterBehavior:", nf_set_behavior as usize),
            ("locale", nf_locale as usize),
            ("setLocale:", nf_set_locale as usize),
            ("currencyCode", nf_currency_code as usize),
            ("setCurrencyCode:", nf_set_currency_code as usize),
            ("currencySymbol", nf_currency_symbol as usize),
            ("setCurrencySymbol:", nf_set_currency_symbol as usize),
            ("internationalCurrencySymbol", nf_currency_code as usize),
            ("setInternationalCurrencySymbol:", nf_set_currency_code as usize),
            ("minimumIntegerDigits", nf_min_integer as usize),
            ("setMinimumIntegerDigits:", nf_set_min_integer as usize),
            ("maximumIntegerDigits", nf_max_integer as usize),
            ("setMaximumIntegerDigits:", nf_set_max_integer as usize),
            ("minimumFractionDigits", nf_min_fraction as usize),
            ("setMinimumFractionDigits:", nf_set_min_fraction as usize),
            ("maximumFractionDigits", nf_max_fraction as usize),
            ("setMaximumFractionDigits:", nf_set_max_fraction as usize),
            ("usesGroupingSeparator", nf_grouping as usize),
            ("setUsesGroupingSeparator:", nf_set_grouping as usize),
            ("groupingSize", nf_group_size as usize),
            ("setGroupingSize:", nf_set_group_size as usize),
            ("groupingSeparator", nf_group as usize),
            ("setGroupingSeparator:", nf_set_group as usize),
            ("currencyGroupingSeparator", nf_group as usize),
            ("setCurrencyGroupingSeparator:", nf_set_group as usize),
            ("decimalSeparator", nf_decimal as usize),
            ("setDecimalSeparator:", nf_set_decimal as usize),
            ("currencyDecimalSeparator", nf_decimal as usize),
            ("setCurrencyDecimalSeparator:", nf_set_decimal as usize),
            ("percentSymbol", nf_percent as usize),
            ("setPercentSymbol:", nf_set_percent as usize),
            ("minusSign", nf_minus as usize),
            ("setMinusSign:", nf_set_minus as usize),
            ("roundingMode", nf_rounding as usize),
            ("setRoundingMode:", nf_set_rounding as usize),
            ("multiplier", nf_multiplier as usize),
            ("setMultiplier:", nf_set_multiplier as usize),
            ("positiveFormat", nf_positive_format as usize),
            ("setPositiveFormat:", nf_set_positive_format as usize),
            ("negativeFormat", nf_negative_format as usize),
            ("setNegativeFormat:", nf_set_negative_format as usize),
            ("positivePrefix", nf_positive_prefix as usize),
            ("setPositivePrefix:", nf_set_positive_prefix as usize),
            ("positiveSuffix", nf_positive_suffix as usize),
            ("setPositiveSuffix:", nf_set_positive_suffix as usize),
            ("negativePrefix", nf_negative_prefix as usize),
            ("setNegativePrefix:", nf_set_negative_prefix as usize),
            ("negativeSuffix", nf_negative_suffix as usize),
            ("setNegativeSuffix:", nf_set_negative_suffix as usize),
            ("zeroSymbol", nf_zero_symbol as usize),
            ("setZeroSymbol:", nf_set_zero_symbol as usize),
            ("nilSymbol", nf_nil_symbol as usize),
            ("setNilSymbol:", nf_set_nil_symbol as usize),
            ("isLenient", nf_lenient as usize),
            ("setLenient:", nf_set_lenient as usize),
            ("allowsFloats", nf_allows_floats as usize),
            ("setAllowsFloats:", nf_set_allows_floats as usize),
            ("generatesDecimalNumbers", nf_generates_decimals as usize),
            ("setGeneratesDecimalNumbers:", nf_set_generates_decimals as usize),
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_for(style: i64, identifier: &str) -> Settings {
        let info = locale_info(identifier);
        let country = split_identifier(identifier).1;
        let code = info.currency_code.clone();
        let symbol = currency_symbol(&info, &code, &country);
        Settings::for_style(style, &info, &code, &symbol)
    }

    #[test]
    fn numbers_format_like_ios() {
        let us = |style| settings_for(style, "en_US");
        assert_eq!(format_number(1234.5, &us(STYLE_CURRENCY)), "$1,234.50");
        assert_eq!(format_number(-0.99, &us(STYLE_CURRENCY)), "-$0.99");
        assert_eq!(format_number(1234567.891, &us(STYLE_DECIMAL)), "1,234,567.891");
        assert_eq!(format_number(2.5, &us(STYLE_NONE)), "2");
        assert_eq!(format_number(0.256, &us(STYLE_PERCENT)), "26%");
        assert_eq!(format_number(21.0, &us(STYLE_ORDINAL)), "21st");
        assert_eq!(format_number(1234.0, &us(STYLE_SPELL_OUT)), "one thousand two hundred thirty-four");
        assert_eq!(format_number(1.99, &settings_for(STYLE_CURRENCY, "de_DE")), "1,99\u{a0}€");
        assert_eq!(format_number(1200.0, &settings_for(STYLE_CURRENCY, "ja_JP")), "￥1,200");
        assert_eq!(format_number(5.0, &settings_for(STYLE_CURRENCY, "en_CA@currency=CAD")), "$5.00");
        assert_eq!(format_number(5.0, &settings_for(STYLE_CURRENCY, "en_US@currency=CAD")), "CA$5.00");
        // Integers keep every digit.
        assert_eq!(format_amount(Amount::Integer(i64::MAX as i128), &us(STYLE_DECIMAL)), "9,223,372,036,854,775,807");
        assert_eq!(format_amount(Amount::Integer(-(1 << 53) - 1), &us(STYLE_NONE)), "-9007199254740993");
        let mut pattern = us(STYLE_DECIMAL);
        apply_pattern("#,##0.00", &mut pattern, false);
        assert_eq!(format_number(1234.0, &pattern), "1,234.00");
    }

    #[test]
    fn numbers_parse_back() {
        assert_eq!(parse_number("$1,234.50", &settings_for(STYLE_CURRENCY, "en_US"), false), Some(1234.5));
        assert_eq!(parse_number("1.234,5", &settings_for(STYLE_DECIMAL, "de_DE"), false), Some(1234.5));
        assert_eq!(parse_number("-42", &settings_for(STYLE_DECIMAL, "en_US"), false), Some(-42.0));
        assert_eq!(parse_number("50%", &settings_for(STYLE_PERCENT, "en_US"), false), Some(0.5));
        assert_eq!(parse_number("12abc", &settings_for(STYLE_DECIMAL, "en_US"), false), None);
        assert_eq!(parse_amount("9,007,199,254,740,993", &settings_for(STYLE_DECIMAL, "en_US"), false), Some(Amount::Integer((1 << 53) + 1)));
    }
}
