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

Media analysis and download policy intentionally arrive in later plans.
