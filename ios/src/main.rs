use gamebridge_ios::macho::{BindKind, Image};
use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!("usage: ios-run fixups <program>          print the program's fix-up lists");
    eprintln!("       ios-run run <App.app> [--keep-going]  start the game (64-bit ARM only)");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("fixups") if args.len() == 3 => print_fixups(&args[2]),
        Some("run") if args.len() >= 3 => run(&args[2], args[3..].iter().any(|a| a == "--keep-going")),
        _ => usage(),
    }
}

#[cfg(target_arch = "aarch64")]
fn run(app: &str, keep_going: bool) -> ExitCode {
    gamebridge_ios::trap::KEEP_GOING.store(keep_going, std::sync::atomic::Ordering::Relaxed);
    match gamebridge_ios::run::run(app) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("can't start {app}: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(target_arch = "aarch64"))]
fn run(_app: &str, _keep_going: bool) -> ExitCode {
    eprintln!("running games needs a 64-bit ARM build (use qemu-aarch64 on a PC)");
    ExitCode::FAILURE
}

fn print_fixups(path: &str) -> ExitCode {
    let file = match std::fs::read(path) {
        Ok(file) => file,
        Err(error) => {
            eprintln!("can't read {path}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let image = match Image::parse(&file) {
        Ok(image) => image,
        Err(error) => {
            eprintln!("can't read {path}: {error}");
            return ExitCode::FAILURE;
        }
    };
    for address in &image.rebases {
        println!("rebase {address:#x}");
    }
    for bind in &image.binds {
        let kind = match bind.kind {
            BindKind::Normal => "bind",
            BindKind::Lazy => "lazy",
            BindKind::Weak => "weak",
        };
        println!("{kind} {:#x} {} {} {}", bind.address, bind.addend, image.library_name(bind.library_ordinal), bind.symbol);
    }
    ExitCode::SUCCESS
}
