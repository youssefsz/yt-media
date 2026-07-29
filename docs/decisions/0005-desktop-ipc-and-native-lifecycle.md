# 0005: Use One Managed Desktop Service and Generated IPC DTOs

- Status: Accepted
- Date: 2026-07-29

## Context

The durable engine queue must be exposed to a Tauri webview without moving product rules, process
arguments, raw paths, or unrestricted native access into TypeScript. Desktop startup must recover
the single-writer database before claiming health, and event listeners must reconnect without
inventing state from a potentially incomplete stream.

## Decision

- Tauri manages one lazily initialized `ApplicationService`. It owns verified tool resolution,
  analyzer composition, the durable queue, event forwarding, and bounded shutdown. Commands are
  transport adapters over that service.
- Rust IPC request, response, error, and event DTOs are separate from engine domain types. `ts-rs`
  generates one checked-in TypeScript module; `pnpm ipc:check` fails if regeneration differs.
- The frontend installs its event listener before requesting bootstrap, then discards buffered
  events through the snapshot boundary. The engine supplies the authoritative job snapshot and
  monotonic boundary as one reconnect contract.
- IPC errors contain stable codes, bounded safe messages, and explicit safe details. Native logs
  may retain source chains, but process output and internal paths are never serialized implicitly.
- Packaged sidecars retain their target-qualified filenames and `SHA256SUMS` inventory as Tauri
  resources. The engine verifies checksums and exact identities before the desktop composes
  analyzer or download services. Managed, bundled, and development sources retain the engine's
  existing precedence.
- The webview receives only named application commands. Native dialog and opener plugins are
  called from Rust and are not granted as webview capabilities; shell and arbitrary filesystem
  permissions remain absent.
- The single-instance plugin is registered before application setup. A second launch focuses the
  existing main window before another service can open the database. Exit requests start one
  asynchronous bounded shutdown, then allow the native event loop to exit.

## Consequences

The desktop and CLI share queue, recovery, output, media, and process behavior. TypeScript can
render authoritative DTOs without duplicating domain enums or constructing tool commands. A tool
failure produces a degraded bootstrap with usable history and settings; a persistence failure
produces a failed bootstrap and starts no work.

Sidecar staging remains an explicit verified build step because executable artifacts are ignored
and never committed. Release signing, managed update acquisition, and distribution remain Plan
07 responsibilities.
