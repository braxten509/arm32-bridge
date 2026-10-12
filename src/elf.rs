//! Loads 32-bit ARM Android shared libraries (ELF32) into guest memory:
//! copies the segments, fixes up addresses (relocations), points imports at
//! bridge stand-ins, then runs the library's start-up code.

use std::path::{Path, PathBuf};

use crate::mem;
use crate::runtime::{call_guest, shared, stub_for, Uc};

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_ARM_EXIDX: u32 = 0x7000_0001;

const DT_NEEDED: u32 = 1;
const DT_PLTRELSZ: u32 = 2;
const DT_HASH: u32 = 4;
const DT_STRTAB: u32 = 5;
const DT_SYMTAB: u32 = 6;
const DT_INIT: u32 = 12;
const DT_REL: u32 = 17;
const DT_RELSZ: u32 = 18;
const DT_JMPREL: u32 = 23;
const DT_INIT_ARRAY: u32 = 25;
const DT_INIT_ARRAYSZ: u32 = 27;

const R_ARM_ABS32: u32 = 2;
const R_ARM_GLOB_DAT: u32 = 21;
const R_ARM_JUMP_SLOT: u32 = 22;
const R_ARM_RELATIVE: u32 = 23;

const STT_OBJECT: u8 = 1;

pub struct Library {
    pub name: String,
    pub base: u32,
    pub size: u32,
    symbol_table: u32,
    string_table: u32,
    symbol_count: u32,
    pub init_functions: Vec<u32>,
}

pub struct Symbol {
    pub name: String,
    pub value: u32,
    pub kind: u8,
    pub defined: bool,
}

impl Library {
    pub fn symbol(&self, _uc: &Uc, index: u32) -> Symbol {
        let memory = crate::runtime::mem();
        let entry = self.symbol_table + index * 16;
        let name_offset = memory.read_u32(entry);
        let value = memory.read_u32(entry + 4);
        let info = memory.read_u8(entry + 12);
        let section = memory.read_u16(entry + 14);
        Symbol {
            name: memory.string(self.string_table + name_offset),
            value,
            kind: info & 0xF,
            defined: section != 0,
        }
    }

    pub fn export(&self, uc: &Uc, name: &str) -> Option<u32> {
        (1..self.symbol_count).find_map(|i| {
            let symbol = self.symbol(uc, i);
            (symbol.defined && symbol.name == name).then(|| self.base + symbol.value)
        })
    }

    pub fn exports(&self, uc: &Uc) -> Vec<(String, u32)> {
        (1..self.symbol_count)
            .map(|i| self.symbol(uc, i))
            .filter(|s| s.defined && !s.name.is_empty())
            .map(|s| (s.name, self.base + s.value))
            .collect()
    }
}

pub struct Loader {
    next_base: u32,
    search_dirs: Vec<PathBuf>,
    pub libraries: Vec<Library>,
}

impl Loader {
    pub fn new(search_dirs: Vec<PathBuf>) -> Loader {
        Loader { next_base: mem::LIB_BASE, search_dirs, libraries: Vec::new() }
    }

