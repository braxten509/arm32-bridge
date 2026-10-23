//! Property lists (Apple's settings file format), XML and binary, read into
//! our NSDictionary/NSArray/NSString/NSNumber objects and written as XML.

use crate::fw::{collections, string};
use crate::objc::runtime::Id;

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

pub fn parse(bytes: &[u8]) -> Option<Value> {
    if bytes.starts_with(b"bplist00") {
        parse_binary(bytes)
    } else {
        parse_xml(&String::from_utf8_lossy(bytes))
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
            "date" => {
                self.text_until_close("date")?;
                Value::Date(0.0)
            }
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

pub fn to_object(value: &Value) -> Id {
    match value {
        Value::Dict(entries) => {
            let pairs: Vec<(Id, Id)> = entries.iter().map(|(k, v)| (string::make(k), to_object(v))).collect();
            let dict = collections::dictionary_from(&pairs);
            crate::objc::runtime::send(dict, "mutableCopy", &[])
        }
        Value::Array(items) => {
            let list: Vec<Id> = items.iter().map(to_object).collect();
            crate::objc::runtime::send(collections::array_from(&list), "mutableCopy", &[])
        }
        Value::String(s) => string::make(s),
        Value::Integer(n) => collections::number_int(*n),
        Value::Real(f) => collections::number_float(*f),
        Value::Bool(b) => collections::number_bool(*b),
        Value::Data(bytes) => crate::fw::data::make(bytes),
        Value::Date(_) => collections::number_float(0.0),
        Value::Uid(n) => collections::number_int(*n as i64),
    }
}

pub fn from_object(object: Id) -> Option<Value> {
    if object == 0 {
        return None;
    }
    if string::is_string(object) {
        return Some(Value::String(string::get(object)));
    }
    if collections::is_dictionary(object) {
        let entries = collections::dictionary_pairs(object)
            .into_iter()
            .filter_map(|(k, v)| Some((string::get(k), from_object(v)?)))
            .collect();
        return Some(Value::Dict(entries));
    }
    if collections::is_array(object) {
        return Some(Value::Array(collections::array_items(object).into_iter().filter_map(from_object).collect()));
    }
    if collections::is_number(object) {
        let value = collections::number_as_f64(object);
        return Some(if value.fract() == 0.0 { Value::Integer(value as i64) } else { Value::Real(value) });
    }
    if crate::fw::data::is_data(object) {
        return Some(Value::Data(crate::fw::data::bytes(object)));
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
            Value::Date(_) => out.push_str(&format!("{pad}<date>2001-01-01T00:00:00Z</date>\n")),
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
