//! Command-line entry point for YT Media.

#![forbid(unsafe_code)]

use clap::Parser;

/// Local command-line interface for YT Media.
#[derive(Debug, Parser)]
#[command(name = "yt-media", version, about)]
struct Cli;

fn main() {
    Cli::parse();
}
