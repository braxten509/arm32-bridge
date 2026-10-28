//! NSXMLParser: reads XML and tells its delegate what it meets, in order
//! (element starts with their attributes, text, element ends), as
//! Foundation's event-driven parser does. cocos2d reads its tile maps this way.

use crate::fw::object::{define_class, methods};
use crate::fw::uikit::{property_get, property_set};
use crate::fw::{collections, string};
use crate::objc::runtime::{self, Id, Sel};
use crate::symbols::Registry;

/// One thing met in the XML, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Start { name: String, attributes: Vec<(String, String)> },
    Text(String),
    End(String),
}

/// Text with XML's entities (&amp; &lt; &gt; &quot; &apos; &#n; &#xn;) decoded.
fn decode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.find(';').filter(|&end| end < 12) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let name = &rest[1..end];
        let letter = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => name
                .strip_prefix("#x")
                .or_else(|| name.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| name.strip_prefix('#').and_then(|n| n.parse().ok()))
                .and_then(char::from_u32),
        };
        match letter {
            Some(letter) => {
                out.push(letter);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The events of a whole document, or where (line) and why it is broken.
pub fn events(text: &str) -> Result<Vec<Event>, (usize, String)> {
    let mut out = Vec::new();
    read(text, &mut out).map(|()| out)
}

/// Reads `text` into `out`; on a fault `out` keeps the events before it.
fn read(text: &str, out: &mut Vec<Event>) -> Result<(), (usize, String)> {
    let line_at = |at: usize| text[..at].matches('\n').count() + 1;
    let fail = |at: usize, why: &str| Err((line_at(at), why.to_string()));
    let mut open: Vec<String> = Vec::new();
    let mut seen_root = false;
    let mut at = 0;
    while at < text.len() {
        let rest = &text[at..];
        if !rest.starts_with('<') {
            let end = rest.find('<').unwrap_or(rest.len());
            let piece = &rest[..end];
            if !open.is_empty() {
                out.push(Event::Text(decode(piece)));
            } else if !piece.trim().is_empty() {
                return fail(at, "text outside the root element");
            }
            at += end;
            continue;
        }
        let skip_to = |marker: &str, from: usize| rest[from..].find(marker).map(|end| from + end + marker.len());
        if rest.starts_with("<?") {
            at += skip_to("?>", 2).ok_or((line_at(at), "unclosed <?".to_string()))?;
        } else if rest.starts_with("<!--") {
            at += skip_to("-->", 4).ok_or((line_at(at), "unclosed comment".to_string()))?;
        } else if rest.starts_with("<![CDATA[") {
            let end = rest.find("]]>").ok_or((line_at(at), "unclosed CDATA".to_string()))?;
            if open.is_empty() {
                return fail(at, "CDATA outside the root element");
            }
            out.push(Event::Text(rest[9..end].to_string()));
            at += end + 3;
        } else if rest.starts_with("<!") {
            // <!DOCTYPE ...> (with an internal subset in brackets).
            let end = match (rest.find('['), rest.find('>')) {
                (Some(open), Some(close)) if open < close => skip_to("]>", open),
                (_, Some(close)) => Some(close + 1),
                _ => None,
            };
            at += end.ok_or((line_at(at), "unclosed <!".to_string()))?;
        } else if let Some(closing) = rest.strip_prefix("</") {
            let end = closing.find('>').ok_or((line_at(at), "unclosed end tag".to_string()))?;
            let name = closing[..end].trim();
            if open.pop().as_deref() != Some(name) {
                return fail(at, &format!("end tag </{name}> doesn't match"));
            }
            out.push(Event::End(name.to_string()));
            at += 2 + end + 1;
        } else {
            // <name attr="value" ...> or <name ... />
            let tag_end = {
                let mut quote = None;
                rest.char_indices().skip(1).find(|&(_, c)| match quote {
                    Some(q) => {
                        if c == q {
                            quote = None;
                        }
                        false
                    }
                    None if c == '"' || c == '\'' => {
                        quote = Some(c);
                        false
                    }
                    None => c == '>',
                })
            };
            let Some((end, _)) = tag_end else { return fail(at, "unclosed tag") };
            let inner = &rest[1..end];
            let (inner, empty) = match inner.strip_suffix('/') {
                Some(inner) => (inner, true),
                None => (inner, false),
            };
            let name_end = inner.find(|c: char| c.is_whitespace()).unwrap_or(inner.len());
            let name = &inner[..name_end];
            if name.is_empty() {
                return fail(at, "a tag without a name");
            }
            if open.is_empty() && seen_root {
                return fail(at, "a second root element");
            }
            let mut attributes = Vec::new();
            let mut attrs = inner[name_end..].trim_start();
            while !attrs.is_empty() {
                let Some(equals) = attrs.find('=') else { return fail(at, "an attribute without a value") };
                let key = attrs[..equals].trim().to_string();
                let value_part = attrs[equals + 1..].trim_start();
                let Some(quote) = value_part.chars().next().filter(|&c| c == '"' || c == '\'') else {
                    return fail(at, "an attribute value without quotes");
                };
                let Some(close) = value_part[1..].find(quote) else { return fail(at, "an unclosed attribute value") };
                attributes.push((key, decode(&value_part[1..1 + close])));
                attrs = value_part[close + 2..].trim_start();
            }
            out.push(Event::Start { name: name.to_string(), attributes });
            seen_root = true;
            if empty {
                out.push(Event::End(name.to_string()));
            } else {
                open.push(name.to_string());
            }
            at += end + 1;
        }
    }
    if let Some(name) = open.last() {
        return fail(text.len(), &format!("<{name}> is not closed"));
    }
    if !seen_root {
        return fail(text.len(), "no element");
    }
    Ok(())
}

/// A document's bytes as text: by its byte order mark (UTF-8, UTF-16 either
/// way), else by how it starts ("<?" in UTF-16) or the encoding its
/// declaration names (UTF-8 when it names none).
fn document_text(bytes: &[u8]) -> String {
    let utf16 = |bytes: &[u8], big: bool| {
        let units: Vec<u16> = bytes.chunks_exact(2).map(|c| if big { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) }).collect();
        String::from_utf16_lossy(&units)
    };
    match bytes {
        [0xEF, 0xBB, 0xBF, rest @ ..] => return String::from_utf8_lossy(rest).into_owned(),
        [0xFF, 0xFE, rest @ ..] => return utf16(rest, false),
        [0xFE, 0xFF, rest @ ..] => return utf16(rest, true),
        [b'<', 0, b'?', 0, ..] => return utf16(bytes, false),
        [0, b'<', 0, b'?', ..] => return utf16(bytes, true),
        _ => {}
    }
    // The declaration is ASCII: <?xml version="1.0" encoding="..."?>
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(200)]).into_owned();
    let declared = head
        .strip_prefix("<?xml")
        .and_then(|rest| rest.split("?>").next())
        .and_then(|declaration| declaration.split("encoding").nth(1))
        .and_then(|after| after.trim_start().strip_prefix('=').map(str::trim_start))
        .and_then(|value| {
            let quote = value.chars().next().filter(|&q| q == '"' || q == '\'')?;
            value[1..].split(quote).next().map(str::to_ascii_lowercase)
        });
    match declared.as_deref() {
        None | Some("utf-8" | "utf8" | "us-ascii" | "ascii") => String::from_utf8_lossy(bytes).into_owned(),
        // Each byte is the character with its number.
        Some("iso-8859-1" | "latin1" | "iso_8859-1" | "iso-latin-1") => bytes.iter().map(|&b| b as char).collect(),
        // The same, but 0x80-0x9F are letters and signs (euro, curly quotes, ...).
        Some("windows-1252" | "cp1252") => bytes.iter().map(|&b| windows_1252(b)).collect(),
        Some(other) => {
            crate::log!("NSXMLParser: encoding {other} not known, read as UTF-8");
            String::from_utf8_lossy(bytes).into_owned()
        }
    }
}

