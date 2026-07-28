//! Terminal adapter for the reusable YT Media analysis engine.

#![forbid(unsafe_code)]

use std::{
    collections::BTreeMap,
    error::Error,
    ffi::OsString,
    io::{self, Write},
    path::{Path, PathBuf},
};

use clap::{Parser, Subcommand, error::ErrorKind};
use serde::Serialize;
use yt_media_engine::{
    analysis::{
        AnalysisTools, AnalyzeError, Analyzer, CompatibilityWork, FormatOption, MediaInfo, MediaUrl,
    },
    cancellation::CancellationToken,
    resolver::{ResolutionMode, ToolResolutionConfig, ToolResolutionError, ToolResolver},
    target::SupportedTarget,
    tool::Tool,
};

const ANALYZE_SCHEMA_VERSION: u32 = 1;

/// Stable process exit codes for the CLI contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(i32)]
pub enum ExitCode {
    /// The command completed successfully.
    Success = 0,
    /// Arguments or the media URL were invalid.
    InvalidInput = 2,
    /// The URL or extractor result named unsupported content.
    UnsupportedContent = 3,
    /// Required tools were absent, invalid, or had unexpected identities.
    UnavailableTools = 4,
    /// Extraction or analysis failed.
    AnalysisFailure = 5,
    /// Ctrl+C cancelled the operation after process-tree cleanup.
    Cancelled = 6,
    /// The CLI adapter or runtime failed unexpectedly.
    InternalFailure = 70,
}

impl ExitCode {
    /// Returns the portable integer process status.
    #[must_use]
    pub const fn value(self) -> i32 {
        self as i32
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "yt-media",
    version,
    about = "Local-first media analysis",
    color = clap::ColorChoice::Never
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Analyze one public, on-demand `YouTube` video.
    #[command(about = "Analyze one public, on-demand YouTube video")]
    Analyze {
        /// Standard watch, youtu.be, or Shorts URL.
        url: String,
        /// Emit exactly one schema-versioned JSON document.
        #[arg(long)]
        json: bool,
        /// Directory containing exact yt-dlp, `FFmpeg`, and Deno executables.
        #[arg(long, value_name = "PATH")]
        tool_dir: Option<PathBuf>,
    },
}

#[derive(Debug)]
enum CommandFailure {
    InvalidInput(String),
    UnsupportedContent(String),
    UnavailableTools(String),
    Analysis(String),
    Cancelled,
    Internal(String),
}

impl CommandFailure {
    fn exit_code(&self) -> ExitCode {
        match self {
            Self::InvalidInput(_) => ExitCode::InvalidInput,
            Self::UnsupportedContent(_) => ExitCode::UnsupportedContent,
            Self::UnavailableTools(_) => ExitCode::UnavailableTools,
            Self::Analysis(_) => ExitCode::AnalysisFailure,
            Self::Cancelled => ExitCode::Cancelled,
            Self::Internal(_) => ExitCode::InternalFailure,
        }
    }

    fn message(&self) -> &str {
        match self {
            Self::InvalidInput(message)
            | Self::UnsupportedContent(message)
            | Self::UnavailableTools(message)
            | Self::Analysis(message)
            | Self::Internal(message) => message,
            Self::Cancelled => "analysis was cancelled",
        }
    }
}

#[derive(Serialize)]
struct AnalyzeDocument<'a> {
    schema_version: u32,
    media: &'a MediaInfo,
}

/// Parses process arguments, executes the requested command, and writes the stable terminal
/// contract to the real standard streams.
pub async fn run_environment() -> ExitCode {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    let stdout = io::stdout();
    let stderr = io::stderr();
    run(arguments, &mut stdout.lock(), &mut stderr.lock()).await
}

async fn run(arguments: Vec<OsString>, stdout: &mut dyn Write, stderr: &mut dyn Write) -> ExitCode {
    let cli = match Cli::try_parse_from(arguments) {
        Ok(cli) => cli,
        Err(error) => return render_clap_error(&error, stdout, stderr),
    };
    let cancellation = CancellationToken::new();
    let operation = execute(cli.command, cancellation.clone());
    tokio::pin!(operation);

    let result = tokio::select! {
        result = &mut operation => result,
        signal = tokio::signal::ctrl_c() => {
            match signal {
                Ok(()) => {
                    cancellation.cancel();
                    operation.await
                }
                Err(error) => Err(CommandFailure::Internal(format!(
                    "could not listen for Ctrl+C: {error}"
                ))),
            }
        }
    };
    match result {
        Ok(output) => match write_success(stdout, &output) {
            Ok(()) => {
                let _ignored = write_success_diagnostics(stderr, &output);
                ExitCode::Success
            }
            Err(error) => {
                let _ignored =
                    write_diagnostic(stderr, &format!("could not write command output: {error}"));
                ExitCode::InternalFailure
            }
        },
        Err(failure) => {
            let exit_code = failure.exit_code();
            let _ignored = write_diagnostic(stderr, failure.message());
            exit_code
        }
    }
}

