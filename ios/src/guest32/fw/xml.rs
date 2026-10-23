//! libxml2's text writer (xmlTextWriter*), which iOS ships as a system
//! library: XML written to a file the way libxml2 writes it (no indenting,
//! `<a/>` for empty elements, attribute values escaped).

use super::{forget_handle, handle_to_guest, handle_to_host};
use arm32_bridge::handler;
use arm32_bridge::runtime::{mem, ArgSource, Ret};
use std::io::Write;

struct Writer {
    out: std::io::BufWriter<std::fs::File>,
    /// Open elements, innermost last.
    open: Vec<String>,
    /// The innermost element's start tag still takes attributes.
    in_start_tag: bool,
}

fn escape(text: &str, attribute: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attribute => out.push_str("&quot;"),
            '\n' if attribute => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            '\t' if attribute => out.push_str("&#9;"),
            c => out.push(c),
        }
    }
    out
}

impl Writer {
    /// Writes `text`; the byte count, or -1 (libxml2's results).
    fn put(&mut self, text: &str) -> i32 {
        match self.out.write_all(text.as_bytes()) {
            Ok(()) => text.len() as i32,
            Err(_) => -1,
        }
    }

    fn close_start_tag(&mut self) -> i32 {
        if !self.in_start_tag {
            return 0;
        }
        self.in_start_tag = false;
        self.put(">")
    }

    fn start_element(&mut self, name: &str) -> i32 {
        let closed = self.close_start_tag();
        self.open.push(name.to_string());
        self.in_start_tag = true;
        sum(closed, self.put(&format!("<{name}")))
    }

    fn attribute(&mut self, name: &str, value: &str) -> i32 {
        if !self.in_start_tag {
            return -1;
        }
        self.put(&format!(" {name}=\"{}\"", escape(value, true)))
    }

    fn text(&mut self, value: &str) -> i32 {
        let closed = self.close_start_tag();
        sum(closed, self.put(&escape(value, false)))
    }

    fn end_element(&mut self) -> i32 {
        let Some(name) = self.open.pop() else { return -1 };
        if self.in_start_tag {
            self.in_start_tag = false;
            self.put("/>")
        } else {
            self.put(&format!("</{name}>"))
        }
    }
}

fn sum(a: i32, b: i32) -> i32 {
    if a < 0 || b < 0 {
        -1
    } else {
        a + b
    }
}

fn with_writer(guest: u32, work: impl FnOnce(&mut Writer) -> i32) -> Ret {
    let host = handle_to_host(guest);
    if host == 0 {
        return Ret::U32(-1i32 as u32);
    }
    Ret::U32(work(unsafe { &mut *(host as *mut Writer) }) as u32)
}

fn text(guest: u32) -> String {
    if guest == 0 {
        String::new()
    } else {
        mem().string(guest)
    }
}

pub fn register() {
    let function = |name: &str, handler: Option<arm32_bridge::runtime::Handler>| super::super::function(name, handler.expect(name));
    // xmlNewTextWriterFilename(uri, compression).
    function(
        "xmlNewTextWriterFilename",
        handler!(|uc, a| {
            let (uri, _compression) = (text(a.u32()), a.u32());
            let path = uri.strip_prefix("file://").unwrap_or(&uri);
            match std::fs::File::create(path) {
                Ok(file) => {
                    let writer = Box::new(Writer { out: std::io::BufWriter::new(file), open: Vec::new(), in_start_tag: false });
                    Ret::U32(handle_to_guest(Box::into_raw(writer) as usize))
                }
                Err(e) => {
                    crate::log!("xmlNewTextWriterFilename: {path}: {e}");
                    Ret::U32(0)
                }
            }
        }),
    );
    function(
        "xmlFreeTextWriter",
        handler!(|uc, a| {
            let host = forget_handle(a.u32());
            if host != 0 {
                let mut writer = unsafe { Box::from_raw(host as *mut Writer) };
                let _ = writer.out.flush();
            }
            Ret::Void
        }),
    );
    // xmlTextWriterStartDocument(writer, version, encoding, standalone).
    function(
        "xmlTextWriterStartDocument",
        handler!(|uc, a| {
            let (writer, version, encoding, standalone) = (a.u32(), text(a.u32()), text(a.u32()), text(a.u32()));
            let mut head = format!("<?xml version=\"{}\"", if version.is_empty() { "1.0" } else { &version });
            if !encoding.is_empty() {
                head.push_str(&format!(" encoding=\"{encoding}\""));
            }
            if !standalone.is_empty() {
                head.push_str(&format!(" standalone=\"{standalone}\""));
            }
            head.push_str("?>\n");
            with_writer(writer, |w| w.put(&head))
        }),
    );
    function(
        "xmlTextWriterEndDocument",
        handler!(|uc, a| with_writer(a.u32(), |w| {
            let mut total = 0;
            while !w.open.is_empty() {
                total = sum(total, w.end_element());
            }
            sum(total, w.put("\n"))
        })),
    );
    function(
        "xmlTextWriterStartElement",
        handler!(|uc, a| {
            let (writer, name) = (a.u32(), text(a.u32()));
            with_writer(writer, |w| w.start_element(&name))
        }),
    );
    function("xmlTextWriterEndElement", handler!(|uc, a| with_writer(a.u32(), |w| w.end_element())));
    function("xmlTextWriterFullEndElement", handler!(|uc, a| with_writer(a.u32(), |w| {
        let closed = w.close_start_tag();
        sum(closed, w.end_element())
    })));
    function(
        "xmlTextWriterWriteAttribute",
        handler!(|uc, a| {
            let (writer, name, value) = (a.u32(), text(a.u32()), text(a.u32()));
            with_writer(writer, |w| w.attribute(&name, &value))
        }),
    );
    // xmlTextWriterWriteFormatAttribute(writer, name, format, ...).
    function(
        "xmlTextWriterWriteFormatAttribute",
        handler!(|uc, a| {
            let (writer, name, format) = (a.u32(), text(a.u32()), a.u32());
            let value = String::from_utf8_lossy(&arm32_bridge::imports::printf::format(mem(), format, a)).into_owned();
            with_writer(writer, |w| w.attribute(&name, &value))
        }),
    );
    function(
        "xmlTextWriterWriteString",
        handler!(|uc, a| {
            let (writer, value) = (a.u32(), text(a.u32()));
            with_writer(writer, |w| w.text(&value))
        }),
    );
    // xmlTextWriterWriteElement(writer, name, content).
    function(
        "xmlTextWriterWriteElement",
        handler!(|uc, a| {
            let (writer, name, value) = (a.u32(), text(a.u32()), text(a.u32()));
            with_writer(writer, |w| {
                let start = w.start_element(&name);
                let body = w.text(&value);
                sum(sum(start, body), w.end_element())
            })
        }),
    );
    function(
        "xmlTextWriterFlush",
        handler!(|uc, a| with_writer(a.u32(), |w| if w.out.flush().is_ok() { 0 } else { -1 })),
    );
}
