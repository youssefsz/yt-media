# YT Media

Local-first media tooling built around one reusable Rust engine, with a command-line interface and
a native cross-platform desktop application.

> **Status:** the toolchain and engine-process implementation is present; native artifacts still
> require the six-target workflow verification recorded in Plan 01. Media analysis, downloads,
> conversion, queueing, and the approved product UI are intentionally not implemented yet.

## Architecture

```text
apps/
  cli/                  Rust command-line application
  desktop/              Svelte 5 UI with a Tauri 2 native shell
crates/
  engine/               Headless, reusable Rust engine
docs/
  architecture.md       Dependency rules and data flow
  design/               Approved UI reference and validation rules
plans/                  Ordered implementation roadmap and status
```

The desktop app and CLI will both depend on the engine. The engine must not depend on either
presentation layer. See [docs/architecture.md](docs/architecture.md) and [AGENTS.md](AGENTS.md).

## Implementation Roadmap

Work is divided into dependency-ordered, independently verifiable milestones under
[plans/](plans/README.md). The registry records exactly which plan is ready, active, blocked, or
completed so work can continue without relying on chat history.

## Stack

- Rust 2024 for the engine, CLI, and native shell
- Tauri 2 with native operating-system window decorations
- Svelte 5 and strict TypeScript for the desktop interface
- Vite 7 for frontend development and production bundling
- Cargo and pnpm workspaces with committed lockfiles

Dependencies are added only when real code needs them. The engine currently owns the typed,
asynchronous external-process boundary and verified tool resolution used by later media features.

## Prerequisites

- Rust 1.96 or newer
  - Windows development must use the MSVC toolchain
- Node.js 22.12 or newer
- pnpm 10.33 or newer
- The platform prerequisites from the
  [Tauri documentation](https://v2.tauri.app/start/prerequisites/)

## Setup

```bash
pnpm install
cargo check --workspace
pnpm check
```

Useful commands:

```bash
pnpm dev            # launch the Tauri desktop scaffold
pnpm build          # build frontend assets
pnpm check          # strict Svelte and TypeScript checks
pnpm lint           # ESLint and Clippy
pnpm test           # Rust workspace tests
pnpm format:check   # Prettier and rustfmt verification
cargo run -p yt-media-cli -- --help
```

## External Tools

The bundled baseline is pinned to yt-dlp `2026.06.09`, FFmpeg/FFprobe `8.0.1`, and Deno `2.8.1`.
Downloaded binaries and native build outputs remain in ignored directories. The committed
[sidecar inventory](sidecars/README.md) documents sources, checksums, native build flags, target
names, smoke probes, and staging commands.

Runtime resolution is deliberately ordered:

1. an explicit per-tool development override;
2. a checksum- and identity-verified managed update;
3. the checksum- and identity-verified bundled baseline;
4. development-only `PATH` discovery.

Production mode is the default and never requires or consults `PATH`. Application adapters must
opt into development discovery explicitly; they must not duplicate this policy.

## Design Reference

The approved desktop direction is stored at
[docs/design/final-ui-reference.png](docs/design/final-ui-reference.png). It is a reference for the
application content—not a reason to fake operating-system chrome. Review
[docs/design/README.md](docs/design/README.md) before any UI implementation.

## Contributing

Read [CONTRIBUTING.md](CONTRIBUTING.md) and [AGENTS.md](AGENTS.md) before making changes.

## License

No project license has been selected yet. Do not redistribute the project until a license is
chosen and documented.
