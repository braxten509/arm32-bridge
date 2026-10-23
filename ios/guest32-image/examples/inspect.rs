//! Reads a 32-bit ARM iPhone program and prints what the parser and planner
//! decided. With `--fixups`, `--symbols` or `--sections` it prints the same
//! lines as llvm-objdump --macho --rebase/--bind/--weak-bind/--lazy-bind,
//! llvm-nm -a -p -x and llvm-objdump --macho -h, so tools/compare-llvm.sh can
//! diff them. Nothing is run.
//!
//! `--objc` summarizes the Objective-C metadata; `--framework-selectors`
//! lists the selectors the program sends that it does not implement itself.
//!
//! usage: inspect <file> [--fixups | --symbols | --sections | --exports | --objc | --framework-selectors]

use guest32_image::*;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: inspect <file> [--fixups | --symbols | --sections]");
    let mode = args.next().unwrap_or_default();
    let file = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let image = match Image32::parse(&file) {
        Ok(image) => image,
        Err(error) => {
            println!("error: {error}");
            std::process::exit(2);
        }
    };
    match mode.as_str() {
        "--fixups" => print_fixups(&image),
        "--symbols" => print_symbols(&image),
        "--sections" => print_sections(&image),
        "--exports" => print_exports(&image, &file),
        "--objc" => print_objc(&image, &file, false),
        "--framework-selectors" => print_objc(&image, &file, true),
        _ => print_summary(&image),
    }
}

/// `--objc`: counts and every class with its superclass; `--framework-selectors`:
/// the selectors in `__objc_selrefs` that no class or category of the program
/// itself implements (so they can only be answered by Apple's classes).
fn print_objc(image: &Image32, file: &[u8], framework_only: bool) {
    let memory = ImageMemory::new(image, file).unwrap();
    let objc = read_objc(&memory, &ObjcSections::from_image(image)).unwrap_or_else(|error| {
        println!("error: {error}");
        std::process::exit(2)
    });
    let mut own = std::collections::HashSet::new();
    for class in &objc.classes {
        own.extend(class.instance.methods.iter().chain(&class.meta.methods).map(|m| m.name.as_str()));
    }
    for category in &objc.categories {
        own.extend(category.instance_methods.iter().chain(&category.class_methods).map(|m| m.name.as_str()));
    }
    if framework_only {
        let mut names: Vec<&str> = objc.selector_names().into_iter().filter(|n| !own.contains(n)).collect();
        names.sort_unstable();
        for name in names {
            println!("{name}");
        }
        return;
    }
    println!(
        "classes {} categories {} protocols {} selrefs {} (distinct {}) classrefs {} superrefs {} cfstrings {} imageinfo {:x?}",
        objc.classes.len(),
        objc.categories.len(),
        objc.protocols_by_address.len(),
        objc.selector_refs.len(),
        objc.selector_names().len(),
        objc.class_refs.len(),
        objc.super_refs.len(),
        objc.cfstrings.len(),
        objc.image_info
    );
    for class in &objc.classes {
        println!(
            "class {} : {:?} size {} methods {}+{} ivars {}",
            class.name,
            class.superclass,
            class.instance.instance_size,
            class.instance.methods.len(),
            class.meta.methods.len(),
            class.instance.ivars.len()
        );
    }
    for category in &objc.categories {
        println!("category {:?} ({}) methods {}+{}", category.class, category.name, category.instance_methods.len(), category.class_methods.len());
    }
}

