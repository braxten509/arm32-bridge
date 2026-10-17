//! Finds and rewrites a loaded library's import slots.
//!
//! A library calls functions from other libraries through a table of
//! addresses (the GOT), filled in by Android's loader. Writing our own
//! function's address into a slot makes that library call ours instead. Slots
//! come from the library's relocations: JUMP_SLOT (calls), GLOB_DAT and ABS64
//! (addresses taken), including Android's packed relocation format (APS2).
//! Only the in-memory image is read; files on disk are never opened.

use std::ffi::CStr;

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_GNU_RELRO: u32 = 0x6474_e552;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;

const DT_NULL: i64 = 0;
const DT_PLTRELSZ: i64 = 2;
const DT_STRTAB: i64 = 5;
const DT_SYMTAB: i64 = 6;
const DT_RELA: i64 = 7;
const DT_RELASZ: i64 = 8;
const DT_JMPREL: i64 = 23;
const DT_ANDROID_RELA: i64 = 0x6000_0011;
const DT_ANDROID_RELASZ: i64 = 0x6000_0012;

// Relocation kinds that write a symbol's address (ARM64; x86-64 for tests on the PC).
#[cfg(target_arch = "aarch64")]
const ADDRESS_RELOCATIONS: [u32; 3] = [257, 1025, 1026];
#[cfg(target_arch = "x86_64")]
const ADDRESS_RELOCATIONS: [u32; 3] = [1, 6, 7];

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ProgramHeader {
    pub kind: u32,
    pub flags: u32,
    pub offset: u64,
    pub address: u64,
    pub physical_address: u64,
    pub file_size: u64,
    pub memory_size: u64,
    pub align: u64,
}

#[repr(C)]
struct Dynamic {
    tag: i64,
    value: u64,
}

#[repr(C)]
struct Symbol {
    name: u32,
    info: u8,
    other: u8,
    section: u16,
    value: u64,
    size: u64,
}

#[repr(C)]
struct Rela {
    offset: u64,
    info: u64,
    addend: i64,
}

/// What dl_iterate_phdr reports for one library (bionic and glibc agree on this layout).
#[repr(C)]
struct PhdrInfo {
    address: usize,
    name: *const libc::c_char,
    headers: *const ProgramHeader,
    header_count: u16,
    adds: u64,
    subs: u64,
}

extern "C" {
    fn dl_iterate_phdr(
        callback: unsafe extern "C" fn(*mut PhdrInfo, usize, *mut libc::c_void) -> libc::c_int,
        data: *mut libc::c_void,
    ) -> libc::c_int;
}

/// A loaded library: where it sits and its program headers (in its own memory).
#[derive(Clone)]
pub struct Image {
    pub name: String,
    pub base: usize,
    pub headers: Vec<ProgramHeader>,
}

/// Every loaded library, in load order. Their memory may be gone by the time
/// the list is used (a library can be unloaded): use `with_each_image` to read
/// or change a library's memory.
pub fn images() -> Vec<Image> {
    let mut list = Vec::new();
    with_each_image(|image| list.push(image.clone()));
    list
}

/// Calls `visit` for every loaded library while Android's loader lock is held,
/// so none of them can be unloaded meanwhile. `visit` must not load or unload
/// libraries.
pub fn with_each_image(mut visit: impl FnMut(&Image)) {
    unsafe extern "C" fn each(info: *mut PhdrInfo, _size: usize, data: *mut libc::c_void) -> libc::c_int {
        let info = &*info;
        let visit = &mut *(data as *mut &mut dyn FnMut(&Image));
        let name = if info.name.is_null() { String::new() } else { CStr::from_ptr(info.name).to_string_lossy().into_owned() };
        let headers = if info.headers.is_null() {
            Vec::new()
        } else {
            std::slice::from_raw_parts(info.headers, info.header_count as usize).to_vec()
        };
        visit(&Image { name, base: info.address, headers });
        0
    }
    let mut visit: &mut dyn FnMut(&Image) = &mut visit;
    unsafe { dl_iterate_phdr(each, &mut visit as *mut &mut dyn FnMut(&Image) as *mut libc::c_void) };
}

/// How many libraries have been loaded so far (the loader's own counter): a
/// cheap way to notice new ones.
pub fn load_count() -> u64 {
    unsafe extern "C" fn first(info: *mut PhdrInfo, size: usize, data: *mut libc::c_void) -> libc::c_int {
        // Older loaders report a shorter record without the counters.
        let has_counters = size >= std::mem::offset_of!(PhdrInfo, subs) + 8;
        *(data as *mut u64) = if has_counters { (*info).adds } else { u64::MAX };
        1
    }
    let mut count = 0u64;
    unsafe { dl_iterate_phdr(first, &mut count as *mut u64 as *mut libc::c_void) };
    count
}

