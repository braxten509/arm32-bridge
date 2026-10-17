//! apk-convert: turns a 32-bit-only Android game APK into one that runs on
//! 64-bit-only phones, the same way on the PC (tools/convert.sh) and on the
//! phone (the Game Bridge app). The result is unsigned; the caller signs it.
//!
//! - Every 32-bit library lib<name>.so is replaced by the shared stand-in and
//!   kept next to it as lib<name>-arm32.so. The bridge is added once as
//!   libarm32_bridge.so; each stand-in hands its original to it when Java
//!   loads the stand-in.
//! - Known fixes for particular games (fixes.rs) are applied to their exact
//!   libraries, and their bridge settings go in a settings file next to the bridge.
//! - Known Java-side problems are fixed in the dex files (dex.rs), and
//!   android:extractNativeLibs is turned on so the parts are files (manifest.rs).
//! - Phone functions the libraries import that the bridge lacks are listed.
//! - A 64-bit game (with --pacer) keeps its own libraries and gets Game
//!   Bridge's frame helper instead (pacer/): libgamebridge_pacer.so next to its
//!   libraries, the Java class that loads it as the next classesN.dex, and that
//!   class listed in the manifest as a content provider (manifest.rs). The dex
//!   fixes apply too (the onDrawFrame frame limiter one matters here).
//! - A bundle (.xapk, .apkm, .apks: one game split over several APKs, see
//!   bundle.rs) becomes a folder instead: every piece, converted or copied, as
//!   its own APK (sign them all with the same key and install them together),
//!   plus the game data under obb/ (copy it to /sdcard/Android/obb/).
//!   Pieces made only for other kinds of phones (x86) are left out.
//!
//! usage: apk-convert <game.apk|bundle> <out.apk|out folder> --bridge <libarm32_bridge.so> --standin <libstandin.so>
//!                    [--pacer <libgamebridge_pacer.so>] [--work <scratch dir>] [--abi <output ABI, default arm64-v8a>] [--min-target-sdk <N>]
//!        apk-convert --inspect <game.apk|bundle> [--base <out.apk>] [--work <scratch dir>]
//! (`--abi x86_64` makes a copy for the PC's Android emulator, with an x86_64 bridge and stand-in.)
//! (`--min-target-sdk N` raises the game's target Android version to N: phones without root refuse older ones.)
//! `--inspect` only looks: it prints `kind apk|bundle`, `data yes|no` for a bundle (carries game data),
//! `download yes|no` (the game's own code starts Google Play's expansion-file download), `bits 32|64|none`
//! (what native code the game has) and `engine yes|no` (built with a known game engine, or with its own: a library that
//! draws and plays sound itself and is bigger than the Java code, see own_engine), and with --base copies a bundle's base
//! APK (the piece with the Java code) out, for reading its name and icon.
//!
//! Progress goes to stdout, one line each: `step <text>`, `abi <abi>`,
//! `fix <file>: <what>`, `warn <text>`, `missing <name>`, `piece <file>`,
//! `obb <path>`, `done <out>`, or `error <text>`.
//! Exit codes: 0 converted, 2 usage, 3 already 64-bit (and no --pacer given), 4 no ARM code, 1 other failure.

mod bundle;
mod dex;
mod fixes;
mod icon;
mod manifest;
mod needed;

use std::fs::File;
use std::io::{Read, Seek, Write};
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
/// The 64-bit frame helper's file name in a converted 64-bit game.
const PACER_FILE: &str = "libgamebridge_pacer.so";
/// The Java class that loads it (pacer/java, built by tools/build-pacer-dex.sh).
const PACER_DEX: &[u8] = include_bytes!("../assets/pacer.dex");
const PACER_CLASS: &str = "dev.braxten.gamebridge.Pacer";
/// 64-bit games' libraries are kept 16 KB aligned: phones with 16 KB memory pages need it.
const LIBRARY_ALIGNMENT_64: u16 = 16384;
/// 32-bit ARM library folders, the better one first.
const ARM32_ABIS: [&str; 2] = ["armeabi-v7a", "armeabi"];