fn print_summary(image: &Image32) {
    let slice = &image.slice;
    println!(
        "slice: {:?} at {:#x} size {:#x}; universal choices {:?}",
        slice.subtype, slice.file_offset, slice.size, slice.alternatives
    );
    println!("file type {} flags {:#x} pie={}", image.file_type, image.flags, image.is_pie());
    if let Some(encryption) = &image.encryption {
        println!("encryption: cryptid {} range {:#x}+{:#x}", encryption.id, encryption.offset, encryption.size);
    }
    match &image.entry {
        Some(Entry32::Main { address, entry_offset, stack_size }) => {
            println!("entry: LC_MAIN offset {entry_offset:#x} address {address:#x} stack {stack_size:#x}")
        }
        Some(Entry32::UnixThread(state)) => println!(
            "entry: LC_UNIXTHREAD pc {:#x} sp {:#x} lr {:#x} cpsr {:#x} r {:x?}",
            state.pc, state.sp, state.lr, state.cpsr, state.r
        ),
        None => println!("entry: none"),
    }
    for segment in &image.segments {
        println!(
            "segment {:<12} vm {:#010x}+{:#x} file {:#x}+{:#x} prot {}/{} sections {} guard={}",
            segment.name,
            segment.vm_address,
            segment.vm_size,
            segment.file_offset,
            segment.file_size,
            segment.initial_protection,
            segment.max_protection,
            segment.sections.len(),
            segment.is_null_guard()
        );
    }
    println!("dylibs {} symbols {} indirect {}", image.dylibs.len(), image.symbols.len(), image.indirect_symbols.len());
    let count = |source: BindSource| image.binds.iter().filter(|b| b.source == source).count();
    println!(
        "rebases {} binds normal {} weak {} lazy {} strong-weak-definitions {}",
        image.rebases.len(),
        count(BindSource::Normal),
        count(BindSource::Weak),
        count(BindSource::Lazy),
        image.weak_definitions.len()
    );
    let text = image.rebases.iter().filter(|r| r.kind.is_text()).count()
        + image.binds.iter().filter(|b| b.kind.is_text()).count();
    println!("text fix-ups {text}");
    println!("export trie {:?}", image.export_trie);
    println!("other commands {:x?}", image.other_commands.iter().map(|c| c.0).collect::<Vec<_>>());

    let layout = MemoryLayout {
        mapped: AddressRange::new(0x1000, 0xC000_0000),
        code: AddressRange::new(0x1000, 0xC000_0000),
        reserved: Vec::new(),
        page_size: 0x1000,
        allow_slide: true,
    };
    match plan_load(image, &layout) {
        Ok(plan) => {
            for segment in &plan.segments {
                println!(
                    "plan segment {:<12} {:#010x}+{:#x} pages {:#x}..{:#x} zero-fill {:#x} shares-page={}",
                    segment.name, segment.start, segment.size, segment.pages.start, segment.pages.end, segment.zero_fill, segment.shares_page
                );
            }
            println!("plan null guard {:x?}", plan.null_guard);
            let (entry, thumb, kind) = match &plan.entry {
                Some(e) => (e.address, e.thumb, if matches!(e.source, Entry32::Main { .. }) { "main" } else { "unixthread" }),
                None => (0, false, "none"),
            };
            println!("plan: slide={:#x} entry={entry:#x} thumb={thumb} kind={kind} fixups={}", plan.slide, plan.fixups.len());
        }
        Err(error) => println!("plan error: {error}"),
    }
}

fn segment_and_section(image: &Image32, segment: usize, address: u32) -> (String, String) {
    let segment = image.segments[segment].name.clone();
    let section = image.section_containing(address).map(|s| s.name.clone()).unwrap_or_default();
    (segment, section)
}

/// llvm's guessLibraryShortName for the common shapes: framework name, or
/// the file name up to its first dot.
fn short_name(path: &str) -> String {
    let base = path.rsplit('/').next().unwrap_or(path);
    let base = base.split('.').next().unwrap_or(base);
    base.trim_end_matches("_debug").trim_end_matches("_profile").to_string()
}

fn dylib_name(image: &Image32, library: Option<LibraryRef>) -> String {
    match library {
        Some(LibraryRef::SelfImage) => "this-image".into(),
        Some(LibraryRef::MainExecutable) => "main-executable".into(),
        Some(LibraryRef::FlatLookup) => "flat-namespace".into(),
        Some(LibraryRef::WeakLookup) => "weak".into(),
        Some(library @ LibraryRef::Dylib(_)) => short_name(&image.dylib(library).unwrap().path),
        None => String::new(),
    }
}

