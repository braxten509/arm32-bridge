//! Loads 32-bit ARM Android shared libraries (ELF32) into guest memory:
//! copies the segments, fixes up addresses (relocations), points imports at
//! bridge stand-ins, then runs the library's start-up code.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use crate::mem;
use crate::runtime::{call_guest, shared, stub_for, Uc};

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_ARM_EXIDX: u32 = 0x7000_0001;
const PF_W: u32 = 2;

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
const STB_WEAK: u8 = 2;
const STT_FUNC: u8 = 2;
const R_ARM_GLOB_DAT: u32 = 21;
const R_ARM_JUMP_SLOT: u32 = 22;
const R_ARM_RELATIVE: u32 = 23;

const STT_OBJECT: u8 = 1;

/// 32-bit phone libraries that 64-bit-only phones no longer have, built into
/// the bridge (build.rs). A game's own copy, if it ships one, comes first.
const BUILT_IN: &[(&str, &[u8])] = &[("libz.so", include_bytes!(concat!(env!("OUT_DIR"), "/guest/libz.so")))];

/// File name ending of a 32-bit library packed next to its 64-bit replacement.
pub const ORIGINAL_SUFFIX: &str = "-arm32.so";

pub struct Library {
    pub name: String,
    pub base: u32,
    pub size: u32,
    symbol_table: u32,
    string_table: u32,
    symbol_count: u32,
    pub init_functions: Vec<u32>,
    /// Whether `init_functions` have been handed out to run.
    started: bool,
    /// Built against the platform's own C library (e.g. Apple's), not bionic:
    /// its imports go to the platform's table (see `PlatformHooks::library_import`).
    pub platform_abi: bool,
}

pub struct Symbol {
    pub name: String,
    pub value: u32,
    pub kind: u8,
    pub defined: bool,
    /// STB_WEAK: may stay undefined (its address is then 0).
    pub weak: bool,
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
            weak: info >> 4 == STB_WEAK,
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
    /// The library being loaded now is built for the platform's C library.
    platform_abi: bool,
}

impl Loader {
    pub fn new(search_dirs: Vec<PathBuf>) -> Loader {
        Loader { next_base: mem::layout().libraries.0, search_dirs, libraries: Vec::new(), platform_abi: false }
    }

    /// Loads `path` and any 32-bit libraries it needs that sit next to it.
    /// Start-up code is not run here; see `run_init`.
    pub fn load(&mut self, uc: &mut Uc, path: &Path) -> Result<usize, String> {
        let file = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        self.load_bytes(uc, name, &file)
    }

