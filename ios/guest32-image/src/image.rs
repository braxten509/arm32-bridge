//! The Mach-O32 header, load commands, segments, sections, symbols and entry.

use crate::error::ErrorKind::{
    Encrypted, InvalidReference, Malformed, NoMatchingSlice, OutOfRange, Overflow, Overlap, Truncated, Unsupported,
};
use crate::error::{fail, Error, Result};
use crate::opcodes::{self, Bind32, BindSource, LibraryRef, Rebase32, SegmentSpan, WeakDefinition};
use crate::read::{bytes, c_string, name16, range, u16_le, u32_be, u32_le, u64_le, u8_at};

pub const CPU_TYPE_ARM: u32 = 12;
/// Header flag: the program may be loaded at any page-aligned slide.
pub const MH_PIE: u32 = 0x0020_0000;

const FAT_MAGIC: u32 = 0xCAFE_BABE;
const FAT_MAGIC_64: u32 = 0xCAFE_BABF;
const MH_MAGIC: u32 = 0xFEED_FACE;
const MH_CIGAM: u32 = 0xCEFA_EDFE;
const MH_MAGIC_64: u32 = 0xFEED_FACF;
const MH_EXECUTE: u32 = 2;
const MH_DYLIB: u32 = 6;
const MH_BUNDLE: u32 = 8;
const HEADER_SIZE: u64 = 28;
const LC_REQ_DYLD: u32 = 0x8000_0000;

const LC_SEGMENT: u32 = 0x1;
const LC_SYMTAB: u32 = 0x2;
const LC_THREAD: u32 = 0x4;
const LC_UNIXTHREAD: u32 = 0x5;
const LC_DYSYMTAB: u32 = 0xB;
const LC_LOAD_DYLIB: u32 = 0xC;
const LC_ID_DYLIB: u32 = 0xD;
const LC_ROUTINES: u32 = 0x11;
const LC_SEGMENT_64: u32 = 0x19;
const LC_LAZY_LOAD_DYLIB: u32 = 0x20;
const LC_ENCRYPTION_INFO: u32 = 0x21;
const LC_DYLD_INFO: u32 = 0x22;
const LC_LOAD_WEAK_DYLIB: u32 = 0x8000_0018;
const LC_REEXPORT_DYLIB: u32 = 0x8000_001F;
const LC_DYLD_INFO_ONLY: u32 = 0x8000_0022;
const LC_LOAD_UPWARD_DYLIB: u32 = 0x8000_0023;
const LC_MAIN: u32 = 0x8000_0028;
const LC_ENCRYPTION_INFO_64: u32 = 0x2C;

/// Commands that only describe the file (signatures, versions, tables for
/// tools). They are kept in [`Image32::other_commands`] but change nothing
/// about loading.
const DESCRIPTIVE_COMMANDS: &[u32] = &[
    0xE,         // LC_LOAD_DYLINKER
    0x1B,        // LC_UUID
    0x1D,        // LC_CODE_SIGNATURE
    0x1E,        // LC_SEGMENT_SPLIT_INFO
    0x24,        // LC_VERSION_MIN_MACOSX
    0x25,        // LC_VERSION_MIN_IPHONEOS
    0x26,        // LC_FUNCTION_STARTS
    0x27,        // LC_DYLD_ENVIRONMENT
    0x29,        // LC_DATA_IN_CODE
    0x2A,        // LC_SOURCE_VERSION
    0x2B,        // LC_DYLIB_CODE_SIGN_DRS
    0x2D,        // LC_LINKER_OPTION
    0x2E,        // LC_LINKER_OPTIMIZATION_HINT
    0x2F,        // LC_VERSION_MIN_TVOS
    0x30,        // LC_VERSION_MIN_WATCHOS
    0x31,        // LC_NOTE
    0x32,        // LC_BUILD_VERSION
    0x8000_001C, // LC_RPATH
];

const VM_PROT_WRITE: u32 = 0x2;
const VM_PROT_EXECUTE: u32 = 0x4;

/// The 32-bit ARM CPU kinds a slice can be built for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArmSubtype {
    V6,
    V7,
    V7F,
    V7S,
    V7K,
    Other(u32),
}

impl ArmSubtype {
    pub fn from_raw(value: u32) -> Self {
        match value & 0x00FF_FFFF {
            6 => ArmSubtype::V6,
            9 => ArmSubtype::V7,
            10 => ArmSubtype::V7F,
            11 => ArmSubtype::V7S,
            12 => ArmSubtype::V7K,
            other => ArmSubtype::Other(other),
        }
    }
}

