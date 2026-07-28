# YT Media

Local-first media tooling built around one reusable Rust engine, with a command-line interface and
a native cross-platform desktop application.

> **Status:** repository foundation only. Media analysis, downloads, conversion, queueing, and the
> approved product UI are intentionally not implemented yet.

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

Dependencies are added only when real code needs them. The engine therefore begins
dependency-free instead of carrying a speculative framework stack.

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