/// A Windows-1252 byte's character (0x80-0x9F differ from Latin-1; its
/// five unused bytes stay their own numbers, as Apple's decoder keeps them).
fn windows_1252(byte: u8) -> char {
    const HIGH: [u16; 32] = [
        0x20AC, 0x0081, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0x008D, 0x017D, 0x008F,
        0x0090, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x009D, 0x017E, 0x0178,
    ];
    match byte {
        0x80..=0x9F => char::from_u32(HIGH[(byte - 0x80) as usize] as u32).unwrap_or('\u{FFFD}'),
        _ => byte as char,
    }
}

// ----- NSXMLParser { isa, ... }: the data and delegate kept as properties -----

fn get_id(object: Id, key: &'static str) -> Id {
    property_get(object, key).unwrap_or(0) as Id
}

extern "C" fn parser_init_with_data(this: Id, _: Sel, data: Id) -> Id {
    if data == 0 {
        return 0;
    }
    // The parser's own copy (+1, held for good: parsers are never freed).
    let copy = crate::fw::data::make(&crate::fw::data::bytes(data));
    property_set(this, "data", copy as u64);
    this
}

extern "C" fn parser_init_with_url(this: Id, s: Sel, url: Id) -> Id {
    let path = if url == 0 { 0 } else { runtime::send(url, "path", &[]) };
    match (path != 0).then(|| std::fs::read(string::get(path))).and_then(Result::ok) {
        Some(bytes) => {
            let data = crate::fw::data::make(&bytes);
            let parser = parser_init_with_data(this, s, data);
            crate::fw::counted::release(data);
            parser
        }
        None => 0,
    }
}

