// The app build includes Java_* entry points generated for one game by
// tools/gen_exports.py; ARM32_EXPORTS_RS points at that file.
fn main() {
    println!("cargo:rerun-if-env-changed=ARM32_EXPORTS_RS");
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("exports.rs");
    match std::env::var("ARM32_EXPORTS_RS") {
        Ok(path) => {
            println!("cargo:rerun-if-changed={path}");
            std::fs::copy(&path, &out).expect("copy generated exports");
        }
        Err(_) => std::fs::write(&out, "// No game entry points in this build.\n").unwrap(),
    }
}
