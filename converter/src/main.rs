//! apk-convert: turns a 32-bit-only Android game APK into one that runs on
//! 64-bit-only phones, the same way on the PC (tools/convert.sh) and on the
//! phone (the Game Bridge app). The result is unsigned; the caller signs it.
//!
//! - Every 32-bit library lib<name>.so is replaced by the shared stand-in and
//!   kept next to it as lib<name>-arm32.so. The bridge is added once as
//!   libarm32_bridge.so; each stand-in hands its original to it when Java
//!   loads the stand-in.
//! - Known Java-side problems are fixed in the dex files (dex.rs), and
//!   android:extractNativeLibs is turned on so the parts are files (manifest.rs).
//! - Phone functions the libraries import that the bridge lacks are listed.
//!
//! usage: apk-convert <game.apk> <out.apk> --bridge <libarm32_bridge.so> --standin <libstandin.so>
//!                    [--work <scratch dir>] [--abi <output ABI, default arm64-v8a>]
//! (`--abi x86_64` makes a copy for the PC's Android emulator, with an x86_64 bridge and stand-in.)
//!
//! Progress goes to stdout, one line each: `step <text>`, `abi <abi>`,
//! `fix <file>: <what>`, `warn <text>`, `missing <name>`, `done <out.apk>`, or `error <text>`.
//! Exit codes: 0 converted, 2 usage, 3 already 64-bit, 4 no 32-bit ARM code, 1 other failure.

mod dex;
mod manifest;

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// Alignment for uncompressed native libraries (Android maps them directly).
const LIBRARY_ALIGNMENT: u16 = 4096;
/// zipalign's alignment for other uncompressed entries.
const ENTRY_ALIGNMENT: u16 = 4;
/// The annotation marking a native method that is called without env and class.
const CRITICAL_NATIVE: &[u8] = b"Ldalvik/annotation/optimization/CriticalNative;";
/// The bridge's file name in the converted APK; the stand-in looks for it.
const BRIDGE_FILE: &str = "libarm32_bridge.so";

struct Options {
    input: PathBuf,
    output: PathBuf,
    bridge: PathBuf,
    standin: PathBuf,
    work: PathBuf,
    /// The ABI folder the bridge, stand-ins and originals go in.
    abi: String,
}

fn parse() -> Result<Options, String> {
    let mut arguments = std::env::args().skip(1);
    let mut positional = Vec::new();
    let (mut bridge, mut standin, mut work, mut abi) = (None, None, None, None);
    while let Some(argument) = arguments.next() {
        let mut value = || arguments.next().ok_or(format!("{argument} needs a value"));
        match argument.as_str() {
            "--bridge" => bridge = Some(PathBuf::from(value()?)),
            "--standin" => standin = Some(PathBuf::from(value()?)),
            "--work" => work = Some(PathBuf::from(value()?)),
            "--abi" => abi = Some(value()?),
            _ if argument.starts_with("--") => return Err(format!("unknown option {argument}")),
            _ => positional.push(PathBuf::from(argument)),
        }
    }
    let [input, output]: [PathBuf; 2] = positional.try_into().map_err(|_| "need <game.apk> <out.apk>".to_string())?;
    Ok(Options {
        input,
        output,
        bridge: bridge.ok_or("--bridge is required")?,
        standin: standin.ok_or("--standin is required")?,
        work: work.unwrap_or_else(std::env::temp_dir),
        abi: abi.unwrap_or_else(|| "arm64-v8a".into()),
    })
}

fn main() -> ExitCode {
    let options = match parse() {
        Ok(options) => options,
        Err(error) => {
            eprintln!("apk-convert: {error}");
            eprintln!("usage: apk-convert <game.apk> <out.apk> --bridge <lib> --standin <lib> [--work <dir>] [--abi <abi>]");
            return ExitCode::from(2);
        }
    };
    match convert(&options) {
        Ok(()) => {
            println!("done {}", options.output.display());
            ExitCode::SUCCESS
        }
        Err(Failure::Already64) => {
            println!("error This game already has 64-bit code, so it doesn't need converting.");
            ExitCode::from(3)
        }
        Err(Failure::No32) => {
            println!("error This app has no 32-bit ARM code, so there is nothing to convert.");
            ExitCode::from(4)
        }
        Err(Failure::Other(error)) => {
            println!("error {error}");
            ExitCode::FAILURE
        }
    }
}

enum Failure {
    Already64,
    No32,
    Other(String),
}

impl<E: std::fmt::Display> From<E> for Failure {
    fn from(error: E) -> Self {
        Failure::Other(error.to_string())
    }
}