extern "C" fn parser_delegate(this: Id, _: Sel) -> Id {
    get_id(this, "delegate")
}
extern "C" fn parser_set_delegate(this: Id, _: Sel, delegate: Id) {
    property_set(this, "delegate", delegate as u64);
}
extern "C" fn parser_set_flag(_this: Id, _: Sel, _value: u8) {}
extern "C" fn parser_error(this: Id, _: Sel) -> Id {
    get_id(this, "error")
}
extern "C" fn parser_line(this: Id, _: Sel) -> isize {
    property_get(this, "line").unwrap_or(0) as isize
}
extern "C" fn parser_abort(this: Id, _: Sel) {
    property_set(this, "aborted", 1);
}

fn tell(delegate: Id, name: &str, args: &[usize]) {
    if delegate == 0 || !runtime::responds(delegate, name) {
        return;
    }
    if args.len() <= 4 {
        runtime::send(delegate, name, args);
        return;
    }
    // parser:didStartElement:namespaceURI:qualifiedName:attributes: takes five.
    let s = runtime::sel(name);
    if let Some(imp) = runtime::imp_for(delegate, s) {
        let f: extern "C" fn(Id, Sel, usize, usize, usize, usize, usize) = unsafe { std::mem::transmute(imp) };
        f(delegate, s, args[0], args[1], args[2], args[3], args[4]);
    }
}

/// -parse: the delegate hears every event; NO when the XML is broken
/// (parser:parseErrorOccurred: after the events before the fault) or the
/// delegate stopped it.
extern "C" fn parser_parse(this: Id, _: Sel) -> bool {
    let text = document_text(&crate::fw::data::bytes(get_id(this, "data")));
    let text = text.as_str();
    let delegate = get_id(this, "delegate");
    property_set(this, "aborted", 0);
    let stopped = |parser: Id| property_get(parser, "aborted") == Some(1);
    // The events before a fault are still told, as a streaming parser would.
    let mut events = Vec::new();
    let broken = read(text, &mut events).err();
    tell(delegate, "parserDidStartDocument:", &[this]);
    for event in events {
        if stopped(this) {
            return false;
        }
        match event {
            Event::Start { name, attributes } => {
                let pairs: Vec<(Id, Id)> = attributes.iter().map(|(k, v)| (string::make(k), string::make(v))).collect();
                let attributes = collections::dictionary_from(&pairs);
                tell(delegate, "parser:didStartElement:namespaceURI:qualifiedName:attributes:", &[this, string::make(&name), 0, 0, attributes]);
            }
            Event::Text(text) => tell(delegate, "parser:foundCharacters:", &[this, string::make(&text)]),
            Event::End(name) => tell(delegate, "parser:didEndElement:namespaceURI:qualifiedName:", &[this, string::make(&name), 0, 0]),
        }
    }
    if stopped(this) {
        return false;
    }
    if let Some((line, why)) = broken {
        crate::log!("NSXMLParser: the XML is broken at line {line}: {why}");
        property_set(this, "line", line as u64);
        let error = crate::fw::misc::error("NSXMLParserErrorDomain", 111, &why);
        crate::fw::counted::retain(error);
        property_set(this, "error", error as u64);
        tell(delegate, "parser:parseErrorOccurred:", &[this, error]);
        return false;
    }
    tell(delegate, "parserDidEndDocument:", &[this]);
    true
}

