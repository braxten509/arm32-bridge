// The library loader wrappers (dlopen, android_dlopen_ext, dlsym) are written in
// C: they pass their caller's address on to Android's loader, which picks the
// library namespace from it.
fn main() {
    println!("cargo:rerun-if-changed=src/loader.c");
    cc::Build::new().file("src/loader.c").flag("-fvisibility=hidden").compile("pacer_loader");
}
