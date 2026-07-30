---
id: '07'
title: Verified tool updates and cross-platform release
status: in-progress
depends_on:
  - '06'
unlocks:
  - '08'
started_at: '2026-07-30T13:21:27Z'
completed_at: null
implementation_commits:
  - 8ca47961921bd8fa8db0be606123d121e2e08ab0
  - 03f38248ef192eabb84b01a2e9c8be53c62c7365
  - 70d8252d4c71524822e18b23b708b390484a5596
  - 968ca3f572bbbbbda2f81e521d4b69554c0b77e1
  - 6a3f78b427946531d413d58a5a06c74ad0dd52a7
  - 01fc69a22a22f6da4f33a7d4a494d5cf8357f64e
  - 0f785126e363beeef910369063c98e5e282581ea
  - 24487a1c621fd549b42a617dc018f95795548a1a
  - fc65989886ecfe6d2f9284e39a8c146ea79ee5bd
  - 1e7c1c67de39fdde8835d031f315f1a4755b0807
  - 120b4ee80b6b61f3ac2c0f298e6bed61c068e892
  - 2be78794799f2634221ac3d80ffbb053385718ac
  - 00186a20762da9bd85bfccc6aac0fc3b47ac1345
  - 2e79ca8c5a7ae25ae887230134b0d7a1a5bb1e27
  - 2c8bad062793dbde40fa203c57b6109560e0178d
  - 3c7a1addc34e9c23671ea35f13f578aa362d3c98
  - 708aefdd094606de3363a44bbfa6bd7f85d9feb9
  - d0b5b9d9908b687e997ff9198eeef6c79d07b5d9
  - e5925cfd338ade56e8c20c55c333a2107d47551a
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

- macOS code signing replaces nested executable signatures after the immutable baseline inventory
  is generated. Exact whole-file checksums therefore remain authoritative through pre-sign desktop
  staging and for CLI/tool archives. Following Apple's nested-code model, package inspection and
  runtime validate each sidecar signature and the containing application's deep, strict seal, whose
  nested-code records bind those signed executables. The application bundle is never modified, and
  version/codec/EJS probes run against the original signed executable.
- Linux desktop builds disable linuxdeploy's default ELF stripping and use its documented
  `PATCHELF` override to delegate all operations except RPATH mutation of the four authenticated
  sidecar filenames. linuxdeploy otherwise rewrites every ELF in AppImage `usr/bin`, including
  standalone yt-dlp and Deno payloads. Debian and AppImage packages must preserve and prove the
  exact staged sidecar digests.
- Controlled-fixture analysis retains a 30-second default ceiling, with reviewed target-specific
  limits of 35 seconds for Windows x64 and 45 seconds for macOS Intel. Consecutive Windows x64
  prepare runs measured 25.055 and 32.466 seconds on GitHub-hosted runners; the scoped ceiling
  allows about 8 percent above the observed high without relaxing Windows ARM64, Linux, or Apple
  Silicon targets.
- The user explicitly added the missing analyzed-video preview to Plan 07 after observing the
  packaged Windows application. yt-dlp returned an HTTPS `i.ytimg.com` thumbnail, but the desktop
  content-security policy rejected that host. The packaged CSP now admits only
  `https://i.ytimg.com` for remote images, retaining the existing local/data/blob sources and
  avoiding a broad `https:` or `http:` image permission.

## Current Verification Evidence

Recorded at `2026-07-30T22:22:21Z` for source commit
`e5925cfd338ade56e8c20c55c333a2107d47551a`.

