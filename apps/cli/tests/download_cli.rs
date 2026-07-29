//! Black-box contracts for `yt-media download`.

use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use tempfile::{TempDir, tempdir};
use yt_media_engine::{target::SupportedTarget, tool::Tool};

#[cfg(unix)]
use std::{
    process::Stdio,
    thread,
    time::{Duration, Instant},
};

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

fn run_download(
    tools: &Path,
    output: &Path,
    format: &str,
    quality: &str,
    additional: &[&str],
) -> Result<Output, Box<dyn Error>> {
    let data = tempdir()?;
    let mut command = Command::new(cli_path()?);
    command
        .args(["--data-dir"])
        .arg(data.path())
        .args([
            "download",
            "https://youtu.be/dQw4w9WgXcQ",
            "--format",
            format,
            "--quality",
            quality,
            "--output",
        ])
        .arg(output)
        .args(["--tool-dir"])
        .arg(tools)
        .args(additional)
        .env("YT_MEDIA_TEST_SCENARIO", "success")
        .env("YT_MEDIA_TEST_OUTPUT_FORMAT", format);
    Ok(command.output()?)
}

#[test]
fn human_mode_streams_progress_to_stderr_and_only_final_path_to_stdout()
-> Result<(), Box<dyn Error>> {
    let tools = fixture_tool_directory()?;
    let output = tempdir()?;
    fs::write(output.path().join("résumé.mp3"), b"user")?;
    let result = run_download(
        tools.path(),
        output.path(),
        "mp3",
        "192",
        &["--name", "résumé?.mp3"],
    )?;
    assert_eq!(result.status.code(), Some(0));
    let stdout = String::from_utf8(result.stdout)?;
    let stderr = String::from_utf8(result.stderr)?;
    assert_eq!(stdout.lines().count(), 1);
    let final_path = PathBuf::from(stdout.trim());
    assert!(final_path.is_file());
    assert_eq!(
        final_path.file_name().and_then(|value| value.to_str()),
        Some("résumé (1).mp3")
    );
    assert_eq!(fs::read(output.path().join("résumé.mp3"))?, b"user");
    for stage in [
        "analyzing",
        "downloading",
        "converting",
        "finalizing",
        "completed",
    ] {
        assert!(stderr.contains(stage), "missing stage {stage}");
    }
    assert!(!stdout.contains("downloading"));
    Ok(())
}

#[test]
fn json_mode_is_versioned_ndjson_with_events_and_final_result() -> Result<(), Box<dyn Error>> {
    let tools = fixture_tool_directory()?;
    let output = tempdir()?;
    let result = run_download(
        tools.path(),
        output.path(),
        "mp4",
        "720",
        &["--json", "--name", "video"],
    )?;
    assert_eq!(result.status.code(), Some(0));
    assert!(result.stderr.is_empty());
    let text = String::from_utf8(result.stdout)?;
    let documents = text
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert!(documents.len() >= 4);
    assert!(
        documents
            .iter()
            .all(|document| document["schema_version"] == 1)
    );
    assert_eq!(documents[0]["event"], "stage");
    assert_eq!(documents[0]["stage"], "analyzing");
    let final_document = documents.last().ok_or("missing final NDJSON result")?;
    assert_eq!(final_document["event"], "result");
    assert_eq!(final_document["result"]["output"]["format"], "mp4");
    let path = final_document["result"]["path"]
        .as_str()
        .ok_or("final path was not a string")?;
    assert!(Path::new(path).is_file());
    Ok(())
}

#[test]
fn invalid_quality_and_destination_have_stable_exit_codes() -> Result<(), Box<dyn Error>> {
    let missing_tools = tempdir()?;
    let output = tempdir()?;
    let invalid = run_download(missing_tools.path(), output.path(), "mp3", "129", &[])?;
    assert_eq!(invalid.status.code(), Some(2));
    assert!(invalid.stdout.is_empty());

    let tools = fixture_tool_directory()?;
    let not_directory = output.path().join("file");
    fs::write(&not_directory, b"not a directory")?;
    let invalid_destination =
        run_download(tools.path(), &not_directory, "mp3", "192", &["--json"])?;
    assert_eq!(invalid_destination.status.code(), Some(8));
    let documents = String::from_utf8(invalid_destination.stdout)?;
    let final_document: serde_json::Value = serde_json::from_str(
        documents
            .lines()
            .last()
            .ok_or("missing final destination error")?,
    )?;
    assert_eq!(final_document["event"], "result");
    assert_eq!(final_document["error"]["code"], 8);
    assert!(!invalid_destination.stderr.is_empty());
    Ok(())
}

#[cfg(unix)]
#[test]
fn ctrl_c_cancels_download_and_reaps_the_process_tree() -> Result<(), Box<dyn Error>> {
    let tools = fixture_tool_directory()?;
    let output = tempdir()?;
    let data = tempdir()?;
    let heartbeat = output.path().join("heartbeat");
    let mut child = Command::new(cli_path()?)
        .args(["--data-dir"])
        .arg(data.path())
        .args([
            "download",
            "https://youtu.be/dQw4w9WgXcQ",
            "--format",
            "mp3",
            "--quality",
            "192",
            "--output",
        ])
        .arg(output.path())
        .args(["--tool-dir"])
        .arg(tools.path())
        .env("YT_MEDIA_TEST_SCENARIO", "download-sleep")
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
    let final_files = fs::read_dir(output.path())?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("mp3"))
        })
        .count();
    assert_eq!(final_files, 0);
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
            return Err("download fixture heartbeat did not start".into());
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
            return Err("download fixture survived CLI cancellation".into());
        }
    }
}
