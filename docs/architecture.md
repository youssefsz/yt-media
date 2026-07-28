# Architecture

## Dependency Direction

```text
┌──────────────────┐      ┌──────────────────┐
│ Desktop app      │      │ CLI              │
│ Svelte + Tauri   │      │ terminal adapter │
└────────┬─────────┘      └────────┬─────────┘
         │                         │
         └───────────┬─────────────┘
                     ▼
            ┌──────────────────┐
            │ Rust engine      │
            │ domain + use     │
            │ cases + ports    │
            └────────┬─────────┘
                     ▼
            ┌──────────────────┐
            │ Local adapters   │
            │ process + files  │
            └──────────────────┘
```

The engine is the reusable product. Desktop and CLI are delivery mechanisms.

## Engine Responsibilities

- URL validation and media analysis
- Normalized media metadata and available formats
- Format-selection policy
- Download and conversion job lifecycle
- Queue state, cancellation, retries, and cleanup
- Progress and typed failures
- Output naming and destination policy
- Ports for process execution, filesystem access, and tool discovery

The engine does not know about Tauri commands, webviews, Svelte stores, terminal colors, native
dialogs, or window state.

## Application Responsibilities

The CLI translates arguments and signals into engine requests, renders progress, and maps outcomes
to stable exit codes and optional machine-readable output.

The desktop Rust shell translates typed IPC messages, owns native dialogs and platform integration,
and emits engine events to the UI. The Svelte layer renders state and user intent; it does not
execute media tools or implement product policy.

## External Tools

`yt-dlp`, FFmpeg, FFprobe, and any required JavaScript runtime are adapters, not the public engine
API. Their command syntax and output formats must remain behind a version-aware boundary. Process
arguments are passed without shell interpolation, and child lifecycles are tied to cancellable
jobs.

The process adapter owns one POSIX process group or Windows Job Object per invocation. It drains
stdout and stderr concurrently, retains exact raw bytes within independent byte and line limits,
and reaps the complete group after success, cancellation, timeout, or caller drop. A testable
`ProcessRunner` port keeps process behavior out of media policy.

Sidecars are described by the versioned manifest under `sidecars/`. Upstream downloads are checked
before extraction; archive entries are normalized using portable path rules; links, special files,
path traversal, absolute paths, and duplicate destinations are rejected. Native FFmpeg builds emit
a target-specific receipt whose executable hashes are required before probes or staging.

Runtime resolution is engine-owned and ordered: explicit override, verified managed update,
verified bundled baseline, then development-only `PATH`. Production resolution never depends on
`PATH`. Downloaded artifacts, build outputs, receipts, and staging directories are ignored; only
manifests, build definitions, checksums, and provenance are committed.

The manually dispatched six-runner workflow uploads private workflow artifacts. It never creates a
public release. Distribution license review remains a separate release decision.

## Workspace Evolution

Begin with one cohesive engine crate. Split a module into another crate only when it has a stable
responsibility, independent tests, and a dependency boundary worth enforcing. This keeps the code
modular without manufacturing layers before the domain exists.

Record durable architecture decisions under `docs/decisions/` before merging the change that
depends on them.
