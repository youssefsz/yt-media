//! Durable queue scheduling, persistence, recovery, and ownership integration tests.

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use rusqlite::Connection;
use tempfile::tempdir;
use yt_media_engine::jobs::{QueueConcurrency, SettingsPatch};
use yt_media_engine::{
    analysis::MediaUrl,
    cancellation::CancellationToken,
    download::{
        AudioQuality, Destination, DownloadRequest, DownloadService, DownloadTools, OutputName,
        OutputSelection,
    },
    jobs::{JobQueue, JobState, OutputAvailability},
    path::ExecutablePath,
    process::{
        CapturedOutput, OutputStream, ProcessError, ProcessEvent, ProcessExitStatus, ProcessOutput,
        ProcessRunner, ProcessSpec, StreamCapture,
    },
};

const ANALYSIS: &str = include_str!("fixtures/analysis/progressive.json");
const AUDIO_ONLY: &str = include_str!("fixtures/analysis/audio-only.json");
const PROBE: &str = r#"{"streams":[{"codec_type":"audio","codec_name":"mp3"}],"format":{"format_name":"mp3","duration":"212.400"}}"#;

#[derive(Default)]
struct QueueFixtureRunner {
    block_downloads: AtomicBool,
    active_downloads: AtomicUsize,
    maximum_active: AtomicUsize,
    started_targets: Mutex<Vec<PathBuf>>,
    analysis_override: Mutex<Option<&'static str>>,
    fail_download: AtomicBool,
    hold_until_released: AtomicBool,
    release_downloads: tokio::sync::Notify,
}

impl QueueFixtureRunner {
    fn blocking() -> Self {
        Self {
            block_downloads: AtomicBool::new(true),
            ..Self::default()
        }
    }

    fn analysis(&self) -> std::io::Result<&'static str> {
        self.analysis_override
            .lock()
            .map_err(|_| std::io::Error::other("fixture lock poisoned"))
            .map(|analysis| analysis.unwrap_or(ANALYSIS))
    }

    async fn run_download(
        &self,
        target: PathBuf,
        cancellation: CancellationToken,
    ) -> Result<ProcessOutput, ProcessError> {
        self.started_targets
            .lock()
            .map_err(|_| ProcessError::Write(std::io::Error::other("fixture lock poisoned")))?
            .push(target.clone());
        let active = self.active_downloads.fetch_add(1, Ordering::AcqRel) + 1;
        let _active_guard = ActiveDownloadGuard(&self.active_downloads);
        self.maximum_active.fetch_max(active, Ordering::AcqRel);
        if self.fail_download.load(Ordering::Acquire) {
            return Ok(failed_download());
        }
        while self.hold_until_released.load(Ordering::Acquire) {
            tokio::select! {
                () = cancellation.cancelled() => {
                    return Err(cancelled_process());
                }
                () = self.release_downloads.notified() => {}
            }
        }
        if self.block_downloads.load(Ordering::Acquire) {
            let partial = target.with_file_name(format!(
                "{}.part",
                target
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or_default()
            ));
            fs::write(partial, b"resumable queue fixture").map_err(ProcessError::Write)?;
            cancellation.cancelled().await;
            return Err(cancelled_process());
        }
        tokio::select! {
            () = cancellation.cancelled() => {
                return Err(cancelled_process());
            }
            () = tokio::time::sleep(Duration::from_millis(40)) => {}
        }
        fs::write(target, b"downloaded queue fixture").map_err(ProcessError::Write)?;
        Ok(success_stderr(
            b"yt-media-progress|downloading|50|100|100|10|5\nyt-media-progress|finished|100|100|100|0|0\n",
        ))
    }
}

struct ActiveDownloadGuard<'a>(&'a AtomicUsize);

