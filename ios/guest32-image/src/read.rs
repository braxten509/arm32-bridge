//! Checked little/big-endian field reads and LEB128 numbers.

use crate::error::{fail, Error, ErrorKind, Result};

pub(crate) fn range(data: &[u8], offset: u64, size: u64, what: &str) -> Result<std::ops::Range<usize>> {
    let end = offset
        .checked_add(size)
        .ok_or_else(|| Error::new(ErrorKind::Overflow, format!("{what}: {offset:#x}+{size:#x} overflows")))?;
    if end > data.len() as u64 {
        return fail(
            ErrorKind::Truncated,
            format!("{what}: {offset:#x}+{size:#x} lies outside the {:#x}-byte file", data.len()),
        );
    }
    Ok(offset as usize..end as usize)
}

pub(crate) fn bytes<'a>(data: &'a [u8], offset: u64, size: u64, what: &str) -> Result<&'a [u8]> {
    let r = range(data, offset, size, what)?;
    Ok(&data[r])
}

pub(crate) fn u32_le(data: &[u8], offset: u64, what: &str) -> Result<u32> {
    let raw = bytes(data, offset, 4, what)?;
    Ok(u32::from_le_bytes(raw.try_into().unwrap()))
}

pub(crate) fn u16_le(data: &[u8], offset: u64, what: &str) -> Result<u16> {
    let raw = bytes(data, offset, 2, what)?;
    Ok(u16::from_le_bytes(raw.try_into().unwrap()))
}

pub(crate) fn u64_le(data: &[u8], offset: u64, what: &str) -> Result<u64> {
    let raw = bytes(data, offset, 8, what)?;
    Ok(u64::from_le_bytes(raw.try_into().unwrap()))
}

pub(crate) fn u32_be(data: &[u8], offset: u64, what: &str) -> Result<u32> {
    let raw = bytes(data, offset, 4, what)?;
    Ok(u32::from_be_bytes(raw.try_into().unwrap()))
}

pub(crate) fn u8_at(data: &[u8], offset: u64, what: &str) -> Result<u8> {
    Ok(bytes(data, offset, 1, what)?[0])
}

/// A fixed 16-byte name field, cut at its first zero byte.
pub(crate) fn name16(data: &[u8], offset: u64, what: &str) -> Result<String> {
    let raw = bytes(data, offset, 16, what)?;
    let end = raw.iter().position(|&b| b == 0).unwrap_or(16);
    Ok(String::from_utf8_lossy(&raw[..end]).into_owned())
}

/// A zero-ended string that must end inside `data`.
pub(crate) fn c_string(data: &[u8], what: &str) -> Result<String> {
    let end = data
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| Error::new(ErrorKind::Malformed, format!("{what}: string has no ending zero")))?;
    Ok(String::from_utf8_lossy(&data[..end]).into_owned())
}

/// Reads an opcode stream one byte or LEB128 number at a time.
pub(crate) struct Stream<'a> {
    pub bytes: &'a [u8],
    pub position: usize,
    pub what: &'static str,
}

impl<'a> Stream<'a> {
    pub fn new(bytes: &'a [u8], what: &'static str) -> Self {
        Stream { bytes, position: 0, what }
    }

    pub fn done(&self) -> bool {
        self.position >= self.bytes.len()
    }

    pub fn byte(&mut self) -> Result<u8> {
        let value = *self.bytes.get(self.position).ok_or_else(|| {
            Error::new(ErrorKind::Truncated, format!("{} ends inside an opcode at {:#x}", self.what, self.position))
        })?;
        self.position += 1;
        Ok(value)
    }

    /// Unsigned LEB128; more than 64 bits of value is an error, not a silent cut.
    pub fn uleb(&mut self) -> Result<u64> {
        let start = self.position;
        let mut result = 0u64;
        let mut shift = 0u32;
        loop {
            let byte = self.byte()?;
            let payload = u64::from(byte & 0x7F);
            let fits = if shift < 64 { shift == 0 || payload >> (64 - shift) == 0 } else { payload == 0 };
            if !fits {
                return fail(ErrorKind::Overflow, format!("{}: ULEB at {start:#x} is wider than 64 bits", self.what));
            }
            if shift < 64 {
                result |= payload << shift;
            }
            shift = shift.saturating_add(7);
            if byte & 0x80 == 0 {
                return Ok(result);
            }
        }
    }

    /// Signed LEB128 with the same 64-bit limit.
    pub fn sleb(&mut self) -> Result<i64> {
        let start = self.position;
        let mut result = 0i64;
        let mut shift = 0u32;
        loop {
            let byte = self.byte()?;
            let payload = i64::from(byte & 0x7F);
            let fits = if shift < 63 {
                true
            } else if shift == 63 {
                payload == 0 || payload == 0x7F
            } else {
                // Only sign padding (all 0s or all 1s matching the sign) may follow.
                payload == if result < 0 { 0x7F } else { 0 }
            };
            if !fits {
                return fail(ErrorKind::Overflow, format!("{}: SLEB at {start:#x} is wider than 64 bits", self.what));
            }
            if shift < 64 {
                result |= payload << shift;
            }
            shift = shift.saturating_add(7);
            if byte & 0x80 == 0 {
                if shift < 64 && byte & 0x40 != 0 {
                    result |= -1i64 << shift;
                }
                return Ok(result);
            }
        }
    }

    pub fn c_string(&mut self) -> Result<String> {
        let rest = &self.bytes[self.position.min(self.bytes.len())..];
        let text = c_string(rest, self.what)?;
        // Step over the raw bytes; the lossy text can differ in length.
        self.position += rest.iter().position(|&b| b == 0).unwrap() + 1;
        Ok(text)
    }
}

/// A 64-bit LEB value used as a 32-bit address step. dyld on 32-bit ARM does
/// this arithmetic in 32 bits, so a "negative" 64-bit step means moving back.
/// Anything that is neither a 32-bit value nor a sign-extended negative 32-bit
/// value cannot come from a sane linker and is refused.
pub(crate) fn step32(value: u64, what: &str) -> Result<u32> {
    if value <= u64::from(u32::MAX) {
        return Ok(value as u32);
    }
    if value >= 0xFFFF_FFFF_8000_0000 {
        return Ok(value as u32);
    }
    fail(ErrorKind::Overflow, format!("{what}: step {value:#x} does not fit 32 bits"))
}
