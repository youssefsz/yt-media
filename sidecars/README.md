# Sidecar Inventory and Supply Chain

This directory records the bundled baseline for yt-dlp, FFmpeg, FFprobe, and Deno. No executable is
committed. Downloads, source trees, native builds, receipts, and staged artifacts live under
ignored `.sidecar-cache/` and `target/sidecars/` directories.

The machine-readable source of truth is [`manifest.v1.json`](manifest.v1.json). It covers:

| Target                      | yt-dlp asset               | Deno asset                           |
| --------------------------- | -------------------------- | ------------------------------------ |
| `x86_64-pc-windows-msvc`    | `yt-dlp.exe`               | `deno-x86_64-pc-windows-msvc.zip`    |
| `aarch64-pc-windows-msvc`   | `yt-dlp_arm64.exe`         | `deno-aarch64-pc-windows-msvc.zip`   |
| `x86_64-apple-darwin`       | `yt-dlp_macos` (universal) | `deno-x86_64-apple-darwin.zip`       |
| `aarch64-apple-darwin`      | `yt-dlp_macos` (universal) | `deno-aarch64-apple-darwin.zip`      |
| `x86_64-unknown-linux-gnu`  | `yt-dlp_linux`             | `deno-x86_64-unknown-linux-gnu.zip`  |
| `aarch64-unknown-linux-gnu` | `yt-dlp_linux_aarch64`     | `deno-aarch64-unknown-linux-gnu.zip` |

## Pinned Sources

- yt-dlp `2026.06.09`: official GitHub release assets and upstream `SHA2-256SUMS`.
- Deno `2.8.1`: official GitHub release ZIPs, archive checksum files, and unpacked executable
  checksum files.
- FFmpeg/FFprobe `8.0.1`: `ffmpeg-8.0.1.tar.xz`, tag `n8.0.1`, commit
  `894da5ca7d742e4429ffb2af534fcda0103ef593`.
- x264: stable commit `b35605ace3ddf7c1a5d67a2eb553f034aef41d55`.
- LAME: source release `3.100`.

The manifest contains each URL, source ref, byte size, SHA-256 digest, executable path, and
provenance record. The yt-dlp standalone binaries report GPL-3.0-or-later licensing upstream. Deno
reports MIT. The FFmpeg build enables GPL code through libx264. These records are an inventory, not
a project-wide compatibility determination; distribution requires a separate license decision.

## Native FFmpeg Build

`cargo xtask sidecars build` orchestrates native commands with argument arrays. It does not
construct a shell command string. The pinned x264 and LAME sources are built statically, followed by
FFmpeg with the recorded configuration:

```text
--pkg-config-flags=--static
--enable-gpl
--enable-libx264
--enable-libmp3lame
--enable-static
--disable-shared
--disable-avdevice
--disable-network
--disable-doc
--disable-debug
--disable-ffplay
--disable-programs
--enable-ffmpeg
--enable-ffprobe
--disable-autodetect
```

Target-specific prefix, include, and library paths are also recorded in each build receipt.
`SOURCE_DATE_EPOCH=1763603031`, `ZERO_AR_DATE=1`, and compiler prefix-map flags remove known
host-path and archive-time variance. Required output capabilities are libx264 H.264, native AAC,
libmp3lame MP3, and the MP4 muxer. Device capture and network protocols are excluded; the product
hands FFmpeg explicit local files.

Windows ARM64 configures x264 with `--disable-asm` because CLANGARM64's COFF assembler cannot pass
x264's GNU AArch64 assembly probe. The portable C implementation remains enabled and the workflow
keeps the verified upstream source byte-exact. Every native dependency and FFmpeg build step
explicitly selects CLANGARM64's `clang`, `clang++`, and target binutils rather than allowing
Autoconf to find the x86_64 MinGW `gcc` exposed by the inherited runner `PATH`. Before compiling,
xtask requires `clang -dumpmachine` to report an AArch64 target. The workflow still requires FFmpeg
to expose the `libx264` encoder before it can stage an artifact.

