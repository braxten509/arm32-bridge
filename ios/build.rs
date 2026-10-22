//! Build steps for gamebridge-ios.

#[path = "build/method_types.rs"]
mod method_types;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    method_types::generate();
}
