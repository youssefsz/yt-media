---
id: '02'
title: Analysis engine and CLI slice
status: completed
depends_on:
  - '01'
unlocks:
  - '03'
started_at: '2026-07-28T19:46:44Z'
completed_at: '2026-07-28T20:36:13Z'
implementation_commits:
  - fc1a41a
  - 6e61303
---

# Plan 02: Analysis Engine and CLI Slice

## Objective

Implement the first real vertical slice: validate a public YouTube URL, analyze one on-demand video
through yt-dlp, normalize its metadata and usable MP3/MP4 choices in the engine, and expose the same
result through a human-readable and machine-readable CLI command.

## Public Interfaces

- Introduce engine types for `MediaUrl`, `MediaId`, `MediaInfo`, `Thumbnail`, `Duration`,
  `OutputKind`, `FormatId`, `FormatOption`, codec/container descriptors, and typed `AnalyzeError`.
- Expose an asynchronous analyzer service accepting a validated URL and cancellation token and
  returning normalized `MediaInfo`; keep raw yt-dlp JSON private to the adapter.
- Add:

  ```text
  yt-media analyze <URL> [--json] [--tool-dir <PATH>]
  ```

- Human output goes to stdout and actionable diagnostics to stderr. `--json` emits one stable JSON
  document to stdout and no decorative text. Define documented exit codes for invalid input,
  unsupported content, unavailable tools, extraction failure, cancellation, and internal failure.

## Behavior

- Accept single public on-demand YouTube videos, including normal watch URLs, `youtu.be` links, and
  Shorts. Canonicalize host and video identity without following arbitrary non-YouTube redirects.
- Reject playlists, active live streams, private/account-required videos, cookie/login options, and
  unsupported hosts with typed user-facing errors. A URL containing both a video and playlist ID is
  analyzed as the single video only.
- Invoke yt-dlp with `--ignore-config`, no cookies, no playlist traversal, simulation, one
  machine-readable JSON result, explicit Deno runtime path, and explicit FFmpeg location.
- Parse only documented machine-readable output. Preserve unknown fields by ignoring them; reject
  missing identity/title/duration or structurally invalid format data with contextual errors.
- Normalize output choices:
  - MP3 choices are 128, 192, 256, and 320 kbps when a usable audio source exists.
  - MP4 choices are distinct available source heights, descending, without upscaling. Each choice
    records fps, estimated size when available, selected source streams, and whether merge or
    transcode will be required for H.264/AAC compatibility.
  - Prefer native H.264 video and AAC/M4A audio at the selected height. Mark, but do not perform,
    required transcoding when compatible streams are unavailable.
- Treat title, uploader, view count, upload date, thumbnail URL, duration, filesize, and codec
  details as untrusted external data. Bound strings and collections before exposing them.

## Tests

- Unit-test URL canonicalization and rejection across watch, short, shortened, playlist, live,
  malformed, deceptive-host, non-HTTP, and oversized inputs.
- Parse committed sanitized yt-dlp JSON fixtures for progressive, adaptive, audio-only, missing-size,
  high-fps, 4K, and malformed responses.
- Contract-test exact yt-dlp argv, config isolation, Deno/FFmpeg paths, cancellation, warning
  handling, invalid UTF-8, non-zero exits, and output-size limits using the fixture process runner.
- Black-box test the compiled CLI for human output, JSON schema, stderr separation, exit codes, and
  Ctrl+C cancellation.
- Provide an opt-in manual live smoke command for one maintainer-controlled public test video; it is
  never a required routine CI test.

## Acceptance Criteria

- Engine and CLI return the same normalized result for every fixture.
- No raw yt-dlp model escapes the adapter and no product rule is duplicated in the CLI.
- `--json` output is documented and covered by compatibility tests.
- Invalid or unsupported URLs fail before spawning yt-dlp whenever possible.
- Unit, contract, CLI integration, and root quality gates pass on Windows, macOS, and Linux.
- README documents the analysis command and the public-video-only v1 boundary.

## Decisions and Deviations

- Version the CLI success document independently from yt-dlp as schema `1`; raw extractor JSON
  remains private and forward-compatible through ignored unknown fields.
- Cap a URL at 2 KiB, yt-dlp stdout/stderr at 4 MiB/256 KiB, formats at 512, thumbnails at 100
  input records and 20 public records, and every exposed string at a field-specific limit.
- Represent MP4 work as `none`, `merge`, `video-transcode`, `audio-transcode`, or
  `video-and-audio-transcode`. A transcode classification may also require combining separately
  selected streams; Plan 03 owns that orchestration.
- Resolve and identity-probe yt-dlp, FFmpeg, and Deno through the Plan 01 resolver. The analyzer
  receives only those explicit paths and reuses the existing `ProcessRunner`.
- No fixed Plan 02 requirement is intentionally weakened or deferred.

## Completion Evidence

- Completed at: `2026-07-28T20:36:13Z`.
- Implementation commits:
  - `fc1a41a` — bounded media analysis engine, URL validation, private yt-dlp adapter,
    normalization rules, CLI command, fixtures, and contract/integration tests.
  - `6e61303` — public JSON schema, architecture decision, user and contributor documentation,
    and recorded Plan 02 decisions.
- Local verification on commit `6e61303`:
  - `pnpm format:check`, `pnpm lint`, `pnpm check`, `pnpm test`, and `pnpm build` passed on
    2026-07-28.
  - `cargo test --workspace --all-features` passed 37 engine analysis/unit tests, 10 process
    integration tests, 2 CLI unit tests, 8 compiled CLI integration tests, 16 `xtask` tests, the
    sidecar manifest contract test, and all doctests.
  - `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps` passed.
  - Engine and CLI all-target, all-feature `cargo check` and Clippy passed for the
    `x86_64-unknown-linux-gnu` target from Windows, in addition to the native Windows gates.
  - `git diff --check` passed, and `git ls-files` found no tracked executable, library, archive, or
    sidecar binary.
- Hosted verification:
  - [CI run `30396594762`](https://github.com/youssefsz/yt-media/actions/runs/30396594762)
    passed the Linux quality gates, including the real Unix Ctrl+C and process-tree cleanup test,
    plus native Rust workspace checks on Windows and macOS for `6e61303`.
- The opt-in live smoke command is documented but was not run because routine verification uses
  controlled fixtures and does not contact third-party media.
