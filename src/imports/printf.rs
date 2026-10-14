//! printf-style formatting for 32-bit callers. Each conversion's argument is
//! read with 32-bit sizes (long and pointers are 4 bytes) and then formatted
//! by the host's snprintf, so the output matches what a real libc prints.

use std::ffi::CString;

use crate::mem::GuestMem;
use crate::runtime::ArgSource;

pub fn format(mem: GuestMem, format_address: u32, args: &mut dyn ArgSource) -> Vec<u8> {
    format_text(mem, mem.c_str(format_address).to_bytes(), args)
}

/// Formats `format` (UTF-8 text already read from the guest; wide formats
/// are converted first). `%ls`/`%S` take a wchar_t string and `%lc`/`%C` a
/// wide character, written out as UTF-8.
pub fn format_text(mem: GuestMem, format: &[u8], args: &mut dyn ArgSource) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < format.len() {
        if format[i] != b'%' {
            out.push(format[i]);
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        if i < format.len() && format[i] == b'%' {
            out.push(b'%');
            i += 1;
            continue;
        }
        // Flags, width, precision. '*' takes an int argument.
        let mut spec = String::from("%");
        while i < format.len() && b"-+ #0'".contains(&format[i]) {
            spec.push(format[i] as char);
            i += 1;
        }
        let read_number = |i: &mut usize, spec: &mut String, args: &mut dyn ArgSource| {
            if *i < format.len() && format[*i] == b'*' {
                spec.push_str(&args.i32().to_string());
                *i += 1;
            } else {
                while *i < format.len() && format[*i].is_ascii_digit() {
                    spec.push(format[*i] as char);
                    *i += 1;
                }
            }
        };
        read_number(&mut i, &mut spec, args);
        if i < format.len() && format[i] == b'.' {
            spec.push('.');
            i += 1;
            read_number(&mut i, &mut spec, args);
        }
        // Length: on ARM32, l / z / t / j-less sizes are 4 bytes, ll / j / q are 8.
        let mut wide = false;
        let length_start = i;
        while i < format.len() && b"hlLqjzt".contains(&format[i]) {
            i += 1;
        }
        let length = &format[length_start..i];
        if length == b"ll" || length == b"q" || length == b"j" || length == b"L" {
            wide = true;
        }
        let Some(&conversion) = format.get(i) else {
            out.extend_from_slice(&format[start..]);
            break;
        };
        i += 1;
        let short_length = if length == b"h" { "h" } else if length == b"hh" { "hh" } else { "" };
        match conversion {
            b'd' | b'i' => {
                let value = if wide { args.i64() } else { args.i32() as i64 };
                let value = match short_length {
                    "h" => value as i16 as i64,
                    "hh" => value as i8 as i64,
                    _ => value,
                };
                host_format(&mut out, &format!("{spec}lld"), |f, b, n| unsafe { libc::snprintf(b, n, f, value) });
            }
            b'u' | b'x' | b'X' | b'o' => {
                let value = if wide { args.u64() } else { args.u32() as u64 };
                let value = match short_length {
                    "h" => value as u16 as u64,
                    "hh" => value as u8 as u64,
                    _ => value,
                };
                let c = conversion as char;
                host_format(&mut out, &format!("{spec}ll{c}"), |f, b, n| unsafe { libc::snprintf(b, n, f, value) });
            }
            b'c' | b'C' if length == b"l" || conversion == b'C' => {
                let value = args.u32();
                let text = CString::new(char::from_u32(value).unwrap_or('\u{FFFD}').to_string()).unwrap_or_default();
                host_format(&mut out, &format!("{spec}s"), |f, b, n| unsafe { libc::snprintf(b, n, f, text.as_ptr()) });
            }
            b's' | b'S' if length == b"l" || conversion == b'S' => {
                let pointer = args.u32();
                let mut text = String::new();
                if pointer == 0 {
                    text.push_str("(null)");
                }
                let mut at = pointer;
                while pointer != 0 {
                    let c = mem.read_u32(at);
                    if c == 0 {
                        break;
                    }
                    text.push(char::from_u32(c).unwrap_or('\u{FFFD}'));
                    at += 4;
                }
                let text = CString::new(text).unwrap_or_default();
                host_format(&mut out, &format!("{spec}s"), |f, b, n| unsafe { libc::snprintf(b, n, f, text.as_ptr()) });
            }
            b'c' => {
                let value = args.i32();
                host_format(&mut out, &format!("{spec}c"), |f, b, n| unsafe { libc::snprintf(b, n, f, value) });
            }
            b'p' => {
                let value = args.u32();
                if value == 0 {
                    out.extend_from_slice(b"(nil)");
                } else {
                    host_format(&mut out, &format!("{spec}#x"), |f, b, n| unsafe { libc::snprintf(b, n, f, value) });
                }
            }
            b's' => {
                let pointer = args.u32();
                let text = if pointer == 0 { c"(null)".as_ptr() } else { mem.host(pointer) as *const libc::c_char };
                host_format(&mut out, &format!("{spec}s"), |f, b, n| unsafe { libc::snprintf(b, n, f, text) });
            }
            b'f' | b'F' | b'e' | b'E' | b'g' | b'G' | b'a' | b'A' => {
                // long double is a plain double on ARM32.
                let value = args.f64();
                let c = conversion as char;
                host_format(&mut out, &format!("{spec}{c}"), |f, b, n| unsafe { libc::snprintf(b, n, f, value) });
            }
            b'n' => {
                let pointer = args.u32();
                if pointer != 0 {
                    mem.write_u32(pointer, out.len() as u32);
                }
            }
            b'm' => {
                let text = std::io::Error::last_os_error().to_string();
                out.extend_from_slice(text.as_bytes());
            }
            _ => out.extend_from_slice(&format[start..i]),
        }
    }
    out
}

fn host_format(
    out: &mut Vec<u8>,
    spec: &str,
    call: impl Fn(*const libc::c_char, *mut libc::c_char, usize) -> libc::c_int,
) {
    let spec = CString::new(spec).unwrap();
    let needed = call(spec.as_ptr(), std::ptr::null_mut(), 0);
    if needed <= 0 {
        return;
    }
    let mut buffer = vec![0u8; needed as usize + 1];
    call(spec.as_ptr(), buffer.as_mut_ptr() as *mut libc::c_char, buffer.len());
    buffer.truncate(needed as usize);
    out.extend_from_slice(&buffer);
}
