---
id: '01'
title: Toolchain and engine foundations
status: in-progress
depends_on: []
unlocks:
  - '02'
started_at: '2026-07-28T15:49:13Z'
completed_at: null
implementation_commits: []
---

# Plan 01: Toolchain and Engine Foundations

## Objective

Establish the engine's safe asynchronous process boundary and a reproducible sidecar supply chain
for all six release targets. This milestone proves that yt-dlp, FFmpeg, FFprobe, and Deno can be
located, verified, invoked, cancelled, and staged for packaging without implementing media
analysis or downloads.

## Technical References

- [yt-dlp release assets and dependencies](https://github.com/yt-dlp/yt-dlp/blob/master/README.md)
- [yt-dlp EJS runtime requirements](https://github.com/yt-dlp/yt-dlp/wiki/ejs)
- [Tauri external binary packaging](https://v2.tauri.app/develop/sidecar/)
- [yt-dlp FFmpeg build coverage](https://github.com/yt-dlp/FFmpeg-Builds)
- [GitHub-hosted runner target labels](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)

## Fixed Decisions

- Supported target triples:
  - `x86_64-pc-windows-msvc`
  - `aarch64-pc-windows-msvc`
  - `x86_64-apple-darwin`
  - `aarch64-apple-darwin`
  - `x86_64-unknown-linux-gnu`
  - `aarch64-unknown-linux-gnu`
- Pin the baseline to yt-dlp `2026.06.09`, Deno `2.8.1`, and FFmpeg `8.0.1`. If an asset is
  withdrawn or demonstrably unusable before implementation, stop and document the blocker instead
  of silently substituting a version.
- Use official yt-dlp and Deno release assets. Build FFmpeg and FFprobe reproducibly from the pinned
  source tag because no single maintained binary source covers macOS and Windows ARM64.
- The FFmpeg build must include local-file support, stream copy/remuxing, AAC, H.264 via libx264,
  and MP3 via libmp3lame. Disable unrelated device capture and server-oriented features where doing
  so is supported consistently.
- Do not commit external binaries. Downloaded and built artifacts live in ignored caches and release
  staging directories; the repository commits manifests, build definitions, checksums, and
  provenance.
- Add a conventional Rust `xtask` binary under `tools/xtask` for sidecar fetch, verify, probe, and
  stage commands. Do not implement this automation in platform-specific shell scripts.
- Engine process execution uses argument arrays, piped standard streams, process-group ownership,
  bounded output, cancellation, and typed failures. It never constructs a shell command.
- Runtime tool precedence is explicit override, verified managed update, bundled baseline, then
  development-only `PATH` discovery. Production desktop builds never require `PATH`.

## Implementation

### Sidecar manifest and build pipeline

- Define a versioned manifest containing tool name, semantic/version string, target triple, source
  URL or source ref, archive format, executable paths, SHA-256 digests, size, and provenance.
- Make `cargo xtask sidecars fetch --target <triple>` idempotently populate an ignored cache,
  verify every digest before extraction, reject archive path traversal, and never execute an
  unverified file.
- Make `verify` rehash cached files and run version probes; make `stage` copy only verified
  executables into a clean target-specific staging directory using Tauri's target-triple naming.
- Add a manually dispatched CI workflow that builds FFmpeg/FFprobe natively on the six selected
  GitHub-hosted runners, records source refs and build configuration, probes required encoders and
  muxers, and emits checksummed artifacts. It must not publish a public release automatically.
- Produce one inventory document describing exact sources, build flags, expected binary names, and
  how the desktop and CLI release jobs will consume the staged artifacts.

### Engine boundary

- Add focused modules for target/platform identification, tool descriptors and paths, the manifest,
  tool resolution, process specifications, process events, cancellation, and typed errors.
- Define a testable asynchronous `ProcessRunner` port and a production Tokio implementation.
  Capture stdout and stderr without deadlock, enforce configured byte/line limits, expose exit
  status, and redact no data by accident when returning diagnostic context.
- Own child lifecycles. Cancellation and parent drop must terminate the entire child process tree
  on Windows, macOS, and Linux, then reap it without zombies.
- Validate executable paths as files and reject unexpected tool identity or version probe output.
  No Tauri, Svelte, terminal formatting, or media-specific policy enters the engine.

## Tests

- Unit-test manifest schema/version handling, duplicate targets, missing tools, malformed hashes,
  unsupported triples, precedence, and path validation.
- Test archive extraction against traversal, absolute paths, duplicate destinations, symlinks, and
  checksum mismatches.
- Use a compiled fixture child process to test argv fidelity, stdout/stderr interleaving, large
  output limits, non-zero exits, cancellation, timeout, and child-tree cleanup.
- Run sidecar smoke tests for every produced target artifact: exact version output, yt-dlp EJS
  availability with the paired Deno path, FFprobe startup, and FFmpeg availability of H.264/AAC/MP3
  plus MP4 muxing.
- Keep network-dependent artifact tests separate from routine engine unit tests. CI consumes pinned
  fixtures or explicit sidecar-build artifacts, never mutable `latest` URLs.

## Acceptance Criteria

- All six target manifests resolve a complete verified tool set and no external binary is tracked
  by Git.
- A clean machine can fetch or build, verify, probe, and stage one target using documented commands.
- Process tests prove exact argument passing and complete cancellation/cleanup behavior.
- The engine remains independent of Tauri and presentation concerns.
- Root quality gates and the six-target sidecar workflow pass.
- Architecture and contributor documentation describe the tool supply chain and local development
  override mechanism.

## Decisions and Deviations

- Pin x264 to stable commit `b35605ace3ddf7c1a5d67a2eb553f034aef41d55` and LAME to
  release `3.100`. Their source archive sizes and SHA-256 digests are part of every target's
  FFmpeg provenance record.
- Record target-native FFmpeg and FFprobe executable sizes and SHA-256 digests in the mandatory
  per-target build receipt. The baseline manifest pins their source archive, output paths, and
  receipt contract; it cannot truthfully predeclare bytes that do not exist until each native
  runner builds them.
- Keep the routine EJS pairing probe deterministic and network-free: verify the exact Deno
  identity, execute restricted JavaScript, and require yt-dlp to accept that executable as its sole
  configured runtime. The explicit `--ejs-url` smoke exercises live EJS extraction separately and
  rejects yt-dlp's standard missing-runtime and challenge-solving warnings.
- No fixed Plan 01 requirement has been weakened or substituted.

## Completion Evidence

Populate during closeout:

- Completed at:
- Implementation commits:
- Sidecar workflow run/artifacts:
- Verification commands and results:
