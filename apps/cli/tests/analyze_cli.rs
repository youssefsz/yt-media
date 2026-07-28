//! Black-box contracts for `yt-media analyze`.

use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use tempfile::{TempDir, tempdir};
use yt_media_engine::{target::SupportedTarget, tool::Tool};

#[cfg(unix)]
use std::{
    thread,
    time::{Duration, Instant},
};

const EXPECTED_ANALYZE_V1: &[u8] = include_bytes!("fixtures/expected-analyze-v1.json");

fn cli_path() -> Result<PathBuf, Box<dyn Error>> {
    option_env!("CARGO_BIN_EXE_yt-media")
        .map(PathBuf::from)
        .ok_or_else(|| "yt-media binary path is unavailable".into())
}

fn fixture_path() -> Result<PathBuf, Box<dyn Error>> {
    option_env!("CARGO_BIN_EXE_yt-media-tool-fixture")
        .map(PathBuf::from)
        .ok_or_else(|| "tool fixture is unavailable; run tests with feature `test-fixture`".into())
}

fn fixture_tool_directory() -> Result<TempDir, Box<dyn Error>> {
    let directory = tempdir()?;
    let target = SupportedTarget::current()?;
    let fixture = fixture_path()?;
    for tool in Tool::ALL {
        let destination = directory.path().join(tool.executable_name(target));
        fs::copy(&fixture, &destination)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = fs::metadata(&destination)?.permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&destination, permissions)?;
        }
    }
    Ok(directory)
}

fn run_analyze(
    tools: &Path,
    scenario: &str,
    additional: &[&str],
) -> Result<Output, Box<dyn Error>> {
    let mut command = Command::new(cli_path()?);
    command
        .args(["analyze", "https://youtu.be/dQw4w9WgXcQ", "--tool-dir"])
        .arg(tools)
        .args(additional)
        .env("YT_MEDIA_TEST_SCENARIO", scenario);
    Ok(command.output()?)
}

#[test]
fn human_output_is_stdout_and_warnings_are_stderr() -> Result<(), Box<dyn Error>> {
    let tools = fixture_tool_directory()?;
    let success = run_analyze(tools.path(), "success", &[])?;
    assert_eq!(success.status.code(), Some(0));
    let stdout = String::from_utf8(success.stdout)?;
    let stderr = String::from_utf8(success.stderr)?;
    assert!(stdout.contains("Title: Sanitized progressive fixture"));
    assert!(stdout.contains("URL: https://www.youtube.com/watch?v=dQw4w9WgXcQ"));
    assert!(stdout.contains("MP3 320 kbps"));
    assert!(stdout.contains("MP4 720p"));
    assert!(stderr.is_empty());

    let warning = run_analyze(tools.path(), "warning", &[])?;
    assert_eq!(warning.status.code(), Some(0));
    assert!(String::from_utf8(warning.stdout)?.starts_with("Title:"));
    assert_eq!(
        String::from_utf8(warning.stderr)?,
        "warning: WARNING: sanitized fixture warning\n"
    );
    Ok(())
}

#[test]
fn json_mode_is_one_stable_undecorated_document() -> Result<(), Box<dyn Error>> {
    let tools = fixture_tool_directory()?;
    let output = run_analyze(tools.path(), "warning", &["--json"])?;
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let text = String::from_utf8(output.stdout)?;
    assert_eq!(text.lines().count(), 1);
    assert!(!text.contains("Title:"));
    assert!(!text.contains("\u{1b}["));
    let document: serde_json::Value = serde_json::from_str(&text)?;
    assert_eq!(document["schema_version"], 1);
    assert_eq!(
        document["media"]["url"],
        "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
    );
    assert_eq!(document["media"]["duration"], 212_400);
    assert_eq!(document["media"]["formats"][0]["kind"], "mp3");
    assert_eq!(document["media"]["formats"][0]["bitrate_kbps"], 128);
    assert_eq!(
        document["media"]["warnings"][0],
        "WARNING: sanitized fixture warning"
    );
    Ok(())
}

