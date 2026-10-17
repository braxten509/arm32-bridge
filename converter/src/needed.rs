//! The libraries an ARM library asks for (its DT_NEEDED names), read from its
//! program headers, for 32-bit and 64-bit little-endian ELF files.

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const DT_NULL: u64 = 0;
const DT_NEEDED: u64 = 1;
const DT_STRTAB: u64 = 5;

/// The names of the libraries `elf` needs, or None if it isn't a readable ELF library.
pub fn needed(elf: &[u8]) -> Option<Vec<String>> {
    if elf.get(..4)? != b"\x7fELF" || *elf.get(5)? != 1 {
        return None;
    }
    let wide = match *elf.get(4)? {
        1 => false,
        2 => true,
        _ => return None,
    };
    // Every offset comes from the file, so all arithmetic on them is checked.
    let bytes = |at: usize, count: usize| elf.get(at..at.checked_add(count)?);
    let half = |at: usize| Some(u16::from_le_bytes(bytes(at, 2)?.try_into().ok()?) as u64);
    let word = |at: usize| Some(u32::from_le_bytes(bytes(at, 4)?.try_into().ok()?) as u64);
    let long = |at: usize| Some(u64::from_le_bytes(bytes(at, 8)?.try_into().ok()?));
    // A field that is 4 bytes wide in 32-bit files and 8 in 64-bit ones.
    let address = |at32: usize, at64: usize| if wide { long(at64) } else { word(at32) };

    let (phoff, phentsize, phnum) = if wide { (long(0x20)?, half(0x36)?, half(0x38)?) } else { (word(0x1c)?, half(0x2a)?, half(0x2c)?) };
    if phentsize < if wide { 56 } else { 32 } {
        return None;
    }
    // (type, file offset, address, size in the file) of each segment.
    let segments: Vec<(u32, u64, u64, u64)> = (0..phnum)
        .map(|index| {
            let at = usize::try_from(phoff.checked_add(index * phentsize)?).ok()?;
            // The header is in the file whole, so the field offsets below stay in range.
            bytes(at, phentsize as usize)?;
            Some((word(at)? as u32, address(at + 4, at + 8)?, address(at + 8, at + 16)?, address(at + 16, at + 32)?))
        })
        .collect::<Option<_>>()?;
    let file_offset = |at: u64| {
        segments.iter()
            .find(|&&(kind, _, start, size)| kind == PT_LOAD && at >= start && at - start < size)
            .and_then(|&(_, offset, start, _)| offset.checked_add(at - start))
    };

    let &(_, dynamic, _, dynamic_size) = segments.iter().find(|segment| segment.0 == PT_DYNAMIC)?;
    let entry_size = if wide { 16 } else { 8 };
    let mut tags = Vec::new();
    for index in 0..dynamic_size / entry_size {
        let at = usize::try_from(dynamic.checked_add(index * entry_size)?).ok()?;
        bytes(at, entry_size as usize)?;
        let tag = if wide { long(at)? } else { word(at)? };
        if tag == DT_NULL {
            break;
        }
        tags.push((tag, if wide { long(at + 8)? } else { word(at + 4)? }));
    }
    let strings = file_offset(tags.iter().find(|(tag, _)| *tag == DT_STRTAB)?.1)?;
    tags.iter()
        .filter(|(tag, _)| *tag == DT_NEEDED)
        .map(|&(_, name)| {
            let start = usize::try_from(strings.checked_add(name)?).ok()?;
            let rest = elf.get(start..)?;
            Some(String::from_utf8_lossy(&rest[..rest.iter().position(|&byte| byte == 0)?]).into_owned())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal 64-bit library: one load segment holding the dynamic entries and their strings.
    fn library64(names: &[&str]) -> Vec<u8> {
        let mut strings = vec![0u8];
        let offsets: Vec<u64> = names.iter().map(|name| {
            let at = strings.len() as u64;
            strings.extend_from_slice(name.as_bytes());
            strings.push(0);
            at
        }).collect();
        let (phoff, dynamic) = (64u64, 64 + 2 * 56);
        let tags: Vec<(u64, u64)> = offsets.iter().map(|&at| (DT_NEEDED, at)).chain([(DT_STRTAB, 0), (DT_NULL, 0)]).collect();
        let strtab = dynamic + tags.len() as u64 * 16;
        // Addresses are the file offsets plus 0x1000.
        let tags: Vec<(u64, u64)> = tags.into_iter().map(|(tag, value)| if tag == DT_STRTAB { (tag, strtab + 0x1000) } else { (tag, value) }).collect();
        let total = strtab + strings.len() as u64;
        let mut elf = vec![0u8; 64];
        elf[..6].copy_from_slice(b"\x7fELF\x02\x01");
        elf[0x20..0x28].copy_from_slice(&phoff.to_le_bytes());
        elf[0x36..0x38].copy_from_slice(&56u16.to_le_bytes());
        elf[0x38..0x3a].copy_from_slice(&2u16.to_le_bytes());
        for (kind, offset, size) in [(PT_LOAD, 0, total), (PT_DYNAMIC, dynamic, strtab - dynamic)] {
            let mut header = vec![0u8; 56];
            header[..4].copy_from_slice(&kind.to_le_bytes());
            header[8..16].copy_from_slice(&offset.to_le_bytes());
            header[16..24].copy_from_slice(&(offset + 0x1000).to_le_bytes());
            header[32..40].copy_from_slice(&size.to_le_bytes());
            elf.extend(header);
        }
        for (tag, value) in tags {
            elf.extend(tag.to_le_bytes());
            elf.extend(value.to_le_bytes());
        }
        elf.extend(strings);
        elf
    }

    #[test]
    fn reads_needed_names() {
        let elf = library64(&["libGLESv2.so", "libOpenSLES.so", "libc.so"]);
        assert_eq!(needed(&elf).unwrap(), ["libGLESv2.so", "libOpenSLES.so", "libc.so"]);
        assert_eq!(needed(&library64(&[])).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn refuses_other_files() {
        assert!(needed(b"PK\x03\x04 not an elf").is_none());
        let mut cut = library64(&["libc.so"]);
        cut.truncate(100);
        assert!(needed(&cut).is_none());
    }

    #[test]
    fn survives_offsets_near_the_end_of_memory() {
        let mut elf = library64(&["libc.so"]);
        elf[0x20..0x28].copy_from_slice(&(u64::MAX - 8).to_le_bytes());
        assert!(needed(&elf).is_none());
        // A load segment whose file offset would wrap around.
        let mut elf = library64(&["libc.so"]);
        elf[64 + 8..64 + 16].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(needed(&elf).is_none());
        // Too-small program header entries.
        let mut elf = library64(&["libc.so"]);
        elf[0x36..0x38].copy_from_slice(&8u16.to_le_bytes());
        assert!(needed(&elf).is_none());
    }
}