struct Options {
    input: PathBuf,
    output: PathBuf,
    bridge: PathBuf,
    standin: PathBuf,
    /// The 64-bit frame helper; without it 64-bit games are refused.
    pacer: Option<PathBuf>,
    work: PathBuf,
    /// The ABI folder the bridge, stand-ins and originals go in.
    abi: String,
    /// Raise the game's target Android version to at least this (phones without root refuse older ones).
    min_target_sdk: Option<u32>,
}

enum Command {
    Convert(Options),
    Inspect { input: PathBuf, base: Option<PathBuf>, work: PathBuf },
}

fn parse() -> Result<Command, String> {
    let mut arguments = std::env::args().skip(1);
    let mut positional = Vec::new();
    let (mut bridge, mut standin, mut work, mut abi, mut base, mut pacer) = (None, None, None, None, None, None);
    let mut min_target_sdk = None;
    let mut inspect = false;
    while let Some(argument) = arguments.next() {
        let mut value = || arguments.next().ok_or(format!("{argument} needs a value"));
        match argument.as_str() {
            "--bridge" => bridge = Some(PathBuf::from(value()?)),
            "--standin" => standin = Some(PathBuf::from(value()?)),
            "--pacer" => pacer = Some(PathBuf::from(value()?)),
            "--work" => work = Some(PathBuf::from(value()?)),
            "--abi" => abi = Some(value()?),
            "--base" => base = Some(PathBuf::from(value()?)),
            "--min-target-sdk" => {
                let text = value()?;
                min_target_sdk = Some(text.parse().map_err(|_| format!("--min-target-sdk needs a number, not {text}"))?);
            }
            "--inspect" => inspect = true,
            _ if argument.starts_with("--") => return Err(format!("unknown option {argument}")),
            _ => positional.push(PathBuf::from(argument)),
        }
    }
    let work = work.unwrap_or_else(std::env::temp_dir);
    if inspect {
        let [input]: [PathBuf; 1] = positional.try_into().map_err(|_| "--inspect needs one <game file>".to_string())?;
        return Ok(Command::Inspect { input, base, work });
    }
    let [input, output]: [PathBuf; 2] = positional.try_into().map_err(|_| "need <game.apk> <out.apk>".to_string())?;
    Ok(Command::Convert(Options {
        input,
        output,
        bridge: bridge.ok_or("--bridge is required")?,
        standin: standin.ok_or("--standin is required")?,
        pacer,
        work,
        abi: abi.unwrap_or_else(|| "arm64-v8a".into()),
        min_target_sdk,
    }))
}

fn main() -> ExitCode {
    let options = match parse() {
        Ok(Command::Convert(options)) => options,
        Ok(Command::Inspect { input, base, work }) => {
            return match inspect(&input, base.as_deref(), &work) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    println!("error {error}");
                    ExitCode::FAILURE
                }
            };
        }
        Err(error) => {
            eprintln!("apk-convert: {error}");
            eprintln!("usage: apk-convert <game.apk|bundle> <out.apk|folder> --bridge <lib> --standin <lib> [--work <dir>] [--abi <abi>]");
            eprintln!("       apk-convert --inspect <game.apk|bundle> [--base <out.apk>] [--work <dir>]");
            return ExitCode::from(2);
        }
    };
    let scratch = options.work.join(format!("apk-convert-pieces-{}", std::process::id()));
    let result = match bundle::open(&options.input, &scratch) {
        Ok(Some(bundle)) => convert_bundle(&options, bundle),
        Ok(None) => convert(&options),
        Err(error) => Err(Failure::Other(error)),
    };
    let _ = std::fs::remove_dir_all(&scratch);
    match result {
        Ok(()) => {
            println!("done {}", options.output.display());
            ExitCode::SUCCESS
        }
        Err(Failure::Already64) => {
            println!("error This game already has 64-bit code, so it doesn't need converting.");
            ExitCode::from(3)
        }
        Err(Failure::No32) => {
            println!("error This app has no ARM code, so there is nothing to convert.");
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

/// The library file name if `name` is a library directly in lib/<abi>/.
fn library_in(abi: &str, name: &str) -> Option<String> {
    name.strip_prefix(&format!("lib/{abi}/")).filter(|file| file.ends_with(".so") && !file.contains('/')).map(String::from)
}

/// What native code a set of file names (one APK, or every piece of a bundle) has:
/// 64 for any 64-bit ARM library, else the best 32-bit ARM folder, else none.
enum Bits {
    SixtyFour,
    ThirtyTwo(&'static str),
    None,
}

fn bits<'a>(names: impl Iterator<Item = &'a String> + Clone) -> Bits {
    if names.clone().any(|name| library_in("arm64-v8a", name).is_some()) {
        return Bits::SixtyFour;
    }
    match ARM32_ABIS.into_iter().find(|abi| names.clone().any(|name| library_in(abi, name).is_some())) {
        Some(abi) => Bits::ThirtyTwo(abi),
        None => Bits::None,
    }
}

fn read_libraries<R: Read + Seek>(archive: &mut ZipArchive<R>, abi: &str) -> Result<Vec<(String, Vec<u8>)>, Failure> {
    let names: Vec<String> = archive.file_names().map(String::from).collect();
    let mut libraries = Vec::new();
    for name in &names {
        if let Some(file) = library_in(abi, name) {
            let mut bytes = Vec::new();
            archive.by_name(name)?.read_to_end(&mut bytes)?;
            libraries.push((file, bytes));
        }
    }
    Ok(libraries)
}

/// The bridge and stand-in builds, read from the paths given.
struct Parts {
    bridge: Vec<u8>,
    standin: Vec<u8>,
}

impl Parts {
    fn read(options: &Options) -> Result<Parts, Failure> {
        let read = |path: &Path| std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()));
        Ok(Parts { bridge: read(&options.bridge)?, standin: read(&options.standin)? })
    }
}

fn convert(options: &Options) -> Result<(), Failure> {
    println!("step Reading the game");
    let mut archive = ZipArchive::new(File::open(&options.input)?)?;
    let carries_game_data = std::fs::metadata(&options.input)?.len() > dex::PLAY_EXPANSION_APK_LIMIT;
    let names: Vec<String> = archive.file_names().map(String::from).collect();
    let abi = match bits(names.iter()) {
        Bits::SixtyFour => {
            let helper = read_pacer(options)?;
            println!("abi arm64-v8a");
            println!("step Adding the frame helper");
            let native = Native::Paced { helper: Some(&helper), loader: true };
            return rewrite(&mut archive, &options.output, options, carries_game_data, native, &[]);
        }
        Bits::None => return Err(Failure::No32),
        Bits::ThirtyTwo(abi) => abi,
    };
    let mut libraries = read_libraries(&mut archive, abi)?;
    println!("abi {abi}");

    println!("step Checking which phone functions the game needs");
    report_missing(&options.work, &libraries)?;

    let applied = known_fixes(&mut libraries);
    println!("step Swapping in the bridge");
    let parts = Parts::read(options)?;
    let bridge = Bridge { library: &parts.bridge, settings: &applied.settings };
    let native = Native::Bridged { bridge: Some(bridge), standin: &parts.standin, libraries: &libraries };
    rewrite(&mut archive, &options.output, options, carries_game_data, native, &[])
}

/// The 64-bit frame helper, or Already64 when none was given.
fn read_pacer(options: &Options) -> Result<Vec<u8>, Failure> {
    let path = options.pacer.as_ref().ok_or(Failure::Already64)?;
    std::fs::read(path).map_err(|e| Failure::Other(format!("read {}: {e}", path.display())))
}

