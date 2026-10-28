//! Property lists (Apple's settings file format), XML and binary, read into
//! our NSDictionary/NSArray/NSString/NSNumber objects and written back.

use crate::fw::{collections, string};
use crate::objc::runtime::{Class, Id, Sel};

#[derive(Debug, Clone)]
pub enum Value {
    Dict(Vec<(String, Value)>),
    Array(Vec<Value>),
    String(String),
    Integer(i64),
    Real(f64),
    Bool(bool),
    Data(Vec<u8>),
    Date(f64),
    /// An object number inside a keyed archive (binary lists only; XML
    /// ones write it as a dictionary with the key "CF$UID").
    Uid(u64),
}

impl Value {
    /// The object number this value stands for in a keyed archive.
    pub fn as_uid(&self) -> Option<u64> {
        match self {
            Value::Uid(n) => Some(*n),
            Value::Dict(entries) if entries.len() == 1 && entries[0].0 == "CF$UID" => match entries[0].1 {
                Value::Integer(n) if n >= 0 => Some(n as u64),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Dict(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

/// A property list: XML, binary, or a real old-style `{..}` or `(..)`
/// document (a `.strings` table without its braces is not a list: see
/// `parse_strings`).
pub fn parse(bytes: &[u8]) -> Option<Value> {
    parse_any(bytes, false)
}

/// A `.strings` table, whose old-style text may leave out the braces:
/// `"key";` alone means the key is its own value, and an empty file is an
/// empty table.
pub fn parse_strings(bytes: &[u8]) -> Option<Value> {
    parse_any(bytes, true)
}

fn parse_any(bytes: &[u8], strings: bool) -> Option<Value> {
    if bytes.starts_with(b"bplist00") {
        return parse_binary(bytes);
    }
    let text = decode_text(bytes);
    let head = text.trim_start();
    if head.starts_with("<?xml") || head.starts_with("<!DOCTYPE") || head.starts_with("<plist") {
        return parse_xml(&text);
    }
    if strings || head.starts_with('{') || head.starts_with('(') {
        parse_text(&text)
    } else {
        None
    }
}

/// Text in a list file: UTF-16 with a byte order mark, else UTF-8.
fn decode_text(bytes: &[u8]) -> String {
    let units = |big: bool| bytes[2..].chunks_exact(2).map(move |c| if big { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) });
    match bytes {
        [0xFF, 0xFE, ..] => char::decode_utf16(units(false)).map(|c| c.unwrap_or('\u{FFFD}')).collect(),
        [0xFE, 0xFF, ..] => char::decode_utf16(units(true)).map(|c| c.unwrap_or('\u{FFFD}')).collect(),
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

// ----- old-style text (.strings files and OpenStep lists) -----

/// An OpenStep text list: `{ key = value; }`, `( a, b )`, quoted or bare
/// strings and `<hex>` data. A .strings file is a dictionary without the
/// braces, where `"key";` alone means the key is its own value.
fn parse_text(text: &str) -> Option<Value> {
    let mut reader = Text { chars: text.chars().collect(), at: 0 };
    reader.skip_space();
    if reader.peek().is_none() {
        return Some(Value::Dict(Vec::new()));
    }
    if reader.peek() == Some('{') || reader.peek() == Some('(') || reader.peek() == Some('<') {
        let value = reader.value()?;
        reader.skip_space();
        if reader.peek().is_none() {
            return Some(value);
        }
        reader.at = 0;
    }
    let entries = reader.entries(None)?;
    Some(Value::Dict(entries))
}

struct Text {
    chars: Vec<char>,
    at: usize,
}

impl Text {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    /// Skips white space and `/* */` and `//` comments.
    fn skip_space(&mut self) {
        loop {
            match (self.peek(), self.chars.get(self.at + 1)) {
                (Some(c), _) if c.is_whitespace() => self.at += 1,
                (Some('/'), Some('*')) => {
                    self.at += 2;
                    while self.at < self.chars.len() && !(self.chars[self.at] == '*' && self.chars.get(self.at + 1) == Some(&'/')) {
                        self.at += 1;
                    }
                    self.at = (self.at + 2).min(self.chars.len());
                }
                (Some('/'), Some('/')) => {
                    while self.peek().is_some_and(|c| c != '\n') {
                        self.at += 1;
                    }
                }
                _ => return,
            }
        }
    }

    /// `key = value;` pairs up to `close` (or the end of the text).
    fn entries(&mut self, close: Option<char>) -> Option<Vec<(String, Value)>> {
        let mut entries = Vec::new();
        loop {
            self.skip_space();
            match self.peek() {
                None if close.is_none() => return Some(entries),
                c if c == close => {
                    self.at += 1;
                    return Some(entries);
                }
                None => return None,
                _ => {}
            }
            let key = self.string()?;
            self.skip_space();
            let value = match self.peek()? {
                ';' => Value::String(key.clone()),
                '=' => {
                    self.at += 1;
                    let value = self.value()?;
                    self.skip_space();
                    value
                }
                _ => return None,
            };
            if self.peek()? != ';' {
                return None;
            }
            self.at += 1;
            entries.push((key, value));
        }
    }

    fn value(&mut self) -> Option<Value> {
        self.skip_space();
        match self.peek()? {
            '{' => {
                self.at += 1;
                self.entries(Some('}')).map(Value::Dict)
            }
            '(' => {
                self.at += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_space();
                    if self.peek()? == ')' {
                        self.at += 1;
                        return Some(Value::Array(items));
                    }
                    items.push(self.value()?);
                    self.skip_space();
                    match self.peek()? {
                        ',' => self.at += 1,
                        ')' => {}
                        _ => return None,
                    }
                }
            }
            '<' => {
                self.at += 1;
                let mut hex = String::new();
                loop {
                    match self.peek()? {
                        '>' => break,
                        c if c.is_ascii_hexdigit() => hex.push(c),
                        c if c.is_whitespace() => {}
                        _ => return None,
                    }
                    self.at += 1;
                }
                self.at += 1;
                if hex.len() % 2 != 0 {
                    return None;
                }
                (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok()).collect::<Option<Vec<u8>>>().map(Value::Data)
            }
            _ => self.string().map(Value::String),
        }
    }

    /// A quoted string with C escapes, or a bare word.
    fn string(&mut self) -> Option<String> {
        let quote = self.peek()?;
        if quote != '"' && quote != '\'' {
            let start = self.at;
            while self.peek().is_some_and(|c| c.is_alphanumeric() || "_$+/:.-".contains(c)) {
                self.at += 1;
            }
            return (self.at > start).then(|| self.chars[start..self.at].iter().collect());
        }
        self.at += 1;
        let mut out = String::new();
        loop {
            let c = self.peek()?;
            self.at += 1;
            if c == quote {
                return Some(out);
            }
            if c != '\\' {
                out.push(c);
                continue;
            }
            let e = self.peek()?;
            self.at += 1;
            match e {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'a' => out.push('\u{7}'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                'v' => out.push('\u{b}'),
                'U' | 'u' => {
                    let digits: String = self.chars[self.at..].iter().take(4).take_while(|c| c.is_ascii_hexdigit()).collect();
                    self.at += digits.len();
                    let unit = u32::from_str_radix(&digits, 16).ok()?;
                    out.push(char::from_u32(unit).unwrap_or('\u{FFFD}'));
                }
                '0'..='7' => {
                    let mut code = e.to_digit(8)?;
                    for _ in 0..2 {
                        match self.peek().and_then(|c| c.to_digit(8)) {
                            Some(d) => {
                                code = code * 8 + d;
                                self.at += 1;
                            }
                            None => break,
                        }
                    }
                    out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                }
                other => out.push(other),
            }
        }
    }
}

// ----- XML -----

struct Xml<'a> {
    text: &'a str,
    at: usize,
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

impl<'a> Xml<'a> {
    /// Next tag: (name, is_closing, is_self_closing).
    fn tag(&mut self) -> Option<(String, bool, bool)> {
        loop {
            let start = self.at + self.text[self.at..].find('<')?;
            let end = start + self.text[start..].find('>')?;
            self.at = end + 1;
            let inner = &self.text[start + 1..end];
            if inner.starts_with('?') || inner.starts_with('!') {
                continue;
            }
            let closing = inner.starts_with('/');
            let self_closing = inner.ends_with('/');
            let name = inner.trim_start_matches('/').trim_end_matches('/').split_whitespace().next()?.to_string();
            return Some((name, closing, self_closing));
        }
    }

    fn text_until_close(&mut self, name: &str) -> Option<String> {
        let close = format!("</{name}>");
        let end = self.at + self.text[self.at..].find(&close)?;
        let value = unescape(&self.text[self.at..end]);
        self.at = end + close.len();
        Some(value)
    }

    fn value_from(&mut self, name: &str, self_closing: bool) -> Option<Value> {
        Some(match name {
            "dict" => {
                let mut entries = Vec::new();
                if self_closing {
                    return Some(Value::Dict(entries));
                }
                loop {
                    let (tag, closing, _) = self.tag()?;
                    if closing && tag == "dict" {
                        break;
                    }
                    if tag != "key" {
                        return None;
                    }
                    let key = self.text_until_close("key")?;
                    let (tag, _, self_closing) = self.tag()?;
                    let value = self.value_from(&tag, self_closing)?;
                    entries.push((key, value));
                }
                Value::Dict(entries)
            }
            "array" => {
                let mut items = Vec::new();
                if self_closing {
                    return Some(Value::Array(items));
                }
                loop {
                    let (tag, closing, self_closing) = self.tag()?;
                    if closing && tag == "array" {
                        break;
                    }
                    items.push(self.value_from(&tag, self_closing)?);
                }
                Value::Array(items)
            }
            "string" => Value::String(if self_closing { String::new() } else { self.text_until_close("string")? }),
            "integer" => Value::Integer(self.text_until_close("integer")?.trim().parse().ok()?),
            "real" => Value::Real(self.text_until_close("real")?.trim().parse().ok()?),
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            "data" => Value::Data(base64_decode(&self.text_until_close("data")?)),
            "date" => Value::Date(date_seconds(&self.text_until_close("date")?).unwrap_or(0.0)),
            _ => return None,
        })
    }
}

fn parse_xml(text: &str) -> Option<Value> {
    let mut xml = Xml { text, at: 0 };
    loop {
        let (name, closing, self_closing) = xml.tag()?;
        if name == "plist" && !closing {
            let (name, _, self_closing) = xml.tag()?;
            return xml.value_from(&name, self_closing);
        }
        if !closing && name != "plist" {
            return xml.value_from(&name, self_closing);
        }
    }
}

fn base64_decode(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0;
    for c in text.bytes() {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => continue,
        };
        buffer = (buffer << 6) | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    out
}

pub fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

// ----- binary -----

fn parse_binary(bytes: &[u8]) -> Option<Value> {
    let trailer = bytes.get(bytes.len().checked_sub(32)?..)?;
    let offset_size = trailer[6] as usize;
    let ref_size = trailer[7] as usize;
    let count = u64::from_be_bytes(trailer[8..16].try_into().ok()?) as usize;
    let top = u64::from_be_bytes(trailer[16..24].try_into().ok()?) as usize;
    let table = u64::from_be_bytes(trailer[24..32].try_into().ok()?) as usize;
    let read_n = |at: usize, n: usize| -> Option<usize> {
        let slice = bytes.get(at..at + n)?;
        Some(slice.iter().fold(0usize, |acc, &b| (acc << 8) | b as usize))
    };
    let offsets: Vec<usize> = (0..count).map(|i| read_n(table + i * offset_size, offset_size)).collect::<Option<_>>()?;

    fn object(bytes: &[u8], offsets: &[usize], ref_size: usize, index: usize, depth: usize) -> Option<Value> {
        if depth > 64 {
            return None;
        }
        let read_n = |at: usize, n: usize| -> Option<usize> {
            let slice = bytes.get(at..at + n)?;
            Some(slice.iter().fold(0usize, |acc, &b| (acc << 8) | b as usize))
        };
        let at = *offsets.get(index)?;
        let marker = *bytes.get(at)?;
        let (kind, info) = (marker >> 4, (marker & 0x0F) as usize);
        let length = |at: usize| -> Option<(usize, usize)> {
            if info != 0x0F {
                return Some((info, at + 1));
            }
            let int_marker = *bytes.get(at + 1)?;
            let n = 1usize << (int_marker & 0x0F);
            Some((read_n(at + 2, n)?, at + 2 + n))
        };
        Some(match kind {
            0x0 => match info {
                8 => Value::Bool(false),
                9 => Value::Bool(true),
                _ => Value::Integer(0),
            },
            0x1 => {
                let n = 1usize << info;
                let raw = read_n(at + 1, n)? as u64;
                Value::Integer(if n == 8 { raw as i64 } else { raw as i64 })
            }
            0x2 => {
                let n = 1usize << info;
                let raw = bytes.get(at + 1..at + 1 + n)?;
                Value::Real(if n == 4 {
                    f32::from_be_bytes(raw.try_into().ok()?) as f64
                } else {
                    f64::from_be_bytes(raw.try_into().ok()?)
                })
            }
            0x8 => Value::Uid(read_n(at + 1, info + 1)? as u64),
            0x3 => Value::Date(f64::from_be_bytes(bytes.get(at + 1..at + 9)?.try_into().ok()?)),
            0x4 => {
                let (n, start) = length(at)?;
                Value::Data(bytes.get(start..start + n)?.to_vec())
            }
            0x5 => {
                let (n, start) = length(at)?;
                Value::String(String::from_utf8_lossy(bytes.get(start..start + n)?).into_owned())
            }
            0x6 => {
                let (n, start) = length(at)?;
                let raw = bytes.get(start..start + n * 2)?;
                let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
                Value::String(String::from_utf16_lossy(&units))
            }
            0xA => {
                let (n, start) = length(at)?;
                let mut items = Vec::with_capacity(n);
                for i in 0..n {
                    let child = read_n(start + i * ref_size, ref_size)?;
                    items.push(object(bytes, offsets, ref_size, child, depth + 1)?);
                }
                Value::Array(items)
            }
            0xD => {
                let (n, start) = length(at)?;
                let mut entries = Vec::with_capacity(n);
                for i in 0..n {
                    let key = read_n(start + i * ref_size, ref_size)?;
                    let value = read_n(start + (n + i) * ref_size, ref_size)?;
                    let key = match object(bytes, offsets, ref_size, key, depth + 1)? {
                        Value::String(s) => s,
                        _ => return None,
                    };
                    entries.push((key, object(bytes, offsets, ref_size, value, depth + 1)?));
                }
                Value::Dict(entries)
            }
            _ => return None,
        })
    }
    object(bytes, &offsets, ref_size, top, 0)
}

// ----- objects -----

/// The list as objects, its dictionaries and arrays mutable.
pub fn to_object(value: &Value) -> Id {
    to_objects(value, true)
}

/// The list as objects, its dictionaries and arrays immutable (what
/// NSUserDefaults keeps and hands back).
pub fn to_immutable_object(value: &Value) -> Id {
    to_objects(value, false)
}

fn to_objects(value: &Value, mutable: bool) -> Id {
    let made = |object: Id| if mutable { crate::objc::runtime::send(object, "mutableCopy", &[]) } else { object };
    match value {
        Value::Dict(entries) => {
            let pairs: Vec<(Id, Id)> = entries.iter().map(|(k, v)| (string::make(k), to_objects(v, mutable))).collect();
            made(collections::dictionary_from(&pairs))
        }
        Value::Array(items) => {
            let list: Vec<Id> = items.iter().map(|item| to_objects(item, mutable)).collect();
            made(collections::array_from(&list))
        }
        Value::String(s) => string::make(s),
        Value::Integer(n) => collections::number_int(*n),
        Value::Real(f) => collections::number_float(*f),
        Value::Bool(b) => collections::number_bool(*b),
        Value::Data(bytes) => crate::fw::data::make(bytes),
        Value::Date(seconds) => crate::fw::misc::date(*seconds),
        Value::Uid(n) => collections::number_int(*n as i64),
    }
}

/// Seconds since 2001-01-01 as an XML list's date text (whole seconds, UTC).
fn date_text(seconds: f64) -> String {
    let total = (seconds.floor() as i64).saturating_add(978_307_200);
    let (days, rest) = (total.div_euclid(86_400), total.rem_euclid(86_400));
    // Days since 1970-01-01 to a calendar date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rest / 3600, rest / 60 % 60, rest % 60)
}

/// An XML list's date text ("2026-10-29T20:53:20Z") as seconds since 2001-01-01.
fn date_seconds(text: &str) -> Option<f64> {
    let text = text.trim().trim_end_matches('Z');
    let (date, time) = text.split_once('T').unwrap_or((text, "00:00:00"));
    let mut date_parts = date.splitn(3, '-').map(|p| p.parse::<i64>());
    let (year, month, day) = (date_parts.next()?.ok()?, date_parts.next().unwrap_or(Ok(1)).ok()?, date_parts.next().unwrap_or(Ok(1)).ok()?);
    let mut time_parts = time.splitn(3, ':').map(|p| p.parse::<f64>());
    let (hour, minute, second) = (time_parts.next()?.ok()?, time_parts.next().unwrap_or(Ok(0.0)).ok()?, time_parts.next().unwrap_or(Ok(0.0)).ok()?);
    // A calendar date to days since 1970-01-01 (Howard Hinnant's algorithm).
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some((days * 86_400) as f64 + hour * 3600.0 + minute * 60.0 + second - 978_307_200.0)
}

pub fn from_object(object: Id) -> Option<Value> {
    if object == 0 {
        return None;
    }
    if string::is_string(object) {
        return Some(Value::String(string::get(object)));
    }
    // One value that isn't a list (or a key that isn't a string) makes the
    // whole list not one, as Apple's: nothing is dropped quietly.
    if collections::is_dictionary(object) {
        let entries = collections::dictionary_pairs(object)
            .into_iter()
            .map(|(k, v)| Some((string::is_string(k).then(|| string::get(k))?, from_object(v)?)))
            .collect::<Option<_>>()?;
        return Some(Value::Dict(entries));
    }
    if collections::is_array(object) {
        return Some(Value::Array(collections::array_items(object).into_iter().map(from_object).collect::<Option<_>>()?));
    }
    if collections::is_number(object) {
        // The number's own kind and exact value (integers don't pass through a double).
        return Some(if collections::number_is_bool(object) {
            Value::Bool(collections::number_as_i64(object) != 0)
        } else if collections::number_is_float(object) {
            Value::Real(collections::number_as_f64(object))
        } else {
            Value::Integer(collections::number_as_i64(object))
        });
    }
    if crate::fw::data::is_data(object) {
        return Some(Value::Data(crate::fw::data::bytes(object)));
    }
    let date_class = crate::objc::runtime::class_named("NSDate");
    if date_class != 0 && crate::objc::runtime::send(object, "isKindOfClass:", &[date_class]) & 0xFF != 0 {
        return Some(Value::Date(crate::fw::dates::date_seconds(object)));
    }
    crate::log!("can't save {} in a settings file", string::describe(object));
    None
}

pub fn to_xml(value: &Value) -> String {
    fn write(value: &Value, out: &mut String, indent: usize) {
        let pad = "\t".repeat(indent);
        match value {
            Value::Dict(entries) => {
                out.push_str(&format!("{pad}<dict>\n"));
                for (key, value) in entries {
                    out.push_str(&format!("{pad}\t<key>{}</key>\n", escape(key)));
                    write(value, out, indent + 1);
                }
                out.push_str(&format!("{pad}</dict>\n"));
            }
            Value::Array(items) => {
                out.push_str(&format!("{pad}<array>\n"));
                for item in items {
                    write(item, out, indent + 1);
                }
                out.push_str(&format!("{pad}</array>\n"));
            }
            Value::String(s) => out.push_str(&format!("{pad}<string>{}</string>\n", escape(s))),
            Value::Integer(n) => out.push_str(&format!("{pad}<integer>{n}</integer>\n")),
            Value::Real(f) => out.push_str(&format!("{pad}<real>{f}</real>\n")),
            Value::Bool(true) => out.push_str(&format!("{pad}<true/>\n")),
            Value::Bool(false) => out.push_str(&format!("{pad}<false/>\n")),
            Value::Data(bytes) => out.push_str(&format!("{pad}<data>{}</data>\n", base64_encode(bytes))),
            Value::Date(seconds) => out.push_str(&format!("{pad}<date>{}</date>\n", date_text(*seconds))),
            Value::Uid(n) => out.push_str(&format!("{pad}<dict>\n{pad}\t<key>CF$UID</key>\n{pad}\t<integer>{n}</integer>\n{pad}</dict>\n")),
        }
    }
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n",
    );
    write(value, &mut out, 0);
    out.push_str("</plist>\n");
    out
}

/// A list in Apple's binary format (bplist00): every value an object in
/// the table (none shared), the top one first.
pub fn to_binary(value: &Value) -> Vec<u8> {
    enum Node {
        Bytes(Vec<u8>),
        Array(Vec<usize>),
        Dict(Vec<usize>, Vec<usize>),
    }
    fn header(marker: u8, length: usize, out: &mut Vec<u8>) {
        if length < 15 {
            out.push(marker | length as u8);
        } else {
            out.push(marker | 0xF);
            out.extend(integer(length as i64));
        }
    }
    fn integer(n: i64) -> Vec<u8> {
        match n {
            0..=0xFF => vec![0x10, n as u8],
            0x100..=0xFFFF => [vec![0x11], (n as u16).to_be_bytes().to_vec()].concat(),
            0x1_0000..=0xFFFF_FFFF => [vec![0x12], (n as u32).to_be_bytes().to_vec()].concat(),
            _ => [vec![0x13], n.to_be_bytes().to_vec()].concat(),
        }
    }
    fn text(s: &str) -> Vec<u8> {
        let mut out = Vec::new();
        if s.is_ascii() {
            header(0x50, s.len(), &mut out);
            out.extend(s.as_bytes());
        } else {
            let units: Vec<u16> = s.encode_utf16().collect();
            header(0x60, units.len(), &mut out);
            out.extend(units.iter().flat_map(|u| u.to_be_bytes()));
        }
        out
    }
    fn add(value: &Value, nodes: &mut Vec<Node>) -> usize {
        let index = nodes.len();
        nodes.push(Node::Bytes(Vec::new()));
        let node = match value {
            Value::Dict(entries) => {
                let keys = entries.iter().map(|(key, _)| add(&Value::String(key.clone()), nodes)).collect();
                let values = entries.iter().map(|(_, value)| add(value, nodes)).collect();
                Node::Dict(keys, values)
            }
            Value::Array(items) => Node::Array(items.iter().map(|item| add(item, nodes)).collect()),
            Value::String(s) => Node::Bytes(text(s)),
            Value::Integer(n) => Node::Bytes(integer(*n)),
            Value::Real(f) => Node::Bytes([vec![0x23], f.to_be_bytes().to_vec()].concat()),
            Value::Bool(b) => Node::Bytes(vec![if *b { 0x09 } else { 0x08 }]),
            Value::Data(bytes) => {
                let mut out = Vec::new();
                header(0x40, bytes.len(), &mut out);
                out.extend(bytes);
                Node::Bytes(out)
            }
            Value::Date(seconds) => Node::Bytes([vec![0x33], seconds.to_be_bytes().to_vec()].concat()),
            Value::Uid(n) => Node::Bytes([vec![0x87], n.to_be_bytes().to_vec()].concat()),
        };
        nodes[index] = node;
        index
    }
    let mut nodes = Vec::new();
    add(value, &mut nodes);
    let width = |largest: usize| match largest {
        0..=0xFF => 1,
        0x100..=0xFFFF => 2,
        0x1_0000..=0xFFFF_FFFF => 4,
        _ => 8,
    };
    let ref_size = width(nodes.len());
    let reference = |n: usize, out: &mut Vec<u8>| out.extend(&(n as u64).to_be_bytes()[8 - ref_size..]);
    let mut out = b"bplist00".to_vec();
    let mut offsets = Vec::with_capacity(nodes.len());
    for node in &nodes {
        offsets.push(out.len());
        match node {
            Node::Bytes(bytes) => out.extend(bytes),
            Node::Array(items) => {
                header(0xA0, items.len(), &mut out);
                items.iter().for_each(|&n| reference(n, &mut out));
            }
            Node::Dict(keys, values) => {
                header(0xD0, keys.len(), &mut out);
                keys.iter().chain(values).for_each(|&n| reference(n, &mut out));
            }
        }
    }
    let table = out.len();
    let offset_size = width(table);
    for offset in offsets {
        out.extend(&(offset as u64).to_be_bytes()[8 - offset_size..]);
    }
    out.extend([0; 6]);
    out.extend([offset_size as u8, ref_size as u8]);
    out.extend((nodes.len() as u64).to_be_bytes());
    out.extend(0u64.to_be_bytes());
    out.extend((table as u64).to_be_bytes());
    out
}

// ----- NSPropertyListSerialization -----

/// NSPropertyListXMLFormat_v1_0 and NSPropertyListBinaryFormat_v1_0.
const XML_FORMAT: usize = 100;
const BINARY_FORMAT: usize = 200;

fn put<T>(out: *mut T, value: T) {
    if !out.is_null() {
        unsafe { *out = value };
    }
}

/// A list as data (autoreleased) in the format asked for (XML or binary;
/// OpenStep can't be written).
fn to_data(list: Id, format: usize) -> Option<Id> {
    let value = from_object(list)?;
    let bytes = match format {
        XML_FORMAT => to_xml(&value).into_bytes(),
        BINARY_FORMAT => to_binary(&value),
        _ => return None,
    };
    Some(crate::fw::counted::autorelease(crate::fw::data::make(&bytes)))
}

fn from_data(data: Id, format: *mut usize) -> Option<Id> {
    if data == 0 || !crate::fw::data::is_data(data) {
        return None;
    }
    let bytes = crate::fw::data::bytes(data);
    let value = parse(&bytes)?;
    put(format, if bytes.starts_with(b"bplist") { BINARY_FORMAT } else { XML_FORMAT });
    Some(to_object(&value))
}

extern "C" fn serialize(_: Class, _: Sel, list: Id, format: usize, _options: usize, error: *mut Id) -> Id {
    to_data(list, format).unwrap_or_else(|| {
        put(error, crate::fw::misc::error("NSCocoaErrorDomain", 3851, "The property list can't be written."));
        0
    })
}

extern "C" fn serialize_old(_: Class, _: Sel, list: Id, format: usize, description: *mut Id) -> Id {
    to_data(list, format).unwrap_or_else(|| {
        put(description, string::make("Property list invalid for format"));
        0
    })
}

extern "C" fn deserialize(_: Class, _: Sel, data: Id, _options: usize, format: *mut usize, error: *mut Id) -> Id {
    from_data(data, format).unwrap_or_else(|| {
        put(error, crate::fw::misc::error("NSCocoaErrorDomain", 3840, "The data couldn't be read because it isn't in the correct format."));
        0
    })
}

extern "C" fn deserialize_old(_: Class, _: Sel, data: Id, _mutability: usize, format: *mut usize, description: *mut Id) -> Id {
    from_data(data, format).unwrap_or_else(|| {
        put(description, string::make("Unexpected character at line 1"));
        0
    })
}

/// Whether everything in the list can be written (strings, numbers, data,
/// arrays and dictionaries).
extern "C" fn is_valid(_: Class, _: Sel, list: Id, _format: usize) -> bool {
    fn valid(object: Id) -> bool {
        if collections::is_dictionary(object) {
            collections::dictionary_pairs(object).into_iter().all(|(key, value)| string::is_string(key) && valid(value))
        } else if collections::is_array(object) {
            collections::array_items(object).into_iter().all(valid)
        } else {
            string::is_string(object) || collections::is_number(object) || crate::fw::data::is_data(object)
        }
    }
    list != 0 && valid(list)
}

pub fn register(_: &mut crate::symbols::Registry) {
    let class = crate::fw::object::define_class("NSPropertyListSerialization", "NSObject", 8);
    crate::fw::object::class_methods(
        class,
        &[
            ("dataWithPropertyList:format:options:error:", serialize as usize),
            ("dataFromPropertyList:format:errorDescription:", serialize_old as usize),
            ("propertyListWithData:options:format:error:", deserialize as usize),
            ("propertyListFromData:mutabilityOption:format:errorDescription:", deserialize_old as usize),
            ("propertyList:isValidForFormat:", is_valid as usize),
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::objc::runtime;

    /// Games save their state with NSPropertyListSerialization: a list
    /// written to data reads back the same (with the format it was in),
    /// through the old and new methods; data that isn't a list gives nil
    /// and an error.
    #[test]
    fn property_lists_turn_into_data_and_back() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let class = runtime::class_named("NSPropertyListSerialization");
        let hero = collections::dictionary_from(&[
            (string::make("name"), string::make("Bob")),
            (string::make("level"), collections::number_int(3)),
            (string::make("items"), collections::array_from(&[string::make("sword"), collections::number_float(1.5)])),
        ]);
        assert!(runtime::send(class, "propertyList:isValidForFormat:", &[hero, XML_FORMAT]) & 0xFF != 0);
        let mut error: Id = 0;
        let data = runtime::send(class, "dataWithPropertyList:format:options:error:", &[hero, BINARY_FORMAT, 0, &mut error as *mut Id as usize]);
        assert!(data != 0 && error == 0, "written");
        let mut format = 0usize;
        let back = runtime::send(class, "propertyListWithData:options:format:error:", &[data, 0, &mut format as *mut usize as usize, &mut error as *mut Id as usize]);
        assert!(runtime::send(back, "isEqual:", &[hero]) & 0xFF != 0, "read back the same: {}", string::describe(back));
        assert!(format == XML_FORMAT || format == BINARY_FORMAT);

        let mut description: Id = 0;
        let old = runtime::send(class, "dataFromPropertyList:format:errorDescription:", &[hero, XML_FORMAT, &mut description as *mut Id as usize]);
        let back = runtime::send(class, "propertyListFromData:mutabilityOption:format:errorDescription:", &[old, 0, 0, &mut description as *mut Id as usize]);
        assert!(runtime::send(back, "isEqual:", &[hero]) & 0xFF != 0);
        assert_eq!(description, 0);

        // The format asked for is the one written (OpenStep, 1, can't be written).
        let written = |format: usize| {
            let mut error: Id = 0;
            let data = runtime::send(class, "dataWithPropertyList:format:options:error:", &[hero, format, 0, &mut error as *mut Id as usize]);
            (if data == 0 { Vec::new() } else { crate::fw::data::bytes(data) }, error)
        };
        assert!(written(BINARY_FORMAT).0.starts_with(b"bplist00"), "binary asked for");
        assert!(written(XML_FORMAT).0.starts_with(b"<?xml"));
        let (nothing, refused) = written(1);
        assert!(nothing.is_empty() && refused != 0, "OpenStep: an error, not XML");

        let junk = crate::fw::data::make(b"not a list");
        let back = runtime::send(class, "propertyListWithData:options:format:error:", &[junk, 0, 0, &mut error as *mut Id as usize]);
        assert_eq!(back, 0);
        assert!(error != 0, "an error for data that isn't a list");
    }

    /// Plain list parsing takes XML, binary and real old-style `{..}` or
    /// `(..)` documents; empty text, comments alone or a stray word is not a
    /// list (a damaged keychain file, say), so callers can tell corruption.
    #[test]
    fn plain_property_list_parsing_refuses_text_that_is_not_a_list() {
        assert!(parse(b"").is_none(), "empty text");
        assert!(parse(b"/* nothing */\n").is_none(), "comments alone");
        assert!(parse(b"stray").is_none(), "a bare word");
        assert!(matches!(parse(b"{ key = value; }"), Some(Value::Dict(entries)) if entries.len() == 1));
        assert!(matches!(parse(b"( one, two )"), Some(Value::Array(items)) if items.len() == 2));
        // .strings tables still load leniently, an empty one as an empty table.
        assert!(matches!(parse_strings(b""), Some(Value::Dict(entries)) if entries.is_empty()));
        assert!(matches!(parse_strings(b"\"key\";"), Some(Value::Dict(entries)) if entries.len() == 1 && entries[0].0 == "key"));
    }

    /// A list holding anything that isn't one (deep inside too), or a
    /// dictionary with a key that isn't a string, is not a property list:
    /// writeToFile: says NO and NSUserDefaults refuses it, nothing is dropped.
    #[test]
    fn apple_lists_with_a_bad_value_inside_are_refused_whole() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let thing = runtime::send(runtime::class_named("NSObject"), "new", &[]);
        let one = runtime::send(runtime::class_named("NSNumber"), "numberWithInt:", &[1]);
        let fine = string::make("fine");
        let inner = collections::array_from(&[fine, thing]);
        assert!(from_object(inner).is_none(), "an array with an object");
        assert!(from_object(collections::dictionary_from(&[(string::make("k"), inner)])).is_none(), "nested");
        assert!(from_object(collections::dictionary_from(&[(one, fine)])).is_none(), "a number key");
        assert!(from_object(collections::dictionary_from(&[(string::make("k"), fine)])).is_some());
        let path = std::env::temp_dir().join(format!("apple-plist-refused-{}.plist", std::process::id()));
        let written = runtime::send(inner, "writeToFile:atomically:", &[string::make(path.to_str().unwrap()), 1]) & 0xFF;
        assert_eq!(written, 0);
        assert!(!path.exists());
    }

    /// Dates save with their real value and load back as NSDate, in XML and
    /// binary lists (they were dropped on save and read as the number 0).
    #[test]
    fn apple_dates_save_and_load_as_dates() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let when = 815_000_000.5;
        let saved = from_object(collections::array_from(&[crate::fw::misc::date(when)]));
        assert!(matches!(&saved, Some(Value::Array(items)) if matches!(items[..], [Value::Date(d)] if d == when)), "{saved:?}");
        let xml = to_xml(saved.as_ref().unwrap());
        assert!(xml.contains("<date>2026-10-29T20:53:20Z</date>"), "{xml}");
        for bytes in [xml.into_bytes(), to_binary(saved.as_ref().unwrap())] {
            let back = to_object(&parse(&bytes).unwrap());
            let date = collections::array_items(back)[0];
            let is_date = runtime::send(date, "isKindOfClass:", &[runtime::class_named("NSDate")]) & 0xFF != 0;
            assert!(is_date, "{}", string::describe(date));
            assert_eq!(crate::fw::misc::date_value(date).floor(), when.floor());
        }
    }

    /// Numbers keep their kind and exact value through a list, both formats:
    /// integers past 2^53, doubles with no fraction, booleans.
    #[test]
    fn numbers_keep_their_exact_values_and_kinds() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let class = runtime::class_named("NSPropertyListSerialization");
        let big = 9_007_199_254_740_993i64;
        let list = collections::array_from(&[collections::number_int(big), collections::number_int(-5), collections::number_float(2.0), collections::number_bool(true)]);
        for format in [XML_FORMAT, BINARY_FORMAT] {
            let mut error: Id = 0;
            let data = runtime::send(class, "dataWithPropertyList:format:options:error:", &[list, format, 0, &mut error as *mut Id as usize]);
            let back = runtime::send(class, "propertyListWithData:options:format:error:", &[data, 0, 0, &mut error as *mut Id as usize]);
            let items = collections::array_items(back);
            assert_eq!((collections::number_as_i64(items[0]), collections::number_as_i64(items[1])), (big, -5), "format {format}");
            assert!(collections::number_is_float(items[2]) && collections::number_as_f64(items[2]) == 2.0, "format {format}: a double stays one");
            assert!(collections::number_is_bool(items[3]) && collections::number_as_i64(items[3]) == 1, "format {format}: a boolean stays one");
        }
    }
}