/// Which slice of a universal file to read, most wanted first.
#[derive(Clone, Debug)]
pub struct SliceChoice {
    pub preference: Vec<ArmSubtype>,
}

impl Default for SliceChoice {
    /// Plain armv7 first: armv7s adds instructions (integer divide, VFPv4)
    /// that a translator may not cover; armv6 runs last.
    fn default() -> Self {
        SliceChoice { preference: vec![ArmSubtype::V7, ArmSubtype::V7S, ArmSubtype::V6] }
    }
}

/// Where the chosen 32-bit ARM program sits inside the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThinSlice {
    pub file_offset: u64,
    pub size: u64,
    pub subtype: ArmSubtype,
    /// Every 32-bit ARM subtype a universal file offered (empty for thin files).
    pub alternatives: Vec<ArmSubtype>,
}

/// Picks the slice to read: the whole file if thin, else the best ARM slice.
pub fn choose_slice(file: &[u8], choice: &SliceChoice) -> Result<ThinSlice> {
    let magic = u32_be(file, 0, "file magic")?;
    if magic == FAT_MAGIC_64 {
        return fail(Unsupported, "64-bit universal header (FAT_MAGIC_64)");
    }
    if magic != FAT_MAGIC {
        let subtype = ArmSubtype::from_raw(u32_le(file, 8, "cpu subtype")?);
        return Ok(ThinSlice { file_offset: 0, size: file.len() as u64, subtype, alternatives: Vec::new() });
    }
    let count = u64::from(u32_be(file, 4, "universal slice count")?);
    range(file, 8, count.checked_mul(20).ok_or_else(|| Error::new(Overflow, "slice count overflows"))?, "slice list")?;
    let mut slices = Vec::new();
    for index in 0..count {
        let at = 8 + index * 20;
        if u32_be(file, at, "slice cpu")? != CPU_TYPE_ARM {
            continue;
        }
        let subtype = ArmSubtype::from_raw(u32_be(file, at + 4, "slice subtype")?);
        let offset = u64::from(u32_be(file, at + 8, "slice offset")?);
        let size = u64::from(u32_be(file, at + 12, "slice size")?);
        slices.push((subtype, offset, size));
    }
    let alternatives: Vec<ArmSubtype> = slices.iter().map(|s| s.0).collect();
    for wanted in &choice.preference {
        if let Some(&(subtype, offset, size)) = slices.iter().find(|s| s.0 == *wanted) {
            range(file, offset, size, "chosen slice")?;
            if offset < 8 + count * 20 {
                return fail(Overlap, "chosen slice overlaps the universal header");
            }
            return Ok(ThinSlice { file_offset: offset, size, subtype, alternatives });
        }
    }
    fail(NoMatchingSlice, format!("no wanted 32-bit ARM slice; file has {alternatives:?}"))
}

/// A byte range inside the chosen slice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByteRange {
    pub offset: u32,
    pub size: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section32 {
    pub segment_name: String,
    pub name: String,
    pub address: u32,
    pub size: u32,
    pub offset: u32,
    pub align: u32,
    pub flags: u32,
    pub reserved1: u32,
    pub reserved2: u32,
}

impl Section32 {
    /// `flags & SECTION_TYPE`, e.g. 0x7 lazy pointers, 0x9 initializers.
    pub fn section_type(&self) -> u8 {
        (self.flags & 0xFF) as u8
    }

    /// Zero-fill sections take address space but no file bytes.
    pub fn is_zero_fill(&self) -> bool {
        matches!(self.section_type(), 0x1 | 0xC | 0x12)
    }

