---
id: '08'
title: Developer bootstrap and local tool experience
status: blocked
depends_on:
  - '07'
unlocks: []
started_at: null
completed_at: null
implementation_commits: []
---

# Plan 08: Developer Bootstrap and Local Tool Experience

## Objective

Make a clean clone predictable to develop on: after installing the documented Rust, Node.js, and
pnpm prerequisites, a developer can run the standard development command without manually
installing media tools, editing `PATH`, downloading private workflow artifacts, or diagnosing
vendor-specific version output.

## Clean-Clone Experience

- Make `pnpm dev` run an idempotent native-tool bootstrap before Tauri starts. A separate explicit
  doctor/bootstrap command may also exist, but it must not be required for the normal documented
  path.
- Detect the current supported target through the existing engine target model. Never infer an
  architecture from filenames, mutable environment aliases, or host commands whose output varies
  by shell.
- Reuse an already verified local tool set without network access. On a cold cache, show concise
  progress while obtaining the exact target archive published by Plan 07.
- Store downloaded archives, partials, verification records, and expanded tools only in ignored
  repository cache or platform cache locations. Do not commit executables, downloaded archives, or
  developer-specific absolute paths.
- Document the supported one-command path, prerequisites, cache location, offline behavior, cache
  repair, and complete removal. Remove instructions that require developers to prepend tool
  directories to `PATH`.

## Artifact Resolution and Security

- Consume immutable, per-target baseline archives and the signed canonical manifest established by
  Plan 07. A contributor must not need access to private GitHub Actions artifacts, repository
  secrets, or the maintainer's account.
- Verify the manifest signature, target triple, application compatibility, archive size, and
  SHA-256 digest before extraction. Reuse the existing confined extraction and executable identity
  probes; do not add a second weaker verification path in JavaScript or shell.
- Stage the four tools as one atomic verified set. Never mix a newly downloaded tool with system
  FFmpeg, Deno, or yt-dlp merely because a matching command happens to be on `PATH`.
- Serialize concurrent bootstrap attempts, recover cleanly from interruption, and remove only
  bootstrap-owned partial files. A failed download or verification must leave the last verified
  cache intact.
- Support an explicit offline mode. When no verified cache exists, fail before launching Tauri with
  an actionable message containing the target, attempted source, and exact repair command.

## Desktop Development Integration

- Pass the verified development tool-set directory explicitly into the native desktop service.
  Production resolution remains restricted to the immutable bundled baseline and verified managed
  updates; developer convenience must not weaken release behavior.
- Ensure `pnpm dev`, direct Tauri development, and documented IDE launch configurations use the
  same bootstrap contract instead of maintaining separate setup instructions.
- Replace generic production-oriented startup text during development with a diagnostic that names
  the failed tool, candidate source, and verification reason without exposing secrets or dumping
  unbounded process output into the UI.
- Make `Retry startup` genuinely rerun recoverable tool initialization after repair, or remove the
  action and clearly require a restart. A control must not claim to retry while returning the same
  cached failure.
- Keep the healthy UI free of setup banners. Tool bootstrap runs before the application window is
  shown whenever the failure can be determined by the launcher.

## Tests and CI

- Add deterministic tests for cold bootstrap, verified cache hit, offline cache hit, interrupted
  download, corrupt digest, invalid signature, wrong target, wrong executable identity, missing
  artifact, concurrent launch, and cache repair.
- Add resolver and desktop-service tests proving development bootstrap cannot change production
  resolution priority or permit arbitrary executables.
- Exercise clean-clone bootstrap on Windows x64/ARM64, macOS Intel/Apple Silicon, and Linux
  x64/ARM64 using the repository's native runner matrix. Routine tests use controlled artifact
  fixtures; a separate integration gate consumes the real published baseline archives.
- Run a native smoke on Windows, macOS, and Linux that starts from an empty cache, launches the
  desktop application, observes healthy tool status, exits, relaunches offline, and proves the
  verified cache is reused.
- Keep download logs bounded and redact credentials, signed URLs, access tokens, and local user
  paths from uploaded CI evidence.

## Acceptance Criteria

- On every supported target, the documented clean-clone flow reaches a healthy Tauri window without
  manual media-tool installation, manual `PATH` changes, or private workflow-artifact access.
- A warm verified cache works offline, while a cold offline launch fails early with one accurate,
  actionable diagnostic and no partially initialized application window.
- Corrupt, unsigned, wrong-target, incomplete, and identity-mismatched tool sets are rejected
  without damaging a previously verified cache.
- Development setup never falls back to unrelated system tools and does not weaken bundled or
  managed production verification.
- Startup retry behavior matches its label and is covered by a native service test.
- Clean-clone matrix tests, native desktop smoke checks, security fault injection, documentation
  verification, and the root quality gates pass.

## Decisions and Deviations

- Plan 08 depends on Plan 07 because public immutable archives and the signed update manifest are
  the durable distribution boundary. The temporary private workflow-artifact workaround used
  during Plan 06 is diagnostic evidence, not the supported developer workflow.
- No fixed Plan 08 requirement may be weakened by accepting vendor-suffixed tools from `PATH`,
  committing large executables to Git, or requiring a maintainer-authenticated GitHub CLI session.

Record any further accepted deviation here before code depends on it.

## Completion Evidence

- Completed at:
- Implementation commits:
- Clean-clone matrix workflow/artifacts:
- Native smoke evidence:
- Verification commands and results:
