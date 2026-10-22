// Builds the C++ runtime that games linking Apple's GNU libstdc++ use
// (ios/cxxrt: Apple's libstdc++ 104.1 sources, GCC 4.2.1's libsupc++ and
// DWARF unwinder, GPL with the runtime exception, adapted to Apple's arm64
// layouts; see ios/cxxrt/README.md).
//
// Every object is joined into one, and all of its own names are made local,
// so nothing in it can meet the host's C++ library or unwinder (or anything
// else) at link or load time. The names left visible are the table
// `gamebridge_cxxrt_exports` (Apple name -> address, generated here from
// ios/cxxrt/exports.txt, which ios/tools/cxxrt-exports.sh makes) and
// `gamebridge_unwind_add_image`.
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=cxxrt");
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("aarch64") {
        return;
    }
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("cxxrt");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("cxxrt");
    std::fs::create_dir_all(&out).unwrap();

    let mut sources = Vec::new();
    for folder in ["src", "libsupc++", "unwind", "glue"] {
        for entry in std::fs::read_dir(root.join(folder)).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "cc" || e == "c") {
                sources.push(path);
            }
        }
    }
    sources.sort();
    let table = out.join("exports_table.c");
    std::fs::write(&table, export_table(&root.join("exports.txt"))).unwrap();

    let build = cc::Build::new();
    let cxx = build.clone().cpp(true).get_compiler();
    let cc = build.get_compiler();
    let include = |name: &str| format!("-I{}", root.join(name).display());
    let flags: Vec<String> = [
        "-c", "-O2", "-fPIC", "-std=gnu++98", "-nostdinc++", "-fsigned-char",
        "-fvisibility=hidden", "-fvisibility-global-new-delete=force-hidden",
        "-fexceptions", "-frtti", "-mno-outline-atomics", "-w",
        "-D_GAMEBRIDGE_CXXRT=1", "-include", "gamebridge_prelude.h",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain([include("adapt"), include("include"), include("libsupc++")])
    .collect();
    // The unwinder (C): GCC's DWARF unwinder and Game Bridge's glue.
    let c_flags: Vec<String> = ["-c", "-O2", "-fPIC", "-std=gnu99", "-fvisibility=hidden", "-mno-outline-atomics", "-w"]
        .iter()
        .map(|s| s.to_string())
        .chain([include("unwind"), include("libsupc++")])
        .collect();

    // Compile in parallel: about 50 files.
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let objects: Vec<PathBuf> = sources
        .iter()
        .map(|s| {
            let folder = s.parent().unwrap().file_name().unwrap().to_string_lossy().replace('+', "x");
            out.join(format!("{folder}-{}.o", s.file_stem().unwrap().to_string_lossy()))
        })
        .collect();
    let jobs: Vec<(PathBuf, PathBuf)> = sources.iter().cloned().zip(objects.iter().cloned()).collect();
    let queue = std::sync::Mutex::new(jobs.into_iter());
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| loop {
                let Some((source, object)) = queue.lock().unwrap().next() else { break };
                let c = source.extension().is_some_and(|e| e == "c");
                let mut command = if c { cc.to_command() } else { cxx.to_command() };
                command.args(if c { &c_flags } else { &flags }).arg(&source).arg("-o").arg(&object);
                run(command, &format!("compiling {}", source.display()));
            });
        }
    });
    let table_object = out.join("exports_table.o");
    let mut command = cc.to_command();
    command.args(["-c", "-O1", "-fPIC", "-fvisibility=hidden"]).arg(&table).arg("-o").arg(&table_object);
    run(command, "compiling the export table");

    // One object, then every name it defines made local except the table
    // (and the unwinder's image registration). All of the runtime's code
    // goes into one section between two marks, so the unwinder knows which
    // frames are its own.
    let script = out.join("join.ld");
    std::fs::write(
        &script,
        "SECTIONS {\n  .text.gamebridge_cxxrt : {\n    HIDDEN(gamebridge_cxxrt_text_start = .);\n    \
         *(.text .text.*)\n    HIDDEN(gamebridge_cxxrt_text_end = .);\n  }\n}\n",
    )
    .unwrap();
    // Joining first keeps one copy of each inline function (COMDAT group);
    // then the groups, now single, are dropped so the sections can be merged.
    let tools = llvm_tools(&build);
    let first = out.join("gamebridge_cxxrt-groups.o");
    let mut command = cxx.to_command();
    command.args(["-r", "-nostdlib", "-fuse-ld=lld", "-o"]).arg(&first).args(&objects).arg(&table_object);
    run(command, "joining the C++ runtime objects");
    let mut command = Command::new(tools.join("llvm-objcopy"));
    command.args(["--remove-section=.group"]).arg(&first);
    run(command, "dropping the joined object's COMDAT groups");
    let joined = out.join("gamebridge_cxxrt.o");
    let mut command = cxx.to_command();
    command.args(["-r", "-nostdlib", "-fuse-ld=lld"]).arg(format!("-Wl,-T,{}", script.display()));
    command.arg("-o").arg(&joined).arg(&first);
    run(command, "merging the C++ runtime's code");
    let mut command = Command::new(tools.join("llvm-objcopy"));
    command.arg("--localize-hidden").arg(&joined);
    run(command, "making the C++ runtime's names local");
    let archive = out.join("libgamebridge_cxxrt.a");
    let _ = std::fs::remove_file(&archive);
    let mut command = Command::new(tools.join("llvm-ar"));
    command.arg("rcs").arg(&archive).arg(&joined);
    run(command, "archiving the C++ runtime");
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=gamebridge_cxxrt");
}

fn run(mut command: Command, what: &str) {
    let status = command.status().unwrap_or_else(|error| panic!("{what}: could not start ({error})"));
    assert!(status.success(), "{what} failed: {command:?}");
}

/// The folder with llvm-objcopy and llvm-ar: next to the archiver cc uses,
/// or the NDK's (NDK, default ~/Android/Sdk/ndk/30.0.16248370).
fn llvm_tools(build: &cc::Build) -> PathBuf {
    let archiver = build.get_archiver();
    let program = PathBuf::from(archiver.get_program());
    if let Some(folder) = program.parent().filter(|f| f.join("llvm-objcopy").exists()) {
        return folder.to_path_buf();
    }
    let ndk = std::env::var("NDK").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(std::env::var("HOME").unwrap()).join("Android/Sdk/ndk/30.0.16248370")
    });
    ndk.join("toolchains/llvm/prebuilt/linux-x86_64/bin")
}

/// C source of the table: { Apple name without its leading "_", address, 1 if data }.
fn export_table(list: &Path) -> String {
    let text = std::fs::read_to_string(list).unwrap();
    let mut declarations = String::new();
    let mut rows = String::new();
    for (index, line) in text.lines().filter(|l| !l.starts_with('#') && !l.is_empty()).enumerate() {
        let (kind, name) = line.split_once(' ').unwrap();
        declarations.push_str(&format!("extern char s{index} __asm__(\"{name}\");\n"));
        rows.push_str(&format!("  {{\"{name}\", &s{index}, {}}},\n", u8::from(kind == "D")));
    }
    format!(
        "struct gamebridge_cxxrt_export {{ const char* name; void* address; int data; }};\n{declarations}\
         __attribute__((visibility(\"default\"))) const struct gamebridge_cxxrt_export gamebridge_cxxrt_exports[] = {{\n{rows}  {{0, 0, 0}}\n}};\n"
    )
}
