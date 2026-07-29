//! Black-box durable queue, history, settings, and isolated data-directory contracts.

use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use tempfile::{TempDir, tempdir};
use yt_media_engine::{target::SupportedTarget, tool::Tool};

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

fn run(data: &Path, arguments: &[&str]) -> Result<Output, Box<dyn Error>> {
    Ok(Command::new(cli_path()?)
        .arg("--data-dir")
        .arg(data)
        .args(arguments)
        .output()?)
}

fn parse_json(output: &Output) -> Result<serde_json::Value, Box<dyn Error>> {
    if !output.status.success() {
        return Err(format!(
            "command failed with {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

#[test]
fn jobs_history_and_settings_persist_across_separate_invocations() -> Result<(), Box<dyn Error>> {
    let data = tempdir()?;
    let output = tempdir()?;
    let tools = fixture_tool_directory()?;
    let download = Command::new(cli_path()?)
        .arg("--data-dir")
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
        .args(["--name", "persistent", "--json"])
        .env("YT_MEDIA_TEST_SCENARIO", "success")
        .output()?;
    assert_eq!(
        download.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&download.stderr)
    );
    let final_document: serde_json::Value = serde_json::from_str(
        String::from_utf8(download.stdout)?
            .lines()
            .last()
            .ok_or("download emitted no final result")?,
    )?;
    let job_id = final_document["result"]["job_id"]
        .as_str()
        .ok_or("final result had no job ID")?;

    let list = parse_json(&run(data.path(), &["jobs", "list", "--json"])?)?;
    assert_eq!(list["schema_version"], 1);
    assert_eq!(list["jobs"].as_array().map(Vec::len), Some(1));
    assert_eq!(list["jobs"][0]["id"], job_id);
    assert_eq!(list["jobs"][0]["state"], "completed");
    assert_eq!(list["jobs"][0]["output_availability"], "present");

    let get = parse_json(&run(data.path(), &["jobs", "get", job_id, "--json"])?)?;
    assert_eq!(get["job"]["id"], job_id);
    assert_eq!(get["job"]["attempt_count"], 1);

    let history = parse_json(&run(data.path(), &["history", "list", "--json"])?)?;
    assert_eq!(history["jobs"][0]["id"], job_id);

    let destination = output.path().to_string_lossy().into_owned();
    let updated = parse_json(&run(
        data.path(),
        &[
            "settings",
            "set",
            "--default-destination",
            &destination,
            "--concurrency",
            "4",
            "--update-preference",
            "disabled",
            "--format",
            "mp4",
            "--quality",
            "720",
            "--json",
        ],
    )?)?;
    assert_eq!(updated["settings"]["queue_concurrency"], 4);
    assert_eq!(updated["settings"]["update_preference"], "disabled");
    assert_eq!(updated["settings"]["last_output"]["format"], "mp4");
    assert_eq!(updated["settings"]["last_output"]["quality"], 720);

    let shown = parse_json(&run(data.path(), &["settings", "show", "--json"])?)?;
    assert_eq!(shown["settings"], updated["settings"]);

    let removed = run(data.path(), &["history", "remove", job_id])?;
    assert!(removed.status.success());
    assert!(output.path().join("persistent.mp3").is_file());
    let empty = parse_json(&run(data.path(), &["jobs", "list", "--json"])?)?;
    assert_eq!(empty["jobs"].as_array().map(Vec::len), Some(0));
    Ok(())
}

#[test]
fn isolated_data_directories_do_not_share_history() -> Result<(), Box<dyn Error>> {
    let first = tempdir()?;
    let second = tempdir()?;
    let first_list = parse_json(&run(first.path(), &["jobs", "list", "--json"])?)?;
    let second_list = parse_json(&run(second.path(), &["jobs", "list", "--json"])?)?;
    assert_eq!(first_list["jobs"].as_array().map(Vec::len), Some(0));
    assert_eq!(second_list["jobs"].as_array().map(Vec::len), Some(0));
    assert_ne!(
        fs::canonicalize(first.path())?,
        fs::canonicalize(second.path())?
    );
    Ok(())
}
