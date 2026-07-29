---
id: '03'
title: Download and conversion CLI slice
status: completed
depends_on:
  - '02'
unlocks:
  - '04'
started_at: '2026-07-28T22:44:44Z'
completed_at: '2026-07-29T00:05:52Z'
implementation_commits:
  - 'bfdeda20f685e3c0927677592d82a1098c013208'
  - 'e4c238e1e595af6920b30595491c8d51fc277b29'
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

- Model pause as a controlled, fully reaped stop that preserves only yt-dlp-owned resumable
  partials. Plan 03 does not add persistence or automatic restart; callers resume by starting the
  same request again, while Plan 04 owns durable recovery.
- Publish job events through a bounded broadcast stream so a slow or abandoned renderer cannot
  block subprocess pipe drainage. Stage changes and completion remain authoritative through the
  separate completion handle; lagging event consumers receive the stream's explicit lag signal.
- Reserve candidate names with exclusive hidden lock files and publish a verified same-directory
  temporary through a no-clobber hard link followed by temporary cleanup. This gives deterministic
  collision handling without exposing an empty false final file or using platform-specific unsafe
  rename APIs.
- Treat yt-dlp progress templates and FFmpeg `-progress pipe:1` as private bounded line protocols.
  Progress updates may be coalesced when the event buffer is full, but stage transitions and the
  final typed result are never inferred from presentation-layer state.
- Keep generated media fixtures runtime-only and gate tests that require the pinned real FFmpeg
  toolchain behind an explicit environment variable. Routine engine and CLI contract tests use
  compiled fixture executables and never contact third-party media.

## Completion Evidence

- Completed at: `2026-07-29T00:05:52Z`.
- Implementation commits:
  - `bfdeda2` — typed download job engine, format/conversion orchestration, bounded progress,
    cancellation and pause behavior, safe naming and no-clobber publication, fixtures, and engine
    tests.
  - `e4c238e` — human and NDJSON CLI download surfaces, exit and signal behavior, black-box tests,
    public contract documentation, architecture decision, and contributor documentation.
- Local verification on commit `e4c238e`:
  - `pnpm format:check`, `pnpm lint`, `pnpm check`, `pnpm test`, and `pnpm build` passed on
    2026-07-28.
  - `cargo test --workspace --all-features` passed 97 unit, contract, integration, compiled CLI,
    `xtask`, and manifest tests plus all doctests.
  - `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps` passed.
  - Engine and CLI all-target, all-feature `cargo check` and Clippy passed for the
    `x86_64-unknown-linux-gnu` target from Windows, in addition to the native Windows gates.
  - The opt-in real-media integration test passed with installed FFmpeg and FFprobe 8.0.1,
    covering MP3 metadata, H.264/AAC stream-copy merge, VP9/Opus to H.264/AAC transcode, duration,
    `yuv420p`, no-upscale behavior, and non-empty output.
  - `git diff --check` passed.
- Hosted verification:
  - [CI run `30408644171`](https://github.com/youssefsz/yt-media/actions/runs/30408644171)
    passed Linux formatting, lint, TypeScript/Svelte checks, all tests, and frontend build, plus
    native Rust workspace checks on Windows and macOS for `e4c238e`.
  - [Sidecar run `30408943963`](https://github.com/youssefsz/yt-media/actions/runs/30408943963)
    passed all six release targets. Every job fetched pinned inputs, built FFmpeg/FFprobe natively,
    verified executable digests and versions, probed H.264, AAC, MP3, MP4, FFprobe, Deno, and
    yt-dlp/Deno compatibility, staged Tauri-named executables, and uploaded a private artifact.
  - Private artifact records:
    - `sidecars-x86_64-pc-windows-msvc`: ID `8708007336`, 88,440,198 bytes,
      `sha256:9c7227ed0c547f42ba45abb556af0777203ee1f9b767f3210a25013244b91cc7`.
    - `sidecars-aarch64-pc-windows-msvc`: ID `8708021474`, 85,335,540 bytes,
      `sha256:91abbb492f3764a44937c611b9a58abd92e39386d4d07d7a9bb652a44c11cbe6`.
    - `sidecars-x86_64-apple-darwin`: ID `8707877334`, 103,580,717 bytes,
      `sha256:ba1b190a33c4b673a31469aca572216c53d4e4ccd937f5e4b47c6bdf56546b52`.
    - `sidecars-aarch64-apple-darwin`: ID `8707753516`, 99,177,503 bytes,
      `sha256:233675456b363219fe1cc7b4d9e4da5ee2d28e18a2d2381b17af328d64c7f58f`.
    - `sidecars-x86_64-unknown-linux-gnu`: ID `8707796663`, 113,952,185 bytes,
      `sha256:7c396adc77a990113421f07d24c6635c9b3f8bbc3d9d31b57f755dfd6176249e`.
    - `sidecars-aarch64-unknown-linux-gnu`: ID `8707770288`, 111,238,064 bytes,
      `sha256:dadaf95ff93114eef939c3883b6ff1da9fd26f3a286f24065c988e74fc8ecedc`.
- The opt-in live YouTube smoke command was not run because routine verification uses controlled
  fixtures and no maintainer-controlled public smoke URL was supplied.
