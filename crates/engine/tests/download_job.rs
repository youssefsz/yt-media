//! Deterministic download orchestration and cleanup contract tests.

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use tempfile::tempdir;
use yt_media_engine::{
    analysis::MediaUrl,
    cancellation::CancellationToken,
    download::{
        AudioQuality, Destination, DownloadError, DownloadRequest, DownloadService, DownloadTools,
        JobEventKind, JobStage, OutputName, OutputSelection, VideoQuality,
    },
    path::ExecutablePath,
    process::{
        CapturedOutput, OutputStream, ProcessError, ProcessEvent, ProcessExitStatus, ProcessOutput,
        ProcessRunner, ProcessSpec, StreamCapture,
    },
};

const ANALYSIS: &str = include_str!("fixtures/analysis/progressive.json");
const ADAPTIVE_ANALYSIS: &str = include_str!("fixtures/analysis/adaptive.json");
const PROBE: &str = r#"{"streams":[{"codec_type":"audio","codec_name":"mp3"}],"format":{"format_name":"mp3","duration":"212.400"}}"#;
const MP4_PROBE: &str = r#"{"streams":[{"codec_type":"video","codec_name":"h264","width":1920,"height":1080,"pix_fmt":"yuv420p"},{"codec_type":"audio","codec_name":"aac"}],"format":{"format_name":"mov,mp4","duration":"90.0"}}"#;

struct FixtureRunner {
    calls: Mutex<Vec<(PathBuf, Vec<OsString>)>>,
    analysis: &'static str,
}

impl Default for FixtureRunner {
    fn default() -> Self {
        Self {
            calls: Mutex::default(),
            analysis: ANALYSIS,
        }
    }
}

impl FixtureRunner {
    fn adaptive() -> Self {
        Self {
            calls: Mutex::default(),
            analysis: ADAPTIVE_ANALYSIS,
        }
    }
}

struct PausingRunner;

#[async_trait]
impl ProcessRunner for PausingRunner {
    async fn run(
        &self,
        spec: ProcessSpec,
        cancellation: CancellationToken,
    ) -> Result<ProcessOutput, ProcessError> {
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
            return Ok(success_stdout(ANALYSIS.as_bytes()));
        }
        let output_index = rendered
            .iter()
            .position(|argument| argument == "--output")
            .and_then(|index| index.checked_add(1))
            .ok_or_else(|| {
                ProcessError::Write(std::io::Error::other("fixture yt-dlp output missing"))
            })?;
        let Some(target) = arguments.get(output_index) else {
            return Err(ProcessError::Write(std::io::Error::other(
                "fixture yt-dlp output value missing",
            )));
        };
        let mut partial = PathBuf::from(target);
        let partial_name = format!(
            "{}.part",
            partial
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
        );
        partial.set_file_name(partial_name);
        fs::write(partial, b"resumable partial").map_err(ProcessError::Write)?;
        cancellation.cancelled().await;
        Err(ProcessError::Cancelled {
            output: CapturedOutput::default(),
        })
    }
}

#[async_trait]
impl ProcessRunner for FixtureRunner {
    async fn run(
        &self,
        spec: ProcessSpec,
        _cancellation: CancellationToken,
    ) -> Result<ProcessOutput, ProcessError> {
        let executable = spec.executable().to_path_buf();
        let arguments = spec
            .argument_values()
            .map(OsString::from)
            .collect::<Vec<_>>();
        self.calls
            .lock()
            .map_err(|_| ProcessError::Write(std::io::Error::other("fixture lock poisoned")))?
            .push((executable.clone(), arguments.clone()));
        let rendered = arguments
            .iter()
            .map(|value| value.to_string_lossy())
            .collect::<Vec<_>>();
        if rendered
            .iter()
            .any(|argument| argument == "--dump-single-json")
        {
            return Ok(success_stdout(self.analysis.as_bytes()));
        }
        let name = executable
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if name.contains("ffprobe") {
            let probe = rendered
                .last()
                .filter(|target| target.ends_with(".mp4"))
                .map_or(PROBE, |_| MP4_PROBE);
            return Ok(success_stdout(probe.as_bytes()));
        }
        if name.contains("ffmpeg") {
            let Some(target) = arguments.last() else {
                return Err(ProcessError::Write(std::io::Error::other(
                    "fixture FFmpeg target missing",
                )));
            };
            fs::write(PathBuf::from(target), b"encoded fixture").map_err(ProcessError::Write)?;
            return Ok(success_stdout(
                b"out_time_us=106200000\nout_time_us=212400000\nprogress=end\n",
            ));
        }
        let output_index = rendered
            .iter()
            .position(|argument| argument == "--output")
            .and_then(|index| index.checked_add(1))
            .ok_or_else(|| {
                ProcessError::Write(std::io::Error::other("fixture yt-dlp output missing"))
            })?;
        let Some(target) = arguments.get(output_index) else {
            return Err(ProcessError::Write(std::io::Error::other(
                "fixture yt-dlp output value missing",
            )));
        };
        fs::write(PathBuf::from(target), b"downloaded fixture").map_err(ProcessError::Write)?;
        Ok(success_stderr(
            b"yt-media-progress|downloading|50|100|100|10|5\nyt-media-progress|finished|100|100|100|0|0\n",
        ))
    }
}

