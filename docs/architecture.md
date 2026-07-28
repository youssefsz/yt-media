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

No sidecar is committed until its distribution, update, checksum, provenance, and license strategy
is documented.

## Workspace Evolution

Begin with one cohesive engine crate. Split a module into another crate only when it has a stable
responsibility, independent tests, and a dependency boundary worth enforcing. This keeps the code
modular without manufacturing layers before the domain exists.

Record durable architecture decisions under `docs/decisions/` before merging the change that
depends on them.
