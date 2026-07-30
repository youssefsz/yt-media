//! Build script for the YT Media desktop shell.

fn main() {
    println!("cargo:rerun-if-changed=icon-source.svg");
    println!("cargo:rerun-if-changed=icons");
    tauri_build::build();
}