fn success_stdout(bytes: &[u8]) -> ProcessOutput {
    success_capture(bytes, &[], OutputStream::Stdout)
}

fn success_stderr(bytes: &[u8]) -> ProcessOutput {
    success_capture(&[], bytes, OutputStream::Stderr)
}

fn success_capture(stdout: &[u8], stderr: &[u8], event_stream: OutputStream) -> ProcessOutput {
    let event_bytes = match event_stream {
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
            events: if event_bytes.is_empty() {
                Vec::new()
            } else {
                vec![ProcessEvent {
                    sequence: 0,
                    stream: event_stream,
                    bytes: event_bytes.to_vec(),
                }]
            },
        },
    }
}

fn capture(bytes: &[u8]) -> StreamCapture {
    let lines = if bytes.is_empty() {
        0
    } else {
        bytes
            .split(|byte| *byte == b'\n')
            .count()
            .saturating_sub(usize::from(bytes.last() == Some(&b'\n')))
    };
    StreamCapture {
        bytes: bytes.to_vec(),
        observed_bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        observed_lines: u64::try_from(lines).unwrap_or(u64::MAX),
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

fn make_tools(directory: &Path) -> Result<DownloadTools, Box<dyn std::error::Error>> {
    Ok(DownloadTools::new(
        make_executable(&directory.join("yt-dlp"))?,
        make_executable(&directory.join("ffmpeg"))?,
        make_executable(&directory.join("ffprobe"))?,
        make_executable(&directory.join("deno"))?,
    ))
}

#[tokio::test]
async fn split_stream_job_uses_analyzed_total_from_the_first_progress_event()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempdir()?;
    let tools = make_tools(directory.path())?;
    let runner = Arc::new(FixtureRunner::adaptive());
    let service = DownloadService::with_runner(tools, runner);
    let request = DownloadRequest {
        url: MediaUrl::parse("https://www.youtube.com/watch?v=dQw4w9WgXcQ")?,
        output: OutputSelection::Mp4(VideoQuality::try_from(1080)?),
        destination: Destination::new(directory.path())?,
        name: Some(OutputName::new("split-stream.mp4")?),
    };
    let started = service.start(request);
    let mut events = started.events;
    started.completion.wait().await?;

    let mut progress = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let JobEventKind::Progress { progress: snapshot } = event.kind
            && snapshot.stage == JobStage::Downloading
        {
            progress.push(snapshot);
        }
    }
    let first = progress
        .first()
        .ok_or_else(|| std::io::Error::other("no download progress was emitted"))?;
    assert_eq!(first.completed, 50);
    assert_eq!(first.total, Some(13_500_000));
    assert!(first.percent.is_some_and(|percent| percent > 0.0));
    assert!(
        progress
            .windows(2)
            .all(|pair| pair[0].percent <= pair[1].percent)
    );
    assert_eq!(
        progress.last().and_then(|snapshot| snapshot.percent),
        Some(100.0)
    );
    Ok(())
}