    pub fn contains(&self, address: u32) -> bool {
        address >= self.address && u64::from(address) < u64::from(self.address) + u64::from(self.size)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment32 {
    pub name: String,
    pub vm_address: u32,
    pub vm_size: u32,
    pub file_offset: u32,
    pub file_size: u32,
    pub max_protection: u32,
    pub initial_protection: u32,
    pub flags: u32,
    pub sections: Vec<Section32>,
}

impl Segment32 {
    /// The low no-access guard (normally `__PAGEZERO`): starts at 0, has no
    /// file bytes and no access rights. It is never mapped; the planner keeps
    /// it as a range the guest must leave unmapped.
    pub fn is_null_guard(&self) -> bool {
        self.vm_address == 0 && self.file_size == 0 && self.initial_protection == 0 && self.max_protection == 0
    }

    pub fn executable(&self) -> bool {
        self.initial_protection & VM_PROT_EXECUTE != 0
    }

    pub fn writable(&self) -> bool {
        self.initial_protection & VM_PROT_WRITE != 0
    }

    pub fn vm_end(&self) -> u64 {
        u64::from(self.vm_address) + u64::from(self.vm_size)
    }

    pub fn contains(&self, address: u32) -> bool {
        address >= self.vm_address && u64::from(address) < self.vm_end()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DylibKind {
    Normal,
    Weak,
    Reexport,
    Upward,
    Lazy,
}

/// A library load command; its position (from 1) is its bind ordinal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dylib {
    pub path: String,
    pub kind: DylibKind,
    pub current_version: u32,
    pub compatibility_version: u32,
}

/// `ARM_THREAD_STATE` as stored in `LC_UNIXTHREAD`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArmThreadState {
    pub r: [u32; 13],
    pub sp: u32,
    pub lr: u32,
    pub pc: u32,
    pub cpsr: u32,
}

/// How the program wants to be started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry32 {
    /// `LC_MAIN`: dyld calls `main(argc, argv, envp, apple)` as a normal
    /// function at `address` (bit 0 set means Thumb code).
    Main { address: u32, entry_offset: u64, stack_size: u64 },
    /// `LC_UNIXTHREAD`: dyld jumps to `pc` (the program's own `start`) with a
    /// kernel-style stack: argc at [sp], then argv, envp and apple pointers.
    /// It is not a function call; the state's registers are as stored.
    UnixThread(ArmThreadState),
}

impl Entry32 {
    /// Unslid entry address with the Thumb bit as stored in the file.
    pub fn address(&self) -> u32 {
        match self {
            Entry32::Main { address, .. } => *address,
            Entry32::UnixThread(state) => state.pc,
        }
    }

    pub fn is_thumb(&self) -> bool {
        match self {
            Entry32::Main { address, .. } => address & 1 != 0,
            Entry32::UnixThread(state) => state.pc & 1 != 0 || state.cpsr & 0x20 != 0,
        }
    }

    /// The address of the first instruction (Thumb bit cleared).
    pub fn code_address(&self) -> u32 {
        self.address() & !1
    }
}

/// One `nlist` entry, kept whole (stabs, undefined and weak entries included)
/// so a later weak-coalescing step has the raw facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol32 {
    pub name: String,
    pub string_index: u32,
    pub n_type: u8,
    pub n_sect: u8,
    pub n_desc: u16,
    pub value: u32,
}

