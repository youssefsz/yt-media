# YT Media

Local-first media tooling built around one reusable Rust engine, with a command-line interface and
a native cross-platform desktop application.

> **Status:** verified toolchain foundations plus public-video analysis and download CLI slices are
> implemented. Persistent jobs, recovery, and the approved product UI are intentionally not
> implemented yet.

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

Dependencies are added only when real code needs them. The engine owns the typed asynchronous
process boundary, verified tool resolution, URL policy, bounded yt-dlp adapter, normalized metadata,
and deterministic MP3/MP4 format choices.

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
cargo run -p yt-media-cli --bin yt-media -- --help
```

## Analyze a Video

Analyze one public, on-demand YouTube video without downloading it:

```bash
cargo run -p yt-media-cli --bin yt-media -- analyze \
  "https://www.youtube.com/watch?v=dQw4w9WgXcQ" \
  --tool-dir "/path/to/verified/tools"
```

Accepted inputs are standard `youtube.com/watch` URLs, `youtu.be` links, and Shorts URLs. A video
URL that also contains a playlist parameter is treated as that one video. V1 does not accept
playlist-only URLs, active or upcoming live streams, private or account-required videos, cookies,
browser-cookie import, login mechanisms, unsupported hosts, or non-HTTP(S) URLs.

`--tool-dir` must contain the current platform's exact `yt-dlp`, `ffmpeg`, and `deno` executable
names. Each executable is canonicalized and identity-probed against the pinned Plan 01 version
before analysis. When `--tool-dir` is omitted, the CLI enables the engine's development-only
`PATH` discovery and performs the same identity probes; production resolution never uses `PATH`.

Human output is written to stdout. Warnings and actionable failures are written to stderr:

```text
Title: Example video
URL: https://www.youtube.com/watch?v=dQw4w9WgXcQ
Duration: 3:32
Uploader: Example channel
Formats:
  MP3 128 kbps (source 140)
  MP3 192 kbps (source 140)
  MP3 256 kbps (source 140)
  MP3 320 kbps (source 140)
  MP4 1080p, 30 fps, 13500000 bytes (video 137, audio 140, merge)
```

Machine-readable mode emits exactly one JSON document and no decoration:

```bash
cargo run -p yt-media-cli --bin yt-media -- analyze "$URL" --json --tool-dir "$TOOL_DIR"
```

The complete field contract and compatibility policy are documented in
[`docs/analyze-json-v1.md`](docs/analyze-json-v1.md).

| Exit code | Meaning                                           |
| --------- | ------------------------------------------------- |
| `0`       | Success                                           |
| `2`       | Invalid arguments or URL                          |
| `3`       | Unsupported content                               |
| `4`       | Required tool unavailable or invalid              |
| `5`       | Extraction, bounded-protocol, or analysis failure |
| `6`       | Cancelled with Ctrl+C after child-process cleanup |
| `70`      | Internal CLI/runtime/output failure               |

Ctrl+C cancels through the engine token and the Plan 01 process owner terminates and reaps the
complete yt-dlp process tree before exit.

## Download and Convert

Produce a constant-bitrate MP3:

```bash
cargo run -p yt-media-cli --bin yt-media -- download "$URL" \
  --format mp3 --quality 192 --output "/selected/directory" \
  --tool-dir "/path/to/verified/tools"
```

Produce a compatibility MP4 at one height returned by `analyze`:

```bash
cargo run -p yt-media-cli --bin yt-media -- download "$URL" \
  --format mp4 --quality 1080 --output "/selected/directory" \
  --tool-dir "/path/to/verified/tools"
```

MP3 quality is one of `128`, `192`, `256`, or `320` kbps. MP4 quality is an exact available source
height and is never upscaled. The engine re-analyzes immediately before download, downloads the
freshly selected sources, stream-copies compatible H.264/AAC media, transcodes incompatible streams
with the locked compatibility settings, and verifies every successful output through FFprobe.

`--name` supplies an optional stem. The engine sanitizes it for every supported filesystem and adds
the extension. Existing files are never overwritten; collisions use deterministic ` (1)`, ` (2)`,
and later suffixes.

Human progress and warnings go to stderr. On success, stdout contains only the final path. `--json`
emits versioned NDJSON events followed by one final result; the complete schema and exit-code
contract are documented in [`docs/download-ndjson-v1.md`](docs/download-ndjson-v1.md).

Pause is available through the reusable engine API. It stops and reaps the current process while
retaining only documented yt-dlp resumable partials and a small versioned ownership marker. Ctrl+C
requests cancellation, removes owned temporary, partial, and ownership files, and exits only after
the child process tree is reaped.

### Opt-in live smoke

Routine tests never contact YouTube. A maintainer can explicitly supply one controlled public,
on-demand test video and verified tool directory:

```bash
export YT_MEDIA_SMOKE_URL="https://www.youtube.com/watch?v=MAINTAINER_VIDEO_ID"
cargo run -p yt-media-cli --bin yt-media -- \
  analyze "$YT_MEDIA_SMOKE_URL" --json --tool-dir "$TOOL_DIR"
```

PowerShell:

```powershell
$env:YT_MEDIA_SMOKE_URL = 'https://www.youtube.com/watch?v=MAINTAINER_VIDEO_ID'
cargo run -p yt-media-cli --bin yt-media -- analyze `
  $env:YT_MEDIA_SMOKE_URL --json --tool-dir $env:TOOL_DIR
```

The maintainer chooses and reviews that URL; no mutable third-party video is embedded in CI.

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
