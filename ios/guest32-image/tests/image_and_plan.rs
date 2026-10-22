//! Parser and planner checks on small hand-built Mach-O32 files.

use guest32_image::*;

const ARMV7: u32 = 9;
const ARMV7S: u32 = 11;

enum Start {
    Main(u64),
    Thread { pc: u32, cpsr: u32 },
}

struct Opts {
    subtype: u32,
    file_type: u32,
    pie: bool,
    start: Start,
    cryptid: u32,
    data_file_offset: u32,
    rebase: Vec<u8>,
    bind: Vec<u8>,
    weak: Vec<u8>,
    lazy: Vec<u8>,
    extra_commands: Vec<Vec<u8>>,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            subtype: ARMV7,
            file_type: 2,
            pie: false,
            start: Start::Main(0x401),
            cryptid: 0,
            data_file_offset: 0x1000,
            rebase: Vec::new(),
            bind: Vec::new(),
            weak: Vec::new(),
            lazy: Vec::new(),
            extra_commands: Vec::new(),
        }
    }
}

fn put(out: &mut Vec<u8>, values: &[u32]) {
    for value in values {
        out.extend_from_slice(&value.to_le_bytes());
    }
}

fn name16(out: &mut Vec<u8>, name: &str) {
    let mut raw = [0u8; 16];
    raw[..name.len()].copy_from_slice(name.as_bytes());
    out.extend_from_slice(&raw);
}

/// LC_SEGMENT with sections (name, address, size, file offset, flags).
fn segment(name: &str, vm: u32, vm_size: u32, file_offset: u32, file_size: u32, prot: u32, sections: &[(&str, u32, u32, u32, u32)]) -> Vec<u8> {
    let mut out = Vec::new();
    put(&mut out, &[0x1, 56 + 68 * sections.len() as u32]);
    name16(&mut out, name);
    put(&mut out, &[vm, vm_size, file_offset, file_size, prot, prot, sections.len() as u32, 0]);
    for &(section, address, size, offset, flags) in sections {
        name16(&mut out, section);
        name16(&mut out, name);
        put(&mut out, &[address, size, offset, 2, 0, 0, flags, 0, 0]);
    }
    out
}

/// A thin armv7 program: __PAGEZERO 0..0x1000, __TEXT 0x1000 (r-x),
/// __DATA 0x2000 (rw-), __LINKEDIT 0x3000 holding the opcode streams and symbols.
fn build(opts: &Opts) -> Vec<u8> {
    let mut linkedit = Vec::new();
    let mut place = |bytes: &[u8]| {
        let offset = 0x2000 + linkedit.len() as u32;
        linkedit.extend_from_slice(bytes);
        (offset, bytes.len() as u32)
    };
    let rebase = place(&opts.rebase);
    let bind = place(&opts.bind);
    let weak = place(&opts.weak);
    let lazy = place(&opts.lazy);
    let mut symbols = Vec::new();
    put(&mut symbols, &[1]);
    symbols.extend_from_slice(&[0x0F, 1, 0x08, 0x00]);
    put(&mut symbols, &[0x1401]);
    put(&mut symbols, &[7]);
    symbols.extend_from_slice(&[0x01, 0, 0x00, 0x01]);
    put(&mut symbols, &[0]);
    let symbol_table = place(&symbols);
    let strings = place(b"\0_main\0_foo\0\0");
    let linkedit_size = linkedit.len() as u32;

    let mut commands: Vec<Vec<u8>> = vec![
        segment("__PAGEZERO", 0, 0x1000, 0, 0, 0, &[]),
        segment("__TEXT", 0x1000, 0x1000, 0, 0x1000, 5, &[("__text", 0x1400, 0x400, 0x400, 0x8000_0400)]),
        segment("__DATA", 0x2000, 0x1000, opts.data_file_offset, 0x1000, 3, &[("__data", 0x2000, 0x100, opts.data_file_offset, 0)]),
        segment("__LINKEDIT", 0x3000, 0x1000, 0x2000, linkedit_size, 1, &[]),
    ];
    let mut info = Vec::new();
    put(&mut info, &[0x8000_0022, 48, rebase.0, rebase.1, bind.0, bind.1, weak.0, weak.1, lazy.0, lazy.1, 0, 0]);
    commands.push(info);
    let mut symtab = Vec::new();
    put(&mut symtab, &[0x2, 24, symbol_table.0, 2, strings.0, strings.1]);
    commands.push(symtab);
    let mut dylib = Vec::new();
    let path = b"/usr/lib/libSystem.B.dylib\0\0";
    put(&mut dylib, &[0xC, 24 + path.len() as u32, 24, 2, 0x10000, 0x10000]);
    dylib.extend_from_slice(path);
    commands.push(dylib);
    let mut start = Vec::new();
    match opts.start {
        Start::Main(offset) => {
            put(&mut start, &[0x8000_0028, 24]);
            start.extend_from_slice(&offset.to_le_bytes());
            start.extend_from_slice(&0u64.to_le_bytes());
        }
        Start::Thread { pc, cpsr } => {
            put(&mut start, &[0x5, 84, 1, 17]);
            put(&mut start, &[0; 13]);
            put(&mut start, &[0, 0, pc, cpsr]);
        }
    }
    commands.push(start);
    let mut encryption = Vec::new();
    put(&mut encryption, &[0x21, 20, 0x400, 0x400, opts.cryptid]);
    commands.push(encryption);
    commands.extend(opts.extra_commands.iter().cloned());

    let mut file = vec![0u8; 0x2000 + linkedit.len()];
    let body: Vec<u8> = commands.concat();
    assert!(28 + body.len() <= 0x400, "test commands overflow the header page");
    let mut header = Vec::new();
    let flags = 0x85 | if opts.pie { MH_PIE } else { 0 };
    put(&mut header, &[0xFEED_FACE, 12, opts.subtype, opts.file_type, commands.len() as u32, body.len() as u32, flags]);
    file[..28].copy_from_slice(&header);
    file[28..28 + body.len()].copy_from_slice(&body);
    file[0x2000..].copy_from_slice(&linkedit);
    file
}