impl Symbol32 {
    pub fn is_debug(&self) -> bool {
        self.n_type & 0xE0 != 0
    }
    pub fn is_external(&self) -> bool {
        !self.is_debug() && self.n_type & 0x01 != 0
    }
    pub fn is_undefined(&self) -> bool {
        !self.is_debug() && self.n_type & 0x0E == 0
    }
    pub fn is_section_defined(&self) -> bool {
        !self.is_debug() && self.n_type & 0x0E == 0x0E
    }
    /// `N_ARM_THUMB_DEF`: the value is Thumb code (add 1 when calling).
    pub fn is_thumb_definition(&self) -> bool {
        self.n_desc & 0x0008 != 0
    }
    /// `N_WEAK_DEF`: a weak definition that coalescing may replace.
    pub fn is_weak_definition(&self) -> bool {
        self.n_desc & 0x0080 != 0
    }
    /// `N_WEAK_REF`: an import that may be missing.
    pub fn is_weak_reference(&self) -> bool {
        self.n_desc & 0x0040 != 0
    }
    /// Two-level library ordinal of an undefined symbol.
    pub fn library_ordinal(&self) -> u8 {
        (self.n_desc >> 8) as u8
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncryptionInfo {
    pub offset: u32,
    pub size: u32,
    pub id: u32,
}

/// A decoded 32-bit ARM program. Holds no file bytes; segment and section
/// offsets are relative to [`Image32::slice`] (see [`Image32::slice_bytes`]).
#[derive(Clone, Debug)]
pub struct Image32 {
    pub slice: ThinSlice,
    pub cpu_subtype: u32,
    pub file_type: u32,
    pub flags: u32,
    pub segments: Vec<Segment32>,
    pub dylibs: Vec<Dylib>,
    /// The install name of a library image (`LC_ID_DYLIB`).
    pub install_name: Option<String>,
    pub entry: Option<Entry32>,
    pub rebases: Vec<Rebase32>,
    /// Normal, then weak, then lazy records, each in stream order.
    pub binds: Vec<Bind32>,
    pub weak_definitions: Vec<WeakDefinition>,
    pub symbols: Vec<Symbol32>,
    /// Raw `LC_DYSYMTAB` indirect table (symbol indexes or LOCAL/ABS marks).
    pub indirect_symbols: Vec<u32>,
    /// The export trie, bounds-checked but not decoded.
    pub export_trie: Option<ByteRange>,
    pub encryption: Option<EncryptionInfo>,
    /// Load commands kept but not interpreted: (command, range in the slice).
    pub other_commands: Vec<(u32, ByteRange)>,
}

struct DyldInfo {
    rebase: (u32, u32),
    bind: (u32, u32),
    weak: (u32, u32),
    lazy: (u32, u32),
    export: (u32, u32),
}

impl Image32 {
    pub fn parse(file: &[u8]) -> Result<Image32> {
        Self::parse_with(file, &SliceChoice::default())
    }

    pub fn parse_with(file: &[u8], choice: &SliceChoice) -> Result<Image32> {
        let slice = choose_slice(file, choice)?;
        let data = bytes(file, slice.file_offset, slice.size, "slice")?;
        let magic = u32_le(data, 0, "magic")?;
        match magic {
            MH_MAGIC => {}
            MH_MAGIC_64 => return fail(Unsupported, "64-bit Mach-O; this reader is for 32-bit images"),
            MH_CIGAM => return fail(Unsupported, "big-endian 32-bit Mach-O"),
            other => return fail(Malformed, format!("not a Mach-O file (magic {other:#x})")),
        }
        if u32_le(data, 4, "cpu type")? != CPU_TYPE_ARM {
            return fail(NoMatchingSlice, "not a 32-bit ARM image");
        }
        let cpu_subtype = u32_le(data, 8, "cpu subtype")?;
        if !slice.alternatives.is_empty() && ArmSubtype::from_raw(cpu_subtype) != slice.subtype {
            return fail(Malformed, "slice header disagrees with the universal header's subtype");
        }
        let file_type = u32_le(data, 12, "file type")?;
        if !matches!(file_type, MH_EXECUTE | MH_DYLIB | MH_BUNDLE) {
            return fail(Unsupported, format!("file type {file_type} (only programs, libraries and bundles)"));
        }
        let command_count = u32_le(data, 16, "command count")?;
        let commands_size = u64::from(u32_le(data, 20, "command size")?);
        let flags = u32_le(data, 24, "header flags")?;
        range(data, HEADER_SIZE, commands_size, "load commands")?;
        let commands_end = HEADER_SIZE + commands_size;

        let mut image = Image32 {
            slice: slice.clone(),
            cpu_subtype,
            file_type,
            flags,
            segments: Vec::new(),
            dylibs: Vec::new(),
            install_name: None,
            entry: None,
            rebases: Vec::new(),
            binds: Vec::new(),
            weak_definitions: Vec::new(),
            symbols: Vec::new(),
            indirect_symbols: Vec::new(),
            export_trie: None,
            encryption: None,
            other_commands: Vec::new(),
        };
        let mut symtab: Option<[u32; 4]> = None;
        let mut dysymtab: Option<[u32; 18]> = None;
        let mut dyld_info: Option<DyldInfo> = None;
        let mut main: Option<(u64, u64)> = None;
        let mut thread: Option<ArmThreadState> = None;

        let mut at = HEADER_SIZE;
        for index in 0..command_count {
            let command = u32_le(data, at, "load command")?;
            let size = u64::from(u32_le(data, at + 4, "load command size")?);
            if size < 8 || size % 4 != 0 {
                return fail(Malformed, format!("load command {index} has size {size}"));
            }
            let end = at + size;
            if end > commands_end {
                return fail(Malformed, format!("load command {index} runs past the command area"));
            }
            let body = &data[at as usize..end as usize];
            let need = |minimum: u64| -> Result<()> {
                if size < minimum {
                    return fail(Malformed, format!("load command {index} ({command:#x}) is shorter than {minimum}"));
                }
                Ok(())
            };
            match command {
                LC_SEGMENT => {
                    need(56)?;
                    image.segments.push(read_segment(body, index)?);
                }
                LC_SYMTAB => {
                    need(24)?;
                    if symtab.is_some() {
                        return fail(Malformed, "two LC_SYMTAB commands");
                    }
                    symtab = Some(std::array::from_fn(|i| u32_le(body, 8 + 4 * i as u64, "").unwrap()));
                }
                LC_DYSYMTAB => {
                    need(80)?;
                    if dysymtab.is_some() {
                        return fail(Malformed, "two LC_DYSYMTAB commands");
                    }
                    dysymtab = Some(std::array::from_fn(|i| u32_le(body, 8 + 4 * i as u64, "").unwrap()));
                }
                LC_LOAD_DYLIB | LC_LOAD_WEAK_DYLIB | LC_REEXPORT_DYLIB | LC_LOAD_UPWARD_DYLIB | LC_LAZY_LOAD_DYLIB => {
                    need(24)?;
                    let kind = match command {
                        LC_LOAD_DYLIB => DylibKind::Normal,
                        LC_LOAD_WEAK_DYLIB => DylibKind::Weak,
                        LC_REEXPORT_DYLIB => DylibKind::Reexport,
                        LC_LOAD_UPWARD_DYLIB => DylibKind::Upward,
                        _ => DylibKind::Lazy,
                    };
                    image.dylibs.push(Dylib {
                        path: command_string(body, 8, index)?,
                        kind,
                        current_version: u32_le(body, 16, "")?,
                        compatibility_version: u32_le(body, 20, "")?,
                    });
                }
                LC_ID_DYLIB => {
                    need(24)?;
                    image.install_name = Some(command_string(body, 8, index)?);
                }
                LC_MAIN => {
                    need(24)?;
                    if main.is_some() {
                        return fail(Malformed, "two LC_MAIN commands");
                    }
                    main = Some((u64_le(body, 8, "")?, u64_le(body, 16, "")?));
                }
                LC_UNIXTHREAD => {
                    if thread.is_some() {
                        return fail(Malformed, "two LC_UNIXTHREAD commands");
                    }
                    thread = Some(read_thread(body, index)?);
                }
                LC_DYLD_INFO | LC_DYLD_INFO_ONLY => {
                    need(48)?;
                    if dyld_info.is_some() {
                        return fail(Malformed, "two LC_DYLD_INFO commands");
                    }
                    let field = |n: u64| u32_le(body, 8 + 4 * n, "").unwrap();
                    dyld_info = Some(DyldInfo {
                        rebase: (field(0), field(1)),
                        bind: (field(2), field(3)),
                        weak: (field(4), field(5)),
                        lazy: (field(6), field(7)),
                        export: (field(8), field(9)),
                    });
                }
                LC_ENCRYPTION_INFO => {
                    need(20)?;
                    if image.encryption.is_some() {
                        return fail(Malformed, "two LC_ENCRYPTION_INFO commands");
                    }
                    let info = EncryptionInfo {
                        offset: u32_le(body, 8, "")?,
                        size: u32_le(body, 12, "")?,
                        id: u32_le(body, 16, "")?,
                    };
                    range(data, u64::from(info.offset), u64::from(info.size), "encrypted range")?;
                    if info.id != 0 {
                        return fail(Encrypted, format!("slice is encrypted (cryptid {})", info.id));
                    }
                    image.encryption = Some(info);
                }
                LC_SEGMENT_64 | LC_ENCRYPTION_INFO_64 => {
                    return fail(Malformed, format!("64-bit load command {command:#x} in a 32-bit image"))
                }
                LC_THREAD => return fail(Unsupported, "LC_THREAD (only LC_UNIXTHREAD or LC_MAIN)"),
                LC_ROUTINES => return fail(Unsupported, "LC_ROUTINES initializer"),
                other if DESCRIPTIVE_COMMANDS.contains(&other) || other & LC_REQ_DYLD == 0 => {
                    // Kept for callers to see, never silently dropped. Unknown
                    // commands without the "dyld must understand" bit are safe
                    // for dyld to skip, so they are kept the same way.
                    image.other_commands.push((other, ByteRange { offset: at as u32, size: size as u32 }));
                }
                other => {
                    return fail(Unsupported, format!("load command {other:#x} must be understood but is not handled"))
                }
            }
            at = end;
        }

        image.check_segments(data)?;

        let header_vm = image
            .segments
            .iter()
            .find(|s| s.file_offset == 0 && s.file_size > 0)
            .map(|s| s.vm_address);
        image.entry = match (main, thread) {
            (Some(_), Some(_)) => return fail(Malformed, "both LC_MAIN and LC_UNIXTHREAD"),
            (Some((entry_offset, stack_size)), None) => {
                let base = header_vm.ok_or_else(|| Error::new(Malformed, "no segment maps the header for LC_MAIN"))?;
                let address = u64::from(base)
                    .checked_add(entry_offset)
                    .filter(|&a| a <= u64::from(u32::MAX))
                    .ok_or_else(|| Error::new(Overflow, format!("LC_MAIN entry offset {entry_offset:#x} overflows")))?;
                Some(Entry32::Main { address: address as u32, entry_offset, stack_size })
            }
            (None, Some(state)) => Some(Entry32::UnixThread(state)),
            (None, None) => None,
        };
        match &image.entry {
            Some(entry) => {
                let code = entry.code_address();
                if !image.segments.iter().any(|s| s.executable() && s.contains(code)) {
                    return fail(InvalidReference, format!("entry {code:#x} is not inside a code segment"));
                }
            }
            None if file_type == MH_EXECUTE => return fail(Malformed, "program has no LC_MAIN or LC_UNIXTHREAD"),
            None => {}
        }

        if let Some([symbol_offset, count, string_offset, string_size]) = symtab {
            image.symbols = read_symbols(data, symbol_offset, count, string_offset, string_size, image.section_count())?;
        }
        if let Some(fields) = dysymtab {
            image.indirect_symbols = read_dysymtab(data, &fields, image.symbols.len())?;
        }

        match dyld_info {
            Some(info) => image.read_dyld_info(data, &info)?,
            None if !image.dylibs.is_empty() => {
                return fail(Unsupported, "imports without LC_DYLD_INFO (classic relocations or chained fix-ups)")
            }
            None => {}
        }
        Ok(image)
    }

    fn section_count(&self) -> usize {
        self.segments.iter().map(|s| s.sections.len()).sum()
    }

    fn check_segments(&self, data: &[u8]) -> Result<()> {
        for (index, segment) in self.segments.iter().enumerate() {
            let what = format!("segment {index} ({})", segment.name);
            if segment.vm_end() > 1 << 32 {
                return fail(OutOfRange, format!("{what} ends past the 32-bit address space"));
            }
            if segment.file_size > segment.vm_size {
                return fail(Malformed, format!("{what} has more file bytes than address space"));
            }
            range(data, u64::from(segment.file_offset), u64::from(segment.file_size), &what)?;
            if segment.name == "__PAGEZERO" && !segment.is_null_guard() {
                return fail(Malformed, format!("{what} is named __PAGEZERO but is mapped or not at 0"));
            }
            if segment.initial_protection == 0 && segment.vm_size != 0 && !segment.is_null_guard() {
                return fail(Unsupported, format!("{what} is a no-access segment that is not the low guard"));
            }
            for section in &segment.sections {
                let what = format!("section {},{}", section.segment_name, section.name);
                if section.segment_name != segment.name {
                    return fail(Malformed, format!("{what} sits in segment {}", segment.name));
                }
                let end = u64::from(section.address) + u64::from(section.size);
                if section.address < segment.vm_address || end > segment.vm_end() {
                    return fail(OutOfRange, format!("{what} lies outside its segment"));
                }
                if !section.is_zero_fill() && section.size != 0 {
                    let start = u64::from(section.offset);
                    let file_end = start + u64::from(section.size);
                    let segment_file_end = u64::from(segment.file_offset) + u64::from(segment.file_size);
                    if start < u64::from(segment.file_offset) || file_end > segment_file_end {
                        return fail(OutOfRange, format!("{what} file bytes lie outside its segment's"));
                    }
                }
            }
            for (other_index, other) in self.segments.iter().enumerate().skip(index + 1) {
                let overlaps = segment.vm_size != 0
                    && other.vm_size != 0
                    && u64::from(segment.vm_address) < other.vm_end()
                    && u64::from(other.vm_address) < segment.vm_end();
                if overlaps {
                    return fail(Overlap, format!("{what} overlaps segment {other_index} ({})", other.name));
                }
            }
        }
        Ok(())
    }

    fn read_dyld_info(&mut self, data: &[u8], info: &DyldInfo) -> Result<()> {
        let spans: Vec<SegmentSpan> = self
            .segments
            .iter()
            .map(|s| SegmentSpan {
                vm_address: s.vm_address,
                vm_size: s.vm_size,
                writable: s.writable(),
                executable: s.executable(),
            })
            .collect();
        fn stream_bytes<'a>(data: &'a [u8], (offset, size): (u32, u32), what: &str) -> Result<&'a [u8]> {
            bytes(data, u64::from(offset), u64::from(size), what)
        }
        let stream = |part: (u32, u32), what: &str| stream_bytes(data, part, what);
        self.rebases = opcodes::decode_rebases(stream(info.rebase, "rebase stream")?, &spans)?;
        let count = self.dylibs.len();
        let normal = opcodes::decode_binds(stream(info.bind, "bind stream")?, BindSource::Normal, &spans, count)?;
        let weak = opcodes::decode_binds(stream(info.weak, "weak bind stream")?, BindSource::Weak, &spans, count)?;
        let lazy = opcodes::decode_binds(stream(info.lazy, "lazy bind stream")?, BindSource::Lazy, &spans, count)?;
        self.binds = normal.binds;
        self.binds.extend(weak.binds);
        self.binds.extend(lazy.binds);
        self.weak_definitions = weak.weak_definitions;
        if info.export.1 != 0 {
            stream(info.export, "export trie")?;
            self.export_trie = Some(ByteRange { offset: info.export.0, size: info.export.1 });
        }
        Ok(())
    }

