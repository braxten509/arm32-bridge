// Builds the 32-bit libraries the bridge carries for games (phone libraries
// that 64-bit-only phones no longer have), from the Android NDK's 32-bit ARM
// static libraries. NDK points at the NDK (default: the one in ~/Android/Sdk).
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=NDK");
    println!("cargo:rerun-if-changed=build.rs");
    let ndk = std::env::var("NDK").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(std::env::var("HOME").unwrap()).join("Android/Sdk/ndk/30.0.16248370")
    });
    let toolchain = ndk.join("toolchains/llvm/prebuilt/linux-x86_64");
    let clang = toolchain.join("bin/armv7a-linux-androideabi21-clang");
    let static_libraries = toolchain.join("sysroot/usr/lib/arm-linux-androideabi");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("guest");
    std::fs::create_dir_all(&out).unwrap();

    // libz.so: all of zlib, exported.
    let archive = static_libraries.join("libz.a");
    println!("cargo:rerun-if-changed={}", archive.display());
    let status = Command::new(&clang)
        .args(["-shared", "-fPIC", "-Wl,-soname,libz.so", "-Wl,--whole-archive"])
        .arg(&archive)
        .args(["-Wl,--no-whole-archive", "-o"])
        .arg(out.join("libz.so"))
        .status()
        .unwrap_or_else(|error| panic!("could not run the NDK's clang ({}): {error}", clang.display()));
    assert!(status.success(), "building the 32-bit libz.so failed");
}
