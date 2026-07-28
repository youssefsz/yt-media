---
id: '01'
title: Toolchain and engine foundations
status: completed
depends_on: []
unlocks:
  - '02'
started_at: '2026-07-28T15:49:13Z'
completed_at: '2026-07-28T19:32:06Z'
implementation_commits:
  - b54e7db
  - aaca747
  - 12c704e
  - 8d076c1
  - 5c08e7c
  - 8672f2f
  - fc09863
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
- Preserve verified tar source files' ordinary Unix permission bits while stripping set-id and
  sticky bits. Native configuration helpers such as `config.guess` require their upstream
  executable bit; ZIP release tools continue to receive explicit post-verification permissions.
- Build x264 without assembly only on Windows ARM64. CLANGARM64's COFF assembler cannot satisfy
  x264's GNU AArch64 assembly probe, while the portable C implementation still supplies the
  required libx264 H.264 encoder and remains subject to the same capability probe.
- Select the CLANGARM64 LLVM tools explicitly for Windows ARM64 native builds and verify that
  `clang -dumpmachine` reports an AArch64 target before compiling any dependency. The inherited
  GitHub runner `PATH` also exposes an x86_64 MinGW `gcc`; allowing Autoconf to choose plain `gcc`
  compiled x86 objects inside the ARM64 job and caused misleading x264 and LAME failures. Keep the
  verified x264 source byte-exact, retain its Windows ARM64 `--disable-asm` configuration, and
  leave every other target's native tool selection unchanged.
- Retry only transient artifact-download failures (`408`, `429`, transport timeouts/connect
  failures, and `5xx`) with a bounded three-attempt budget. Digest and exact-size checks remain
  mandatory after every successful response.
- No fixed Plan 01 requirement has been weakened or substituted.

## Completion Evidence

- Completed at: `2026-07-28T19:32:06Z`.
- Implementation commits:
  - `b54e7db` — safe asynchronous process boundary, target/tool contracts, manifest and receipt
    validation, verified resolution, and process-tree tests.
  - `aaca747` — six-target inventory, secure archive handling, `xtask` sidecar automation, and the
    manually dispatched native workflow.
  - `12c704e` — architecture decision, contributor guidance, and sidecar inventory/runbook.
  - `8d076c1` — portable native-source extraction, retry policy, and Unix/macOS portability fixes.
  - `5c08e7c` — the target-scoped Windows ARM64 x264 portable-C configuration.
  - `8672f2f` — bounded typed FFmpeg configure diagnostics.
  - `fc09863` — explicit, verified Windows ARM64 LLVM tool selection with the obsolete source
    mutation removed.
- Root and contract verification on commit `fc09863`:
  - `pnpm format:check`, `pnpm lint`, `pnpm check`, `pnpm test`, and `pnpm build` passed on
    2026-07-28.
  - `cargo test --workspace --all-features` passed 17 engine unit tests, 10 process integration
    tests covering cancellation and complete process-tree cleanup, 16 `xtask` archive, checksum,
    retry, diagnostic, and toolchain tests, and the committed-manifest contract test.
  - `go run github.com/rhysd/actionlint/cmd/actionlint@v1.7.12 .github/workflows/ci.yml .github/workflows/sidecars.yml`
    passed.
  - `git ls-files` found no external executable, sidecar cache, or staged binary.
- Hosted verification:
  - [CI run `30390702709`](https://github.com/youssefsz/yt-media/actions/runs/30390702709)
    passed the Linux quality gates plus native Rust checks on Windows and macOS for `fc09863`.
  - [Sidecar run `30390715706`](https://github.com/youssefsz/yt-media/actions/runs/30390715706)
    completed successfully for all six release targets. Every job fetched pinned inputs, built
    FFmpeg/FFprobe natively, recorded and verified output digests, verified exact tool identities,
    probed libx264 H.264, native AAC, libmp3lame MP3, MP4 muxing, FFprobe, Deno, and the yt-dlp/Deno
    pairing, staged Tauri-named executables, and uploaded a private 14-day workflow artifact.
  - The Windows ARM64 build log recorded
    `native compiler target: aarch64-w64-windows-gnu` before compiling dependencies.
  - Private artifact records:
    - `sidecars-x86_64-pc-windows-msvc`: ID `8701313047`, 88,440,198 bytes,
      `sha256:1c2899c9801f9cc763d4f576a42541ab5f43678eef02979adb3d752bed9147d3`.
    - `sidecars-aarch64-pc-windows-msvc`: ID `8701370272`, 85,335,540 bytes,
      `sha256:42e39460f902cd54acfcc8ed046b027d2b4716dc25391f3c029515c2b4e79001`.
    - `sidecars-x86_64-apple-darwin`: ID `8701215822`, 103,580,717 bytes,
      `sha256:c598f227912eedf679167666efd0c3aff490f82c1e45beec98bd20830e804fe3`.
    - `sidecars-aarch64-apple-darwin`: ID `8700969998`, 99,177,503 bytes,
      `sha256:36efed60686c90a9623a4f75f7981193ee8854ea87bc8f55fbcc0d0fb5a8bdfd`.
    - `sidecars-x86_64-unknown-linux-gnu`: ID `8701020658`, 113,952,185 bytes,
      `sha256:1434b563626e38df070c4e8d09594027ec4a04c9ef9aa83e2928281b866ec2a9`.
    - `sidecars-aarch64-unknown-linux-gnu`: ID `8701005283`, 111,238,064 bytes,
      `sha256:4d205c6aca6d36e181f825be6d1959ab8759d15c9d99f9484f54c86f7a298fa2`.
  - No GitHub release, version tag, or public sidecar artifact was created.

## Resumption Evidence

The former publication and native-runner blocker was removed on 2026-07-28 when the user
authorized publishing the repository and running its private workflow artifacts. Plan 01 resumed
to address failures found by the six-target validation run. Those failures were resolved and every
target passed in sidecar run `30390715706`.
