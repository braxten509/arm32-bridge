//! Places the game's 64-bit code in memory, applies its fix-ups and
//! connects its imports to our stand-ins.

use crate::macho::{BindKind, Image};
use crate::symbols::{self, Kind};
use crate::trap::StubPage;
use std::collections::{BTreeSet, HashSet};
use std::sync::{Mutex, OnceLock};

pub struct Loaded {
    pub image: Image,
    pub slide: u64,
    pub start: usize,
    pub end: usize,
    pub path: String,
    pub app_dir: String,
}

static LOADED: OnceLock<Loaded> = OnceLock::new();
static METHOD_NAMES: OnceLock<Vec<(usize, String)>> = OnceLock::new();

pub fn loaded() -> &'static Loaded {
    LOADED.get().expect("no game loaded")
}

pub fn try_loaded() -> Option<&'static Loaded> {
    LOADED.get()
}

/// A 32-bit game's program path and .app folder (its code runs on the
/// translator, so there is no 64-bit `Loaded`).
static GUEST32_APP: OnceLock<(String, String)> = OnceLock::new();

pub fn set_guest32_app(program_path: &str, app_dir: &str) {
    let _ = GUEST32_APP.set((program_path.to_string(), app_dir.to_string()));
}

/// The game's .app folder (64-bit or 32-bit game).
pub fn app_dir() -> String {
    try_app_dir().expect("no game loaded")
}

pub fn try_app_dir() -> Option<String> {
    match (try_loaded(), GUEST32_APP.get()) {
        (Some(loaded), _) => Some(loaded.app_dir.clone()),
        (None, Some((_, app_dir))) => Some(app_dir.clone()),
        (None, None) => None,
    }
}

/// The game's program file (64-bit or 32-bit game).
pub fn program_path() -> String {
    match (try_loaded(), GUEST32_APP.get()) {
        (Some(loaded), _) => loaded.path.clone(),
        (None, Some((path, _))) => path.clone(),
        (None, None) => panic!("no game loaded"),
    }
}

#[derive(Default)]
pub struct LoadReport {
    pub functions_found: usize,
    pub functions_missing: BTreeSet<String>,
    pub data_found: usize,
    pub data_missing: BTreeSet<String>,
    pub weak_left_empty: BTreeSet<String>,
    pub data_named: BTreeSet<String>,
    pub classes_missing: BTreeSet<String>,
}

pub static LOAD_REPORT: Mutex<Option<LoadReport>> = Mutex::new(None);

extern "C" {
    fn __clear_cache(start: *mut libc::c_char, end: *mut libc::c_char);
}

pub fn clear_cache(start: usize, end: usize) {
    unsafe { __clear_cache(start as *mut _, end as *mut _) }
}

/// Names a code address: "SwordGame!function+0x10", or a plain number.
pub fn describe_address(address: usize) -> String {
    let Some(loaded) = try_loaded() else {
        return describe_host_address(address);
    };
    if address < loaded.start || address >= loaded.end {
        return describe_host_address(address);
    }
    let original = (address as u64).wrapping_sub(loaded.slide);
    let symbols = &loaded.image.symbols;
    let index = symbols.partition_point(|s| s.address <= original);
    let program = loaded.path.rsplit('/').next().unwrap_or("game");
    let mut best: Option<(u64, String)> = (index > 0).then(|| (symbols[index - 1].address, symbols[index - 1].name.clone()));
    let methods = METHOD_NAMES.get_or_init(crate::objc::runtime::game_method_names);
    let method_index = methods.partition_point(|(imp, _)| *imp <= address);
    if method_index > 0 {
        let (imp, name) = &methods[method_index - 1];
        let method_original = (*imp as u64).wrapping_sub(loaded.slide);
        if best.as_ref().map(|(at, _)| method_original > *at).unwrap_or(true) {
            best = Some((method_original, name.clone()));
        }
    }
    match best {
        Some((at, name)) => format!("{program}!{name}+{:#x} ({original:#x})", original - at),
        None => format!("{program}+{original:#x}"),
    }
}

/// Names an address in our own program or a system library ("file+offset"),
/// so `llvm-symbolizer --obj=<file> <offset>` can find the line.
fn describe_host_address(address: usize) -> String {
    let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
    if address != 0 && unsafe { libc::dladdr(address as *const _, &mut info) } != 0 && !info.dli_fname.is_null() {
        let file = unsafe { std::ffi::CStr::from_ptr(info.dli_fname) }.to_string_lossy().into_owned();
        let name = file.rsplit('/').next().unwrap_or(&file).to_string();
        return format!("{name}+{:#x}", address - info.dli_fbase as usize);
    }
    format!("{address:#x}")
}

fn looks_like_string_constant(name: &str) -> bool {
    const ENDINGS: &[&str] = &["Notification", "NotificationName", "Key", "Domain", "Mode", "Identifier", "Name", "Scope"];
    name.chars().next().map(|c| c.is_ascii_uppercase() || c == 'k').unwrap_or(false)
        && ENDINGS.iter().any(|ending| name.ends_with(ending))
}