    /// The chosen slice's bytes inside the original file.
    pub fn slice_bytes<'a>(&self, file: &'a [u8]) -> Result<&'a [u8]> {
        bytes(file, self.slice.file_offset, self.slice.size, "slice")
    }

    /// The decoded export trie (empty if the image has none). `file` is the
    /// whole file the image was parsed from.
    pub fn exports(&self, file: &[u8]) -> Result<Vec<crate::Export32>> {
        match self.export_trie {
            Some(trie) => {
                let slice = self.slice_bytes(file)?;
                crate::decode_export_trie(bytes(slice, u64::from(trie.offset), u64::from(trie.size), "export trie")?)
            }
            None => Ok(Vec::new()),
        }
    }

    /// Unslid address of the header (the segment mapping file offset 0);
    /// export offsets count from here.
    pub fn header_address(&self) -> Option<u32> {
        self.segments.iter().find(|s| s.file_offset == 0 && s.file_size > 0).map(|s| s.vm_address)
    }

    pub fn is_pie(&self) -> bool {
        self.flags & MH_PIE != 0
    }

    /// dyld may load this image at a slide: programs only with `MH_PIE`,
    /// libraries and bundles always.
    pub fn can_slide(&self) -> bool {
        self.is_pie() || matches!(self.file_type, MH_DYLIB | MH_BUNDLE)
    }

    pub fn segment(&self, name: &str) -> Option<&Segment32> {
        self.segments.iter().find(|s| s.name == name)
    }

    pub fn section(&self, segment: &str, name: &str) -> Option<&Section32> {
        self.segments.iter().flat_map(|s| s.sections.iter()).find(|s| s.segment_name == segment && s.name == name)
    }

    pub fn section_containing(&self, address: u32) -> Option<&Section32> {
        self.segments.iter().flat_map(|s| s.sections.iter()).find(|s| s.contains(address))
    }

    pub fn dylib(&self, library: LibraryRef) -> Option<&Dylib> {
        match library {
            LibraryRef::Dylib(n) => self.dylibs.get(n as usize - 1),
            _ => None,
        }
    }
}