    /// Loads `path` and any 32-bit libraries it needs that sit next to it.
    /// Start-up code is not run here; see `run_init`.
    pub fn load(&mut self, uc: &mut Uc, path: &Path) -> Result<usize, String> {
        let file = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if let Some(index) = self.libraries.iter().position(|l| l.name == name) {
            return Ok(index);
        }
        check_header(&file)?;

        let header_u32 = |offset: usize| u32::from_le_bytes(file[offset..offset + 4].try_into().unwrap());
        let header_u16 = |offset: usize| u16::from_le_bytes(file[offset..offset + 2].try_into().unwrap());
        let program_offset = header_u32(0x1C) as usize;
        let program_count = header_u16(0x2C) as usize;

        struct Segment {
            kind: u32,
            offset: u32,
            address: u32,
            file_size: u32,
            memory_size: u32,
        }
        let segments: Vec<Segment> = (0..program_count)
            .map(|i| {
                let at = program_offset + i * 32;
                Segment {
                    kind: header_u32(at),
                    offset: header_u32(at + 4),
                    address: header_u32(at + 8),
                    file_size: header_u32(at + 16),
                    memory_size: header_u32(at + 20),
                }
            })
            .collect();
        let end = segments
            .iter()
            .filter(|s| s.kind == PT_LOAD)
            .map(|s| s.address + s.memory_size)
            .max()
            .ok_or("no loadable segments")?;
        let size = (end + mem::PAGE - 1) & !(mem::PAGE - 1);
        let base = self.next_base;
        if base + size > mem::LIB_END {
            return Err("library area is full".into());
        }
        self.next_base = base + size + mem::PAGE;

        let memory = crate::runtime::mem();
        for segment in segments.iter().filter(|s| s.kind == PT_LOAD) {
            let bytes = &file[segment.offset as usize..(segment.offset + segment.file_size) as usize];
            memory.write_bytes(base + segment.address, bytes);
        }
        if let Some(exidx) = segments.iter().find(|s| s.kind == PT_ARM_EXIDX) {
            // C++ exception unwinding asks for this via dl_unwind_find_exidx.
            let entry = (base, base + size, base + exidx.address, exidx.memory_size / 8);
            shared().state.unwind_tables.push(entry);
        }
        let dynamic = segments.iter().find(|s| s.kind == PT_DYNAMIC).ok_or("no dynamic section")?;

        let mut tags: Vec<(u32, u32)> = Vec::new();
        let mut at = base + dynamic.address;
        loop {
            let (tag, value) = (memory.read_u32(at), memory.read_u32(at + 4));
            if tag == 0 {
                break;
            }
            tags.push((tag, value));
            at += 8;
        }
        let tag = |wanted: u32| tags.iter().find(|(t, _)| *t == wanted).map(|(_, v)| *v);
        let string_table = base + tag(DT_STRTAB).ok_or("no string table")?;
        let symbol_table = base + tag(DT_SYMTAB).ok_or("no symbol table")?;
        let symbol_count = memory.read_u32(base + tag(DT_HASH).ok_or("no DT_HASH")? + 4);

        // Load needed game libraries first so their exports can be used.
        for (_, offset) in tags.iter().filter(|(t, _)| *t == DT_NEEDED) {
            let needed = memory.string(string_table + offset);
            if let Some(found) = self.find_guest_library(&needed) {
                let index = self.load(uc, &found)?;
                for (name, address) in self.libraries[index].exports(uc) {
                    shared().guest_exports.entry(name).or_insert(address);
                }
            }
        }

        let mut library = Library {
            name,
            base,
            size,
            symbol_table,
            string_table,
            symbol_count,
            init_functions: Vec::new(),
        };

        let relocate = |uc: &mut Uc, table: Option<u32>, table_size: Option<u32>| -> Result<(), String> {
            let (Some(table), Some(table_size)) = (table, table_size) else { return Ok(()) };
            for i in 0..table_size / 8 {
                let entry = base + table + i * 8;
                let place = base + memory.read_u32(entry);
                let info = memory.read_u32(entry + 4);
                let (kind, symbol_index) = (info & 0xFF, info >> 8);
                match kind {
                    R_ARM_RELATIVE => memory.write_u32(place, memory.read_u32(place).wrapping_add(base)),
                    R_ARM_ABS32 | R_ARM_GLOB_DAT | R_ARM_JUMP_SLOT => {
                        let symbol = library.symbol(uc, symbol_index);
                        let target = resolve(uc, &library, &symbol);
                        let value = if kind == R_ARM_ABS32 {
                            memory.read_u32(place).wrapping_add(target)
                        } else {
                            target
                        };
                        memory.write_u32(place, value);
                    }
                    0 => {}
                    other => return Err(format!("unsupported relocation type {other} in {}", library.name)),
                }
            }
            Ok(())
        };
        relocate(uc, tag(DT_REL), tag(DT_RELSZ))?;
        relocate(uc, tag(DT_JMPREL), tag(DT_PLTRELSZ))?;

        if let Some(init) = tag(DT_INIT) {
            library.init_functions.push(base + init);
        }
        if let (Some(array), Some(array_size)) = (tag(DT_INIT_ARRAY), tag(DT_INIT_ARRAYSZ)) {
            for i in 0..array_size / 4 {
                let function = memory.read_u32(base + array + i * 4);
                if function != 0 && function != u32::MAX {
                    library.init_functions.push(function);
                }
            }
        }
        for (export, address) in library.exports(uc) {
            shared().guest_exports.entry(export).or_insert(address);
        }
        self.libraries.push(library);
        Ok(self.libraries.len() - 1)
    }

    fn find_guest_library(&self, name: &str) -> Option<PathBuf> {
        self.search_dirs.iter().map(|d| d.join(name)).find(|p| p.is_file())
    }

    /// Runs DT_INIT and every DT_INIT_ARRAY entry (C++ static constructors).
    pub fn run_init(&self, uc: &mut Uc, index: usize, verbose: bool) -> Result<usize, String> {
        let library = &self.libraries[index];
        for (i, function) in library.init_functions.iter().enumerate() {
            if verbose {
                eprintln!("[init] {} {}/{} at {function:#010x}", library.name, i + 1, library.init_functions.len());
            }
            call_guest(uc, *function, &[]).map_err(|e| {
                format!("start-up function {} of {} ({function:#010x}) failed: {e}", i + 1, library.name)
            })?;
        }
        Ok(library.init_functions.len())
    }
}

fn check_header(file: &[u8]) -> Result<(), String> {
    if file.len() < 52 || &file[0..4] != b"\x7fELF" {
        return Err("not an ELF file".into());
    }
    if file[4] != 1 {
        return Err("not a 32-bit ELF file".into());
    }
    let machine = u16::from_le_bytes([file[18], file[19]]);
    if machine != 40 {
        return Err(format!("not ARM code (machine {machine})"));
    }
    Ok(())
}

/// Address for a symbol a relocation refers to: the library's own
/// definition, another loaded game library's export, or a bridge stand-in.
fn resolve(_uc: &mut Uc, library: &Library, symbol: &crate::elf::Symbol) -> u32 {
    if symbol.defined {
        return library.base + symbol.value;
    }
    if symbol.name.is_empty() {
        return 0;
    }
    if let Some(&address) = shared().guest_exports.get(&symbol.name) {
        return address;
    }
    if symbol.kind == STT_OBJECT {
        return crate::imports::data_import(&symbol.name);
    }
    stub_for(&symbol.name)
}