pub fn load(app_dir: &str, program_path: &str) -> Result<&'static Loaded, String> {
    let file = std::fs::read(program_path).map_err(|e| format!("can't read {program_path}: {e}"))?;
    let image = Image::parse(&file).map_err(|e| e.to_string())?;
    if image.encrypted {
        return Err("this program is still encrypted by the App Store".into());
    }
    let base = image.base_address();
    let top = image
        .segments
        .iter()
        .filter(|s| s.name != "__PAGEZERO")
        .map(|s| s.vm_address + s.vm_size)
        .max()
        .ok_or("program has no segments")?;
    let span = (top - base) as usize;
    let memory = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            span,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if memory == libc::MAP_FAILED {
        return Err(format!("can't reserve {span} bytes for the program"));
    }
    let start = memory as usize;
    let slide = (start as u64).wrapping_sub(base);
    for segment in image.segments.iter().filter(|s| s.name != "__PAGEZERO") {
        let from = segment.file_offset as usize;
        let size = segment.file_size.min(segment.vm_size) as usize;
        let bytes = image
            .data
            .get(from..from + size)
            .ok_or_else(|| format!("segment {} lies outside the file", segment.name))?;
        let to = (segment.vm_address - base) as usize;
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), (start + to) as *mut u8, size) };
    }
    let strengthened = strengthen_store_barriers(&image, start, base, span);
    if strengthened > 0 {
        crate::log!("{strengthened} store-only memory barriers made full barriers");
    }
    for &address in &image.rebases {
        let place = address.wrapping_add(slide) as *mut u64;
        unsafe { *place = (*place).wrapping_add(slide) };
    }

    let lazy_names: HashSet<&str> = image
        .binds
        .iter()
        .filter(|b| b.kind == BindKind::Lazy)
        .map(|b| b.symbol.as_str())
        .collect();
    let mut stubs = StubPage::new(4096);
    let mut report = LoadReport::default();
    let mut resolved: std::collections::HashMap<String, usize> = Default::default();
    for bind in image.binds.iter().filter(|b| b.kind != BindKind::Weak) {
        let value = match resolved.get(&bind.symbol) {
            Some(&value) => value,
            None => {
                let value = resolve(&image, slide, bind, &lazy_names, &mut stubs, &mut report);
                resolved.insert(bind.symbol.clone(), value);
                value
            }
        };
        let place = bind.address.wrapping_add(slide) as *mut u64;
        unsafe { *place = (value as u64).wrapping_add(bind.addend as u64) };
    }
    stubs.seal();

    let loaded = Loaded {
        image,
        slide,
        start,
        end: start + span,
        path: program_path.to_string(),
        app_dir: app_dir.to_string(),
    };
    LOADED.set(loaded).map_err(|_| "a game is already loaded")?;
    let loaded = self::loaded();
    crate::cxx::add_image(loaded);

    crate::objc::runtime::load_image(loaded)?;

    for segment in loaded.image.segments.iter().filter(|s| s.name != "__PAGEZERO") {
        let at = start + (segment.vm_address - base) as usize;
        let size = segment.vm_size as usize;
        let protection = segment.initial_protection as i32;
        let mut flags = 0;
        if protection & 1 != 0 {
            flags |= libc::PROT_READ;
        }
        if protection & 2 != 0 {
            flags |= libc::PROT_WRITE;
        }
        if protection & 4 != 0 {
            flags |= libc::PROT_EXEC;
            clear_cache(at, at + size);
        }
        if unsafe { libc::mprotect(at as *mut _, size, flags) } != 0 {
            return Err(format!("can't set access for segment {}", segment.name));
        }
    }
    *LOAD_REPORT.lock().unwrap() = Some(report);
    Ok(loaded)
}

/// Store-only memory barriers (`dsb st`, `dmb ishst`, ...) in the game's
/// code become full barriers. Some games use them on the reading side of
/// thread hand-offs too (Unreal Engine 3's render command queue: read the
/// write position, `dsb st`, read the command). That only orders writes, so
/// a phone core may read the command before it is fully written, which
/// Apple's cores never showed; the game's drawing thread then runs a
/// half-written command and crashes. A full barrier also orders reads and
/// is never wrong in place of a weaker one, only a little slower.
fn strengthen_store_barriers(image: &Image, start: usize, base: u64, span: usize) -> usize {
    const PURE_INSTRUCTIONS: u32 = 0x8000_0000;
    let mut count = 0;
    for section in image.segments.iter().flat_map(|s| &s.sections).filter(|s| s.flags & PURE_INSTRUCTIONS != 0) {
        // Only whole, aligned words inside the program's memory.
        let Some(offset) = section.address.checked_sub(base) else { continue };
        let (at, size) = (start + offset as usize, section.size as usize / 4 * 4);
        if at % 4 != 0 || offset as usize + size > span {
            continue;
        }
        let words = unsafe { std::slice::from_raw_parts_mut(at as *mut u32, size / 4) };
        count += strengthen_barriers_in(words);
    }
    count
}