    /// Loads a library from memory (see `load`).
    pub fn load_bytes(&mut self, uc: &mut Uc, name: String, file: &[u8]) -> Result<usize, String> {
        if let Some(index) = self.libraries.iter().position(|l| l.name == name) {
            return Ok(index);
        }
        check_header(file)?;

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
            flags: u32,
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
                    flags: header_u32(at + 24),
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
        if base + size > mem::layout().libraries.1 {
            return Err("library area is full".into());
        }
        self.next_base = base + size + mem::PAGE;
        crate::log::line(&format!("[bridge] {name} at {base:#010x}-{:#010x}", base + size));

        let memory = crate::runtime::mem();
        for segment in segments.iter().filter(|s| s.kind == PT_LOAD) {
            let bytes = &file[segment.offset as usize..(segment.offset + segment.file_size) as usize];
            memory.write_bytes(base + segment.address, bytes);
            if segment.flags & PF_W == 0 {
                mem::add_fixed(base + segment.address, base + segment.address + segment.memory_size);
            }
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
            let index = if let Some(found) = self.find_guest_library(&needed) {
                Some(self.load(uc, &found)?)
            } else if let Some((_, bytes)) = BUILT_IN.iter().find(|(name, _)| *name == needed) {
                // The built-in libraries are bionic's.
                let outer = std::mem::replace(&mut self.platform_abi, false);
                let loaded = self.load_bytes(uc, needed.clone(), bytes);
                self.platform_abi = outer;
                Some(loaded?)
            } else {
                None
            };
            if let Some(index) = index {
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
            started: false,
            platform_abi: self.platform_abi,
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
        // Functions the bridge steps into (wraps.rs), before any code here runs.
        for index in 1..library.symbol_count {
            let symbol = library.symbol(uc, index);
            if symbol.defined && symbol.kind == STT_FUNC && crate::wraps::wanted(&library.name, &symbol.name) {
                let size = memory.read_u32(library.symbol_table + index * 16 + 8);
                crate::wraps::wrap(&library.name, &symbol.name, base + symbol.value, size);
            }
        }

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

    /// A needed library's 32-bit file. In a converted APK the original of
    /// lib<name>.so sits next to its 64-bit stand-in as lib<name>-arm32.so.
    fn find_guest_library(&self, name: &str) -> Option<PathBuf> {
        let original = format!("{}{ORIGINAL_SUFFIX}", name.strip_suffix(".so").unwrap_or(name));
        let is_arm32 = |path: &PathBuf| {
            let mut header = [0u8; 20];
            std::fs::File::open(path).and_then(|mut file| std::io::Read::read_exact(&mut file, &mut header)).is_ok()
                && header[..5] == *b"\x7fELF\x01"
                && u16::from_le_bytes([header[18], header[19]]) == 40 // EM_ARM
        };
        self.search_dirs
            .iter()
            .flat_map(|d| [d.join(&original), d.join(name)])
            .find(|p| p.is_file() && is_arm32(p))
    }

    /// Start-up code (DT_INIT, then DT_INIT_ARRAY: C++ static constructors) of
    /// libraries that haven't started yet, dependencies first. Each library's
    /// code is handed out once.
    fn take_pending_init(&mut self) -> Vec<(String, Vec<u32>)> {
        self.libraries
            .iter_mut()
            .filter(|library| !library.started)
            .map(|library| {
                library.started = true;
                (library.name.clone(), library.init_functions.clone())
            })
            .collect()
    }
}

/// The one loader every game part shares, so parts Java loads and parts the
/// game's own code opens (dlopen) live in the same guest and see each other.
static SHARED_LOADER: Mutex<Option<Loader>> = Mutex::new(None);

/// Never held while guest code runs: start-up code may open more parts.
fn shared_loader() -> MutexGuard<'static, Option<Loader>> {
    SHARED_LOADER.lock().unwrap_or_else(|e| e.into_inner())
}

/// Sets where the shared loader finds the game's libraries. Only the first call counts.
pub fn set_search_dirs(search_dirs: Vec<PathBuf>) {
    let mut loader = shared_loader();
    if loader.is_none() {
        *loader = Some(Loader::new(search_dirs));
    }
}

/// bionic holds one recursive lock across dlopen (loading and start-up code)
/// and dlsym, so no other thread sees a library whose start-up code is still
/// running; a constructor that opens more libraries just goes deeper. Same
/// here: (owner pthread_t, depth); other threads wait their turn.
static DL_LOCK: Mutex<(u32, u32)> = Mutex::new((0, 0));
const DL_LOCK_KEY: u64 = 4 << 32;

/// Holds the dynamic linker lock until dropped.
pub struct DlLock;

pub fn lock_dl() -> DlLock {
    let me = crate::imports::threads::current_handle();
    loop {
        {
            let mut lock = DL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            if lock.1 == 0 || lock.0 == me {
                *lock = (me, lock.1 + 1);
                return DlLock;
            }
        }
        // Checked again under the waiter list's lock (the futex rule): a
        // release between the check above and the sleep is not missed.
        crate::runtime::wait_until(DL_LOCK_KEY, None, || {
            let lock = DL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            lock.1 != 0 && lock.0 != me
        });
    }
}

impl Drop for DlLock {
    fn drop(&mut self) {
        let mut lock = DL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        lock.1 -= 1;
        if lock.1 == 0 {
            lock.0 = 0;
            drop(lock);
            crate::runtime::wake_on(DL_LOCK_KEY);
        }
    }
}

/// Loads `path` (and the game libraries it needs) into the shared loader,
/// runs start-up code that hasn't run yet, and returns the library's index.
/// Loading a library again returns the same index.
pub fn open(uc: &mut Uc, path: &Path, verbose: bool) -> Result<usize, String> {
    let _dl = lock_dl();
    let (index, pending) = {
        let mut loader = shared_loader();
        let loader = loader.as_mut().ok_or("the loader isn't set up")?;
        let index = loader.load(uc, path)?;
        (index, loader.take_pending_init())
    };
    for (name, functions) in pending {
        for (i, function) in functions.iter().enumerate() {
            if verbose {
                eprintln!("[init] {name} {}/{} at {function:#010x}", i + 1, functions.len());
            }
            call_guest(uc, *function, &[])
                .map_err(|e| format!("start-up function {} of {name} ({function:#010x}) failed: {e}", i + 1))?;
        }
    }
    Ok(index)
}

/// Loads one of the libraries the bridge carries (e.g. "libz.so") into the
/// shared loader and runs its start-up code; its exports join `guest_exports`.
pub fn open_built_in(uc: &mut Uc, name: &str) -> Result<usize, String> {
    let (_, bytes) = BUILT_IN.iter().find(|(built_in, _)| *built_in == name).ok_or_else(|| format!("no built-in library {name}"))?;
    open_bytes(uc, name, bytes, false)
}

/// Loads a library from memory into the shared loader (as `open_built_in`).
/// `platform_abi`: built for the platform's own C library, not bionic.
pub fn open_bytes(uc: &mut Uc, name: &str, bytes: &[u8], platform_abi: bool) -> Result<usize, String> {
    let _dl = lock_dl();
    let (index, pending) = {
        let mut loader = shared_loader();
        let loader = loader.get_or_insert_with(|| Loader::new(Vec::new()));
        loader.platform_abi = platform_abi;
        let loaded = loader.load_bytes(uc, name.to_string(), bytes);
        loader.platform_abi = false;
        (loaded?, loader.take_pending_init())
    };
    for (library, functions) in pending {
        for function in functions {
            call_guest(uc, function, &[]).map_err(|e| format!("start-up code of {library} ({function:#010x}) failed: {e}"))?;
        }
    }
    Ok(index)
}

/// For game code opening a library by name or path (dlopen): opens the
/// game's own 32-bit library of that name, if it has one. None means it is
/// not a game library (a phone library such as libGLESv2.so, or missing).
pub fn open_by_name(uc: &mut Uc, name: &str) -> Option<Result<usize, String>> {
    let found = shared_loader().as_ref()?.find_guest_library(name)?;
    Some(open(uc, &found, false))
}

/// Runs `f` with the shared loader (set up by `set_search_dirs`).
pub fn with_loader<R>(f: impl FnOnce(&Loader) -> R) -> R {
    f(shared_loader().as_ref().expect("the loader isn't set up"))
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
    if crate::runtime::other_platform() {
        if let Some(import) = crate::runtime::platform().library_import {
            return import(&symbol.name, symbol.kind == STT_OBJECT, symbol.weak, library.platform_abi);
        }
    }
    // Data the bridge knows by name counts as data even when the import isn't marked as an object.
    if symbol.kind == STT_OBJECT || crate::imports::known_data_import(&symbol.name) {
        return crate::imports::data_import(&symbol.name);
    }
    // Like the real loader: a weak function nobody defines is NULL, and game
    // code checks for that (C++ thread_local init hooks, optional features).
    if symbol.weak && crate::imports::handler_for(&symbol.name).is_none() {
        return 0;
    }
    stub_for(&symbol.name)
}
