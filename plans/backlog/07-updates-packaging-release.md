---
id: '07'
title: Verified tool updates and cross-platform release
status: ready
depends_on:
  - '06'
unlocks:
  - '08'
started_at: null
completed_at: null
implementation_commits: []
---

# Plan 07: Verified Tool Updates and Cross-Platform Release

## Objective

Finish the production supply chain: retain an always-working bundled baseline, install verified
tool updates with rollback, package desktop and CLI artifacts for all six targets, and prove the
release matrix without publishing until explicitly authorized.

## Verified Tool Updates

- Define a versioned canonical update manifest listing channel, tool versions, target triple, URLs,
  sizes, SHA-256 digests, minimum app version, and creation time. Sign the canonical bytes with
  Ed25519; embed only the public verification key in the application.
- Host manifests and immutable tool archives as repository release assets. Checksums in an unsigned
  or unverified manifest are never sufficient.
- Check in the background at most once every 24 hours when enabled, plus an explicit manual check.
  A failed check never blocks analysis with the bundled baseline and sends no telemetry.
- Download to an application-data staging directory, enforce size limits, verify signature and
  digest before extraction, reject unsafe archive entries, set executable permissions where needed,
  and run version/codec/EJS health probes before activation.
- Activate an entire compatible tool set atomically, never a partially updated mix. Keep the bundled
  baseline and one last-known-good managed set. Roll back automatically after failed health checks
  or repeated startup failures and allow a manual reset to bundled tools.
- Pass `--no-update` to yt-dlp. External tools never modify themselves, the signed application
  bundle, or files outside the managed tool directory.

## Packaging Matrix

- Desktop artifacts:
  - Windows x64 and ARM64: signed per-architecture NSIS installers.
  - macOS Intel and Apple Silicon: separate signed, hardened-runtime, notarized DMGs.
  - Linux x64 and ARM64: AppImage and Debian packages.
- CLI artifacts: one ZIP for each Windows target and one `.tar.xz` for each macOS/Linux target,
  containing the CLI, baseline sidecars, manifest, checksums, notices, and a version file.
- Tauri `externalBin` staging uses exact target-triple names. Each package is inspected to confirm
  it contains the matching yt-dlp, FFmpeg, FFprobe, and Deno baseline and no cache/update artifacts.
- Release workflows build natively on the six GitHub-hosted runner architectures, consume only
  pinned verified sidecar artifacts, use least-privilege permissions, and require an environment
  approval before signing or publishing.

## Release Integrity and Validation

- Generate SHA-256 checksum files, an SBOM, dependency/tool inventory, build provenance, and release
  notes for every artifact. Keep signing/notarization secrets only in protected CI environments.
- Test a clean install and first launch with networking unavailable to prove the bundled tools can
  initialize and report versions; then run a controlled fixture analysis/download with networking
  enabled.
- For every target, test analyze, MP3, direct MP4, transcoded MP4, pause/resume, interrupted recovery,
  update install, corrupt-update rejection, rollback, uninstall, and preservation/removal rules for
  user data.
- Measure installer/archive size, installed size, cold startup, idle memory, active-download memory,
  and analysis latency. Record baselines and fail only on explicit reviewed thresholds; do not
  remove required out-of-box dependencies merely to improve package size.
- Keep public live YouTube tests opt-in/manual. Deterministic release gates use controlled fixtures
  and separately verify that the current pinned yt-dlp/Deno combination passes its health probe.

## Acceptance Criteria

- All six desktop targets and six CLI archives build reproducibly from a clean tagged commit and
  contain verified baseline tools.
- A corrupt, unsigned, wrong-target, oversized, or unhealthy update is rejected without replacing
  the active set.
- Rollback restores a healthy tool set and the immutable bundled baseline always remains usable.
- Installers preserve native system chrome and correctly store queue/history/settings and managed
  updates in platform application-data locations.
- Complete release matrix, update fault-injection, performance recording, and root quality gates
  pass.
- Workflows stop at validated draft artifacts until the user explicitly authorizes publishing.

## Decisions and Deviations

Record any accepted deviation here before code depends on it.

## Completion Evidence

- Completed at:
- Implementation commits:
- Release workflow run/artifacts:
- Performance report:
- Verification commands and results:
