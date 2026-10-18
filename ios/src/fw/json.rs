//! NSJSONSerialization: JSON text to and from Foundation objects
//! (NSDictionary, NSArray, NSString, NSNumber, NSNull).

use crate::fw::object::{class_methods, define_class};
use crate::fw::{collections, data, string};
use crate::objc::runtime::{self, Class, Id, Sel};
use crate::symbols::Registry;

const MUTABLE_CONTAINERS: usize = 1;
const MUTABLE_LEAVES: usize = 2;
const ALLOW_FRAGMENTS: usize = 4;
const PRETTY_PRINTED: usize = 1;

fn null() -> Id {
    runtime::send(runtime::class_named("NSNull"), "null", &[])
}

struct Reader<'a> {
    text: &'a [u8],
    at: usize,
    options: usize,
}

impl Reader<'_> {
    fn space(&mut self) {
        while self.at < self.text.len() && matches!(self.text[self.at], b' ' | b'\t' | b'\n' | b'\r') {
            self.at += 1;
        }
    }

    fn take(&mut self, expected: u8) -> Result<(), String> {
        self.space();
        if self.text.get(self.at) == Some(&expected) {
            self.at += 1;
            Ok(())
        } else {
            Err(format!("expected '{}' at {}", expected as char, self.at))
        }
    }

    fn value(&mut self) -> Result<Id, String> {
        self.space();
        match self.text.get(self.at) {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => {
                let text = self.string()?;
                let made = string::make(&text);
                Ok(if self.options & MUTABLE_LEAVES != 0 { runtime::send(made, "mutableCopy", &[]) } else { made })
            }
            Some(b't') => self.word("true", collections::number_bool(true)),
            Some(b'f') => self.word("false", collections::number_bool(false)),
            Some(b'n') => self.word("null", null()),
            Some(c) if *c == b'-' || c.is_ascii_digit() => self.number(),
            _ => Err(format!("unexpected text at {}", self.at)),
        }
    }

    fn word(&mut self, word: &str, value: Id) -> Result<Id, String> {
        if self.text[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            Ok(value)
        } else {
            Err(format!("unexpected text at {}", self.at))
        }
    }

    fn number(&mut self) -> Result<Id, String> {
        let start = self.at;
        while self.at < self.text.len() && matches!(self.text[self.at], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
            self.at += 1;
        }
        let text = std::str::from_utf8(&self.text[start..self.at]).unwrap_or("");
        if !text.contains(['.', 'e', 'E']) {
            if let Ok(whole) = text.parse::<i64>() {
                return Ok(collections::number_int(whole));
            }
        }
        text.parse::<f64>().map(collections::number_float).map_err(|_| format!("bad number at {start}"))
    }

    fn string(&mut self) -> Result<String, String> {
        self.take(b'"')?;
        let mut units: Vec<u16> = Vec::new();
        let mut plain = Vec::new();
        let flush = |plain: &mut Vec<u8>, units: &mut Vec<u16>| {
            units.extend(String::from_utf8_lossy(plain).encode_utf16());
            plain.clear();
        };
        loop {
            let Some(&c) = self.text.get(self.at) else { return Err("text ends inside a string".into()) };
            self.at += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    flush(&mut plain, &mut units);
                    let Some(&e) = self.text.get(self.at) else { return Err("text ends inside a string".into()) };
                    self.at += 1;
                    match e {
                        b'n' => units.push(b'\n' as u16),
                        b't' => units.push(b'\t' as u16),
                        b'r' => units.push(b'\r' as u16),
                        b'b' => units.push(8),
                        b'f' => units.push(12),
                        b'u' => {
                            let hex = self.text.get(self.at..self.at + 4).ok_or("short \\u escape")?;
                            let value = u16::from_str_radix(std::str::from_utf8(hex).unwrap_or(""), 16).map_err(|_| "bad \\u escape")?;
                            units.push(value);
                            self.at += 4;
                        }
                        other => units.push(other as u16),
                    }
                }
                _ => plain.push(c),
            }
        }
        flush(&mut plain, &mut units);
        Ok(String::from_utf16_lossy(&units))
    }

    fn array(&mut self) -> Result<Id, String> {
        self.take(b'[')?;
        let mut list = Vec::new();
        self.space();
        if self.text.get(self.at) == Some(&b']') {
            self.at += 1;
        } else {
            loop {
                list.push(self.value()?);
                self.space();
                match self.text.get(self.at) {
                    Some(b',') => self.at += 1,
                    Some(b']') => {
                        self.at += 1;
                        break;
                    }
                    _ => return Err(format!("expected ',' or ']' at {}", self.at)),
                }
            }
        }
        let array = collections::array_from(&list);
        Ok(if self.options & MUTABLE_CONTAINERS != 0 { runtime::send(array, "mutableCopy", &[]) } else { array })
    }

    fn object(&mut self) -> Result<Id, String> {
        self.take(b'{')?;
        let mut pairs = Vec::new();
        self.space();
        if self.text.get(self.at) == Some(&b'}') {
            self.at += 1;
        } else {
            loop {
                self.space();
                let key = string::make(&self.string()?);
                self.take(b':')?;
                let value = self.value()?;
                pairs.push((key, value));
                self.space();
                match self.text.get(self.at) {
                    Some(b',') => self.at += 1,
                    Some(b'}') => {
                        self.at += 1;
                        break;
                    }
                    _ => return Err(format!("expected ',' or '}}' at {}", self.at)),
                }
            }
        }
        let dict = collections::dictionary_from(&pairs);
        Ok(if self.options & MUTABLE_CONTAINERS != 0 { runtime::send(dict, "mutableCopy", &[]) } else { dict })
    }
}