/// A big-endian universal file holding the given (cpu, subtype, slice) list.
fn fat(slices: &[(u32, u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0xCAFE_BABEu32.to_be_bytes());
    out.extend_from_slice(&(slices.len() as u32).to_be_bytes());
    let mut offset = 0x4000u32;
    let mut bodies = Vec::new();
    for (cpu, subtype, slice) in slices {
        for value in [*cpu, *subtype, offset, slice.len() as u32, 14] {
            out.extend_from_slice(&value.to_be_bytes());
        }
        bodies.push((offset, slice));
        offset += (slice.len() as u32 + 0x3FFF) & !0x3FFF;
    }
    for (at, slice) in bodies {
        out.resize(at as usize, 0);
        out.extend_from_slice(slice);
    }
    out
}

fn layout(reserved: Vec<AddressRange>, allow_slide: bool) -> MemoryLayout {
    MemoryLayout {
        mapped: AddressRange::new(0x1000, 0xC000_0000),
        code: AddressRange::new(0x1000, 0xC000_0000),
        reserved,
        page_size: 0x1000,
        allow_slide,
    }
}

fn kind_of(result: Result<Image32>) -> ErrorKind {
    match result {
        Ok(_) => panic!("expected an error"),
        Err(error) => error.kind,
    }
}

#[test]
fn lc_main_keeps_thumb_bit_and_non_pie_plans_zero_slide() {
    let image = Image32::parse(&build(&Opts::default())).unwrap();
    assert_eq!(image.slice.subtype, ArmSubtype::V7);
    assert_eq!(image.entry, Some(Entry32::Main { address: 0x1401, entry_offset: 0x401, stack_size: 0 }));
    assert!(image.entry.as_ref().unwrap().is_thumb());
    assert_eq!(image.dylibs[0].path, "/usr/lib/libSystem.B.dylib");
    assert_eq!(image.symbols.len(), 2);
    assert!(image.symbols[0].is_thumb_definition() && image.symbols[1].is_undefined());
    assert_eq!(image.symbols[1].library_ordinal(), 1);

    let plan = plan_load(&image, &layout(Vec::new(), true)).unwrap();
    assert_eq!(plan.slide, 0);
    assert_eq!(plan.null_guard, Some(AddressRange::new(0, 0x1000)));
    let names: Vec<&str> = plan.segments.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["__TEXT", "__DATA", "__LINKEDIT"]);
    let entry = plan.entry.unwrap();
    assert_eq!((entry.address, entry.thumb), (0x1401, true));
}

#[test]
fn universal_file_prefers_armv7_over_armv7s() {
    let armv7s = build(&Opts { subtype: ARMV7S, ..Opts::default() });
    let armv7 = build(&Opts { start: Start::Thread { pc: 0x1400, cpsr: 0 }, ..Opts::default() });
    let both = fat(&[(12, ARMV7S, armv7s.clone()), (12, ARMV7, armv7)]);
    let image = Image32::parse(&both).unwrap();
    assert_eq!(image.slice.subtype, ArmSubtype::V7);
    assert_eq!(image.slice.alternatives, [ArmSubtype::V7S, ArmSubtype::V7]);
    assert!(matches!(image.entry, Some(Entry32::UnixThread(_))));

    let only_s = Image32::parse(&fat(&[(12, ARMV7S, armv7s.clone())])).unwrap();
    assert_eq!(only_s.slice.subtype, ArmSubtype::V7S);
    assert_eq!(kind_of(Image32::parse(&fat(&[(7, 3, armv7s)]))), ErrorKind::NoMatchingSlice);
}

#[test]
fn unix_thread_entry_keeps_state_and_mode() {
    let arm = Image32::parse(&build(&Opts { start: Start::Thread { pc: 0x1400, cpsr: 0 }, ..Opts::default() })).unwrap();
    let Some(Entry32::UnixThread(state)) = &arm.entry else { panic!("not a thread entry") };
    assert_eq!((state.pc, state.cpsr, state.sp), (0x1400, 0, 0));
    assert!(!arm.entry.as_ref().unwrap().is_thumb());

    let thumb = Image32::parse(&build(&Opts { start: Start::Thread { pc: 0x1400, cpsr: 0x20 }, ..Opts::default() })).unwrap();
    assert!(thumb.entry.as_ref().unwrap().is_thumb());
}

fn fixup_opts() -> Opts {
    let mut bind = vec![0x11, 0x41];
    bind.extend_from_slice(b"_foo\0");
    bind.extend_from_slice(&[0x51, 0x60, 0x7C, 0x72, 0x20, 0x90, 0xC0, 0x02, 0x04, 0x40]);
    bind.extend_from_slice(b"_pc\0");
    bind.extend_from_slice(&[0x53, 0x60, 0x00, 0x71, 0x80, 0x0C, 0x90, 0x00]);
    let mut weak = vec![0x48];
    weak.extend_from_slice(b"_strong\0");
    weak.push(0x40);
    weak.extend_from_slice(b"_weakone\0");
    weak.extend_from_slice(&[0x51, 0x72, 0x60, 0x90, 0x00]);
    let mut lazy = vec![0x72, 0x40, 0x11, 0x40];
    lazy.extend_from_slice(b"_bar\0");
    lazy.extend_from_slice(&[0x90, 0x00, 0x72, 0x44, 0x3E, 0x40]);
    lazy.extend_from_slice(b"_baz\0");
    lazy.extend_from_slice(&[0x90, 0x00]);
    Opts {
        rebase: vec![0x11, 0x22, 0x10, 0x52, 0x12, 0x21, 0x80, 0x0A, 0x51, 0x00],
        bind,
        weak,
        lazy,
        ..Opts::default()
    }
}

#[test]
fn rebase_and_bind_records_keep_source_kind_library_addend_and_weakness() {
    let image = Image32::parse(&build(&fixup_opts())).unwrap();
    let rebases: Vec<(u32, usize, FixupKind)> = image.rebases.iter().map(|r| (r.address, r.segment, r.kind)).collect();
    assert_eq!(
        rebases,
        [(0x2010, 2, FixupKind::Pointer), (0x2014, 2, FixupKind::Pointer), (0x1500, 1, FixupKind::TextAbsolute32)]
    );
    let binds: Vec<_> = image
        .binds
        .iter()
        .map(|b| (b.source, b.address, b.kind, b.library, b.symbol.as_str(), b.addend, b.weak_import, b.lazy_offset))
        .collect();
    let lib = Some(LibraryRef::Dylib(1));
    use BindSource::*;
    assert_eq!(
        binds,
        [
            (Normal, 0x2020, FixupKind::Pointer, lib, "_foo", -4, true, None),
            (Normal, 0x2024, FixupKind::Pointer, lib, "_foo", -4, true, None),
            (Normal, 0x202C, FixupKind::Pointer, lib, "_foo", -4, true, None),
            (Normal, 0x1600, FixupKind::TextPcrel32, lib, "_pc", 0, false, None),
            (Weak, 0x2060, FixupKind::Pointer, None, "_weakone", 0, false, None),
            (Lazy, 0x2040, FixupKind::Pointer, lib, "_bar", 0, false, Some(0)),
            (Lazy, 0x2044, FixupKind::Pointer, Some(LibraryRef::FlatLookup), "_baz", 0, false, Some(11)),
        ]
    );
    assert!(image.binds.iter().all(|b| b.width == 4));
    assert_eq!(image.weak_definitions, [WeakDefinition { symbol: "_strong".into(), position: 0 }]);
}