fn read_segment(body: &[u8], index: u32) -> Result<Segment32> {
    let field = |offset: u64| u32_le(body, offset, "segment field");
    let name = name16(body, 8, "segment name")?;
    let section_count = u64::from(field(48)?);
    let needed = section_count
        .checked_mul(68)
        .and_then(|n| n.checked_add(56))
        .ok_or_else(|| Error::new(Overflow, "section count overflows"))?;
    if needed > body.len() as u64 {
        return fail(Malformed, format!("load command {index}: {section_count} sections do not fit the command"));
    }
    let mut sections = Vec::with_capacity(section_count as usize);
    for number in 0..section_count {
        let at = 56 + number * 68;
        let section_field = |offset: u64| u32_le(body, at + offset, "section field");
        let section = Section32 {
            name: name16(body, at, "section name")?,
            segment_name: name16(body, at + 16, "section segment name")?,
            address: section_field(32)?,
            size: section_field(36)?,
            offset: section_field(40)?,
            align: section_field(44)?,
            flags: section_field(56)?,
            reserved1: section_field(60)?,
            reserved2: section_field(64)?,
        };
        let relocation_count = section_field(52)?;
        if relocation_count != 0 {
            return fail(
                Unsupported,
                format!("section {},{} has {relocation_count} classic relocations", section.segment_name, section.name),
            );
        }
        sections.push(section);
    }
    Ok(Segment32 {
        name,
        vm_address: field(24)?,
        vm_size: field(28)?,
        file_offset: field(32)?,
        file_size: field(36)?,
        max_protection: field(40)?,
        initial_protection: field(44)?,
        flags: field(52)?,
        sections,
    })
}