fn quote(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '/' => out.push_str("\\/"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write(object: Id, pretty: bool, depth: usize, out: &mut String) -> Result<(), String> {
    let indent = |out: &mut String, depth: usize| {
        if pretty {
            out.push('\n');
            out.push_str(&"  ".repeat(depth));
        }
    };
    if string::is_string(object) {
        quote(&string::get(object), out);
    } else if collections::is_dictionary(object) {
        out.push('{');
        let pairs = collections::dictionary_pairs(object);
        for (i, (key, value)) in pairs.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            indent(out, depth + 1);
            if !string::is_string(*key) {
                return Err("a dictionary key is not a string".into());
            }
            quote(&string::get(*key), out);
            out.push_str(if pretty { " : " } else { ":" });
            write(*value, pretty, depth + 1, out)?;
        }
        if !pairs.is_empty() {
            indent(out, depth);
        }
        out.push('}');
    } else if collections::is_array(object) {
        out.push('[');
        let items = collections::array_items(object);
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            indent(out, depth + 1);
            write(*item, pretty, depth + 1, out)?;
        }
        if !items.is_empty() {
            indent(out, depth);
        }
        out.push(']');
    } else if collections::is_number(object) {
        if collections::number_is_bool(object) {
            out.push_str(if collections::number_as_i64(object) != 0 { "true" } else { "false" });
        } else {
            let value = collections::number_as_f64(object);
            if value.fract() == 0.0 && value.abs() < 1e15 {
                out.push_str(&collections::number_as_i64(object).to_string());
            } else if value.is_finite() {
                out.push_str(&value.to_string());
            } else {
                return Err("a number is not finite".into());
            }
        }
    } else if object != 0 && object == null() {
        out.push_str("null");
    } else {
        return Err(format!("a {} can't be written as JSON", string::describe_class_of(object)));
    }
    Ok(())
}

fn set_error(error: *mut Id, reason: &str) {
    if !error.is_null() {
        unsafe { *error = crate::fw::misc::error("NSCocoaErrorDomain", 3840, reason) };
    }
}

extern "C" fn json_object_with_data(_: Class, _: Sel, bytes: Id, options: usize, error: *mut Id) -> Id {
    let content = data::bytes(bytes);
    let mut reader = Reader { text: &content, at: 0, options };
    let result = reader.value().and_then(|value| {
        reader.space();
        if reader.at != reader.text.len() {
            return Err(format!("extra text at {}", reader.at));
        }
        let container = collections::is_dictionary(value) || collections::is_array(value);
        if !container && options & ALLOW_FRAGMENTS == 0 {
            return Err("the top level is not an object or array".into());
        }
        Ok(value)
    });
    match result {
        Ok(value) => value,
        Err(reason) => {
            set_error(error, &format!("JSON text not read: {reason}"));
            0
        }
    }
}

extern "C" fn data_with_json_object(_: Class, _: Sel, object: Id, options: usize, error: *mut Id) -> Id {
    let mut out = String::new();
    match write(object, options & PRETTY_PRINTED != 0, 0, &mut out) {
        Ok(()) => data::make(out.as_bytes()),
        Err(reason) => {
            set_error(error, &format!("JSON text not written: {reason}"));
            0
        }
    }
}

extern "C" fn is_valid_json_object(_: Class, _: Sel, object: Id) -> bool {
    (collections::is_dictionary(object) || collections::is_array(object)) && write(object, false, 0, &mut String::new()).is_ok()
}

pub fn register(_: &mut Registry) {
    let class = define_class("NSJSONSerialization", "NSObject", 8);
    class_methods(
        class,
        &[
            ("JSONObjectWithData:options:error:", json_object_with_data as usize),
            ("dataWithJSONObject:options:error:", data_with_json_object as usize),
            ("isValidJSONObject:", is_valid_json_object as usize),
        ],
    );
}