#[tokio::test]
async fn complete_mp3_job_uses_exact_machine_contracts_and_no_clobber_publication()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempdir()?;
    let tools = make_tools(directory.path())?;
    fs::write(directory.path().join("song.mp3"), b"user file")?;
    let runner = Arc::new(FixtureRunner::default());
    let service = DownloadService::with_runner(tools, runner.clone());
    let request = DownloadRequest {
        url: MediaUrl::parse("https://www.youtube.com/watch?v=dQw4w9WgXcQ")?,
        output: OutputSelection::Mp3(AudioQuality::try_from(192)?),
        destination: Destination::new(directory.path())?,
        name: Some(OutputName::new("song.mp3")?),
    };
    let started = service.start(request);
    let mut events = started.events;
    let result = started.completion.wait().await?;
    assert_eq!(
        result.path.file_name().and_then(|value| value.to_str()),
        Some("song (1).mp3")
    );
    assert_eq!(fs::read(directory.path().join("song.mp3"))?, b"user file");
    assert!(result.size_bytes > 0);

    let mut stages = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let JobEventKind::Stage { stage } = event.kind {
            stages.push(stage);
        }
    }
    assert!(stages.contains(&JobStage::Analyzing));
    assert!(stages.contains(&JobStage::Downloading));
    assert!(stages.contains(&JobStage::Converting));
    assert!(stages.contains(&JobStage::Finalizing));
    assert!(stages.contains(&JobStage::Completed));

    let calls = runner
        .calls
        .lock()
        .map_err(|_| std::io::Error::other("fixture lock poisoned"))?;
    let rendered = calls
        .iter()
        .map(|(_, arguments)| {
            arguments
                .iter()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(rendered.len(), 4);
    assert!(
        rendered[0]
            .iter()
            .any(|argument| argument == "--dump-single-json")
    );
    assert!(rendered[1].windows(2).any(|pair| {
        pair == [
            "--progress-template",
            "download:yt-media-progress|%(progress.status)s|%(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.total_bytes_estimate)s|%(progress.speed)s|%(progress.eta)s",
        ]
    }));
    assert!(
        rendered[1]
            .windows(2)
            .any(|pair| pair == ["--progress-delta", "1"])
    );
    assert!(
        rendered[1]
            .windows(2)
            .any(|pair| pair == ["--format", "140"])
    );
    assert!(
        rendered[2]
            .windows(2)
            .any(|pair| pair == ["-c:a", "libmp3lame"])
    );
    assert!(rendered[2].windows(2).any(|pair| pair == ["-b:a", "192k"]));
    assert_eq!(
        &rendered[3][..7],
        [
            "-v",
            "error",
            "-show_entries",
            "format=format_name,duration:stream=codec_type,codec_name,width,height,pix_fmt",
            "-of",
            "json",
            "--",
        ]
    );
    assert!(
        rendered[3]
            .last()
            .is_some_and(|path| path.contains(".yt-media-") && path.ends_with(".work.mp3"))
    );
    Ok(())
}

#[tokio::test]
async fn cancel_removes_owned_partials_and_pause_retains_only_resumable_partials()
-> Result<(), Box<dyn std::error::Error>> {
    for pause in [false, true] {
        let directory = tempdir()?;
        let tool_directory = directory.path().join("tools");
        let output_directory = directory.path().join("output");
        fs::create_dir_all(&tool_directory)?;
        fs::create_dir_all(&output_directory)?;
        let tools = make_tools(&tool_directory)?;
        let service = DownloadService::with_runner(tools, Arc::new(PausingRunner));
        let request = DownloadRequest {
            url: MediaUrl::parse("https://www.youtube.com/watch?v=dQw4w9WgXcQ")?,
            output: OutputSelection::Mp3(AudioQuality::try_from(192)?),
            destination: Destination::new(&output_directory)?,
            name: Some(OutputName::new("controlled")?),
        };
        let started = service.start(request);
        let mut events = started.events;
        loop {
            let event = events.recv().await?;
            if matches!(
                event.kind,
                JobEventKind::Stage {
                    stage: JobStage::Downloading
                }
            ) {
                break;
            }
        }
        if pause {
            started.controls.pause();
        } else {
            started.controls.cancel();
        }
        let result = started.completion.wait().await;
        if pause {
            assert!(matches!(result, Err(DownloadError::Paused)));
        } else {
            assert!(matches!(result, Err(DownloadError::Cancelled)));
        }
        let files = fs::read_dir(&output_directory)?
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        if pause {
            assert_eq!(files.len(), 2);
            assert!(files.iter().any(|file| {
                Path::new(file)
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("part"))
            }));
            assert!(
                files.iter().any(|file| {
                    file.starts_with(".yt-media-") && file.ends_with(".owner.json")
                })
            );
        } else {
            assert!(files.is_empty());
        }
    }
    Ok(())
}