fn print_fixups(image: &Image32) {
    println!("Rebase table:");
    println!("segment  section            address     type");
    for rebase in &image.rebases {
        let (segment, section) = segment_and_section(image, rebase.segment, rebase.address);
        println!("{segment:<8} {section:<18} 0x{:08X}  {}", rebase.address, rebase.kind.name());
    }
    println!("Bind table:");
    println!("segment  section            address    type       addend dylib            symbol");
    for bind in image.binds.iter().filter(|b| b.source == BindSource::Normal) {
        let (segment, section) = segment_and_section(image, bind.segment, bind.address);
        let attribute = if bind.weak_import { " (weak_import)" } else { "" };
        println!(
            "{segment:<8} {section:<18} 0x{:08X} {:<8} {:>8} {:<16} {}{attribute}",
            bind.address,
            bind.kind.name(),
            bind.addend,
            dylib_name(image, bind.library),
            bind.symbol
        );
    }
    println!("Lazy bind table:");
    println!("segment  section            address     dylib            symbol");
    for bind in image.binds.iter().filter(|b| b.source == BindSource::Lazy) {
        let (segment, section) = segment_and_section(image, bind.segment, bind.address);
        println!(
            "{segment:<8} {section:<18} 0x{:08X} {:<16} {}",
            bind.address,
            dylib_name(image, bind.library),
            bind.symbol
        );
    }
    println!("Weak bind table:");
    println!("segment  section            address     type       addend   symbol");
    let weak: Vec<&Bind32> = image.binds.iter().filter(|b| b.source == BindSource::Weak).collect();
    let strong_before = |position: usize| {
        for definition in image.weak_definitions.iter().filter(|d| d.position == position) {
            println!("                                        strong              {}", definition.symbol);
        }
    };
    for (index, bind) in weak.iter().enumerate() {
        strong_before(index);
        let (segment, section) = segment_and_section(image, bind.segment, bind.address);
        println!(
            "{segment:<8} {section:<18} 0x{:08X} {:<8} {:>8}   {}",
            bind.address,
            bind.kind.name(),
            bind.addend,
            bind.symbol
        );
    }
    strong_before(weak.len());
}

fn print_symbols(image: &Image32) {
    for symbol in &image.symbols {
        println!(
            "{:08x} {:02x} {:02x} {:04x} {:08x} {}",
            symbol.value, symbol.n_type, symbol.n_sect, symbol.n_desc, symbol.string_index, symbol.name
        );
    }
}

/// llvm-objdump --exports-trie lines: address (header + offset), name, flags.
fn print_exports(image: &Image32, file: &[u8]) {
    let base = image.header_address().unwrap_or(0);
    let exports = image.exports(file).unwrap_or_else(|error| {
        println!("error: {error}");
        std::process::exit(2)
    });
    for export in exports {
        let mut flags = Vec::new();
        if export.weak {
            flags.push("weak_def".to_string());
        }
        match export.kind {
            ExportKind::ThreadLocal => flags.push("per-thread".into()),
            ExportKind::Absolute => flags.push("absolute".into()),
            ExportKind::Regular => {}
        }
        if let Some(resolver) = export.resolver {
            flags.push(format!("resolver=0x{:08X}", base.wrapping_add(resolver)));
        }
        let attributes = if flags.is_empty() { String::new() } else { format!(" [{}]", flags.join(", ")) };
        match (&export.reexport, export.offset) {
            (Some((ordinal, name)), _) => println!("[re-export] {} from library {ordinal} as {name}{attributes}", export.name),
            (None, Some(offset)) => {
                let address = if export.kind == ExportKind::Absolute { offset } else { base.wrapping_add(offset) };
                println!("0x{address:08X}  {}{attributes}", export.name)
            }
            (None, None) => println!("{} (no address)", export.name),
        }
    }
}

fn print_sections(image: &Image32) {
    let sections = image.segments.iter().flat_map(|s| s.sections.iter());
    for (index, section) in sections.enumerate() {
        println!("{index} {} {:08x} {:08x}", section.name, section.size, section.address);
    }
}
