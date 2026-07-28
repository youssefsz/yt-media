---
id: '03'
title: Download and conversion CLI slice
status: ready
depends_on:
  - '02'
unlocks:
  - '04'
started_at: null
completed_at: null
implementation_commits: []
---

# Plan 03: Download and Conversion CLI Slice

## Objective

Implement reliable MP3 and MP4 production in the engine and prove it through the CLI, including
honest progress, safe filenames, collision handling, cancellation, partial-file ownership, merging,
and compatibility transcoding.

## Public Interfaces

- Add `DownloadRequest`, `OutputSelection`, `AudioQuality`, `VideoQuality`, `Destination`,
  `OutputName`, `JobId`, `JobStage`, `JobProgress`, `JobEvent`, `DownloadResult`, and typed
  `DownloadError`.
- Expose a start operation returning an event stream plus a completion handle and explicit pause or
  cancel controls. The engine, not the CLI, owns state transitions and child processes.
- Add:

  ```text
  yt-media download <URL> --format mp3 --quality <128|192|256|320> --output <DIR> [--name <NAME>] [--json] [--tool-dir <PATH>]
  yt-media download <URL> --format mp4 --quality <HEIGHT> --output <DIR> [--name <NAME>] [--json] [--tool-dir <PATH>]
  ```

- Human progress renders on stderr so stdout can return the final path. `--json` emits versioned
  newline-delimited job events and a final result on stdout.

## Download and Conversion Policy

- Re-analyze immediately before starting and resolve the requested normalized format against that
  result; never trust a stale raw format ID supplied by the user.
- MP3 downloads the best usable audio stream, then encodes with libmp3lame at the selected constant
  bitrate, preserves basic title/artist metadata when safe, and writes an `.mp3` file.
- MP4 never upscales. Prefer direct H.264/AAC downloads and stream-copy merge. When the selected
  height lacks compatible streams, transcode to H.264 with libx264 `-preset fast -crf 20`,
  `yuv420p`, AAC 192 kbps, and `+faststart`; preserve source frame rate.
- Use machine-readable yt-dlp progress templates and FFmpeg `-progress pipe:1`; never scrape human
  progress bars. Emit stages for analyzing, downloading, merging, converting, finalizing,
  completed, paused, cancelled, and failed.
- Sanitize names for all supported filesystems, remove control and reserved characters, prevent
  device names and trailing dots/spaces, preserve a bounded readable stem, and add the extension
  internally.
- Never overwrite an existing user file. Resolve collisions deterministically with ` (1)`, ` (2)`,
  and so on under an exclusive destination reservation.
- Write only to engine-owned temporary/partial paths in the chosen destination and atomically rename
  after validation. Cancel removes owned temporary output; pause or unexpected interruption retains
  resumable yt-dlp partials.
- Ctrl+C requests cancellation once, waits for bounded cleanup, and escalates child-tree termination
  if needed. The CLI exits only after children are reaped.

## Tests

- Unit-test format resolution, no-upscale rules, direct/remux/transcode decisions, MP3 bitrates,
  filename sanitization, collision races, stage transitions, and progress aggregation.
- Contract-test exact yt-dlp and FFmpeg argv, progress parsing, warning/error separation, partial
  paths, non-zero exits, cancellation, pause, and cleanup with fixture executables.
- Use FFmpeg to generate tiny deterministic local media fixtures, then integration-test MP3 output,
  direct MP4 remux, H.264/AAC transcode, metadata, duration tolerance, codec verification through
  FFprobe, and playable non-empty output.
- Black-box test CLI human and NDJSON modes, final-path stdout behavior, exit codes, Ctrl+C, invalid
  qualities, unwritable destinations, existing files, and Unicode names.
- Run an opt-in live smoke download into a temporary directory; it remains outside routine CI.

## Acceptance Criteria

- Every successful result is probed and matches the requested container, compatibility policy, and
  quality without overwriting user data.
- Failed and cancelled jobs leave no false final file; paused/interrupted jobs retain only documented
  resumable partials.
- Progress never blocks pipe readers and stages accurately reflect download versus post-processing.
- CLI and engine share all selection, naming, and lifecycle rules.
- Engine/CLI unit, integration, black-box, and root quality gates pass on Windows, macOS, and Linux.

## Decisions and Deviations

Record any accepted deviation here before code depends on it.

## Completion Evidence

- Completed at:
- Implementation commits:
- Verification commands and results:
