//! arm32-bridge: runs 32-bit ARM Android library code inside a 64-bit
//! process. Proof of concept for playing 32-bit-only Android games on the
//! 64-bit-only Pixel 10.
//!
//! Commands:
//!   load  <lib.so> [--jni-onload] [--trace] [--verbose]
//!       Load a 32-bit library, run its start-up code, optionally JNI_OnLoad,
//!       and report every phone function it used.
//!   call  <lib.so> <function> [number ...]
//!       Load a library and call one exported function with integer arguments.
//!   bench <lib.so> <function> <number>
//!       Time one exported function through the bridge.
//!   imports <lib.so>
//!       Load a library without running it and list the phone functions and
//!       data it imports that the bridge has no handler for (one per line).
//!   ios32 <Mach-O program> [--arch armv7|armv7s|armv6] [--trace]
//!       Load a 32-bit iPhone program, start it as dyld would, and run it on
//!       the translator (never Unicorn) until its first import call.

use arm32_bridge::{elf, imports, ios32, mem, runtime};

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use runtime::{call_guest, Uc};

struct Options {
    trace: bool,
    verbose: bool,
    jni_onload: bool,
    arch: Option<String>,
    positional: Vec<String>,
}

fn parse(arguments: &[String]) -> Options {
    let mut options = Options { trace: false, verbose: false, jni_onload: false, arch: None, positional: Vec::new() };
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--trace" => options.trace = true,
            "--verbose" => options.verbose = true,
            "--jni-onload" => options.jni_onload = true,
            "--arch" => options.arch = arguments.next().cloned(),
            _ => options.positional.push(argument.clone()),
        }
    }
    options
}

fn parse_number(text: &str) -> u32 {
    if let Some(hex) = text.strip_prefix("0x") {
        u32::from_str_radix(hex, 16).expect("hex number")
    } else {
        text.parse::<i64>().expect("number") as u32
    }
}

/// Creates the CPU, loads the library (and game libraries next to it) and
/// runs every library's start-up code, dependencies first.
fn boot(path: &Path, options: &Options) -> Result<(&'static mut Uc, usize), String> {
    runtime::init(options.trace);
    runtime::count_calls();
    let uc = runtime::engine();
    imports::jni::create_vm(uc);
    let directory = path.parent().map(PathBuf::from).unwrap_or_default();
    elf::set_search_dirs(vec![directory]);
    let index = elf::open(uc, path, options.verbose)?;
    let total: usize = elf::with_loader(|loader| loader.libraries.iter().map(|l| l.init_functions.len()).sum());
    eprintln!("[bridge] start-up code finished: {total} functions ran");
    Ok((uc, index))
}

fn print_report(uc: &Uc) {
    let _ = uc;
    let first_use: Vec<&runtime::Stub> = runtime::first_use().into_iter().filter_map(runtime::stub).collect();
    let calls = |stub: &runtime::Stub| stub.calls.load(std::sync::atomic::Ordering::Relaxed);
    let used: Vec<&runtime::Stub> = first_use.iter().copied().filter(|stub| !stub.name.starts_with("jni:")).collect();
    println!("\nPhone functions the game used ({}), in order of first use:", used.len());
    for stub in &used {
        let mark = if stub.handler.is_some() { "" } else { "  <- no handler yet, returned 0" };
        println!("  {:<28} x{}{mark}", stub.name, calls(stub));
    }
    let jni_used: Vec<String> = first_use
        .iter()
        .filter(|stub| stub.name.starts_with("jni:"))
        .map(|stub| format!("{} x{}", &stub.name[4..], calls(stub)))
        .collect();
    let runtime = runtime::shared();
    println!("\nJava (JNI) functions used ({}): {}", jni_used.len(), jni_used.join(", "));
    println!("Java requests: {}", runtime.state.jni.java_calls.len());
    if runtime.missing.is_empty() {
        println!("Phone functions without a handler: none");
    } else {
        println!("Phone functions without a handler ({}): {}", runtime.missing.len(), runtime.missing.join(", "));
    }
}

fn print_fault(uc: &Uc, error: &str) {
    println!("\nFAILED: {error}");
    let _ = uc;
    let recent = runtime::recent_calls();
    println!("Last phone functions called: {}", recent.join(", "));
}