- Root CI: [run 30583741287](https://github.com/youssefsz/yt-media/actions/runs/30583741287)
  passed after one earlier transient CLI fixture failure on run 30578925736 passed its clean rerun.
- Unsigned six-target prepare:
  [run 30583763046](https://github.com/youssefsz/yt-media/actions/runs/30583763046)
  passed preflight plus all six native jobs. Every job built and inspected its CLI archive, update
  archive/manifest, desktop package, checksum inventory, SPDX SBOM, dependency/tool inventory,
  SLSA-style provenance statement, release notes, and performance report inside the ephemeral
  runner. No artifact was uploaded or exposed through a release.
- Native jobs:
  [Windows x64](https://github.com/youssefsz/yt-media/actions/runs/30583763046/job/91011646340),
  [Windows ARM64](https://github.com/youssefsz/yt-media/actions/runs/30583763046/job/91011646317),
  [macOS Intel](https://github.com/youssefsz/yt-media/actions/runs/30583763046/job/91011646315),
  [macOS Apple Silicon](https://github.com/youssefsz/yt-media/actions/runs/30583763046/job/91011646330),
  [Linux x64](https://github.com/youssefsz/yt-media/actions/runs/30583763046/job/91011646332),
  and
  [Linux ARM64](https://github.com/youssefsz/yt-media/actions/runs/30583763046/job/91011646327).

### Inspected artifact evidence

| Target              | Desktop package bytes and SHA-256                                                                                                                                                 | CLI archive bytes and SHA-256                                                           |
| ------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------- |
| Windows x64         | NSIS `73,921,329`; `f54d357547a8bca0cc4ceb580b6c0520943acc790542dfb54ed02c52127dbe57`                                                                                             | ZIP `91,213,445`; `c5b664bb7fbb226ecc6fd93a3c575bb3d83d054d46207ababc4e87180a52783d`    |
| Windows ARM64       | NSIS `69,195,902`; `125e96c4a75052f9fe052ec6293e4fde3729f7e34c65a38ab5a14bd39b8c61a7`                                                                                             | ZIP `87,570,817`; `a18b025c231ccf5b58b3bfccb959677266c001a1e86a70bf26eaa6018b922dbd`    |
| macOS Intel         | DMG `110,796,286`; `d0c62f76c178cbf396dd0058844c2f6864e505a87b8ed87c363505e38fcbf1c7`                                                                                             | tar.xz `77,521,608`; `819a7b88e08744d23f401bbb8e632a98a67437079a1a2a4e649c6c2c5165ad74` |
| macOS Apple Silicon | DMG `104,432,291`; `793e951aeafd61fc7900879e4e92839a6e714b217bed504c47e49c9638c29c88`                                                                                             | tar.xz `73,161,532`; `bb26d70ae41484e7db41a2b8404399ae8298ae7f2fdae2e5dac71ed94f9f0386` |
| Linux x64           | DEB `123,349,862`; `cfc020ddb948c15b9e41d7cfbd6069a330a7a2790dd7a9abfe78a9c5dad51c04`; AppImage `189,405,688`; `ad4af9162d1497cead46634caff7f495e6f9e83f25c7084a8513f2cffad4fdff` | tar.xz `84,209,448`; `113608408985bb886bf7396062748fceafe921329e56bf855da31fd7fe549c9a` |
| Linux ARM64         | DEB `120,183,642`; `c464218d5b991ba1878df009530ee7345f7b9bbe607bb6fc769b92126ab94473`; AppImage `183,495,176`; `d29256cdfe36f5e9ef1bb321a56ca731e33e4f68fceff1e8ae2f57e8d515aad3` | tar.xz `80,447,420`; `1781401c5782fe41d15581f932556dcc761c8ed21ce25e01b1b45e6e4774a6a8` |

### Performance and lifecycle evidence

| Target              | Installed bytes | Offline start ms | Idle RSS bytes | Active-download RSS bytes | Fixture analysis ms |
| ------------------- | --------------: | ---------------: | -------------: | ------------------------: | ------------------: |
| Windows x64         |     202,605,366 |            6,291 |     35,028,992 |                10,969,088 |              24,756 |
| Windows ARM64       |     172,746,241 |            3,916 |     33,734,656 |                13,266,944 |              24,422 |
| macOS Intel         |     213,737,472 |           12,844 |     51,187,712 |                87,003,136 |              21,743 |
| macOS Apple Silicon |     189,276,160 |            5,431 |     79,020,032 |                92,307,456 |              12,799 |
| Linux x64           |     189,405,688 |            4,562 |      2,060,288 |                90,501,120 |              16,169 |
| Linux ARM64         |     183,495,176 |            3,036 |      1,667,072 |                86,929,408 |              14,792 |

All six native jobs passed deterministic analyze, MP3, direct MP4, transcoded MP4, pause/resume,
interrupted recovery, update install, corruption rejection, rollback, CLI inspection, package
inspection, offline startup, persistence, and uninstall checks applicable to that runner.

### Verification commands

The following completed successfully on Windows before the verified source commit was pushed:

```text
pnpm format:check
pnpm lint
pnpm check
pnpm test
pnpm build
cargo test -p xtask performance_thresholds_scope_reviewed_runner_variance
pnpm --filter @yt-media/desktop exec vitest run src/App.test.ts
pnpm --filter @yt-media/desktop exec vitest run src/tauri-config.test.ts
```

The preview diagnosis also ran the pinned yt-dlp sidecar with `--no-update --skip-download` against
the user-reported public video and confirmed the rejected URL was an HTTPS `i.ytimg.com` thumbnail.

## External Completion Blockers

- The protected `release-signing` and `release-publication` environments exist, require reviewer
  approval, and allow only `main`, but currently contain no secrets or variables.
- A signed candidate cannot be truthfully validated without an Ed25519 manifest signing key/public
  key, Apple Developer ID certificate and notarization API credentials, and Azure Artifact Signing
  OIDC/account/profile configuration. Consequently signed Windows installers, hardened/notarized
  macOS DMGs, signed update manifests, attestations, and the collaborator-only signed draft
  workflow have not been run.
- Public publication remains explicitly unauthorized. No GitHub release or tag exists. Publication
  also requires a project `LICENSE`; repository policy forbids choosing one without an explicit
  project decision.
- Plan 07 therefore remains `in-progress`, and Plan 08 remains `blocked`. Do not use the lifecycle
  closeout commit or unlock Plan 08 until the signing credentials and license decision are supplied
  and the protected signed-candidate evidence passes.

## Completion Evidence

- Completed at:
- Implementation commits:
- Release workflow run/artifacts:
- Performance report:
- Verification commands and results:
