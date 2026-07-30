# Release System

The release system builds and validates six native targets without making a release public:

| Platform            | Rust target                 | Desktop package  | CLI archive |
| ------------------- | --------------------------- | ---------------- | ----------- |
| Windows x64         | `x86_64-pc-windows-msvc`    | NSIS             | ZIP         |
| Windows ARM64       | `aarch64-pc-windows-msvc`   | NSIS             | ZIP         |
| macOS Intel         | `x86_64-apple-darwin`       | DMG              | tar.xz      |
| macOS Apple Silicon | `aarch64-apple-darwin`      | DMG              | tar.xz      |
| Linux x64           | `x86_64-unknown-linux-gnu`  | Debian, AppImage | tar.xz      |
| Linux ARM64         | `aarch64-unknown-linux-gnu` | Debian, AppImage | tar.xz      |

## Workflow Boundaries

- `release-prepare.yml` is the safe validation entry point. It checks out an exact `main` commit,
  runs all root gates, builds on six native GitHub-hosted runners, packages and inspects the
  baseline, performs clean/offline startup and uninstall checks where supported, generates
  integrity metadata, and prints evidence. It has `contents: read` and uploads nothing.
- `release-candidate.yml` creates a collaborator-only draft and crosses the protected
  `release-signing` environment. It signs canonical manifests, Windows installers, and macOS
  applications, verifies notarization/signatures, produces GitHub artifact attestations, and
  uploads only validated uniquely named assets to the draft.
- `release-publish.yml` crosses the separate protected `release-publication` environment. It checks
  the exact source, a selected repository `LICENSE`, and the complete six-target desktop/CLI
  inventory before changing an existing draft to the latest public release. Do not dispatch it
  without explicit publication approval.

All workflow action references are immutable full commit SHAs. Release input `source_sha` must be
the full current `origin/main` commit. Preparation uses an immutable versioned tool-asset base URL;
the runtime channel endpoint is a repository release asset whose contents remain untrusted until
the detached signature verifies. Protected signing credentials are bound only to the validation,
signing, or platform-package step that consumes them, never to dependency installation or tests.

## Protected Configuration

Configure `release-signing` with required reviewers, prevent self-review when another authorized
reviewer exists, and restrict deployment branches to `main`. Configure `release-publication`
independently with the same or stricter policy.

Environment variable:

- `YT_MEDIA_UPDATE_PUBLIC_KEY_HEX`: 32-byte Ed25519 public key as 64 lowercase hexadecimal
  characters. This is embedded in production application builds.

Environment secret:

- `YT_MEDIA_TOOL_MANIFEST_SIGNING_KEY_HEX`: matching 32-byte Ed25519 signing seed. Never expose,
  print, commit, or reuse a test key.

Apple environment configuration:

- secrets: `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `KEYCHAIN_PASSWORD`,
  `APPLE_API_KEY_P8`, `APPLE_API_ISSUER`, `APPLE_API_KEY_ID`, `APPLE_TEAM_ID`
- variable: `APPLE_SIGNING_IDENTITY`

Windows Artifact Signing configuration:

- secrets: `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, `AZURE_SUBSCRIPTION_ID`
- variables: `AZURE_ARTIFACT_SIGNING_ENDPOINT`, `AZURE_ARTIFACT_SIGNING_ACCOUNT`,
  `AZURE_ARTIFACT_SIGNING_PROFILE`

The Azure identity must use workload identity federation for this repository/environment and only
the roles required by Artifact Signing. GitHub never stores an Azure client secret.

## Update Contract

`cargo xtask release tools` creates one deterministic target ZIP and canonical unsigned manifest.
`cargo xtask release sign` is usable only with the protected seed. `cargo xtask release verify`
verifies signature, compatibility, archive size/hash, and contents.

At runtime the engine:

1. durably claims a background check at most once per 24 hours, or accepts explicit manual intent;
2. fetches bounded manifest/signature documents and verifies canonical Ed25519 authentication;
3. validates target, versions, timestamp, immutable archive URL, sizes, digests, and complete tool
   inventory;