Expected native output names are `ffmpeg.exe` and `ffprobe.exe` on Windows, and `ffmpeg` and
`ffprobe` elsewhere. A successful build records them as `bin/<name>` in
`ffmpeg-build-receipt.v1.json`.

## Native Build Prerequisites

Use stable Rust on a machine whose architecture matches the requested target. The build command
locates `bash` and `make` through `PATH`; FFmpeg configuration also requires a C/C++ toolchain,
`nasm`, and `pkg-config`.

- Linux: install `build-essential`, `nasm`, and `pkg-config`. The workflow also installs
  `autoconf`, `automake`, `libtool`, and `yasm` so runner images cannot accidentally supply them.
- macOS: install the Xcode command-line tools and the Homebrew packages `autoconf`, `automake`,
  `libtool`, `nasm`, `pkg-config`, and `yasm`.
- Windows: run inside the MSYS2 environment used by the workflow. Use UCRT64 with
  `mingw-w64-ucrt-x86_64-toolchain` and `mingw-w64-ucrt-x86_64-pkgconf` for x64, or CLANGARM64 with
  `mingw-w64-clang-aarch64-toolchain` and `mingw-w64-clang-aarch64-pkgconf` for ARM64. Both require
  `base-devel` and `nasm`.

The workflow definition is the authoritative machine setup. A Windows WSL launcher or unrelated
MinGW installation is not a substitute for the selected MSYS2 environment.

## Commands

Run commands from the repository root:

```text
cargo xtask sidecars fetch --target x86_64-pc-windows-msvc
cargo xtask sidecars build --target x86_64-pc-windows-msvc
cargo xtask sidecars verify --target x86_64-pc-windows-msvc
cargo xtask sidecars probe --target x86_64-pc-windows-msvc
cargo xtask sidecars stage --target x86_64-pc-windows-msvc
```

`fetch` is idempotent. It retries only transient network and HTTP failures with a bounded budget,
then checks download size and SHA-256 before extraction. Extraction rejects absolute paths,
traversal, platform prefixes, duplicate destinations, links, and special files. Verified tar
sources preserve ordinary Unix permission bits but strip set-id and sticky bits. Release executable
hashes are checked again after extraction.

`verify` rehashes all four cached executables, validates the native receipt, and runs bounded exact
version probes. `probe` additionally checks H.264/AAC/MP3 encoders, MP4 muxing, FFprobe startup,
restricted JavaScript execution in the paired Deno binary, and yt-dlp's acceptance of that exact
path as its sole enabled runtime. An explicit network-dependent EJS extraction smoke is available
with `--ejs-url <PUBLIC_TEST_URL>`; it also rejects yt-dlp's standard missing-runtime and challenge
solver warnings. Routine tests never require a mutable third-party URL.

`stage` first performs full verification, then creates a clean target directory with Tauri names:

```text
yt-dlp-<target>[.exe]
ffmpeg-<target>[.exe]
ffprobe-<target>[.exe]
deno-<target>[.exe]
SHA256SUMS
```

Plan 05 can copy that directory into the Tauri `binaries` input and list the four base names in
`bundle.externalBin`. CLI packaging consumes the same verified staged files. Neither application
may bypass the engine's tool resolver.

## Target Matrix

The manually dispatched `Build verified sidecars` workflow uses:

| Target              | GitHub-hosted runner |
| ------------------- | -------------------- |
| Windows x64         | `windows-2025`       |
| Windows ARM64       | `windows-11-arm`     |
| macOS Intel         | `macos-15-intel`     |
| macOS Apple Silicon | `macos-15`           |
| Linux x64           | `ubuntu-24.04`       |
| Linux ARM64         | `ubuntu-24.04-arm`   |

Each native job fetches, builds, records provenance, verifies, probes, stages, and uploads a
short-lived private workflow artifact. It does not create a GitHub release or publish sidecars.
If FFmpeg configuration fails, xtask includes a bounded 64 KiB tail of `ffbuild/config.log` in the
typed failure so the underlying compiler or linker probe is visible in the job log.