#[test]
fn pie_image_slides_past_reserved_range_but_fixed_image_does_not() {
    let reserved = vec![AddressRange::new(0x1000, 0x5000)];
    let pie = Image32::parse(&build(&Opts { pie: true, ..fixup_opts() })).unwrap();
    let plan = plan_load(&pie, &layout(reserved.clone(), true)).unwrap();
    assert_eq!(plan.slide, 0x4000);
    assert_eq!(plan.null_guard, Some(AddressRange::new(0, 0x1000)));
    assert_eq!(plan.entry.as_ref().map(|e| (e.address, e.thumb)), Some((0x5401, true)));
    assert_eq!(plan.fixups.len(), pie.rebases.len() + pie.binds.len());
    for fixup in &plan.fixups {
        let unslid = match fixup.action {
            FixupAction::Rebase(i) => pie.rebases[i].address,
            FixupAction::Bind(i) => pie.binds[i].address,
        };
        assert_eq!(fixup.guest_address, unslid + 0x4000);
    }

    let no_slide = plan_load(&pie, &layout(reserved.clone(), false)).unwrap_err();
    assert_eq!(no_slide.kind, ErrorKind::Overlap);
    let fixed = Image32::parse(&build(&fixup_opts())).unwrap();
    assert_eq!(plan_load(&fixed, &layout(reserved.clone(), true)).unwrap_err().kind, ErrorKind::Overlap);
    // A library slides like dyld slides it, with no MH_PIE flag.
    let library = Image32::parse(&build(&Opts { file_type: 6, ..fixup_opts() })).unwrap();
    assert_eq!(plan_load(&library, &layout(reserved, true)).unwrap().slide, 0x4000);
}

#[test]
fn slide_search_clears_later_segments_and_guard_uses_whole_pages() {
    // Only __DATA hits the reserved range; sliding 0x1000 moves it clear,
    // which a search that only lines up the first segment never tries.
    let mut image = Image32::parse(&build(&Opts { pie: true, ..Opts::default() })).unwrap();
    image.segments[2].vm_address = 0x9000;
    let window = AddressRange::new(0x1000, 0xB000);
    let tight = MemoryLayout {
        mapped: window,
        code: window,
        reserved: vec![AddressRange::new(0x9000, 0xA000)],
        page_size: 0x1000,
        allow_slide: true,
    };
    assert_eq!(plan_load(&image, &tight).unwrap().slide, 0x1000);

    // An unaligned guard [0, 0x1800) shares the page [0x1000, 0x2000) with
    // __TEXT [0x1800, 0x2000); mapping that page would undo the guard.
    let mut image = Image32::parse(&build(&Opts::default())).unwrap();
    image.segments[0].vm_size = 0x1800;
    image.segments[1].vm_address = 0x1800;
    image.segments[1].vm_size = 0x800;
    image.entry = Some(Entry32::Main { address: 0x1801, entry_offset: 0x801, stack_size: 0 });
    assert_eq!(plan_load(&image, &layout(Vec::new(), true)).unwrap_err().kind, ErrorKind::Overlap);
}

#[test]
fn encrypted_slice_is_refused() {
    assert_eq!(kind_of(Image32::parse(&build(&Opts { cryptid: 1, ..Opts::default() }))), ErrorKind::Encrypted);
}

