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

    /// JSON's number rule: -?(0|[1-9][0-9]*)(.[0-9]+)?([eE][+-]?[0-9]+)?
    fn number(&mut self) -> Result<Id, String> {
        let start = self.at;
        let bad = || format!("bad number at {start}");
        let digits = |reader: &mut Self| {
            let first = reader.at;
            while reader.text.get(reader.at).is_some_and(u8::is_ascii_digit) {
                reader.at += 1;
            }
            reader.at - first
        };
        if self.text.get(self.at) == Some(&b'-') {
            self.at += 1;
        }
        let leading_zero = self.text.get(self.at) == Some(&b'0');
        let whole = digits(self);
        if whole == 0 || (leading_zero && whole > 1) {
            return Err(bad());
        }
        let mut fraction = false;
        if self.text.get(self.at) == Some(&b'.') {
            self.at += 1;
            fraction = true;
            if digits(self) == 0 {
                return Err(bad());
            }
        }
        if matches!(self.text.get(self.at), Some(b'e' | b'E')) {
            self.at += 1;
            fraction = true;
            if matches!(self.text.get(self.at), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if digits(self) == 0 {
                return Err(bad());
            }
        }
        let text = std::str::from_utf8(&self.text[start..self.at]).map_err(|_| bad())?;
        if !fraction {
            if let Ok(whole) = text.parse::<i64>() {
                return Ok(collections::number_int(whole));
            }
        }
        text.parse::<f64>().map(collections::number_float).map_err(|_| bad())
    }

    fn string(&mut self) -> Result<String, String> {
        self.take(b'"')?;
        let mut units: Vec<u16> = Vec::new();
        let mut plain = Vec::new();
        let flush = |plain: &mut Vec<u8>, units: &mut Vec<u16>| -> Result<(), String> {
            units.extend(std::str::from_utf8(plain).map_err(|_| "text is not valid UTF-8".to_string())?.encode_utf16());
            plain.clear();
            Ok(())
        };
        loop {
            let Some(&c) = self.text.get(self.at) else { return Err("text ends inside a string".into()) };
            self.at += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    flush(&mut plain, &mut units)?;
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
                            if !hex.iter().all(u8::is_ascii_hexdigit) {
                                return Err("bad \\u escape".into());
                            }
                            let value = u16::from_str_radix(std::str::from_utf8(hex).unwrap_or(""), 16).map_err(|_| "bad \\u escape")?;
                            units.push(value);
                            self.at += 4;
                        }
                        b'"' | b'\\' | b'/' => units.push(e as u16),
                        _ => return Err(format!("bad escape at {}", self.at - 1)),
                    }
                }
                0..=0x1F => return Err(format!("control character inside a string at {}", self.at - 1)),
                _ => plain.push(c),
            }
        }
        flush(&mut plain, &mut units)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str) -> Result<Id, String> {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let mut reader = Reader { text: text.as_bytes(), at: 0, options: ALLOW_FRAGMENTS };
        let value = reader.value()?;
        reader.space();
        if reader.at != reader.text.len() {
            return Err("extra text".into());
        }
        Ok(value)
    }

    #[test]
    fn json_grammar_is_enforced() {
        for good in ["0", "-0", "12", "1.5", "-2e10", "3E+2", "1.25e-3", r#""a\/b\"\\""#, r#""\u00e9""#, "[1,{\"k\":null}]"] {
            assert!(read(good).is_ok(), "{good} should be read");
        }
        for bad in ["01", "1.", ".5", "-", "1e", "+1", r#""\q""#, "\"a\tb\"", r#""\u12G4""#] {
            assert!(read(bad).is_err(), "{bad} should be refused");
        }
        assert_eq!(collections::number_as_i64(read("42").unwrap()), 42);
        assert_eq!(string::get(read(r#""\u00e9x""#).unwrap()), "éx");
    }
}