fn render_clap_error(
    error: &clap::Error,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> ExitCode {
    let rendered = error.to_string();
    if matches!(
        error.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
    ) {
        if stdout.write_all(rendered.as_bytes()).is_ok() {
            ExitCode::Success
        } else {
            ExitCode::InternalFailure
        }
    } else {
        let _ignored = stderr.write_all(rendered.as_bytes());
        ExitCode::InvalidInput
    }
}

enum SuccessOutput {
    Human {
        rendered: String,
        warnings: Vec<String>,
    },
    Json(Vec<u8>),
}

async fn execute(
    command: Command,
    cancellation: CancellationToken,
) -> Result<SuccessOutput, CommandFailure> {
    match command {
        Command::Analyze {
            url,
            json,
            tool_dir,
        } => execute_analyze(&url, json, tool_dir.as_deref(), cancellation).await,
    }
}

async fn execute_analyze(
    raw_url: &str,
    json: bool,
    tool_directory: Option<&Path>,
    cancellation: CancellationToken,
) -> Result<SuccessOutput, CommandFailure> {
    let media_url = MediaUrl::parse(raw_url).map_err(|error| {
        if error.is_unsupported_content() {
            CommandFailure::UnsupportedContent(error.to_string())
        } else {
            CommandFailure::InvalidInput(error.to_string())
        }
    })?;
    let tools = resolve_tools(tool_directory, cancellation.child_token()).await?;
    let analyzer = Analyzer::new(tools);
    let info = analyzer
        .analyze(&media_url, cancellation.clone())
        .await
        .map_err(|error| map_analyze_error(error, cancellation.is_cancelled()))?;

    if json {
        let document = AnalyzeDocument {
            schema_version: ANALYZE_SCHEMA_VERSION,
            media: &info,
        };
        let mut bytes = serde_json::to_vec(&document).map_err(|error| {
            CommandFailure::Internal(format!("could not serialize analysis JSON: {error}"))
        })?;
        bytes.push(b'\n');
        Ok(SuccessOutput::Json(bytes))
    } else {
        Ok(SuccessOutput::Human {
            rendered: render_human(&info),
            warnings: info.warnings,
        })
    }
}

async fn resolve_tools(
    tool_directory: Option<&Path>,
    cancellation: CancellationToken,
) -> Result<AnalysisTools, CommandFailure> {
    let target =
        SupportedTarget::current().map_err(|error| CommandFailure::Internal(error.to_string()))?;
    let mut config = ToolResolutionConfig::default();
    if let Some(directory) = tool_directory {
        config.explicit_overrides = analysis_tool_paths(directory, target);
    } else {
        config.mode = ResolutionMode::Development;
        config.path_environment = std::env::var_os("PATH");
    }
    let resolver = ToolResolver::default();
    let yt_dlp = resolve_one(
        &resolver,
        Tool::YtDlp,
        target,
        &config,
        cancellation.child_token(),
    )
    .await?;
    let ffmpeg = resolve_one(
        &resolver,
        Tool::Ffmpeg,
        target,
        &config,
        cancellation.child_token(),
    )
    .await?;
    let deno = resolve_one(
        &resolver,
        Tool::Deno,
        target,
        &config,
        cancellation.child_token(),
    )
    .await?;
    AnalysisTools::from_resolved(yt_dlp, ffmpeg, deno)
        .map_err(|error| CommandFailure::Internal(error.to_string()))
}

async fn resolve_one(
    resolver: &ToolResolver,
    tool: Tool,
    target: SupportedTarget,
    config: &ToolResolutionConfig,
    cancellation: CancellationToken,
) -> Result<yt_media_engine::resolver::ResolvedTool, CommandFailure> {
    resolver
        .resolve(tool, target, config, cancellation.clone())
        .await
        .map_err(|error| map_tool_error(&error, cancellation.is_cancelled()))
}

fn analysis_tool_paths(directory: &Path, target: SupportedTarget) -> BTreeMap<Tool, PathBuf> {
    [Tool::YtDlp, Tool::Ffmpeg, Tool::Deno]
        .into_iter()
        .map(|tool| (tool, directory.join(tool.executable_name(target))))
        .collect()
}

fn map_tool_error(error: &ToolResolutionError, cancelled: bool) -> CommandFailure {
    if cancelled {
        CommandFailure::Cancelled
    } else {
        CommandFailure::UnavailableTools(format_error_chain(error))
    }
}

fn map_analyze_error(error: AnalyzeError, cancellation_requested: bool) -> CommandFailure {
    match error {
        AnalyzeError::Cancelled => CommandFailure::Cancelled,
        unsupported @ AnalyzeError::UnsupportedContent(_) => {
            CommandFailure::UnsupportedContent(unsupported.to_string())
        }
        _ if cancellation_requested => CommandFailure::Cancelled,
        other => CommandFailure::Analysis(format_error_chain(&other)),
    }
}

fn format_error_chain(error: &(dyn Error + 'static)) -> String {
    let mut messages = vec![error.to_string()];
    let mut source = error.source();
    while let Some(error) = source {
        let message = error.to_string();
        if messages.last() != Some(&message) {
            messages.push(message);
        }
        source = error.source();
    }
    messages.join(": ")
}

fn render_human(info: &MediaInfo) -> String {
    let duration_millis = info.duration.as_millis();
    let minutes = duration_millis / 60_000;
    let seconds = (duration_millis % 60_000) / 1_000;
    let mut lines = vec![
        format!("Title: {}", info.title),
        format!("URL: {}", info.url),
        format!("Duration: {minutes}:{seconds:02}"),
    ];
    if let Some(uploader) = &info.uploader {
        lines.push(format!("Uploader: {uploader}"));
    }
    lines.push("Formats:".to_owned());
    for option in &info.formats {
        lines.push(render_format(option));
    }
    lines.join("\n") + "\n"
}

fn render_format(option: &FormatOption) -> String {
    match option {
        FormatOption::Mp3 {
            bitrate_kbps,
            source,
        } => format!(
            "  MP3 {bitrate_kbps} kbps (source {})",
            source.format_id.as_str()
        ),
        FormatOption::Mp4 {
            height,
            fps,
            estimated_size_bytes,
            video_source,
            audio_source,
            compatibility,
            ..
        } => {
            let fps = fps.map_or_else(|| "unknown fps".to_owned(), |value| format!("{value} fps"));
            let size = estimated_size_bytes.map_or_else(
                || "size unknown".to_owned(),
                |bytes| format!("{bytes} bytes"),
            );
            format!(
                "  MP4 {height}p, {fps}, {size} (video {}, audio {}, {})",
                video_source.format_id.as_str(),
                audio_source.format_id.as_str(),
                render_compatibility(*compatibility)
            )
        }
    }
}

fn render_compatibility(work: CompatibilityWork) -> &'static str {
    match work {
        CompatibilityWork::None => "no merge/transcode",
        CompatibilityWork::Merge => "merge",
        CompatibilityWork::VideoTranscode => "video transcode",
        CompatibilityWork::AudioTranscode => "audio transcode",
        CompatibilityWork::VideoAndAudioTranscode => "video and audio transcode",
    }
}

fn write_success(stdout: &mut dyn Write, output: &SuccessOutput) -> io::Result<()> {
    match output {
        SuccessOutput::Human { rendered, .. } => stdout.write_all(rendered.as_bytes()),
        SuccessOutput::Json(bytes) => stdout.write_all(bytes),
    }
}

fn write_success_diagnostics(stderr: &mut dyn Write, output: &SuccessOutput) -> io::Result<()> {
    if let SuccessOutput::Human { warnings, .. } = output {
        for warning in warnings {
            writeln!(stderr, "warning: {warning}")?;
        }
    }
    Ok(())
}

fn write_diagnostic(stderr: &mut dyn Write, message: &str) -> io::Result<()> {
    writeln!(stderr, "error: {message}")
}

#[cfg(test)]
mod tests {
    use super::{ExitCode, render_clap_error};
    use clap::Parser;

    use super::Cli;

    #[test]
    fn stable_exit_code_values_do_not_drift() {
        assert_eq!(ExitCode::Success.value(), 0);
        assert_eq!(ExitCode::InvalidInput.value(), 2);
        assert_eq!(ExitCode::UnsupportedContent.value(), 3);
        assert_eq!(ExitCode::UnavailableTools.value(), 4);
        assert_eq!(ExitCode::AnalysisFailure.value(), 5);
        assert_eq!(ExitCode::Cancelled.value(), 6);
        assert_eq!(ExitCode::InternalFailure.value(), 70);
    }

    #[test]
    fn help_is_stdout_success() {
        let error = Cli::try_parse_from(["yt-media", "--help"]);
        assert!(error.is_err());
        let Some(error) = error.err() else {
            return;
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert_eq!(
            render_clap_error(&error, &mut stdout, &mut stderr),
            ExitCode::Success
        );
        assert!(!stdout.is_empty());
        assert!(stderr.is_empty());
    }
}
