---
id: '05'
title: Desktop integration and typed IPC
status: completed
depends_on:
  - '04'
unlocks:
  - '06'
started_at: '2026-07-29T08:06:57Z'
completed_at: '2026-07-29T09:06:12Z'
implementation_commits:
  - 'dfc9e909ecc785f48c348c2985a86b2104668d96'
  - '756291614c5d294695b1dc2a2d107728d8d656d8'
---

# Plan 05: Desktop Integration and Typed IPC

## Objective

Connect the proven engine to the Tauri desktop shell through a minimal typed IPC boundary. This
milestone implements native lifecycle and command/event integration, not the polished product UI.

## IPC Contract

- Add the engine as a desktop Rust dependency and hold one application service in managed Tauri
  state. The service owns tool resolution, database, queue, and shutdown; command handlers remain
  thin adapters.
- Define serializable request/response/event DTOs separate from engine domain types. Generate
  checked-in TypeScript DTO definitions from Rust with stable `ts-rs`; keep a small handwritten
  typed invocation layer and make CI fail when regeneration changes tracked output.
- Provide commands for bootstrap state, analyze, enqueue, list jobs, get job, pause, resume, cancel,
  retry, list/delete history, read/update settings, choose destination, reveal completed output, and
  request tool status.
- Emit a versioned job-event envelope containing event sequence, job ID, timestamp, state, progress,
  and optional terminal result/error. On listener reconnect, the frontend obtains an authoritative
  snapshot before consuming later events.
- IPC errors expose stable codes, safe messages, and optional structured details; raw process output,
  paths outside user-facing fields, and internal error chains stay in Rust diagnostics.

## Native Behavior

- Resolve bundled baseline sidecars through application resource paths and managed updates through
  the engine locator. Do not grant the webview general shell execution permission.
- All yt-dlp/FFmpeg/Deno processes are spawned by Rust engine code. Svelte may invoke only named
  Tauri commands and may not construct process arguments or access unrestricted filesystem APIs.
- Use native folder selection for destinations and opener/reveal integration for completed files.
  Capability files allow only the minimum commands/resources needed by the main window.
- Initialize persistence and recover interrupted jobs before sending bootstrap state. A startup
  failure displays recoverable diagnostics rather than opening a falsely healthy UI.
- Coordinate single-instance behavior so a second launch focuses the existing window; it must not
  create a second queue/database writer.
- On app exit, stop accepting new jobs, checkpoint state, request bounded child shutdown, and retain
  interrupted partials. Do not block the native event loop with filesystem or process work.

## Tests

- Unit-test DTO conversion, error redaction, command validation, capability scope, and stale/invalid
  IDs without launching a webview.
- Integration-test every command against a fixture engine and temporary database, including
  bootstrap recovery, event ordering, reconnect snapshot, settings persistence, and shutdown.
- Verify generated TypeScript types and command names match the registered Tauri handlers.
- Test resource/managed/system tool precedence in packaged-development mode for each platform.
- Run native smoke tests that launch the scaffold, invoke bootstrap, analyze through fixture tools,
  enqueue a fixture job, receive progress, cancel it, and close cleanly.

## Acceptance Criteria

- No product rule or process invocation is implemented in TypeScript or Tauri handlers.
- The frontend has a fully typed command/event client without `any`, unchecked casts, or duplicated
  domain enums.
- Capabilities do not expose general shell or arbitrary filesystem execution to the webview.
- Startup, event reconnect, and shutdown preserve the queue invariants from Plan 04.
- Rust/IPC integration, type-generation drift, native smoke, and root quality gates pass on all six
  target architectures where runners are available.

## Decisions and Deviations

No material deviations were required. The packaging-only Tauri resource overlay keeps ignored
sidecar staging artifacts out of ordinary development checks while preserving the verified
resource layout for desktop bundles.

## Completion Evidence

- Completed at: `2026-07-29T09:06:12Z`
- Implementation commits:
  - `dfc9e909ecc785f48c348c2985a86b2104668d96` (`feat(desktop): add typed engine integration`)
  - `756291614c5d294695b1dc2a2d107728d8d656d8`
    (`test(desktop): expand native integration validation`)
- Verification commands and results:
  - `pnpm format:check` — passed.
  - `pnpm lint` — ESLint and workspace Clippy passed with warnings denied.
  - `pnpm check` — Svelte and TypeScript passed with zero errors and warnings.
  - `pnpm test` — frontend behavior, generated IPC drift, all workspace unit tests, fixture command
    integration, reconnect/recovery/persistence, capability, single-instance coordination, and
    bounded-shutdown tests passed.
  - `pnpm build` — production frontend build passed.
  - `cargo test --workspace --all-features` — passed, including native fixture smoke and resolver
    precedence/staged-inventory coverage.
  - `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps` — passed.
  - `pnpm ipc:generate && pnpm ipc:check` — deterministic checked-in TypeScript DTO generation and
    drift verification passed.
  - `git diff --check` — passed.
  - Native Windows Tauri smoke — launched the scaffold, recovered bootstrap state in degraded mode
    without bundled fixture tools, rendered the safe diagnostic, and closed through bounded
    shutdown. The content region was inspected at `960x640`, `1280x800`, and `1600x1000`.
  - [GitHub Actions run 30437699683](https://github.com/youssefsz/yt-media/actions/runs/30437699683)
    — quality gates and native Rust checks passed on Windows x64, Windows ARM64, macOS Intel, macOS
    Apple Silicon, Linux x64, and Linux ARM64.