impl Image {
    /// True when `address .. address + length` lies inside one executable, readable segment.
    pub fn holds_code(&self, address: usize, length: usize) -> bool {
        self.headers.iter().any(|header| {
            let start = self.base + header.address as usize;
            header.kind == PT_LOAD
                && header.flags & (PF_X | PF_R) == (PF_X | PF_R)
                && address >= start
                && address + length <= start + header.memory_size as usize
        })
    }

    /// True when `address` lies in this library's loaded segments.
    pub fn contains(&self, address: usize) -> bool {
        self.headers.iter().any(|header| {
            let start = self.base + header.address as usize;
            header.kind == PT_LOAD && address >= start && address < start + header.memory_size as usize
        })
    }

    fn dynamic(&self) -> Option<*const Dynamic> {
        let header = self.headers.iter().find(|header| header.kind == PT_DYNAMIC)?;
        Some((self.base + header.address as usize) as *const Dynamic)
    }

    /// The library's imported-function slots: (name, slot address) for each
    /// relocation that writes the address of a symbol `wanted` accepts.
    pub fn import_slots(&self, wanted: &dyn Fn(&str) -> bool) -> Vec<(String, usize)> {
        let Some(mut entry) = self.dynamic() else { return Vec::new() };
        let (mut symbols, mut strings) = (0usize, 0usize);
        let (mut plt, mut plt_size, mut rela, mut rela_size, mut packed, mut packed_size) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
        // Android doesn't relocate the dynamic section's addresses; other loaders do.
        let at = |value: u64| if (value as usize) < self.base { self.base + value as usize } else { value as usize };
        unsafe {
            while (*entry).tag != DT_NULL {
                let value = (*entry).value;
                match (*entry).tag {
                    DT_SYMTAB => symbols = at(value),
                    DT_STRTAB => strings = at(value),
                    DT_JMPREL => plt = at(value),
                    DT_PLTRELSZ => plt_size = value as usize,
                    DT_RELA => rela = at(value),
                    DT_RELASZ => rela_size = value as usize,
                    DT_ANDROID_RELA => packed = at(value),
                    DT_ANDROID_RELASZ => packed_size = value as usize,
                    _ => {}
                }
                entry = entry.add(1);
            }
        }
        if symbols == 0 || strings == 0 {
            return Vec::new();
        }
        let mut relocations: Vec<(u64, u64)> = Vec::new();
        for (table, size) in [(plt, plt_size), (rela, rela_size)] {
            if table != 0 && size != 0 {
                let entries = unsafe { std::slice::from_raw_parts(table as *const Rela, size / std::mem::size_of::<Rela>()) };
                relocations.extend(entries.iter().map(|entry| (entry.offset, entry.info)));
            }
        }
        if packed != 0 && packed_size != 0 {
            let data = unsafe { std::slice::from_raw_parts(packed as *const u8, packed_size) };
            relocations.extend(unpack_relocations(data).unwrap_or_default());
        }
        let mut found = Vec::new();
        for (offset, info) in relocations {
            let kind = (info & 0xffff_ffff) as u32;
            let index = (info >> 32) as usize;
            if index == 0 || !ADDRESS_RELOCATIONS.contains(&kind) {
                continue;
            }
            let symbol = unsafe { &*(symbols as *const Symbol).add(index) };
            // Only imports: a symbol the library defines itself is its own business.
            if symbol.section != 0 {
                continue;
            }
            let name = unsafe { CStr::from_ptr((strings + symbol.name as usize) as *const libc::c_char) };
            let Ok(name) = name.to_str() else { continue };
            if wanted(name) {
                found.push((name.to_string(), self.base + offset as usize));
            }
        }
        found
    }

