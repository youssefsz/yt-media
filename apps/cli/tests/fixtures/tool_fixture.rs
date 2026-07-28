//! Multi-name external-tool fixture used by black-box CLI tests.

#![forbid(unsafe_code)]

use std::{
    env,
    error::Error,
    fs::{self, OpenOptions},
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
        if name.contains("ffprobe") {
            println!("ffprobe version 8.0.1");
        } else if name.contains("ffmpeg") {
            println!("ffmpeg version 8.0.1");
        } else {
            return Err(format!("unexpected -version fixture name `{name}`").into());
        }
        return Ok(());
    }
    if name.contains("ffprobe") {
        write_probe()?;
        return Ok(());
    }
    if name.contains("ffmpeg") {
        let target = arguments
            .last()
            .ok_or("FFmpeg fixture received no target path")?;
        fs::write(target, b"encoded CLI fixture")?;
        write_stdout(b"out_time_us=106200000\nout_time_us=212400000\nprogress=end")?;
        return Ok(());
    }
    if !name.contains("yt-dlp") {
        return Err(format!("unexpected fixture invocation name: `{name}`").into());
    }
    if !arguments
        .iter()
        .any(|argument| argument == "--dump-single-json")
    {
        if env::var("YT_MEDIA_TEST_SCENARIO").as_deref() == Ok("download-sleep") {
            sleep_with_heartbeat()?;
            return Ok(());
        }
        let output_index = arguments
            .iter()
            .position(|argument| argument == "--output")
            .and_then(|index| index.checked_add(1))
            .ok_or("yt-dlp fixture received no output path")?;
        let target = arguments
            .get(output_index)
            .ok_or("yt-dlp fixture output path was missing")?;
        fs::write(target, b"downloaded CLI fixture")?;
        eprintln!(
            "yt-media-progress|downloading|50|100|100|10|5\nyt-media-progress|finished|100|100|100|0|0"
        );
        return Ok(());
    }

    run_analysis_scenario()
}

fn run_analysis_scenario() -> Result<(), Box<dyn Error>> {
    match env::var("YT_MEDIA_TEST_SCENARIO")
        .unwrap_or_else(|_| "success".to_owned())
        .as_str()
    {
        "success" | "download-sleep" => write_stdout(PROGRESSIVE)?,
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

fn write_probe() -> io::Result<()> {
    if env::var("YT_MEDIA_TEST_OUTPUT_FORMAT").as_deref() == Ok("mp4") {
        write_stdout(
            br#"{"streams":[{"codec_type":"video","codec_name":"h264","width":1280,"height":720,"pix_fmt":"yuv420p"},{"codec_type":"audio","codec_name":"aac"}],"format":{"format_name":"mov,mp4","duration":"212.400"}}"#,
        )
    } else {
        write_stdout(
            br#"{"streams":[{"codec_type":"audio","codec_name":"mp3"}],"format":{"format_name":"mp3","duration":"212.400"}}"#,
        )
    }
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