pub fn register(_: &mut Registry) {
    let class = define_class("NSXMLParser", "NSObject", 8);
    methods(
        class,
        &[
            ("initWithData:", parser_init_with_data as usize),
            ("initWithContentsOfURL:", parser_init_with_url as usize),
            ("delegate", parser_delegate as usize),
            ("setDelegate:", parser_set_delegate as usize),
            ("setShouldProcessNamespaces:", parser_set_flag as usize),
            ("setShouldReportNamespacePrefixes:", parser_set_flag as usize),
            ("setShouldResolveExternalEntities:", parser_set_flag as usize),
            ("parse", parser_parse as usize),
            ("abortParsing", parser_abort as usize),
            ("parserError", parser_error as usize),
            ("lineNumber", parser_line as usize),
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fw::object::{define_class, methods};
    use crate::fw::{collections, string};
    use crate::objc::runtime::{self, lock, Sel};
    use std::sync::Mutex;

    fn start(name: &str, attributes: &[(&str, &str)]) -> Event {
        Event::Start { name: name.into(), attributes: attributes.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() }
    }

    /// Elements, attributes (either quote, entities decoded), text and
    /// CDATA in order; empty elements start and end; the declaration,
    /// comments and the DOCTYPE are skipped; broken XML says where.
    #[test]
    fn xml_is_read_into_events_in_order() {
        let text = "<?xml version=\"1.0\"?>\n<!DOCTYPE map>\n<!-- a map -->\n<map w='2' name=\"a &amp; b\">\n <tile gid=\"1\"/>\n <data>AB&lt;C<![CDATA[x<y]]></data>\n</map>\n";
        assert_eq!(
            events(text).unwrap(),
            [
                start("map", &[("w", "2"), ("name", "a & b")]),
                Event::Text("\n ".into()),
                start("tile", &[("gid", "1")]),
                Event::End("tile".into()),
                Event::Text("\n ".into()),
                start("data", &[]),
                Event::Text("AB<C".into()),
                Event::Text("x<y".into()),
                Event::End("data".into()),
                Event::Text("\n".into()),
                Event::End("map".into()),
            ]
        );
        assert_eq!(events("<a>&#65;&#x42;</a>").unwrap()[1], Event::Text("AB".into()));
        assert_eq!(events("<a>\n<b></a>").unwrap_err().0, 2, "a wrong end tag, on line 2");
        assert!(events("<a>").is_err(), "not closed");
        assert!(events("").is_err(), "no element");
    }

    static HEARD: Mutex<Vec<String>> = Mutex::new(Vec::new());

    /// The parser tells its delegate each event (attributes as a
    /// dictionary), and parse answers whether the XML was whole; the
    /// delegate may stop it with abortParsing.
    #[test]
    fn the_parser_tells_its_delegate_what_it_reads() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        extern "C" fn started_document(_this: Id, s: Sel, _parser: Id) {
            HEARD.lock().unwrap().push(lock().sel_name(s));
        }
        extern "C" fn started(_this: Id, _: Sel, parser: Id, name: Id, _uri: Id, _qualified: Id, attributes: Id) {
            let pairs: Vec<String> = collections::dictionary_pairs(attributes).into_iter().map(|(k, v)| format!("{}={}", string::get(k), string::get(v))).collect();
            let name = string::get(name);
            HEARD.lock().unwrap().push(format!("start {name} {}", pairs.join(",")));
            if name == "stop" {
                runtime::send(parser, "abortParsing", &[]);
            }
        }
        extern "C" fn found(_this: Id, _: Sel, _parser: Id, text: Id) {
            HEARD.lock().unwrap().push(format!("text {:?}", string::get(text)));
        }
        extern "C" fn ended(_this: Id, _: Sel, _parser: Id, name: Id, _uri: Id, _qualified: Id) {
            HEARD.lock().unwrap().push(format!("end {}", string::get(name)));
        }
        extern "C" fn failed(_this: Id, s: Sel, _parser: Id, _error: Id) {
            HEARD.lock().unwrap().push(lock().sel_name(s));
        }
        let class = define_class("XmlTestDelegate", "NSObject", 8);
        methods(
            class,
            &[
                ("parserDidStartDocument:", started_document as usize),
                ("parserDidEndDocument:", started_document as usize),
                ("parser:didStartElement:namespaceURI:qualifiedName:attributes:", started as usize),
                ("parser:foundCharacters:", found as usize),
                ("parser:didEndElement:namespaceURI:qualifiedName:", ended as usize),
                ("parser:parseErrorOccurred:", failed as usize),
            ],
        );
        let delegate = runtime::send(runtime::send(class, "alloc", &[]), "init", &[]);
        let parse_bytes = |bytes: &[u8]| {
            let data = crate::fw::data::make(bytes);
            let parser = runtime::send(runtime::send(runtime::class_named("NSXMLParser"), "alloc", &[]), "initWithData:", &[data]);
            runtime::send(parser, "setDelegate:", &[delegate]);
            let whole = runtime::send(parser, "parse", &[]) & 0xFF != 0;
            (whole, std::mem::take(&mut *HEARD.lock().unwrap()), runtime::send(parser, "parserError", &[]))
        };
        let parse = |text: &str| parse_bytes(text.as_bytes());
        let (whole, heard, error) = parse("<map w=\"2\"><tile gid=\"1\"/>hi</map>");
        assert!(whole && error == 0);
        assert_eq!(
            heard,
            ["parserDidStartDocument:", "start map w=2", "start tile gid=1", "end tile", "text \"hi\"", "end map", "parserDidEndDocument:"]
        );
        let (whole, heard, error) = parse("<map><b></map>");
        assert!(!whole && error != 0);
        assert_eq!(heard.last().map(String::as_str), Some("parser:parseErrorOccurred:"));
        let (whole, heard, _) = parse("<map><stop/><after/></map>");
        assert!(!whole, "stopped by the delegate");
        assert_eq!(heard, ["parserDidStartDocument:", "start map ", "start stop "]);
        // Other encodings: UTF-16 by its byte order mark (either order), or
        // what the declaration names.
        let text = "<?xml version=\"1.0\" encoding=\"UTF-16\"?><map name=\"café 🗺\">hi</map>";
        let little: Vec<u8> = [0xFF, 0xFE].into_iter().chain(text.encode_utf16().flat_map(u16::to_le_bytes)).collect();
        let big: Vec<u8> = [0xFE, 0xFF].into_iter().chain(text.encode_utf16().flat_map(u16::to_be_bytes)).collect();
        let expected = ["parserDidStartDocument:", "start map name=café 🗺", "text \"hi\"", "end map", "parserDidEndDocument:"];
        for bytes in [little, big] {
            let (whole, heard, _) = parse_bytes(&bytes);
            assert!(whole);
            assert_eq!(heard, expected);
        }
        let latin = b"<?xml version='1.0' encoding='ISO-8859-1'?><map name='caf\xe9'/>";
        let (whole, heard, _) = parse_bytes(latin);
        assert!(whole);
        assert_eq!(heard[1], "start map name=café");
        // Windows-1252 has its own letters where Latin-1 has control codes.
        let windows = b"<?xml version='1.0' encoding='windows-1252'?><map name='\x80\x91x\x92'/>";
        let (whole, heard, _) = parse_bytes(windows);
        assert!(whole);
        assert_eq!(heard[1], "start map name=€‘x’");
    }
}
