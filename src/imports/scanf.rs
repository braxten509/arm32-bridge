//! sscanf-style parsing for 32-bit callers. Numbers are read with the host's
//! strtoll / strtoull / strtod so they parse exactly like a real libc, then
//! stored with 32-bit sizes (int, long and pointers are 4 bytes; long long,
//! intmax_t, double and long double are 8).

use std::ffi::CString;

use crate::mem::GuestMem;
use crate::runtime::ArgSource;

/// Parses the guest string at `input_address` and returns the number of
/// stored conversions, or -1 if the input ran out before the first one.
pub fn scan(mem: GuestMem, input_address: u32, format_address: u32, args: &mut dyn ArgSource) -> i32 {
    let input = mem.c_str(input_address).to_bytes();
    let format = mem.c_str(format_address).to_bytes();
    let (mut i, mut position, mut stored) = (0usize, 0usize, 0i32);
    // -1 when input ends before any conversion was even tried.
    let input_failure = |stored: i32, tried: bool| if stored == 0 && !tried { -1 } else { stored };
    let mut tried = false;
    while i < format.len() {
        let c = format[i];
        if c.is_ascii_whitespace() {
            while position < input.len() && input[position].is_ascii_whitespace() {
                position += 1;
            }
            i += 1;
            continue;
        }
        if c != b'%' || format.get(i + 1) == Some(&b'%') {
            if c == b'%' {
                i += 1;
                while position < input.len() && input[position].is_ascii_whitespace() {
                    position += 1;
                }
            }
            if position >= input.len() {
                return input_failure(stored, tried);
            }
            if input[position] != c {
                return stored;
            }
            position += 1;
            i += 1;
            continue;
        }
        i += 1;
        let skip = format.get(i) == Some(&b'*');
        if skip {
            i += 1;
        }
        let mut width = 0usize;
        while i < format.len() && format[i].is_ascii_digit() {
            width = width * 10 + (format[i] - b'0') as usize;
            i += 1;
        }
        let length_start = i;
        while i < format.len() && b"hlLqjzt".contains(&format[i]) {
            i += 1;
        }
        let length = &format[length_start..i];
        let Some(&conversion) = format.get(i) else {
            return stored;
        };
        i += 1;

        if conversion == b'n' {
            if !skip {
                store_integer(mem, args.u32(), length, position as u64);
            }
            continue;
        }
        if !matches!(conversion, b'c' | b'[') {
            while position < input.len() && input[position].is_ascii_whitespace() {
                position += 1;
            }
        }
        if position >= input.len() {
            return input_failure(stored, tried);
        }
        tried = true;
        let rest = &input[position..];
        let limit = if width == 0 { rest.len() } else { width.min(rest.len()) };

        match conversion {
            b'd' | b'i' | b'u' | b'o' | b'x' | b'X' | b'p' => {
                let base = match conversion {
                    b'd' | b'u' => 10,
                    b'i' => 0,
                    b'o' => 8,
                    _ => 16,
                };
                let text = CString::new(&rest[..limit]).unwrap();
                let mut end: *mut libc::c_char = std::ptr::null_mut();
                let value = unsafe {
                    if conversion == b'd' || conversion == b'i' {
                        libc::strtoll(text.as_ptr(), &mut end, base) as u64
                    } else {
                        libc::strtoull(text.as_ptr(), &mut end, base)
                    }
                };
                let used = end as usize - text.as_ptr() as usize;
                if used == 0 {
                    return stored;
                }
                position += used;
                if !skip {
                    let length: &[u8] = if conversion == b'p' { b"" } else { length };
                    store_integer(mem, args.u32(), length, value);
                    stored += 1;
                }
            }
            b'f' | b'F' | b'e' | b'E' | b'g' | b'G' | b'a' | b'A' => {
                let text = CString::new(&rest[..limit]).unwrap();
                let mut end: *mut libc::c_char = std::ptr::null_mut();
                let value = unsafe { libc::strtod(text.as_ptr(), &mut end) };
                let used = end as usize - text.as_ptr() as usize;
                if used == 0 {
                    return stored;
                }
                position += used;
                if !skip {
                    let target = args.u32();
                    if length.is_empty() {
                        mem.write_u32(target, (value as f32).to_bits());
                    } else {
                        mem.write_u64(target, value.to_bits());
                    }
                    stored += 1;
                }
            }
            b's' => {
                let used = rest[..limit].iter().take_while(|b| !b.is_ascii_whitespace()).count();
                position += used;
                if !skip {
                    let target = args.u32();
                    mem.write_bytes(target, &rest[..used]);
                    mem.write_u8(target + used as u32, 0);
                    stored += 1;
                }
            }
            b'c' => {
                let count = if width == 0 { 1 } else { width };
                if rest.len() < count {
                    return stored;
                }
                position += count;
                if !skip {
                    mem.write_bytes(args.u32(), &rest[..count]);
                    stored += 1;
                }
            }
            b'[' => {
                // Set: optional '^', a leading ']' is literal, ranges like a-z.
                let negate = format.get(i) == Some(&b'^');
                if negate {
                    i += 1;
                }
                let set_start = i;
                if format.get(i) == Some(&b']') {
                    i += 1;
                }
                while i < format.len() && format[i] != b']' {
                    i += 1;
                }
                let set = &format[set_start..i.min(format.len())];
                i += 1;
                let in_set = |byte: u8| {
                    let mut k = 0;
                    let mut found = false;
                    while k < set.len() {
                        if k + 2 < set.len() && set[k + 1] == b'-' {
                            found |= (set[k]..=set[k + 2]).contains(&byte);
                            k += 3;
                        } else {
                            found |= set[k] == byte;
                            k += 1;
                        }
                    }
                    found != negate
                };
                let used = rest[..limit].iter().take_while(|&&b| in_set(b)).count();
                if used == 0 {
                    return stored;
                }
                position += used;
                if !skip {
                    let target = args.u32();
                    mem.write_bytes(target, &rest[..used]);
                    mem.write_u8(target + used as u32, 0);
                    stored += 1;
                }
            }
            _ => return stored,
        }
    }
    stored
}

fn store_integer(mem: GuestMem, target: u32, length: &[u8], value: u64) {
    match length {
        b"hh" => mem.write_u8(target, value as u8),
        b"h" => mem.write_u16(target, value as u16),
        b"ll" | b"q" | b"j" | b"L" => mem.write_u64(target, value),
        _ => mem.write_u32(target, value as u32),
    }
}