/// DSB/DMB with an ST option (OSHST, NSHST, ISHST, ST) become the same
/// barrier with the full option (OSH, NSH, ISH, SY). Returns how many.
fn strengthen_barriers_in(words: &mut [u32]) -> usize {
    let mut count = 0;
    for word in words.iter_mut() {
        let kind = *word & 0xFFFF_F0FF;
        let option = (*word >> 8) & 0xF;
        if (kind == 0xD503_309F || kind == 0xD503_30BF) && option & 0b11 == 0b10 {
            *word |= 1 << 8;
            count += 1;
        }
    }
    count
}

/// Apple libraries whose functions are the same on Linux and Android (same
/// layouts, no extra arguments), so the system's own copy is used.
fn same_library_function(library: &str, name: &str) -> Option<usize> {
    // gzprintf takes extra arguments, which Apple passes differently.
    let (system_names, excluded): (&[&str], &[&str]) = match library.rsplit('/').next()? {
        "libz.1.dylib" | "libz.dylib" => (&["libz.so", "libz.so.1"], &["gzprintf"]),
        _ => return None,
    };
    if excluded.contains(&name) {
        return None;
    }
    let symbol = std::ffi::CString::new(name).ok()?;
    for system in system_names {
        let file = std::ffi::CString::new(*system).unwrap();
        let handle = unsafe { libc::dlopen(file.as_ptr(), libc::RTLD_NOW) };
        if !handle.is_null() {
            let found = unsafe { libc::dlsym(handle, symbol.as_ptr()) } as usize;
            return (found != 0).then_some(found);
        }
    }
    None
}

fn resolve(
    image: &Image,
    slide: u64,
    bind: &crate::macho::Bind,
    lazy_names: &HashSet<&str>,
    stubs: &mut StubPage,
    report: &mut LoadReport,
) -> usize {
    let name = bind.symbol.as_str();
    if bind.library_ordinal == 0 || bind.library_ordinal == -1 {
        if let Some(symbol) = image.symbols.iter().find(|s| s.name == name) {
            return symbol.address.wrapping_add(slide) as usize;
        }
    }
    if let Some((address, kind)) = symbols::lookup(name) {
        match kind {
            Kind::Function => report.functions_found += 1,
            Kind::Data => report.data_found += 1,
        }
        return address;
    }
    if let Some(address) = name.strip_prefix('_').and_then(crate::fw::gl::core_function) {
        report.functions_found += 1;
        return address;
    }
    let library = usize::try_from(bind.library_ordinal).ok().and_then(|n| image.libraries.get(n.wrapping_sub(1)));
    if let Some(address) = library.and_then(|l| same_library_function(l, name.strip_prefix('_').unwrap_or(name))) {
        report.functions_found += 1;
        return address;
    }
    let class_ref = name
        .strip_prefix("_OBJC_CLASS_$_")
        .map(|n| (n, false))
        .or_else(|| name.strip_prefix("_OBJC_METACLASS_$_").map(|n| (n, true)));
    if let Some((class_name, meta)) = class_ref {
        match crate::objc::runtime::existing_class(class_name, meta) {
            Some(_) => report.data_found += 1,
            None => {
                report.classes_missing.insert(class_name.to_string());
            }
        }
        return crate::objc::runtime::placeholder_class(class_name, meta);
    }
    let pretty = name.strip_prefix('_').unwrap_or(name).to_string();
    let is_function = lazy_names.contains(name) || name == "dyld_stub_binder";
    if !is_function && looks_like_string_constant(&pretty) {
        // Apple's NSString constants (notification names, keys, domains)
        // nearly always hold their own name as their text.
        report.data_named.insert(pretty.clone());
        return symbols::leak(crate::fw::string::make(&pretty));
    }
    if bind.weak_import {
        report.weak_left_empty.insert(pretty);
        return 0;
    }
    if is_function {
        report.functions_missing.insert(pretty.clone());
        return stubs.stub(&pretty);
    }
    report.data_missing.insert(pretty);
    // Zeroed room: reads see 0/null instead of crashing on a bad address.
    symbols::leak([0u64; 64])
}

#[cfg(test)]
mod tests {
    use super::strengthen_barriers_in;

    #[test]
    fn store_only_barriers_become_full_barriers() {
        let dsb_st = 0xD503_3E9F;
        let dmb_ishst = 0xD503_3ABF;
        let dmb_ishld = 0xD503_39BF;
        let dsb_sy = 0xD503_3F9F;
        let nop = 0xD503_201F;
        let mut code = [dsb_st, dmb_ishst, dmb_ishld, dsb_sy, nop, 0x9100_0400];
        assert_eq!(strengthen_barriers_in(&mut code), 2);
        assert_eq!(code, [0xD503_3F9F, 0xD503_3BBF, dmb_ishld, dsb_sy, nop, 0x9100_0400]);
    }
}