impl Drop for ActiveDownloadGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[async_trait]
impl ProcessRunner for QueueFixtureRunner {
    async fn run(
        &self,
        spec: ProcessSpec,
        cancellation: CancellationToken,
    ) -> Result<ProcessOutput, ProcessError> {
        let executable = spec
            .executable()
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_owned();
        let arguments = spec
            .argument_values()
            .map(OsString::from)
            .collect::<Vec<_>>();
        let rendered = arguments
            .iter()
            .map(|value| value.to_string_lossy())
            .collect::<Vec<_>>();
        if rendered
            .iter()
            .any(|argument| argument == "--dump-single-json")
        {
            return Ok(success_stdout(
                self.analysis().map_err(ProcessError::Write)?.as_bytes(),
            ));
        }
        if executable.contains("ffprobe") {
            return Ok(success_stdout(PROBE.as_bytes()));
        }
        if executable.contains("ffmpeg") {
            let Some(target) = arguments.last() else {
                return Err(ProcessError::Write(std::io::Error::other(
                    "fixture FFmpeg target missing",
                )));
            };
            fs::write(target, b"encoded queue fixture").map_err(ProcessError::Write)?;
            return Ok(success_stdout(
                b"out_time_us=106200000\nout_time_us=212400000\nprogress=end\n",
            ));
        }
        let output_index = rendered
            .iter()
            .position(|argument| argument == "--output")
            .and_then(|index| index.checked_add(1))
            .ok_or_else(|| {
                ProcessError::Write(std::io::Error::other("fixture output argument missing"))
            })?;
        let Some(target) = arguments.get(output_index).map(PathBuf::from) else {
            return Err(ProcessError::Write(std::io::Error::other(
                "fixture output value missing",
            )));
        };
        self.run_download(target, cancellation).await
    }
}

fn failed_download() -> ProcessOutput {
    ProcessOutput {
        status: ProcessExitStatus {
            success: false,
            code: Some(9),
        },
        capture: CapturedOutput {
            stdout: StreamCapture::default(),
            stderr: capture(b"fixture download failure\n"),
            events: Vec::new(),
        },
    }
}

fn cancelled_process() -> ProcessError {
    ProcessError::Cancelled {
        output: CapturedOutput::default(),
    }
}

fn success_stdout(bytes: &[u8]) -> ProcessOutput {
    success_capture(bytes, &[], OutputStream::Stdout)
}

fn success_stderr(bytes: &[u8]) -> ProcessOutput {
    success_capture(&[], bytes, OutputStream::Stderr)
}

fn success_capture(stdout: &[u8], stderr: &[u8], stream: OutputStream) -> ProcessOutput {
    let bytes = match stream {
        OutputStream::Stdout => stdout,
        OutputStream::Stderr => stderr,
    };
    ProcessOutput {
        status: ProcessExitStatus {
            success: true,
            code: Some(0),
        },
        capture: CapturedOutput {
            stdout: capture(stdout),
            stderr: capture(stderr),
            events: (!bytes.is_empty())
                .then(|| ProcessEvent {
                    sequence: 0,
                    stream,
                    bytes: bytes.to_vec(),
                })
                .into_iter()
                .collect(),
        },
    }
}

fn capture(bytes: &[u8]) -> StreamCapture {
    StreamCapture {
        bytes: bytes.to_vec(),
        observed_bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        observed_lines: u64::try_from(
            bytes
                .split(|byte| *byte == b'\n')
                .count()
                .saturating_sub(usize::from(bytes.last() == Some(&b'\n'))),
        )
        .unwrap_or(u64::MAX),
        truncated: false,
    }
}

fn make_executable(path: &Path) -> Result<ExecutablePath, Box<dyn std::error::Error>> {
    fs::write(path, b"fixture")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
    }
    Ok(ExecutablePath::validate(path)?)
}

fn make_service(
    directory: &Path,
    runner: Arc<dyn ProcessRunner>,
) -> Result<DownloadService, Box<dyn std::error::Error>> {
    let tools = DownloadTools::new(
        make_executable(&directory.join("yt-dlp"))?,
        make_executable(&directory.join("ffmpeg"))?,
        make_executable(&directory.join("ffprobe"))?,
        make_executable(&directory.join("deno"))?,
    );
    Ok(DownloadService::with_runner(tools, runner))
}