fn convert(options: &Options) -> Result<(), Failure> {
    println!("step Reading the game");
    let mut archive = ZipArchive::new(File::open(&options.input)?)?;
    let names: Vec<String> = archive.file_names().map(String::from).collect();
    let library_in = |abi: &str, name: &str| {
        name.strip_prefix(&format!("lib/{abi}/")).filter(|file| file.ends_with(".so") && !file.contains('/')).map(String::from)
    };
    if names.iter().any(|name| library_in("arm64-v8a", name).is_some()) {
        return Err(Failure::Already64);
    }
    let Some(abi) = ["armeabi-v7a", "armeabi"].into_iter().find(|abi| names.iter().any(|name| library_in(abi, name).is_some()))
    else {
        return Err(Failure::No32);
    };

    // The 32-bit libraries, by file name.
    let mut libraries: Vec<(String, Vec<u8>)> = Vec::new();
    for name in &names {
        if let Some(file) = library_in(abi, name) {
            let mut bytes = Vec::new();
            archive.by_name(name)?.read_to_end(&mut bytes)?;
            libraries.push((file, bytes));
        }
    }
    println!("abi {abi}");

    println!("step Checking which phone functions the game needs");
    report_missing(&options.work, &libraries)?;

    println!("step Swapping in the bridge");
    let bridge = std::fs::read(&options.bridge).map_err(|e| format!("read {}: {e}", options.bridge.display()))?;
    let standin = std::fs::read(&options.standin).map_err(|e| format!("read {}: {e}", options.standin.display()))?;
    let mut writer = ZipWriter::new(File::create(&options.output)?);
    let stored = |alignment: u16| {
        SimpleFileOptions::default().compression_method(CompressionMethod::Stored).with_alignment(alignment)
    };
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for index in 0..archive.len() {
        let entry = archive.by_index_raw(index)?;
        let name = entry.name().to_string();
        let signature_file = name.starts_with("META-INF/")
            && (name == "META-INF/MANIFEST.MF"
                || [".SF", ".RSA", ".DSA", ".EC"].iter().any(|ending| name.ends_with(ending)));
        if name.starts_with("lib/") || signature_file || entry.is_dir() {
            continue;
        }
        let is_dex = name.starts_with("classes") && name.ends_with(".dex") && !name.contains('/');
        let is_manifest = name == "AndroidManifest.xml";
        if is_dex || is_manifest || entry.compression() == CompressionMethod::Stored {
            let was_stored = entry.compression() == CompressionMethod::Stored;
            drop(entry);
            let mut bytes = Vec::new();
            archive.by_index(index)?.read_to_end(&mut bytes)?;
            let mut entry_options = stored(ENTRY_ALIGNMENT);
            if is_dex {
                if name == "classes.dex" {
                    println!("step Fixing known Java problems");
                }
                for fix in dex::patch(&mut bytes) {
                    println!("fix {name}: {}: {} -> {}", fix.reason, fix.old, fix.new);
                }
                // Such methods get no env/class arguments; the bridge can't tell them
                // apart at run time (the annotation isn't visible through reflection).
                if bytes.windows(CRITICAL_NATIVE.len()).any(|window| window == CRITICAL_NATIVE) {
                    println!("warn {name} uses @CriticalNative, a call style the bridge doesn't support yet; those calls may get wrong values");
                }
                entry_options = deflated;
            }
            if is_manifest {
                if manifest::extract_native_libs(&mut bytes) == Some(true) {
                    println!("fix {name}: extractNativeLibs (the bridge reads the 32-bit parts as files): false -> true");
                }
                if !was_stored {
                    entry_options = deflated;
                }
            }
            writer.start_file(name.as_str(), entry_options)?;
            writer.write_all(&bytes)?;
        } else {
            writer.raw_copy_file(entry)?;
        }
    }
    let target = |file: &str| format!("lib/{}/{file}", options.abi);
    writer.start_file(target(BRIDGE_FILE), stored(LIBRARY_ALIGNMENT))?;
    writer.write_all(&bridge)?;
    for (file, bytes) in &libraries {
        let stem = file.strip_suffix(".so").unwrap();
        writer.start_file(target(&format!("{stem}-arm32.so")), stored(LIBRARY_ALIGNMENT))?;
        writer.write_all(bytes)?;
        writer.start_file(target(file), stored(LIBRARY_ALIGNMENT))?;
        writer.write_all(&standin)?;
        println!("step Stand-in for {file}");
    }
    writer.finish()?;
    Ok(())
}

/// Lists (`missing <name>`) the phone functions the libraries import that the
/// bridge has no handler for. The libraries are written to a scratch folder
/// so the loader finds the ones they need next to them.
fn report_missing(work: &Path, libraries: &[(String, Vec<u8>)]) -> Result<(), Failure> {
    let folder = work.join(format!("apk-convert-{}", std::process::id()));
    std::fs::create_dir_all(&folder)?;
    for (file, bytes) in libraries {
        std::fs::write(folder.join(file), bytes)?;
    }
    let paths: Vec<PathBuf> = libraries.iter().map(|(file, _)| folder.join(file)).collect();
    let result = arm32_bridge::imports::missing_imports(&paths);
    let _ = std::fs::remove_dir_all(&folder);
    for name in result? {
        println!("missing {name}");
    }
    Ok(())
}
