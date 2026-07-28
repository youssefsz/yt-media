---
id: '05'
title: Desktop integration and typed IPC
status: blocked
depends_on:
  - '04'
unlocks:
  - '06'
started_at: null
completed_at: null
implementation_commits: []
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

Record any accepted deviation here before code depends on it.

## Completion Evidence

- Completed at:
- Implementation commits:
- Verification commands and results:
