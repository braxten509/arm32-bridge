//! Apple's printf-style functions. Apple's 64-bit ARM code passes every
//! extra (variadic) argument on the stack in its own 8-byte slot, unlike
//! Linux and Android, so these read the slots themselves and hand each
//! value to the host's snprintf one at a time.

use std::ffi::CString;

pub struct Args {
    slots: *const u64,
    index: usize,
}

impl Args {
    pub fn new(slots: *const u64) -> Args {
        Args { slots, index: 0 }
    }

    pub fn next(&mut self) -> u64 {
        let value = unsafe { self.slots.add(self.index).read_unaligned() };
        self.index += 1;
        value
    }

    pub fn next_f64(&mut self) -> f64 {
        f64::from_bits(self.next())
    }
}

fn host_format(spec: &[u8], value: impl Fn(*mut libc::c_char, usize, *const libc::c_char) -> i32) -> Vec<u8> {
    let spec = CString::new(spec).unwrap();
    let mut buffer = vec![0u8; 128];
    loop {
        let written = value(buffer.as_mut_ptr() as *mut _, buffer.len(), spec.as_ptr());
        if written < 0 {
            return Vec::new();
        }
        if (written as usize) < buffer.len() {
            buffer.truncate(written as usize);
            return buffer;
        }
        buffer.resize(written as usize + 1, 0);
    }
}

/// Formats like printf. `objects` turns on %@ (Objective-C objects). Wide
/// text (%ls, %lc) is converted in the game's LC_CTYPE.
pub fn format(fmt: &[u8], args: &mut Args, objects: bool) -> Vec<u8> {
    crate::libsystem::extra::with_guest_ctype(|| format_in_locale(fmt, args, objects))
}

fn format_in_locale(fmt: &[u8], args: &mut Args, objects: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(fmt.len() + 16);
    let mut i = 0;
    while i < fmt.len() {
        let c = fmt[i];
        if c != b'%' {
            out.push(c);
            i += 1;
            continue;
        }
        i += 1;
        if i >= fmt.len() {
            break;
        }
        if fmt[i] == b'%' {
            out.push(b'%');
            i += 1;
            continue;
        }
        // Positional arguments (%1$d) are rare; not handled.
        let mut spec = vec![b'%'];
        while i < fmt.len() && b"-+ #0'".contains(&fmt[i]) {
            spec.push(fmt[i]);
            i += 1;
        }
        if i < fmt.len() && fmt[i] == b'*' {
            spec.extend((args.next() as i32).to_string().bytes());
            i += 1;
        } else {
            while i < fmt.len() && fmt[i].is_ascii_digit() {
                spec.push(fmt[i]);
                i += 1;
            }
        }
        if i < fmt.len() && fmt[i] == b'.' {
            spec.push(b'.');
            i += 1;
            if i < fmt.len() && fmt[i] == b'*' {
                spec.extend((args.next() as i32).to_string().bytes());
                i += 1;
            } else {
                while i < fmt.len() && fmt[i].is_ascii_digit() {
                    spec.push(fmt[i]);
                    i += 1;
                }
            }
        }
        let mut length = String::new();
        while i < fmt.len() && b"hlqLjzt".contains(&fmt[i]) {
            length.push(fmt[i] as char);
            i += 1;
        }
        if i >= fmt.len() {
            break;
        }
        let conversion = fmt[i];
        i += 1;
        let long = matches!(length.as_str(), "l" | "ll" | "q" | "j" | "z" | "t");
        match conversion {
            // In NSString formats %C is a unichar and %S a unichar (UTF-16) string.
            b'C' | b'S' if objects => {
                let raw = args.next();
                let text = if conversion == b'C' {
                    char::decode_utf16([raw as u16]).map(|c| c.unwrap_or('\u{FFFD}')).collect::<String>()
                } else if raw == 0 {
                    "(null)".to_string()
                } else {
                    let pointer = raw as usize as *const u16;
                    let length = (0..).take_while(|&i| unsafe { *pointer.add(i) } != 0).count();
                    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(pointer, length) })
                };
                let text = CString::new(text.replace('\0', "")).unwrap();
                spec.push(b's');
                let pointer = text.as_ptr() as usize;
                out.extend(host_format(&spec, |b, n, f| unsafe { libc::snprintf(b, n, f, pointer) }));
            }
            b'd' | b'i' => {
                let raw = args.next();
                let value: i64 = match length.as_str() {
                    "hh" => raw as i8 as i64,
                    "h" => raw as i16 as i64,
                    _ if long => raw as i64,
                    _ => raw as i32 as i64,
                };
                spec.extend(b"ll");
                spec.push(conversion);
                out.extend(host_format(&spec, |b, n, f| unsafe { libc::snprintf(b, n, f, value) }));
            }
            b'u' | b'o' | b'x' | b'X' => {
                let raw = args.next();
                let value: u64 = match length.as_str() {
                    "hh" => raw as u8 as u64,
                    "h" => raw as u16 as u64,
                    _ if long => raw,
                    _ => raw as u32 as u64,
                };
                spec.extend(b"ll");
                spec.push(conversion);
                out.extend(host_format(&spec, |b, n, f| unsafe { libc::snprintf(b, n, f, value) }));
            }
            b'c' | b'C' => {
                let value = args.next() as u32;
                if length == "l" || conversion == b'C' {
                    spec.extend(b"lc");
                } else {
                    spec.push(b'c');
                }
                out.extend(host_format(&spec, |b, n, f| unsafe { libc::snprintf(b, n, f, value) }));
            }
            b's' | b'S' => {
                let pointer = args.next() as usize;
                if length == "l" || conversion == b'S' {
                    spec.extend(b"ls");
                } else {
                    spec.push(b's');
                }
                let null = b"(null)\0";
                let pointer = if pointer == 0 { null.as_ptr() as usize } else { pointer };
                if pointer == null.as_ptr() as usize && (length == "l" || conversion == b'S') {
                    spec.truncate(spec.len() - 2);
                    spec.push(b's');
                }
                out.extend(host_format(&spec, |b, n, f| unsafe { libc::snprintf(b, n, f, pointer) }));
            }
            b'p' => {
                let value = args.next() as usize;
                spec.push(b'p');
                out.extend(host_format(&spec, |b, n, f| unsafe { libc::snprintf(b, n, f, value) }));
            }
            b'f' | b'F' | b'e' | b'E' | b'g' | b'G' | b'a' | b'A' => {
                let value = args.next_f64();
                spec.push(conversion);
                out.extend(host_format(&spec, |b, n, f| unsafe { libc::snprintf(b, n, f, value) }));
            }
            b'n' => {
                let place = args.next() as usize;
                if place != 0 {
                    let count = out.len();
                    unsafe {
                        match length.as_str() {
                            "hh" => *(place as *mut i8) = count as i8,
                            "h" => *(place as *mut i16) = count as i16,
                            _ if long => *(place as *mut i64) = count as i64,
                            _ => *(place as *mut i32) = count as i32,
                        }
                    }
                }
            }
            b'@' if objects => {
                let object = args.next() as usize;
                let text = CString::new(crate::fw::string::describe(object).replace('\0', "")).unwrap();
                spec.push(b's');
                let pointer = text.as_ptr() as usize;
                out.extend(host_format(&spec, |b, n, f| unsafe { libc::snprintf(b, n, f, pointer) }));
            }
            other => {
                out.push(b'%');
                out.push(other);
            }
        }
    }
    out
}

pub unsafe fn c_bytes<'a>(pointer: *const libc::c_char) -> &'a [u8] {
    if pointer.is_null() {
        return b"";
    }
    std::ffi::CStr::from_ptr(pointer).to_bytes()
}

/// Copies formatted text into a C buffer the way snprintf does.
pub unsafe fn write_bounded(buffer: *mut libc::c_char, size: usize, text: &[u8]) -> i32 {
    if size > 0 && !buffer.is_null() {
        let count = text.len().min(size - 1);
        std::ptr::copy_nonoverlapping(text.as_ptr(), buffer as *mut u8, count);
        *buffer.add(count) = 0;
    }
    text.len() as i32
}

/// A wide (4-byte character) format string as UTF-8 for `format`.
pub unsafe fn wide_to_utf8(pointer: *const i32) -> Vec<u8> {
    let mut out = String::new();
    if pointer.is_null() {
        return Vec::new();
    }
    let mut index = 0;
    loop {
        let unit = *pointer.add(index);
        if unit == 0 {
            break;
        }
        out.push(char::from_u32(unit as u32).unwrap_or('\u{FFFD}'));
        index += 1;
    }
    out.into_bytes()
}