fn request(destination: &Path, name: &str) -> Result<DownloadRequest, Box<dyn std::error::Error>> {
    Ok(DownloadRequest {
        url: MediaUrl::parse("https://youtu.be/dQw4w9WgXcQ")?,
        output: OutputSelection::Mp3(AudioQuality::try_from(192)?),
        destination: Destination::new(destination)?,
        name: Some(OutputName::new(name)?),
    })
}

fn mp4_request(
    destination: &Path,
    name: &str,
) -> Result<DownloadRequest, Box<dyn std::error::Error>> {
    Ok(DownloadRequest {
        url: MediaUrl::parse("https://youtu.be/dQw4w9WgXcQ")?,
        output: OutputSelection::Mp4(yt_media_engine::download::VideoQuality::try_from(720)?),
        destination: Destination::new(destination)?,
        name: Some(OutputName::new(name)?),
    })
}

async fn wait_for_active_count(
    queue: &JobQueue,
    expected: usize,
) -> Result<Vec<yt_media_engine::jobs::JobRecord>, Box<dyn std::error::Error>> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let jobs = queue.list().await?;
            let active = jobs
                .iter()
                .filter(|job| job.state.is_process_active())
                .count();
            if active == expected {
                return Ok::<_, yt_media_engine::jobs::QueueError>(jobs);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?
    .map_err(Into::into)
}

async fn wait_for_fixture_downloads(
    runner: &QueueFixtureRunner,
    expected: usize,
) -> Result<(), tokio::time::error::Elapsed> {
    tokio::time::timeout(Duration::from_secs(5), async {
        while runner.active_downloads.load(Ordering::Acquire) != expected {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
}

#[tokio::test]
async fn fifo_concurrency_queued_cancel_and_crash_style_recovery_are_durable()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;
    let data = root.path().join("data");
    let output = root.path().join("output");
    let tools = root.path().join("tools");
    fs::create_dir_all(&output)?;
    fs::create_dir_all(&tools)?;
    let runner = Arc::new(QueueFixtureRunner::blocking());
    let service = make_service(&tools, runner.clone())?;
    let queue = JobQueue::open_with_download_service(&data, service).await?;
    let first = queue.enqueue(request(&output, "first")?).await?;
    let second = queue.enqueue(request(&output, "second")?).await?;
    let third = queue.enqueue(request(&output, "third")?).await?;

    let _jobs = wait_for_active_count(&queue, 2).await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        while runner.maximum_active.load(Ordering::Acquire) < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    let jobs = queue.list().await?;
    assert!(jobs[0].state.is_process_active());
    assert!(jobs[1].state.is_process_active());
    assert_eq!(jobs[2].state, JobState::Queued);
    assert_eq!(jobs[0].id, first.id);
    assert_eq!(jobs[1].id, second.id);
    assert_eq!(jobs[2].id, third.id);
    assert_eq!(runner.maximum_active.load(Ordering::Acquire), 2);

    let cancelled = queue.cancel(&third.id).await?;
    assert_eq!(cancelled.state, JobState::Cancelled);
    queue.shutdown(Duration::from_secs(5)).await?;
    let first_after_shutdown = queue.get(&first.id).await?;
    let second_after_shutdown = queue.get(&second.id).await?;
    assert_eq!(
        first_after_shutdown.state,
        JobState::Interrupted,
        "{:?}",
        first_after_shutdown.error
    );
    assert_eq!(
        second_after_shutdown.state,
        JobState::Interrupted,
        "{:?}",
        second_after_shutdown.error
    );
    let database = queue.database_path().to_path_buf();
    drop(queue);
    tokio::task::yield_now().await;

    let connection = Connection::open(&database)?;
    connection.execute(
        "UPDATE jobs SET state = 'downloading' WHERE id = ?1",
        [first.id.as_str()],
    )?;
    drop(connection);
    let reopened = JobQueue::open(&data).await?;
    assert_eq!(reopened.get(&first.id).await?.state, JobState::Interrupted);
    assert_eq!(runner.active_downloads.load(Ordering::Acquire), 0);
    Ok(())
}

#[tokio::test]
async fn pause_resume_preserves_identity_and_partials_then_history_reports_missing_output()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;
    let data = root.path().join("data");
    let output = root.path().join("output");
    let tools = root.path().join("tools");
    fs::create_dir_all(&output)?;
    fs::create_dir_all(&tools)?;
    let unrelated = output.join("user-owned.part");
    fs::write(&unrelated, b"user data")?;
    let runner = Arc::new(QueueFixtureRunner::blocking());
    let service = make_service(&tools, runner.clone())?;
    let queue = JobQueue::open_with_download_service(&data, service).await?;
    let job = queue.enqueue(request(&output, "resume")?).await?;
    let _jobs = wait_for_active_count(&queue, 1).await?;
    wait_for_fixture_downloads(&runner, 1).await?;
    let paused = queue.pause(&job.id).await?;
    assert_eq!(paused.state, JobState::Paused);
    assert!(paused.owned_partial_paths.len() >= 2);
    assert_eq!(fs::read(&unrelated)?, b"user data");

    runner.block_downloads.store(false, Ordering::Release);
    let resumed = queue.resume(&job.id).await?;
    assert_eq!(resumed.id, job.id);
    assert_eq!(resumed.attempt_count, 2);
    let completed = queue.wait_until_stopped(&job.id).await?;
    assert_eq!(completed.state, JobState::Completed);
    assert_eq!(completed.output_availability, OutputAvailability::Present);
    assert!(completed.owned_partial_paths.is_empty());
    assert_eq!(fs::read(&unrelated)?, b"user data");

    let final_path = completed
        .final_output
        .as_ref()
        .map(|output| output.path.clone())
        .ok_or("completed job had no final output")?;
    fs::remove_file(final_path)?;
    assert_eq!(
        queue.get(&job.id).await?.output_availability,
        OutputAvailability::Missing
    );
    queue.remove_completed(&job.id).await?;
    assert!(queue.get(&job.id).await.is_err());
    Ok(())
}

#[tokio::test]
async fn retry_reanalyzes_and_classifies_a_disappeared_format()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;
    let data = root.path().join("data");
    let output = root.path().join("output");
    let tools = root.path().join("tools");
    fs::create_dir_all(&output)?;
    fs::create_dir_all(&tools)?;
    let runner = Arc::new(QueueFixtureRunner::default());
    runner.fail_download.store(true, Ordering::Release);
    let service = make_service(&tools, runner.clone())?;
    let queue = JobQueue::open_with_download_service(&data, service).await?;
    let job = queue.enqueue(mp4_request(&output, "retry")?).await?;
    let first_failure = queue.wait_until_stopped(&job.id).await?;
    assert_eq!(first_failure.state, JobState::Failed);
    assert_eq!(first_failure.attempt_count, 1);

    runner.fail_download.store(false, Ordering::Release);
    *runner
        .analysis_override
        .lock()
        .map_err(|_| std::io::Error::other("fixture lock poisoned"))? = Some(AUDIO_ONLY);
    let retried = queue.retry(&job.id).await?;
    assert_eq!(retried.attempt_count, 2);
    let second_failure = queue.wait_until_stopped(&job.id).await?;
    assert_eq!(second_failure.state, JobState::Failed);
    assert_eq!(
        second_failure.error.as_ref().map(|error| error.class),
        Some(yt_media_engine::jobs::JobErrorClass::FormatUnavailable)
    );
    assert!(
        second_failure
            .error
            .as_ref()
            .is_some_and(|error| error.message.contains("unavailable"))
    );
    Ok(())
}