#[test]
fn json_schema_v1_matches_the_committed_compatibility_fixture() -> Result<(), Box<dyn Error>> {
    let tools = fixture_tool_directory()?;
    let output = run_analyze(tools.path(), "success", &["--json"])?;
    assert_eq!(output.status.code(), Some(0));
    let actual: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let expected: serde_json::Value = serde_json::from_slice(EXPECTED_ANALYZE_V1)?;
    assert_eq!(actual, expected);
    Ok(())
}

#[test]
fn human_and_json_modes_represent_the_same_engine_result() -> Result<(), Box<dyn Error>> {
    let tools = fixture_tool_directory()?;
    for scenario in [
        "success",
        "adaptive",
        "audio-only",
        "missing-size",
        "high-fps",
        "4k",
        "incompatible",
    ] {
        let human = run_analyze(tools.path(), scenario, &[])?;
        let json = run_analyze(tools.path(), scenario, &["--json"])?;
        assert_eq!(human.status.code(), Some(0), "scenario {scenario}");
        assert_eq!(json.status.code(), Some(0), "scenario {scenario}");
        let human = String::from_utf8(human.stdout)?;
        let document: serde_json::Value = serde_json::from_slice(&json.stdout)?;
        let title = document["media"]["title"]
            .as_str()
            .ok_or("JSON title was not a string")?;
        let url = document["media"]["url"]
            .as_str()
            .ok_or("JSON URL was not a string")?;
        assert!(human.contains(title), "scenario {scenario}");
        assert!(human.contains(url), "scenario {scenario}");
        for option in document["media"]["formats"]
            .as_array()
            .ok_or("JSON formats was not an array")?
        {
            match option["kind"].as_str() {
                Some("mp3") => {
                    let bitrate = option["bitrate_kbps"]
                        .as_u64()
                        .ok_or("MP3 bitrate was not an integer")?;
                    assert!(
                        human.contains(&format!("MP3 {bitrate} kbps")),
                        "scenario {scenario}"
                    );
                }
                Some("mp4") => {
                    let height = option["height"]
                        .as_u64()
                        .ok_or("MP4 height was not an integer")?;
                    assert!(
                        human.contains(&format!("MP4 {height}p")),
                        "scenario {scenario}"
                    );
                }
                _ => return Err("JSON contained an unknown format kind".into()),
            }
        }
    }
    Ok(())
}

#[test]
fn invalid_and_unsupported_urls_fail_before_tool_resolution() -> Result<(), Box<dyn Error>> {
    let missing_tools = tempdir()?;
    let invalid = Command::new(cli_path()?)
        .args([
            "analyze",
            "https://youtube.com.attacker.example/watch?v=dQw4w9WgXcQ",
            "--tool-dir",
        ])
        .arg(missing_tools.path())
        .output()?;
    assert_eq!(invalid.status.code(), Some(2));
    assert!(invalid.stdout.is_empty());
    assert!(String::from_utf8(invalid.stderr)?.contains("unsupported"));

    let cookie_option = Command::new(cli_path()?)
        .args([
            "analyze",
            "https://youtu.be/dQw4w9WgXcQ",
            "--cookies",
            "cookies.txt",
            "--tool-dir",
        ])
        .arg(missing_tools.path())
        .output()?;
    assert_eq!(cookie_option.status.code(), Some(2));
    assert!(cookie_option.stdout.is_empty());

    let unsupported = Command::new(cli_path()?)
        .args([
            "analyze",
            "https://youtube.com/live/dQw4w9WgXcQ",
            "--tool-dir",
        ])
        .arg(missing_tools.path())
        .output()?;
    assert_eq!(unsupported.status.code(), Some(3));
    assert!(unsupported.stdout.is_empty());
    assert!(String::from_utf8(unsupported.stderr)?.contains("live"));
    Ok(())
}