4. streams the archive into application-owned staging with a hard signed size bound;
5. verifies the archive digest before safe extraction and rejects traversal, links, special files,
   duplicate paths, unexpected entries, and expanded-size violations;
6. verifies each executable and probes yt-dlp/Deno, FFmpeg codecs/muxer, FFprobe, and Deno EJS
   behavior;
7. activates the complete directory transaction, retaining one last-known-good set.

Two consecutive managed-start failures trigger rollback. The Settings action “Use bundled tools”
removes managed update state only. Bundled tools and durable jobs/history/settings remain.

CLI, managed-tool archives, and pre-sign desktop staging use exact whole-file checksums. macOS
packaging replaces nested executable signatures, so package inspection and runtime validate each
signed sidecar plus the containing application's deep, strict code-signing seal. The outer
application seal records the nested code identities; version, codec, and EJS probes then run against
the original signed executables. No verification step modifies the application bundle.

Linux package builds set linuxdeploy's `NO_STRIP=1` and provide a narrowly scoped `PATCHELF`
delegate. linuxdeploy otherwise strips and rewrites RPATH on every ELF executable it discovers;
either operation can change authenticated standalone payloads. The delegate refuses only
`--set-rpath` mutations for the four known sidecar filenames and forwards every other operation to
the system `patchelf`. Debian and AppImage inspection still require all packaged sidecars to retain
the exact pre-sign staging digests.

## Local Validation

```text
pnpm format:check
pnpm lint
pnpm check
pnpm test
pnpm build
cargo xtask release --help
```

Routine validation never uses a public mutable media fixture. Live YouTube smoke testing remains
manual and opt-in. Do not commit downloaded archives, sidecars, installers, credentials,
certificates, generated SBOMs, or build output.

Every target must provide non-zero measurements before metadata generation. The reviewed inclusive
limits are:

| Metric                          | Maximum                                         |
| ------------------------------- | ----------------------------------------------- |
| Combined draft artifacts        | 1 GiB                                           |
| Installed desktop application   | 1 GiB                                           |
| Offline cold startup            | 15 s (20 s on macOS Intel)                      |
| Idle resident memory            | 1 GiB                                           |
| Active-download resident memory | 2 GiB                                           |
| Controlled fixture analysis     | 30 s (35 s on Windows x64; 45 s on macOS Intel) |

Metadata generation fails when a measurement is missing, malformed, zero, or above its limit.
Changing a limit requires an explicit review and corresponding evidence; required bundled tools
must not be removed to pass an artifact-size limit.

The macOS Intel limits account for the first six-target prepare run on GitHub's Intel runner:
17.668 s cold startup and 39.627 s controlled-fixture analysis. The rounded limits leave about
13 percent runner variance.

Windows x64 fixture analysis measured 25.055 s and 32.466 s on consecutive native prepare runs.
The target-scoped 35 s limit leaves about 8 percent above the observed high while retaining the
original 30 s limit on Windows ARM64, both Linux targets, and macOS Apple Silicon.

## Primary References

- [Tauri distribution](https://v2.tauri.app/distribute/) and
  [external binaries](https://v2.tauri.app/develop/sidecar/)
- [GitHub artifact attestations](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/use-artifact-attestations),
  [deployment environments](https://docs.github.com/en/actions/how-tos/deploy/configure-and-manage-deployments/manage-environments),
  and [immutable action references](https://docs.github.com/en/actions/security-for-github-actions/security-guides/security-hardening-for-github-actions#using-third-party-actions)
- [Apple notarization workflow](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution)
- [Microsoft Artifact Signing integrations](https://learn.microsoft.com/en-us/azure/artifact-signing/how-to-signing-integrations)
- [yt-dlp options](https://github.com/yt-dlp/yt-dlp#usage-and-options) and
  [FFmpeg documentation](https://ffmpeg.org/documentation.html)
- [ed25519-dalek](https://docs.rs/ed25519-dalek/) and
  [serde_json_canonicalizer](https://docs.rs/serde_json_canonicalizer/)