#[tokio::test]
async fn startup_reports_an_unavailable_destination_without_deleting_retained_data()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;
    let data = root.path().join("data");
    let output = root.path().join("output");
    let detached = root.path().join("detached-output");
    let tools = root.path().join("tools");
    fs::create_dir_all(&output)?;
    fs::create_dir_all(&tools)?;
    let runner = Arc::new(QueueFixtureRunner::blocking());
    let service = make_service(&tools, runner.clone())?;
    let queue = JobQueue::open_with_download_service(&data, service).await?;
    let job = queue.enqueue(request(&output, "detached")?).await?;
    let _jobs = wait_for_active_count(&queue, 1).await?;
    wait_for_fixture_downloads(&runner, 1).await?;
    let paused = queue.pause(&job.id).await?;
    assert!(!paused.owned_partial_paths.is_empty());
    queue.shutdown(Duration::from_secs(5)).await?;
    drop(queue);
    tokio::task::yield_now().await;
    fs::rename(&output, &detached)?;

    let reopened = JobQueue::open(&data).await?;
    let recovered = reopened.get(&job.id).await?;
    assert_eq!(recovered.state, JobState::Paused);
    assert!(!recovered.destination_available);
    assert_eq!(
        recovered.error.as_ref().map(|error| error.class),
        Some(yt_media_engine::jobs::JobErrorClass::DestinationUnavailable)
    );
    assert!(detached.read_dir()?.next().is_some());
    Ok(())
}

#[tokio::test]
async fn explicit_retry_is_appended_behind_existing_fifo_work()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;
    let data = root.path().join("data");
    let output = root.path().join("output");
    let tools = root.path().join("tools");
    fs::create_dir_all(&output)?;
    fs::create_dir_all(&tools)?;
    let runner = Arc::new(QueueFixtureRunner::default());
    runner.fail_download.store(true, Ordering::Release);
    let service = make_service(&tools, runner.clone())?;
    let queue = JobQueue::open_with_download_service(&data, service).await?;
    queue
        .update_settings(SettingsPatch {
            queue_concurrency: Some(QueueConcurrency::try_from(1)?),
            ..SettingsPatch::default()
        })
        .await?;
    let failed = queue.enqueue(request(&output, "failed")?).await?;
    assert_eq!(
        queue.wait_until_stopped(&failed.id).await?.state,
        JobState::Failed
    );

    runner.fail_download.store(false, Ordering::Release);
    runner.block_downloads.store(true, Ordering::Release);
    let blocking = queue.enqueue(request(&output, "blocking")?).await?;
    wait_for_fixture_downloads(&runner, 1).await?;
    let ahead = queue.enqueue(request(&output, "ahead")?).await?;
    queue.retry(&failed.id).await?;
    assert_eq!(queue.get(&ahead.id).await?.state, JobState::Queued);
    assert_eq!(queue.get(&failed.id).await?.state, JobState::Queued);

    queue.cancel(&blocking.id).await?;
    let _jobs = wait_for_active_count(&queue, 1).await?;
    assert!(queue.get(&ahead.id).await?.state.is_process_active());
    assert_eq!(queue.get(&failed.id).await?.state, JobState::Queued);
    queue.shutdown(Duration::from_secs(5)).await?;
    Ok(())
}

#[tokio::test]
async fn two_fixture_downloads_complete_concurrently_under_the_shared_limit()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;
    let data = root.path().join("data");
    let output = root.path().join("output");
    let tools = root.path().join("tools");
    fs::create_dir_all(&output)?;
    fs::create_dir_all(&tools)?;
    let runner = Arc::new(QueueFixtureRunner::default());
    runner.hold_until_released.store(true, Ordering::Release);
    let service = make_service(&tools, runner.clone())?;
    let queue = JobQueue::open_with_download_service(&data, service).await?;
    let first = queue.enqueue(request(&output, "concurrent-one")?).await?;
    let second = queue.enqueue(request(&output, "concurrent-two")?).await?;
    wait_for_fixture_downloads(&runner, 2).await?;
    assert_eq!(runner.maximum_active.load(Ordering::Acquire), 2);
    runner.hold_until_released.store(false, Ordering::Release);
    runner.release_downloads.notify_waiters();
    let first_result = queue.wait_until_stopped(&first.id);
    let second_result = queue.wait_until_stopped(&second.id);
    let (first_result, second_result) = tokio::join!(first_result, second_result);
    assert_eq!(first_result?.state, JobState::Completed);
    assert_eq!(second_result?.state, JobState::Completed);
    assert_eq!(runner.active_downloads.load(Ordering::Acquire), 0);
    Ok(())
}