fn command_load(options: &Options) -> ExitCode {
    let Some(path) = options.positional.get(1) else {
        eprintln!("usage: arm32-bridge load <lib.so> [--jni-onload] [--trace] [--verbose]");
        return ExitCode::from(2);
    };
    let started = Instant::now();
    let (uc, index) = match boot(Path::new(path), options) {
        Ok(booted) => booted,
        Err(error) => {
            eprintln!("[bridge] {error}");
            return ExitCode::FAILURE;
        }
    };
    let (name, onload) = elf::with_loader(|loader| {
        let library = &loader.libraries[index];
        println!("Loaded {} at {:#010x} ({} KiB)", library.name, library.base, library.size / 1024);
        for other in loader.libraries.iter().filter(|l| l.name != library.name) {
            println!("Also loaded {} at {:#010x}", other.name, other.base);
        }
        println!("Start-up functions run: {}", loader.libraries.iter().map(|l| l.init_functions.len()).sum::<usize>());
        (library.name.clone(), library.export(uc, "JNI_OnLoad"))
    });

    let mut passed = true;
    if options.jni_onload {
        let Some(onload) = onload else {
            println!("{name} has no JNI_OnLoad");
            return ExitCode::FAILURE;
        };
        let vm = runtime::shared().state.jni.vm;
        match call_guest(uc, onload, &[vm, 0]) {
            Ok(result) => {
                let version = result as u32;
                let valid = matches!(version, 0x0001_0001 | 0x0001_0002 | 0x0001_0004 | 0x0001_0006 | 0x0001_0008);
                println!(
                    "JNI_OnLoad returned {version:#x} ({})",
                    if valid { format!("JNI {}.{}", version >> 16, version & 0xFFFF) } else { "NOT a JNI version".into() }
                );
                passed &= valid;
            }
            Err(error) => {
                print_fault(uc, &error);
                passed = false;
            }
        }
    }
    print_report(uc);
    println!("\nTime: {:.2} s", started.elapsed().as_secs_f64());
    let has_missing = !runtime::shared().missing.is_empty();
    println!(
        "Result: {}",
        if passed && !has_missing {
            "PASS"
        } else if passed {
            "PASS with stand-ins that returned 0 (see list above)"
        } else {
            "FAIL"
        }
    );
    if passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn command_imports(options: &Options) -> ExitCode {
    let paths: Vec<PathBuf> = options.positional[1..].iter().map(PathBuf::from).collect();
    if paths.is_empty() {
        eprintln!("usage: arm32-bridge imports <lib.so> [<lib.so> ...]");
        return ExitCode::from(2);
    }
    match imports::missing_imports(&paths) {
        Ok(missing) => {
            for name in &missing {
                println!("{name}");
            }
            eprintln!("[bridge] {} imports without a handler", missing.len());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("[bridge] {error}");
            ExitCode::FAILURE
        }
    }
}

fn command_call(options: &Options, timed: bool) -> ExitCode {
    let (Some(path), Some(function)) = (options.positional.get(1), options.positional.get(2)) else {
        eprintln!("usage: arm32-bridge call|bench <lib.so> <function> [number ...]");
        return ExitCode::from(2);
    };
    let arguments: Vec<u32> = options.positional[3..].iter().map(|s| parse_number(s)).collect();
    let (uc, index) = match boot(Path::new(path), options) {
        Ok(booted) => booted,
        Err(error) => {
            eprintln!("[bridge] {error}");
            return ExitCode::FAILURE;
        }
    };
    let Some(address) = elf::with_loader(|loader| loader.libraries[index].export(uc, function)) else {
        eprintln!("[bridge] {function} is not exported");
        return ExitCode::FAILURE;
    };
    let started = Instant::now();
    let result = call_guest(uc, address, &arguments);
    let elapsed = started.elapsed();
    match result {
        Ok(value) => {
            println!("{function} returned {} ({:#x})", value as u32 as i32, value as u32);
            if timed {
                println!("time through the bridge: {:.1} ms", elapsed.as_secs_f64() * 1000.0);
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            print_fault(uc, &error);
            ExitCode::FAILURE
        }
    }
}

fn command_ios32(options: &Options) -> ExitCode {
    let Some(path) = options.positional.get(1) else {
        eprintln!("usage: arm32-bridge ios32 <Mach-O program> [--arch armv7|armv7s|armv6] [--trace]");
        return ExitCode::from(2);
    };
    let fail = |message: String| {
        println!("FAILED: {message}");
        println!("Unicorn fallbacks: {}", arm32_bridge::dynarec::fallback_count());
        println!("Result: FAIL");
        ExitCode::FAILURE
    };
    if !arm32_bridge::dynarec::enabled() || arm32_bridge::dynarec::check::enabled() {
        return fail("ios32 runs only on the translator (ARM64 host, no ARM32_ENGINE=unicorn, no ARM32_CHECK)".into());
    }
    let choice = match ios32::slice_choice(options.arch.as_deref()) {
        Ok(choice) => choice,
        Err(error) => return fail(error),
    };
    let program = match ios32::prepare(Path::new(path), &choice) {
        Ok(program) => program,
        Err(error) => return fail(error),
    };
    let image = &program.image;
    println!(
        "Program: {:?} slice, {}, slide {:#x}",
        image.slice.subtype,
        if image.is_pie() { "position-independent" } else { "fixed addresses" },
        program.plan.slide
    );
    for segment in &program.plan.segments {
        println!("  {:<12} {:#010x}..{:#010x}", segment.name, segment.start, u64::from(segment.start) + u64::from(segment.size));
    }
    if let Some(guard) = program.plan.null_guard {
        println!("  low guard    {:#010x}..{:#010x} (unmapped)", guard.start, guard.end);
    }
    println!("Stand-ins and return traps at {:#010x}", program.layout.stub_base);
    if let Err(error) = mem::set_layout(program.layout.clone()).and_then(|_| runtime::set_platform(&ios32::APPLE32)) {
        return fail(error);
    }
    runtime::init(options.trace);
    runtime::count_calls();
    let uc = runtime::engine();
    let loaded = match ios32::load(&program) {
        Ok(loaded) => loaded,
        Err(error) => return fail(error),
    };
    println!(
        "Loaded: {} rebases, {} binds to stand-ins ({} stand-ins), {} binds inside the program, {} missing weak imports set to 0",
        loaded.rebases, loaded.binds_to_stubs, loaded.stubs, loaded.binds_to_self, loaded.weak_missing
    );
    let start = match ios32::start_state(uc, &program, path) {
        Ok(start) => start,
        Err(error) => return fail(error),
    };
    println!(
        "Start: {} at {:#010x} ({}), sp {:#010x}, r0-r3 {:#x} {:#x} {:#x} {:#x}",
        start.kind,
        start.pc & !1,
        if start.pc & 1 == 1 { "Thumb" } else { "ARM" },
        start.sp,
        start.registers[0],
        start.registers[1],
        start.registers[2],
        start.registers[3]
    );
    let result = ios32::run_initializers(uc, &program, &start)
        .and_then(|count| {
            println!("Start-up functions run: {count}");
            runtime::start_guest(uc, start.pc, &start.registers, start.sp)
        });
    let fallbacks = arm32_bridge::dynarec::fallback_count();
    match (ios32::first_missing_call(), result) {
        (Some((name, registers, lr)), _) => {
            println!(
                "First import call: {name} (r0-r3 {:#x} {:#x} {:#x} {:#x}, returns to {lr:#010x})",
                registers[0], registers[1], registers[2], registers[3]
            );
            println!("Unicorn fallbacks: {fallbacks}");
            let passed = fallbacks == 0;
            println!("Result: {}", if passed { "PASS" } else { "FAIL (fallbacks)" });
            if passed {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        (None, Ok(value)) => fail(format!("the program returned {:#x} without calling an import", value as u32)),
        (None, Err(error)) => fail(error),
    }
}

fn main() -> ExitCode {
    // The command-line tool runs everything on this one thread.
    let _lock = runtime::hold_gil();
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let options = parse(&arguments);
    let code = match options.positional.first().map(String::as_str) {
        Some("load") => command_load(&options),
        Some("call") => command_call(&options, false),
        Some("bench") => command_call(&options, true),
        Some("imports") => command_imports(&options),
        Some("ios32") => command_ios32(&options),
        _ => {
            eprintln!("usage: arm32-bridge load|call|bench|imports|ios32 ... (see src/main.rs)");
            ExitCode::from(2)
        }
    };
    if arm32_bridge::dynarec::enabled() && (runtime::stats_enabled() || arm32_bridge::dynarec::check::enabled()) {
        arm32_bridge::dynarec::print_stats();
    }
    code
}