fn convert_bundle(options: &Options, mut bundle: bundle::Bundle) -> Result<(), Failure> {
    println!("step Reading the game");
    let carries_game_data = std::fs::metadata(&options.input)?.len() > dex::PLAY_EXPANSION_APK_LIMIT;
    let every_name: Vec<String> = bundle.pieces.iter().flat_map(|piece| piece.names()).collect();
    let (abi, helper) = match bits(every_name.iter()) {
        Bits::SixtyFour => ("arm64-v8a", Some(read_pacer(options)?)),
        Bits::None => return Err(Failure::No32),
        Bits::ThirtyTwo(abi) => (abi, None),
    };
    println!("abi {abi}");
    // A piece of only libraries for other phones (config.x86.apk, or the old
    // armeabi when armeabi-v7a is there) can't be installed next to the converted one.
    bundle.pieces.retain(|piece| {
        let names = piece.names();
        let has_libraries = names.iter().any(|name| name.starts_with("lib/") && name.ends_with(".so"));
        let keep = !has_libraries || piece.has_code() || names.iter().any(|name| library_in(abi, name).is_some());
        if !keep {
            println!("step Leaving out {} (made for other phones)", piece.name);
        }
        keep
    });
    if let Some(helper) = &helper {
        println!("step Adding the frame helper");
        std::fs::create_dir_all(&options.output)?;
        let tables = resource_tables(&mut bundle);
        // The helper goes with the game's first 64-bit libraries, the loader with its Java code.
        let mut helper_added = false;
        for piece in bundle.pieces.iter_mut() {
            let has_libraries = piece.names().iter().any(|name| library_in(abi, name).is_some());
            let native = Native::Paced { helper: (!helper_added && has_libraries).then_some(helper.as_slice()), loader: piece.is_base() };
            helper_added |= has_libraries;
            rewrite(&mut piece.archive, &options.output.join(&piece.name), options, carries_game_data, native, &tables)?;
            println!("piece {}", piece.name);
        }
        return copy_game_data(options, &bundle);
    }
    let mut libraries = Vec::new();
    for piece in &mut bundle.pieces {
        libraries.push(read_libraries(&mut piece.archive, abi)?);
    }

    println!("step Checking which phone functions the game needs");
    report_missing(&options.work, &libraries.concat())?;

    let mut settings = String::new();
    for libraries in &mut libraries {
        settings.push_str(&known_fixes(libraries).settings);
    }
    println!("step Swapping in the bridge");
    let parts = Parts::read(options)?;
    std::fs::create_dir_all(&options.output)?;
    let tables = resource_tables(&mut bundle);
    let mut bridge_added = false;
    for (piece, libraries) in bundle.pieces.iter_mut().zip(&libraries) {
        let output = options.output.join(&piece.name);
        // The bridge goes in once, with the first 32-bit libraries; all pieces' libraries end up in one folder.
        let bridge = (!bridge_added && !libraries.is_empty()).then_some(Bridge { library: &parts.bridge, settings: &settings });
        bridge_added |= bridge.is_some();
        let native = Native::Bridged { bridge, standin: &parts.standin, libraries };
        rewrite(&mut piece.archive, &output, options, carries_game_data, native, &tables)?;
        println!("piece {}", piece.name);
    }
    copy_game_data(options, &bundle)
}

/// Every piece's resource table (for the icon: its pictures may be in another piece).
fn resource_tables(bundle: &mut bundle::Bundle) -> Vec<Vec<u8>> {
    bundle.pieces.iter_mut().filter_map(|piece| {
        let mut bytes = Vec::new();
        piece.archive.by_name("resources.arsc").ok()?.read_to_end(&mut bytes).ok()?;
        Some(bytes)
    }).collect()
}

/// A bundle's game data, copied to obb/ in the output folder.
fn copy_game_data(options: &Options, bundle: &bundle::Bundle) -> Result<(), Failure> {
    if !bundle.game_data.is_empty() {
        println!("step Copying game data");
        let mut outer = ZipArchive::new(File::open(&options.input)?)?;
        for (index, relative) in &bundle.game_data {
            let target = options.output.join("obb").join(relative);
            if relative.split('/').any(|part| part == ".." || part.is_empty()) {
                return Err(Failure::Other(format!("game data file with an unsafe path: {relative}")));
            }
            std::fs::create_dir_all(target.parent().unwrap())?;
            std::io::copy(&mut outer.by_index(*index)?, &mut File::create(&target)?)?;
            println!("obb {relative}");
        }
    }
    Ok(())
}