/// A load command's `lc_str` field: an offset into the command to a zero-ended string.
fn command_string(body: &[u8], field: u64, index: u32) -> Result<String> {
    let offset = u64::from(u32_le(body, field, "string offset")?);
    if offset >= body.len() as u64 {
        return fail(Malformed, format!("load command {index}: string offset {offset} is outside the command"));
    }
    c_string(&body[offset as usize..], "load command string")
}

fn read_thread(body: &[u8], index: u32) -> Result<ArmThreadState> {
    const ARM_THREAD_STATE: u32 = 1;
    const ARM_THREAD_STATE_COUNT: u32 = 17;
    let mut at = 8u64;
    let mut found = None;
    while at < body.len() as u64 {
        let flavor = u32_le(body, at, "thread flavor")?;
        let count = u32_le(body, at + 4, "thread state count")?;
        if flavor != ARM_THREAD_STATE || count != ARM_THREAD_STATE_COUNT {
            return fail(Unsupported, format!("load command {index}: thread flavor {flavor} count {count}"));
        }
        if found.is_some() {
            return fail(Malformed, format!("load command {index}: two ARM thread states"));
        }
        let words = bytes(body, at + 8, u64::from(count) * 4, "thread state")
            .map_err(|_| Error::new(Truncated, format!("load command {index}: thread state is cut short")))?;
        let word = |n: usize| u32::from_le_bytes(words[n * 4..n * 4 + 4].try_into().unwrap());
        found = Some(ArmThreadState {
            r: std::array::from_fn(word),
            sp: word(13),
            lr: word(14),
            pc: word(15),
            cpsr: word(16),
        });
        at += 8 + u64::from(count) * 4;
    }
    found.ok_or_else(|| Error::new(Malformed, format!("load command {index}: thread command has no state")))
}

