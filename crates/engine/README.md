# YT Media Engine

Headless Rust library for media analysis, download orchestration, conversion, queueing, and
progress reporting.

The engine owns product behavior and remains independent of Tauri, Svelte, terminal presentation,
and operating-system UI APIs.

Its current foundation provides:

- the six supported release targets and target-specific executable naming;
- versioned sidecar manifest and native-build receipt contracts;
- checksum-, size-, path-, and identity-verified tool sets;
- deterministic override, managed, bundled, and development-only `PATH` resolution;
- a shell-free asynchronous process port with raw-byte diagnostics and bounded output;
- cancellation, timeout, child-tree termination, reaping, and caller-drop cleanup.
- deterministic single-video `YouTube` URL validation and canonicalization;
- a private, bounded, configuration-isolated yt-dlp analysis adapter with explicit Deno and
  `FFmpeg` paths;
- normalized bounded media metadata, source descriptors, MP3 bitrate choices, descending MP4
  source heights, and explicit compatibility work.
- typed download jobs with non-blocking events, authoritative completion, and pause/cancel controls;
- machine-readable yt-dlp and `FFmpeg` progress, MP3 encoding, MP4 merge/transcode policy, and
  bounded final `FFprobe` verification;
- portable output naming, deterministic collision reservations, partial ownership, cleanup, and
  no-clobber publication.

Queueing, persistence, recovery, and presentation remain outside this crate's current implemented
slice.