/// Applies the known fixes for particular games (fixes.rs) and reports them.
fn known_fixes(libraries: &mut [(String, Vec<u8>)]) -> fixes::Applied {
    let applied = fixes::apply(libraries);
    if !applied.report.is_empty() {
        println!("step Applying known fixes for this game");
    }
    for line in &applied.report {
        println!("{line}");
    }
    applied
}

/// The bridge library, and the settings file text to put next to it (empty for none).
#[derive(Clone, Copy)]
struct Bridge<'a> {
    library: &'a [u8],
    settings: &'a str,
}

/// What happens to a piece's native libraries.
enum Native<'a> {
    /// 32-bit: `libraries` swapped for the stand-in (originals kept as
    /// lib<name>-arm32.so) and, when given, the bridge added (with the game's settings file).
    Bridged { bridge: Option<Bridge<'a>>, standin: &'a [u8], libraries: &'a [(String, Vec<u8>)] },
    /// 64-bit: libraries kept; the frame helper added when given, and when
    /// `loader`, the Java class that loads it (with its manifest entry).
    Paced { helper: Option<&'a [u8]>, loader: bool },
}

/// Writes `archive` to `output` without its old signature, with the dex and
/// manifest fixes and its native libraries handled as `native` says.
fn rewrite<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    output: &Path,
    options: &Options,
    carries_game_data: bool,
    native: Native,
    // A bundle's pieces' resource tables (for the icon: its pictures may be in another piece).
    tables: &[Vec<u8>],
) -> Result<(), Failure> {
    let paced = matches!(native, Native::Paced { .. });
    let adds_loader = matches!(native, Native::Paced { loader: true, .. });
    let mut writer = ZipWriter::new(File::create(output)?);
    let stored = |alignment: u16| {
        SimpleFileOptions::default().compression_method(CompressionMethod::Stored).with_alignment(alignment)
    };
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    // Full-size home-screen icon (icon.rs): needs the manifest and the resource table together.
    let read = |archive: &mut ZipArchive<R>, name: &str| -> Option<Vec<u8>> {
        let mut bytes = Vec::new();
        archive.by_name(name).ok()?.read_to_end(&mut bytes).ok()?;
        Some(bytes)
    };
    let round_icons = match (read(archive, "AndroidManifest.xml"), read(archive, "resources.arsc")) {
        (Some(manifest), Some(table)) => icon::round_icons(&manifest, &table, tables),
        _ => None,
    };
    for index in 0..archive.len() {
        let entry = archive.by_index_raw(index)?;
        let name = entry.name().to_string();
        let signature_file = name.starts_with("META-INF/")
            && (name == "META-INF/MANIFEST.MF"
                || [".SF", ".RSA", ".DSA", ".EC"].iter().any(|ending| name.ends_with(ending)));
        if signature_file || entry.is_dir() || (name.starts_with("lib/") && !paced) {
            continue;
        }
        if name.starts_with("lib/") {
            // A 64-bit game keeps its libraries; stored ones stay page-aligned (Android maps them straight from the APK).
            if entry.compression() == CompressionMethod::Stored {
                drop(entry);
                let mut bytes = Vec::new();
                archive.by_index(index)?.read_to_end(&mut bytes)?;
                writer.start_file(name.as_str(), stored(LIBRARY_ALIGNMENT_64))?;
                writer.write_all(&bytes)?;
            } else {
                writer.raw_copy_file(entry)?;
            }
            continue;
        }
        let is_dex = name.starts_with("classes") && name.ends_with(".dex") && !name.contains('/');
        let is_manifest = name == "AndroidManifest.xml";
        if let (Some(icons), "resources.arsc") = (&round_icons, name.as_str()) {
            drop(entry);
            // Android maps the table straight from the APK: stored, 4-byte aligned.
            writer.start_file(name.as_str(), stored(4))?;
            writer.write_all(&icons.table)?;
            continue;
        }
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
                for fix in dex::patch(&mut bytes, carries_game_data) {
                    println!("fix {name}: {}: {} -> {}", fix.reason, fix.old, fix.new);
                }
                // Such methods get no env/class arguments; the bridge can't tell them
                // apart at run time (the annotation isn't visible through reflection).
                if bytes.windows(CRITICAL_NATIVE.len()).any(|window| window == CRITICAL_NATIVE) {
                    println!("warn {name} uses @CriticalNative, a call style the bridge doesn't support yet; those calls may get wrong values");
                }
                entry_options = deflated;
            }
            if is_manifest && adds_loader {
                let package = manifest::package_name(&bytes).ok_or("the manifest has no package name")?;
                let changed = manifest::add_provider(&bytes, PACER_CLASS, &format!("{package}.gamebridge.pacer"))?;
                if changed != bytes {
                    println!("fix {name}: frame helper loader ({PACER_CLASS}) added");
                }
                bytes = changed;
            }
            if is_manifest {
                if !paced && manifest::extract_native_libs(&mut bytes) == Some(true) {
                    println!("fix {name}: extractNativeLibs (the bridge reads the 32-bit parts as files): false -> true");
                }
                if let Some(minimum) = options.min_target_sdk {
                    match manifest::raise_target_sdk(&mut bytes, minimum) {
                        Ok(Some((old, new))) => println!("fix {name}: target Android version (Android refuses games made for older versions): {old} -> {new}"),
                        Ok(None) => {}
                        Err(reason) => println!("warn {name}: can't raise the target Android version: {reason}"),
                    }
                }
                if let Some(icons) = &round_icons {
                    if icon::switch_icons(&mut bytes, &icons.switched) > 0 {
                        println!("fix {name}: full-size home-screen icon (the game's own picture filling the phone's icon shape)");
                    }
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
    for (path, xml) in round_icons.iter().flat_map(|icons| &icons.files) {
        writer.start_file(path.as_str(), deflated)?;
        writer.write_all(xml)?;
    }
    let target = |file: &str| format!("lib/{}/{file}", options.abi);
    let (bridge, standin, libraries): (Option<Bridge>, &[u8], &[(String, Vec<u8>)]) = match native {
        Native::Bridged { bridge, standin, libraries } => (bridge, standin, libraries),
        Native::Paced { helper, loader } => {
            if let Some(helper) = helper {
                // Always next to the game's ARM libraries (the emulator runs those through its translator).
                writer.start_file(format!("lib/arm64-v8a/{PACER_FILE}"), stored(LIBRARY_ALIGNMENT_64))?;
                writer.write_all(helper)?;
            }
            if loader {
                let file = next_dex_name(archive.file_names());
                writer.start_file(file.as_str(), deflated)?;
                writer.write_all(PACER_DEX)?;
                println!("fix {file}: frame helper loader added");
            }
            (None, &[], &[])
        }
    };
    if let Some(bridge) = bridge {
        writer.start_file(target(BRIDGE_FILE), stored(LIBRARY_ALIGNMENT))?;
        writer.write_all(bridge.library)?;
        if !bridge.settings.is_empty() {
            writer.start_file(target(fixes::SETTINGS_FILE), stored(LIBRARY_ALIGNMENT))?;
            writer.write_all(bridge.settings.as_bytes())?;
        }
    }
    for (file, bytes) in libraries {
        let stem = file.strip_suffix(".so").unwrap();
        writer.start_file(target(&format!("{stem}-arm32.so")), stored(LIBRARY_ALIGNMENT))?;
        writer.write_all(bytes)?;
        writer.start_file(target(file), stored(LIBRARY_ALIGNMENT))?;
        writer.write_all(standin)?;
        println!("step Stand-in for {file}");
    }
    writer.finish()?;
    Ok(())
}

/// The next free dex file name: classes.dex, classes2.dex, classes3.dex …
fn next_dex_name<'a>(names: impl Iterator<Item = &'a str>) -> String {
    let highest = names
        .filter_map(|name| name.strip_prefix("classes")?.strip_suffix(".dex"))
        .filter_map(|number| if number.is_empty() { Some(1) } else { number.parse::<u32>().ok() })
        .max();
    match highest {
        None => "classes.dex".into(),
        Some(number) => format!("classes{}.dex", number + 1),
    }
}

/// `--inspect`: what kind of file it is and what native code it has, and for
/// a bundle optionally its base APK copied out.
fn inspect(input: &Path, base: Option<&Path>, work: &Path) -> Result<(), String> {
    let scratch = work.join(format!("apk-convert-pieces-{}", std::process::id()));
    let result = (|| {
        match bundle::open(input, &scratch)? {
            None => {
                let mut archive = ZipArchive::new(File::open(input).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
                println!("kind apk");
                println!("download {}", yes_no(starts_download(&mut archive)));
                let names: Vec<String> = archive.file_names().map(String::from).collect();
                report_code(&names, |abi| own_engine(&mut archive, abi) > java_size(&mut archive));
            }
            Some(mut bundle) => {
                println!("kind bundle");
                println!("data {}", yes_no(!bundle.game_data.is_empty()));
                let piece = match bundle.pieces.iter().position(|piece| piece.has_code()) {
                    // The base, else (an unusual bundle) the first piece with code.
                    Some(first) => {
                        let base = (0..bundle.pieces.len()).find(|&index| bundle.pieces[index].is_base()).unwrap_or(first);
                        &mut bundle.pieces[base]
                    }
                    None => return Err("the bundle has no base APK".into()),
                };
                println!("download {}", yes_no(starts_download(&mut piece.archive)));
                if let Some(base) = base {
                    bundle::save(piece, base).map_err(|e| format!("copy out {}: {e}", piece.name))?;
                }
                let names: Vec<String> = bundle.pieces.iter().flat_map(|piece| piece.names()).collect();
                report_code(&names, |abi| {
                    let engine = bundle.pieces.iter_mut().map(|piece| own_engine(&mut piece.archive, abi)).max().unwrap_or(0);
                    engine > bundle.pieces.iter_mut().map(|piece| java_size(&mut piece.archive)).sum()
                });
            }
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    result
}


/// Libraries of game engines: an app built with one is a game even when its
/// manifest doesn't say so (Unity, Unreal, Godot, cocos2d-x, libGDX, Defold,
/// GameMaker, Solar2D, LÖVE). Only file names in lib/<abi>/ count.
fn is_engine_library(path: &str) -> bool {
    let Some(file) = path.strip_prefix("lib/").and_then(|rest| rest.split_once('/')).map(|(_, file)| file) else { return false };
    let Some(stem) = file.strip_prefix("lib").and_then(|rest| rest.strip_suffix(".so")) else { return false };
    matches!(stem, "unity" | "il2cpp" | "UE4" | "Unreal" | "godot_android" | "gdx" | "dmengine" | "yoyo" | "corona" | "love")
        || stem.starts_with("cocos2d")
}

/// Prints `bits` and `engine` for an APK's or a bundle's file names. `own_engine`
/// says whether the libraries of one ABI folder hold an engine of the app's own; it only runs
/// when no known engine is there.
fn report_code(names: &[String], own_engine: impl FnOnce(&str) -> bool) {
    let (bits, abi) = match bits(names.iter()) {
        Bits::SixtyFour => ("64", Some("arm64-v8a")),
        Bits::ThirtyTwo(abi) => ("32", Some(abi)),
        Bits::None => ("none", None),
    };
    println!("bits {bits}");
    let engine = names.iter().any(|name| is_engine_library(name)) || abi.is_some_and(own_engine);
    println!("engine {}", yes_no(engine));
}

/// Libraries a game's own engine draws with and plays sound through.
const GRAPHICS_LIBRARIES: [&str; 4] = ["libGLESv1_CM.so", "libGLESv2.so", "libGLESv3.so", "libvulkan.so"];
const SOUND_LIBRARIES: [&str; 2] = ["libOpenSLES.so", "libaaudio.so"];

/// The size of the biggest library in lib/<abi>/ that looks like a game engine of the
/// app's own (PvZ 2's libPVZ2.so, for one): it asks for a graphics library and a sound
/// library itself. 0 if there is none. Most ordinary apps leave drawing to Android's views or a
/// web page and the sound to Android's players; those that do it themselves (video calls) do
/// it in a part smaller than their Java code, while a game's engine is most of the game.
fn own_engine<R: Read + Seek>(archive: &mut ZipArchive<R>, abi: &str) -> u64 {
    let names: Vec<String> = archive.file_names().filter(|name| library_in(abi, name).is_some()).map(String::from).collect();
    names.iter()
        .filter_map(|name| {
            let mut file = archive.by_name(name).ok()?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).ok()?;
            needed::needed(&bytes).filter(|needed| draws_and_plays(needed)).map(|_| bytes.len() as u64)
        })
        .max()
        .unwrap_or(0)
}

/// The size of the app's Java code (its classes*.dex files, unpacked).
fn java_size<R: Read + Seek>(archive: &mut ZipArchive<R>) -> u64 {
    let dex_files: Vec<String> = archive.file_names()
        .filter(|name| name.starts_with("classes") && name.ends_with(".dex") && !name.contains('/'))
        .map(String::from)
        .collect();
    dex_files.iter().filter_map(|name| archive.by_name(name).ok().map(|file| file.size())).sum()
}

fn draws_and_plays(needed: &[String]) -> bool {
    needed.iter().any(|name| GRAPHICS_LIBRARIES.contains(&name.as_str()))
        && needed.iter().any(|name| SOUND_LIBRARIES.contains(&name.as_str()))
}

fn yes_no(answer: bool) -> &'static str {
    if answer { "yes" } else { "no" }
}

/// Whether the game's own code starts Google Play's expansion-file download (dex::starts_expansion_download).
fn starts_download<R: Read + Seek>(archive: &mut ZipArchive<R>) -> bool {
    let dex_files: Vec<String> = archive.file_names()
        .filter(|name| name.starts_with("classes") && name.ends_with(".dex") && !name.contains('/'))
        .map(String::from)
        .collect();
    dex_files.iter().any(|name| {
        let mut bytes = Vec::new();
        archive.by_name(name).ok().and_then(|mut file| file.read_to_end(&mut bytes).ok()).is_some()
            && dex::starts_expansion_download(&bytes)
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_libraries() {
        assert!(is_engine_library("lib/arm64-v8a/libunity.so"));
        assert!(is_engine_library("lib/arm64-v8a/libcocos2dcpp.so"));
        assert!(is_engine_library("lib/armeabi-v7a/libUE4.so"));
        assert!(!is_engine_library("lib/arm64-v8a/libunityads.so"));
        assert!(!is_engine_library("assets/libunity.so"));
        assert!(!is_engine_library("lib/arm64-v8a/libc++_shared.so"));
    }

    #[test]
    fn own_engines() {
        let names = |list: &[&str]| list.iter().map(|name| name.to_string()).collect::<Vec<_>>();
        assert!(draws_and_plays(&names(&["libz.so", "libGLESv2.so", "libOpenSLES.so", "libc.so"])));
        assert!(draws_and_plays(&names(&["libvulkan.so", "libaaudio.so"])));
        assert!(!draws_and_plays(&names(&["libGLESv2.so", "libEGL.so", "libc.so"])));
        assert!(!draws_and_plays(&names(&["libOpenSLES.so", "libc.so"])));
    }

    #[test]
    fn next_dex_names() {
        assert_eq!(next_dex_name(["classes.dex", "res/a.xml"].into_iter()), "classes2.dex");
        assert_eq!(next_dex_name(["classes.dex", "classes2.dex", "classes10.dex"].into_iter()), "classes11.dex");
        assert_eq!(next_dex_name(["AndroidManifest.xml"].into_iter()), "classes.dex");
    }
}
