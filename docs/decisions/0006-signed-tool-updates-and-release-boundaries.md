# 0006: Sign Complete Tool Sets and Separate Build, Signing, and Publication

- Status: Accepted
- Date: 2026-07-30

## Context

YT Media must work from a clean install without network access while still allowing yt-dlp,
FFmpeg, FFprobe, and Deno to evolve independently of the signed application. Tool acquisition,
archive extraction, activation, desktop packaging, platform signing, and public publication are
high-impact supply-chain boundaries. Updating tools individually or trusting unsigned checksums
could create incompatible mixtures or allow a compromised channel to replace executable code.

## Decision

- Every desktop and CLI package contains one immutable, target-specific baseline set. Production
  resolution never uses `PATH`, and yt-dlp always receives `--no-update`.
- Managed updates are complete four-tool sets described by RFC 8785 canonical JSON and authenticated
  by a detached Ed25519 signature. The release build embeds only the public key. The signed
  manifest covers schema and channel versions, target, minimum app version, creation time, immutable
  archive URL, exact sizes, SHA-256 digests, executable paths, and tool versions.
- The engine owns update scheduling, signature and compatibility policy, confined extraction,
  version and capability probes, atomic directory activation, last-known-good rollback, repeated
  startup-failure recovery, and reset-to-bundled behavior. Tauri supplies only a bounded HTTPS
  adapter and typed user intent.
- Background checks are durably limited to one attempt per 24 hours. Manual checks are explicit.
  Network or update failure never prevents the already verified bundled baseline from starting.
- Release preparation, protected signing, and public publication are separate workflows. All action
  dependencies are pinned to full commits. Preparation has read-only repository permission and
  retains artifacts only on its ephemeral runner. Signing requires the `release-signing`
  environment; publication requires the independent `release-publication` environment and an exact
  confirmation phrase.
- Windows packages use Microsoft Artifact Signing through GitHub OIDC. macOS packages require a
  Developer ID identity, hardened runtime, notarization, stapling, and Gatekeeper verification.
  Platform credentials and the Ed25519 private seed exist only as protected environment secrets.
- Each target emits checksums, an SPDX SBOM, artifact inventory, provenance, performance evidence,
  and release notes. Packages and CLI archives are extracted and inspected before any draft upload.

## Alternatives Considered

- Letting yt-dlp self-update was rejected because it bypasses application ownership, rollback, and
  package integrity.
- Updating each executable independently was rejected because health depends on a compatible
  yt-dlp, Deno, FFmpeg, and FFprobe set.
- Replacing files in place was rejected because interruption can expose partial state.
- A single workflow with broad write permission was rejected because building does not authorize
  signing, and signing does not authorize public publication.

## Consequences

The application retains an offline-capable recovery baseline even when the update channel,
signature, archive, health probe, or current managed set fails. Managed executable bytes live only
below application data and can be removed without modifying the signed bundle or durable job data.

Production candidates cannot be fully validated without externally supplied Ed25519, Apple
Developer ID/notarization, and Microsoft Artifact Signing credentials. The repository configures
those boundaries but must report them as blocked rather than substituting test credentials.
