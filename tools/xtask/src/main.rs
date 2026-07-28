//! Command-line entry point for repository maintenance.

#![forbid(unsafe_code)]

fn main() {
    if let Err(error) = xtask::sidecars::run_cli() {
        eprintln!("xtask failed: {error}");
        std::process::exit(1);
    }
}
