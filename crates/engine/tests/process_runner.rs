//! Cross-platform process-runner contracts using a compiled fixture child.

use std::{
    error::Error,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use tempfile::tempdir;
use tokio::time::Instant;
use yt_media_engine::{
    cancellation::CancellationToken,
    process::{
        OutputLimit, OutputStream, ProcessError, ProcessRunner, ProcessSpec, TokioProcessRunner,
    },
};

fn fixture_path() -> Result<PathBuf, Box<dyn Error>> {
    option_env!("CARGO_BIN_EXE_yt-media-process-fixture")
        .map(PathBuf::from)
        .ok_or_else(|| {
            "process fixture is unavailable; run tests with feature `test-fixture`".into()
        })
}

#[tokio::test]
async fn preserves_argument_boundaries_exactly() -> Result<(), Box<dyn Error>> {
    let fixture = fixture_path()?;
    let arguments = [
        "plain",
        "contains spaces",
        "\"quoted\"",
        "semi;colon",
        "$(not-a-command)",
        "ampersand&value",
    ];
    let output = TokioProcessRunner
        .run(
            ProcessSpec::new(fixture)
                .argument("argv")
                .arguments(arguments),
            CancellationToken::new(),
        )
        .await?;
    assert!(output.status.success);
    let parsed: Vec<String> = serde_json::from_slice(&output.capture.stdout.bytes)?;
    assert_eq!(parsed, arguments);
    Ok(())
}

#[tokio::test]
async fn drains_and_orders_interleaved_stdout_and_stderr() -> Result<(), Box<dyn Error>> {
    let fixture = fixture_path()?;
    let output = TokioProcessRunner
        .run(
            ProcessSpec::new(fixture)
                .arguments(["interleave", "4", "15"])
                .timeout(Duration::from_secs(5)),
            CancellationToken::new(),
        )
        .await?;
    assert!(output.status.success);
    assert_eq!(
        String::from_utf8(output.capture.stdout.bytes)?,
        "out-0\nout-1\nout-2\nout-3\n"
    );
    assert_eq!(
        String::from_utf8(output.capture.stderr.bytes)?,
        "err-0\nerr-1\nerr-2\nerr-3\n"
    );
    assert!(
        output
            .capture
            .events
            .windows(2)
            .any(|window| window[0].stream != window[1].stream)
    );
    assert!(
        output
            .capture
            .events
            .iter()
            .any(|event| event.stream == OutputStream::Stdout)
    );
    assert!(
        output
            .capture
            .events
            .iter()
            .any(|event| event.stream == OutputStream::Stderr)
    );
    Ok(())
}

#[tokio::test]
async fn bounds_large_output_by_bytes_and_lines_without_deadlock() -> Result<(), Box<dyn Error>> {
    let fixture = fixture_path()?;
    let limit = OutputLimit::new(10_000, 2)?;
    let output = TokioProcessRunner
        .run(
            ProcessSpec::new(fixture)
                .arguments(["large-output", "64", "200"])
                .output_limit(limit)
                .timeout(Duration::from_secs(5)),
            CancellationToken::new(),
        )
        .await?;
    assert!(output.status.success);
    for stream in [&output.capture.stdout, &output.capture.stderr] {
        assert_eq!(stream.bytes.len(), 130);
        assert_eq!(stream.observed_bytes, 13_000);
        assert_eq!(stream.observed_lines, 200);
        assert!(stream.truncated);
    }
    Ok(())
}

#[tokio::test]
async fn exposes_non_zero_exit_status_and_bounded_output() -> Result<(), Box<dyn Error>> {
    let fixture = fixture_path()?;
    let output = TokioProcessRunner
        .run(
            ProcessSpec::new(fixture).arguments(["exit", "37"]),
            CancellationToken::new(),
        )
        .await?;
    assert!(!output.status.success);
    assert_eq!(output.status.code, Some(37));
    Ok(())
}

#[tokio::test]
async fn pipes_stdin_without_text_conversion() -> Result<(), Box<dyn Error>> {
    let fixture = fixture_path()?;
    let bytes = vec![0, 1, b'\n', 0xff, b'z'];
    let output = TokioProcessRunner
        .run(
            ProcessSpec::new(fixture)
                .argument("stdin")
                .stdin(bytes.clone()),
            CancellationToken::new(),
        )
        .await?;
    assert_eq!(output.capture.stdout.bytes, bytes);
    Ok(())
}

#[tokio::test]
async fn retains_invalid_utf8_as_exact_diagnostic_bytes() -> Result<(), Box<dyn Error>> {
    let fixture = fixture_path()?;
    let output = TokioProcessRunner
        .run(
            ProcessSpec::new(fixture).argument("invalid-utf8"),
            CancellationToken::new(),
        )
        .await?;
    assert_eq!(output.capture.stdout.bytes, [0xff, 0xfe, b'\n']);
    Ok(())
}

#[tokio::test]
async fn cancellation_terminates_and_reaps_the_process() -> Result<(), Box<dyn Error>> {
    let fixture = fixture_path()?;
    let cancellation = CancellationToken::new();
    let task_token = cancellation.clone();
    let task = tokio::spawn(async move {
        TokioProcessRunner
            .run(
                ProcessSpec::new(fixture).arguments(["sleep", "10000"]),
                task_token,
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    cancellation.cancel();
    let result = task.await?;
    assert!(matches!(result, Err(ProcessError::Cancelled { .. })));
    Ok(())
}

#[tokio::test]
async fn timeout_terminates_and_reaps_the_process() -> Result<(), Box<dyn Error>> {
    let fixture = fixture_path()?;
    let result = TokioProcessRunner
        .run(
            ProcessSpec::new(fixture)
                .arguments(["sleep", "10000"])
                .timeout(Duration::from_millis(100)),
            CancellationToken::new(),
        )
        .await;
    assert!(matches!(result, Err(ProcessError::TimedOut { .. })));
    Ok(())
}

#[tokio::test]
async fn cancellation_stops_the_complete_child_tree() -> Result<(), Box<dyn Error>> {
    let fixture = fixture_path()?;
    let directory = tempdir()?;
    let heartbeat = directory.path().join("heartbeat");
    let cancellation = CancellationToken::new();
    let task_token = cancellation.clone();
    let task_heartbeat = heartbeat.clone();
    let task = tokio::spawn(async move {
        TokioProcessRunner
            .run(
                ProcessSpec::new(fixture)
                    .argument("tree")
                    .argument(task_heartbeat.into_os_string())
                    .timeout(Duration::from_secs(20)),
                task_token,
            )
            .await
    });
    wait_for_heartbeat(&heartbeat).await?;
    cancellation.cancel();
    let result = task.await?;
    assert!(matches!(result, Err(ProcessError::Cancelled { .. })));
    assert_heartbeat_stopped(&heartbeat).await?;
    Ok(())
}

#[tokio::test]
async fn dropping_the_run_future_still_stops_the_complete_child_tree() -> Result<(), Box<dyn Error>>
{
    let fixture = fixture_path()?;
    let directory = tempdir()?;
    let heartbeat = directory.path().join("drop-heartbeat");
    let task_heartbeat = heartbeat.clone();
    let runner: Arc<dyn ProcessRunner> = Arc::new(TokioProcessRunner);
    let task = tokio::spawn(async move {
        runner
            .run(
                ProcessSpec::new(fixture)
                    .argument("tree")
                    .argument(task_heartbeat.into_os_string())
                    .timeout(Duration::from_secs(20)),
                CancellationToken::new(),
            )
            .await
    });
    wait_for_heartbeat(&heartbeat).await?;
    task.abort();
    let join_result = task.await;
    assert!(join_result.is_err());
    assert_heartbeat_stopped(&heartbeat).await?;
    Ok(())
}

async fn wait_for_heartbeat(path: &Path) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if tokio::fs::metadata(path)
            .await
            .is_ok_and(|metadata| metadata.len() >= 2)
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("heartbeat `{}` did not start", path.display()).into());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn assert_heartbeat_stopped(path: &Path) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut previous = tokio::fs::metadata(path).await?.len();
    let mut stable_samples = 0_u8;
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let current = tokio::fs::metadata(path).await?.len();
        if current == previous {
            stable_samples = stable_samples.saturating_add(1);
            if stable_samples >= 3 {
                return Ok(());
            }
        } else {
            stable_samples = 0;
            previous = current;
        }
        if Instant::now() >= deadline {
            return Err(format!("heartbeat `{}` continued after cleanup", path.display()).into());
        }
    }
}
