//! Desktop executable entry point for YT Media.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![forbid(unsafe_code)]

fn main() {
    if let Err(error) = yt_media_desktop::run() {
        eprintln!("failed to run desktop application: {error}");
        std::process::exit(1);
    }
}