    /// Writes `value` into the slot at `slot`. Slots in the read-only-after-load
    /// part (RELRO) are made writable for the write and read-only again after.
    pub fn write_slot(&self, slot: usize, value: usize) -> bool {
        let page = page_size();
        let start = slot & !(page - 1);
        let in_relro = self.headers.iter().any(|header| {
            let begin = self.base + header.address as usize;
            header.kind == PT_GNU_RELRO && slot >= begin && slot + 8 <= begin + header.memory_size as usize
        });
        let segment_flags = self.headers.iter().find_map(|header| {
            let begin = self.base + header.address as usize;
            (header.kind == PT_LOAD && slot >= begin && slot + 8 <= begin + header.memory_size as usize).then_some(header.flags)
        });
        let Some(flags) = segment_flags else { return false };
        let writable_already = flags & PF_W != 0 && !in_relro;
        unsafe {
            if !writable_already && libc::mprotect(start as *mut libc::c_void, page, libc::PROT_READ | libc::PROT_WRITE) != 0 {
                return false;
            }
            (*(slot as *const std::sync::atomic::AtomicUsize)).store(value, std::sync::atomic::Ordering::SeqCst);
            if !writable_already {
                let mut protection = 0;
                if flags & PF_R != 0 || in_relro {
                    protection |= libc::PROT_READ;
                }
                if flags & PF_W != 0 && !in_relro {
                    protection |= libc::PROT_WRITE;
                }
                if flags & PF_X != 0 {
                    protection |= libc::PROT_EXEC;
                }
                libc::mprotect(start as *mut libc::c_void, page, protection);
            }
        }
        true
    }
}

fn page_size() -> usize {
    unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize }
}

/// Android's packed relocations ("APS2" + signed LEB128 numbers), as (offset, info) pairs.
pub fn unpack_relocations(data: &[u8]) -> Option<Vec<(u64, u64)>> {
    const GROUPED_BY_INFO: i64 = 1;
    const GROUPED_BY_OFFSET_DELTA: i64 = 2;
    const GROUPED_BY_ADDEND: i64 = 4;
    const GROUP_HAS_ADDEND: i64 = 8;
    if data.get(..4)? != b"APS2" {
        return None;
    }
    let mut at = 4;
    let mut next = || -> Option<i64> {
        let (mut value, mut shift) = (0i64, 0u32);
        loop {
            let byte = *data.get(at)?;
            at += 1;
            value |= ((byte & 0x7f) as i64) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                if shift < 64 && byte & 0x40 != 0 {
                    value |= -1i64 << shift;
                }
                return Some(value);
            }
            if shift >= 64 {
                return None;
            }
        }
    };
    let count = next()?;
    let mut offset = next()? as u64;
    let mut list = Vec::with_capacity(count.clamp(0, 1 << 20) as usize);
    let mut done = 0i64;
    while done < count {
        let size = next()?;
        let flags = next()?;
        if size <= 0 {
            return None;
        }
        let offset_delta = if flags & GROUPED_BY_OFFSET_DELTA != 0 { next()? } else { 0 };
        let mut info = if flags & GROUPED_BY_INFO != 0 { next()? as u64 } else { 0 };
        if flags & GROUP_HAS_ADDEND != 0 && flags & GROUPED_BY_ADDEND != 0 {
            next()?;
        }
        for _ in 0..size {
            offset = offset.wrapping_add(if flags & GROUPED_BY_OFFSET_DELTA != 0 { offset_delta } else { next()? } as u64);
            if flags & GROUPED_BY_INFO == 0 {
                info = next()? as u64;
            }
            if flags & GROUP_HAS_ADDEND != 0 && flags & GROUPED_BY_ADDEND == 0 {
                next()?;
            }
            list.push((offset, info));
        }
        done += size;
    }
    Some(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leb(mut value: i64, out: &mut Vec<u8>) {
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            let last = (value == 0 && byte & 0x40 == 0) || (value == -1 && byte & 0x40 != 0);
            out.push(if last { byte } else { byte | 0x80 });
            if last {
                break;
            }
        }
    }

    #[test]
    fn packed_relocations_grouped_and_ungrouped() {
        let mut data = b"APS2".to_vec();
        for value in [
            4, 0x1000, // count, start offset
            2, 2 | 1, 8, (5 << 32) | 1026, // group of 2: same offset step 8, same info
            2, 8, // group of 2 with addends, offsets and infos one by one
            16, (7 << 32) | 1025, 0, 8, 1027, -4,
        ] {
            leb(value, &mut data);
        }
        let list = unpack_relocations(&data).unwrap();
        assert_eq!(
            list,
            vec![(0x1008, (5 << 32) | 1026), (0x1010, (5 << 32) | 1026), (0x1020, (7 << 32) | 1025), (0x1028, 1027)]
        );
    }

    #[test]
    fn packed_relocations_reject_other_formats() {
        assert!(unpack_relocations(b"APS1\x00").is_none());
        assert!(unpack_relocations(b"AP").is_none());
    }

    #[test]
    fn finds_this_test_programs_own_libc_imports() {
        // The test program itself imports libc functions through its GOT.
        let images = images();
        assert!(!images.is_empty());
        assert!(load_count() > 0);
        let any = images.iter().any(|image| !image.import_slots(&|name| name == "malloc").is_empty());
        assert!(any, "some loaded library imports malloc");
    }
}
