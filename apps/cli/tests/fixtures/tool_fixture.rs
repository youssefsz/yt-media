//! Multi-name external-tool fixture used by black-box CLI tests.

#![forbid(unsafe_code)]

use std::{
    env,
    error::Error,
    fs::OpenOptions,
    io::{self, Write},
    thread,
    time::Duration,
};

const PROGRESSIVE: &[u8] =
    include_bytes!("../../../../crates/engine/tests/fixtures/analysis/progressive.json");
const ADAPTIVE: &[u8] =
    include_bytes!("../../../../crates/engine/tests/fixtures/analysis/adaptive.json");
const AUDIO_ONLY: &[u8] =
    include_bytes!("../../../../crates/engine/tests/fixtures/analysis/audio-only.json");
const MISSING_SIZE: &[u8] =
    include_bytes!("../../../../crates/engine/tests/fixtures/analysis/missing-size.json");
const HIGH_FPS: &[u8] =
    include_bytes!("../../../../crates/engine/tests/fixtures/analysis/high-fps.json");
const FOUR_K: &[u8] = include_bytes!("../../../../crates/engine/tests/fixtures/analysis/4k.json");
const INCOMPATIBLE: &[u8] =
    include_bytes!("../../../../crates/engine/tests/fixtures/analysis/incompatible.json");
const MISSING_METADATA: &[u8] =
    include_bytes!("../../../../crates/engine/tests/fixtures/analysis/missing-metadata.json");
const PRIVATE: &[u8] =
    include_bytes!("../../../../crates/engine/tests/fixtures/analysis/private.json");
const LIVE: &[u8] = include_bytes!("../../../../crates/engine/tests/fixtures/analysis/live.json");
const MALFORMED: &[u8] =
    include_bytes!("../../../../crates/engine/tests/fixtures/analysis/malformed.txt");

fn main() {
    if let Err(error) = run() {
        eprintln!("tool fixture failed: {error}");
        std::process::exit(125);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let executable = env::current_exe()?;
    let name = executable
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or("fixture executable has no Unicode filename")?
        .to_ascii_lowercase();
    let arguments = env::args_os().skip(1).collect::<Vec<_>>();
    let first = arguments
        .first()
        .and_then(|argument| argument.to_str())
        .unwrap_or_default();
    let bad_tool = env::var("YT_MEDIA_TEST_BAD_TOOL").ok();
    if bad_tool.as_deref().is_some_and(|tool| name.contains(tool)) {
        println!("unexpected fixture identity 0.0.0");
        return Ok(());
    }
    if first == "--version" {
        if name.contains("yt-dlp") {
            println!("2026.06.09");
        } else if name.contains("deno") {
            println!("deno 2.8.1");
        } else {
            return Err(format!("unexpected --version fixture name `{name}`").into());
        }
        return Ok(());
    }
    if first == "-version" {
        if !name.contains("ffmpeg") {
            return Err(format!("unexpected -version fixture name `{name}`").into());
        }
        println!("ffmpeg version 8.0.1");
        return Ok(());
    }
    if !name.contains("yt-dlp") {
        return Err(format!("only the yt-dlp fixture accepts analysis arguments: `{name}`").into());
    }

    match env::var("YT_MEDIA_TEST_SCENARIO")
        .unwrap_or_else(|_| "success".to_owned())
        .as_str()
    {
        "success" => write_stdout(PROGRESSIVE)?,
        "adaptive" => write_stdout(ADAPTIVE)?,
        "audio-only" => write_stdout(AUDIO_ONLY)?,
        "missing-size" => write_stdout(MISSING_SIZE)?,
        "high-fps" => write_stdout(HIGH_FPS)?,
        "4k" => write_stdout(FOUR_K)?,
        "incompatible" => write_stdout(INCOMPATIBLE)?,
        "missing-metadata" => write_stdout(MISSING_METADATA)?,
        "warning" => {
            eprintln!("WARNING: sanitized fixture warning");
            write_stdout(PROGRESSIVE)?;
        }
        "private" => write_stdout(PRIVATE)?,
        "live" => write_stdout(LIVE)?,
        "nonzero" => {
            eprintln!("sanitized extraction failure");
            std::process::exit(9);
        }
        "invalid-utf8" => io::stdout().write_all(&[0xff, 0xfe, b'\n'])?,
        "multiple-json" => {
            write_stdout(PROGRESSIVE)?;
            write_stdout(PROGRESSIVE)?;
        }
        "malformed" => write_stdout(MALFORMED)?,
        "sleep" => sleep_with_heartbeat()?,
        scenario => return Err(format!("unknown test scenario `{scenario}`").into()),
    }
    Ok(())
}

fn write_stdout(bytes: &[u8]) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(bytes)?;
    stdout.write_all(b"\n")?;
    stdout.flush()
}

fn sleep_with_heartbeat() -> Result<(), Box<dyn Error>> {
    let heartbeat = env::var_os("YT_MEDIA_TEST_HEARTBEAT").ok_or("missing heartbeat path")?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(heartbeat)?;
    loop {
        file.write_all(b".")?;
        file.flush()?;
        thread::sleep(Duration::from_millis(25));
    }
}