fn read_symbols(
    data: &[u8],
    symbol_offset: u32,
    count: u32,
    string_offset: u32,
    string_size: u32,
    section_count: usize,
) -> Result<Vec<Symbol32>> {
    let table = bytes(data, u64::from(symbol_offset), u64::from(count) * 12, "symbol table")?;
    let strings = bytes(data, u64::from(string_offset), u64::from(string_size), "string table")?;
    let mut out = Vec::with_capacity(count as usize);
    for index in 0..u64::from(count) {
        let at = index * 12;
        let string_index = u32_le(table, at, "")?;
        let symbol = Symbol32 {
            name: String::new(),
            string_index,
            n_type: u8_at(table, at + 4, "")?,
            n_sect: u8_at(table, at + 5, "")?,
            n_desc: u16_le(table, at + 6, "")?,
            value: u32_le(table, at + 8, "")?,
        };
        if string_index as usize >= strings.len() && !(string_index == 0 && strings.is_empty()) {
            return fail(InvalidReference, format!("symbol {index} names string {string_index} past the table"));
        }
        let name = if strings.is_empty() {
            String::new()
        } else {
            c_string(&strings[string_index as usize..], "symbol name")?
        };
        if symbol.is_section_defined() && (symbol.n_sect == 0 || usize::from(symbol.n_sect) > section_count) {
            return fail(InvalidReference, format!("symbol {name} names section {}", symbol.n_sect));
        }
        out.push(Symbol32 { name, ..symbol });
    }
    Ok(out)
}

fn read_dysymtab(data: &[u8], fields: &[u32; 18], symbol_count: usize) -> Result<Vec<u32>> {
    const INDIRECT_SYMBOL_LOCAL: u32 = 0x8000_0000;
    const INDIRECT_SYMBOL_ABS: u32 = 0x4000_0000;
    for (first, count, what) in [(0, 1, "local"), (2, 3, "external"), (4, 5, "undefined")] {
        let end = u64::from(fields[first]) + u64::from(fields[count]);
        if end > symbol_count as u64 {
            return fail(InvalidReference, format!("{what} symbol group ends at {end} of {symbol_count}"));
        }
    }
    let external_relocations = fields[15];
    let local_relocations = fields[17];
    if external_relocations != 0 || local_relocations != 0 {
        return fail(
            Unsupported,
            format!("classic dynamic relocations ({external_relocations} external, {local_relocations} local)"),
        );
    }
    let (offset, count) = (fields[12], fields[13]);
    let table = bytes(data, u64::from(offset), u64::from(count) * 4, "indirect symbol table")?;
    let mut out = Vec::with_capacity(count as usize);
    for chunk in table.chunks_exact(4) {
        let value = u32::from_le_bytes(chunk.try_into().unwrap());
        if value & (INDIRECT_SYMBOL_LOCAL | INDIRECT_SYMBOL_ABS) == 0 && value as usize >= symbol_count {
            return fail(InvalidReference, format!("indirect symbol {value} past {symbol_count} symbols"));
        }
        out.push(value);
    }
    Ok(out)
}
