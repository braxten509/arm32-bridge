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

use arm32_bridge::{elf, imports, runtime};

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use runtime::{call_guest, Uc};

struct Options {
    trace: bool,
    verbose: bool,
    jni_onload: bool,
    positional: Vec<String>,
}

fn parse(arguments: &[String]) -> Options {
    let mut options = Options { trace: false, verbose: false, jni_onload: false, positional: Vec::new() };
    for argument in arguments {
        match argument.as_str() {
            "--trace" => options.trace = true,
            "--verbose" => options.verbose = true,
            "--jni-onload" => options.jni_onload = true,
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
fn boot(path: &Path, options: &Options) -> Result<(&'static mut Uc, elf::Loader, usize), String> {
    runtime::init(options.trace);
    let uc = runtime::engine();
    imports::jni::create_vm(uc);
    let directory = path.parent().map(PathBuf::from).unwrap_or_default();
    let mut loader = elf::Loader::new(vec![directory]);
    let index = loader.load(uc, path)?;
    let mut total = 0;
    for i in 0..loader.libraries.len() {
        total += loader.run_init(uc, i, options.verbose)?;
    }
    eprintln!("[bridge] start-up code finished: {total} functions ran");
    Ok((uc, loader, index))
}

fn print_report(uc: &Uc) {
    let _ = uc;
    let runtime = runtime::shared();
    let used: Vec<&runtime::Stub> = runtime
        .first_use
        .iter()
        .map(|&index| &runtime.stubs[(index - 64) as usize])
        .filter(|stub| !stub.name.starts_with("jni:"))
        .collect();
    println!("\nPhone functions the game used ({}), in order of first use:", used.len());
    for stub in &used {
        let mark = if stub.handler.is_some() { "" } else { "  <- no handler yet, returned 0" };
        println!("  {:<28} x{}{mark}", stub.name, stub.calls);
    }
    let jni_used: Vec<String> = runtime
        .first_use
        .iter()
        .map(|&index| &runtime.stubs[(index - 64) as usize])
        .filter(|stub| stub.name.starts_with("jni:"))
        .map(|stub| format!("{} x{}", &stub.name[4..], stub.calls))
        .collect();
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
    let recent: Vec<String> = runtime::shared().recent.iter().cloned().collect();
    println!("Last phone functions called: {}", recent.join(", "));
}

fn command_load(options: &Options) -> ExitCode {
    let Some(path) = options.positional.get(1) else {
        eprintln!("usage: arm32-bridge load <lib.so> [--jni-onload] [--trace] [--verbose]");
        return ExitCode::from(2);
    };
    let started = Instant::now();
    let (uc, loader, index) = match boot(Path::new(path), options) {
        Ok(booted) => booted,
        Err(error) => {
            eprintln!("[bridge] {error}");
            return ExitCode::FAILURE;
        }
    };
    let library = &loader.libraries[index];
    println!("Loaded {} at {:#010x} ({} KiB)", library.name, library.base, library.size / 1024);
    for other in loader.libraries.iter().filter(|l| l.name != library.name) {
        println!("Also loaded {} at {:#010x}", other.name, other.base);
    }
    println!("Start-up functions run: {}", loader.libraries.iter().map(|l| l.init_functions.len()).sum::<usize>());

    let mut passed = true;
    if options.jni_onload {
        let Some(onload) = library.export(uc, "JNI_OnLoad") else {
            println!("{} has no JNI_OnLoad", library.name);
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

fn command_call(options: &Options, timed: bool) -> ExitCode {
    let (Some(path), Some(function)) = (options.positional.get(1), options.positional.get(2)) else {
        eprintln!("usage: arm32-bridge call|bench <lib.so> <function> [number ...]");
        return ExitCode::from(2);
    };
    let arguments: Vec<u32> = options.positional[3..].iter().map(|s| parse_number(s)).collect();
    let (uc, loader, index) = match boot(Path::new(path), options) {
        Ok(booted) => booted,
        Err(error) => {
            eprintln!("[bridge] {error}");
            return ExitCode::FAILURE;
        }
    };
    let Some(address) = loader.libraries[index].export(uc, function) else {
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

fn main() -> ExitCode {
    // The command-line tool runs everything on this one thread.
    let _lock = runtime::hold_gil();
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let options = parse(&arguments);
    match options.positional.first().map(String::as_str) {
        Some("load") => command_load(&options),
        Some("call") => command_call(&options, false),
        Some("bench") => command_call(&options, true),
        _ => {
            eprintln!("usage: arm32-bridge load|call|bench ... (see src/main.rs)");
            ExitCode::from(2)
        }
    }
}