#[test]
fn tool_and_analysis_failures_have_stable_codes_and_streams() -> Result<(), Box<dyn Error>> {
    let missing_tools = tempdir()?;
    let unavailable = run_analyze(missing_tools.path(), "success", &["--json"])?;
    assert_eq!(unavailable.status.code(), Some(4));
    assert!(unavailable.stdout.is_empty());
    assert!(!unavailable.stderr.is_empty());

    let tools = fixture_tool_directory()?;
    let wrong_identity = Command::new(cli_path()?)
        .args(["analyze", "https://youtu.be/dQw4w9WgXcQ", "--tool-dir"])
        .arg(tools.path())
        .env("YT_MEDIA_TEST_BAD_TOOL", "deno")
        .output()?;
    assert_eq!(wrong_identity.status.code(), Some(4));
    assert!(wrong_identity.stdout.is_empty());
    assert!(!wrong_identity.stderr.is_empty());

    for scenario in [
        "nonzero",
        "invalid-utf8",
        "multiple-json",
        "malformed",
        "missing-metadata",
    ] {
        let failed = run_analyze(tools.path(), scenario, &["--json"])?;
        assert_eq!(failed.status.code(), Some(5), "scenario {scenario}");
        assert!(failed.stdout.is_empty(), "scenario {scenario}");
        assert!(!failed.stderr.is_empty(), "scenario {scenario}");
    }
    Ok(())
}

#[test]
fn extractor_private_and_live_results_are_unsupported() -> Result<(), Box<dyn Error>> {
    let tools = fixture_tool_directory()?;
    for scenario in ["private", "live"] {
        let output = run_analyze(tools.path(), scenario, &["--json"])?;
        assert_eq!(output.status.code(), Some(3), "scenario {scenario}");
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }
    Ok(())
}

#[test]
fn closed_stdout_maps_to_internal_failure() -> Result<(), Box<dyn Error>> {
    let tools = fixture_tool_directory()?;
    let mut child = Command::new(cli_path()?)
        .args([
            "analyze",
            "https://youtu.be/dQw4w9WgXcQ",
            "--json",
            "--tool-dir",
        ])
        .arg(tools.path())
        .env("YT_MEDIA_TEST_SCENARIO", "success")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    drop(child.stdout.take());
    let status = child.wait()?;
    assert_eq!(status.code(), Some(70));
    Ok(())
}

#[cfg(unix)]
#[test]
fn ctrl_c_cancels_and_reaps_the_analyzer_process_tree() -> Result<(), Box<dyn Error>> {
    let tools = fixture_tool_directory()?;
    let directory = tempdir()?;
    let heartbeat = directory.path().join("heartbeat");
    let mut child = Command::new(cli_path()?)
        .args([
            "analyze",
            "https://youtu.be/dQw4w9WgXcQ",
            "--json",
            "--tool-dir",
        ])
        .arg(tools.path())
        .env("YT_MEDIA_TEST_SCENARIO", "sleep")
        .env("YT_MEDIA_TEST_HEARTBEAT", &heartbeat)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    wait_for_heartbeat(&heartbeat)?;
    let signal = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()?;
    assert!(signal.success());
    let status = wait_for_exit(&mut child)?;
    assert_eq!(status.code(), Some(6));
    assert_heartbeat_stopped(&heartbeat)?;
    Ok(())
}

#[cfg(unix)]
fn wait_for_heartbeat(path: &Path) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if fs::metadata(path).is_ok_and(|metadata| metadata.len() >= 2) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("analysis fixture heartbeat did not start".into());
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(unix)]
fn wait_for_exit(
    child: &mut std::process::Child,
) -> Result<std::process::ExitStatus, Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            child.kill()?;
            return Err("CLI did not exit after Ctrl+C".into());
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(unix)]
fn assert_heartbeat_stopped(path: &Path) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut previous = fs::metadata(path)?.len();
    let mut stable = 0_u8;
    loop {
        thread::sleep(Duration::from_millis(100));
        let current = fs::metadata(path)?.len();
        if current == previous {
            stable = stable.saturating_add(1);
            if stable >= 3 {
                return Ok(());
            }
        } else {
            stable = 0;
            previous = current;
        }
        if Instant::now() >= deadline {
            return Err("analysis fixture survived CLI cancellation".into());
        }
    }
}