#[test]
fn malformed_streams_and_commands_are_refused() {
    use ErrorKind::*;
    let with = |f: fn(&mut Opts)| {
        let mut opts = Opts::default();
        f(&mut opts);
        kind_of(Image32::parse(&build(&opts)))
    };
    // ULEB wider than 64 bits.
    assert_eq!(with(|o| o.rebase = vec![0x22, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x7F]), Overflow);
    // Pointer rebase into the read-only code segment.
    assert_eq!(with(|o| o.rebase = vec![0x11, 0x21, 0x80, 0x0A, 0x51]), InvalidReference);
    // 4-byte slot that starts 2 bytes before the segment end.
    assert_eq!(with(|o| o.bind = vec![0x11, 0x40, b'_', b'x', 0, 0x72, 0xFE, 0x1F, 0x90]), OutOfRange);
    // Library ordinal 5 with one library.
    assert_eq!(with(|o| o.bind = vec![0x15, 0x40, b'_', b'x', 0, 0x72, 0x00, 0x90]), InvalidReference);
    // Unknown bind opcode.
    assert_eq!(with(|o| o.bind = vec![0xE0]), Malformed);
    // A lazy entry may not lean on the previous entry's symbol.
    assert_eq!(with(|o| o.lazy = vec![0x72, 0x00, 0x11, 0x40, b'_', b'a', 0, 0x90, 0x00, 0x72, 0x04, 0x90, 0x00]), InvalidReference);
    // Segment file bytes past the end of the file.
    assert_eq!(with(|o| o.data_file_offset = 0x10_0000), Truncated);
    // Entry outside any code segment.
    assert_eq!(with(|o| o.start = Start::Main(0x1801)), InvalidReference);
    // Segment overlapping __DATA.
    assert_eq!(with(|o| o.extra_commands = vec![segment("__EXTRA", 0x2800, 0x1000, 0, 0, 3, &[])]), Overlap);
    // Segment ending exactly at 2^32; its end offset has no 32-bit address.
    let top = segment("__TOP", 0xFFFF_F000, 0x1000, 0, 0, 3, &[]);
    let mut opts = Opts { extra_commands: vec![top], rebase: vec![0x24, 0x80, 0x20, 0x00], ..Opts::default() };
    assert_eq!(kind_of(Image32::parse(&build(&opts))), OutOfRange);
    opts.rebase.clear();
    assert!(Image32::parse(&build(&opts)).is_ok(), "the segment itself is allowed");
    // A command dyld must understand but this reader does not.
    assert_eq!(with(|o| o.extra_commands = vec![vec![0x99, 0, 0, 0x80, 8, 0, 0, 0]]), Unsupported);
    // A 64-bit segment command in a 32-bit file.
    assert_eq!(with(|o| o.extra_commands = vec![vec![0x19, 0, 0, 0, 8, 0, 0, 0]]), Malformed);

    // A command size that runs past the command area.
    let mut file = build(&Opts::default());
    file[32..36].copy_from_slice(&0x10000u32.to_le_bytes());
    assert_eq!(kind_of(Image32::parse(&file)), Malformed);

    // Descriptive commands are kept, not dropped.
    let mut uuid = vec![0x1B, 0, 0, 0, 24, 0, 0, 0];
    uuid.resize(24, 0xAB);
    let image = Image32::parse(&build(&Opts { extra_commands: vec![uuid], ..Opts::default() })).unwrap();
    assert_eq!(image.other_commands.iter().map(|c| c.0).collect::<Vec<_>>(), [0x1B]);
}

#[test]
fn malformed_export_tries_are_refused_early() {
    // Two edges to one child.
    let shared = [0, 2, b'a', 0, 10, b'b', 0, 10, 0, 0, 0, 0];
    assert_eq!(decode_export_trie(&shared).unwrap_err().kind, ErrorKind::Malformed);
    // A long chain of one-letter nodes: each name copies the whole path above it.
    let nodes = 2700; // offsets stay within two-byte ULEBs
    let mut chain = Vec::new();
    for node in 1..nodes {
        let child = node * 6;
        chain.extend([0, 1, b'a', 0, 0x80 | (child & 0x7F) as u8, (child >> 7) as u8]);
    }
    chain.extend([0, 0]);
    assert_eq!(decode_export_trie(&chain).unwrap_err().kind, ErrorKind::Malformed);
}
