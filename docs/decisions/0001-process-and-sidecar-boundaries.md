# 0001: Own Process Trees and Verify Sidecars Before Use

- Status: Accepted
- Date: 2026-07-28

## Context

yt-dlp, FFmpeg, FFprobe, and Deno process untrusted media metadata and can remain active for a long
time. The desktop application must not depend on ambient tools, interpolate user input through a
shell, leave descendants running, or execute an artifact before its origin and bytes are known.
Windows and POSIX systems expose different primitives for owning a child tree.

The project also needs native FFmpeg builds for six architectures. A single maintained upstream
binary source does not cover the required macOS and Windows ARM64 combination with the locked codec
set.

## Decision

- The engine exposes a testable asynchronous `ProcessRunner` port. Its Tokio adapter passes an
  executable and argument vector directly to the operating system.
- Every invocation owns a POSIX process group or Windows Job Object. Cancellation, timeout, and
  abandonment terminate the group and reap it.
- stdout and stderr are drained concurrently. Exact bytes are retained within independent byte and
  line limits and returned as ordered diagnostic events.
- Runtime tool resolution is explicit override, verified managed update, verified bundled
  baseline, and development-only `PATH`. Production mode never consults `PATH`.
- A schema-versioned manifest pins upstream URLs or refs, archive formats, sizes, SHA-256 values,
  executable paths, versions, and provenance.
- Official yt-dlp and Deno release assets are used. FFmpeg and FFprobe are built natively from
  `n8.0.1`, with pinned x264 and LAME inputs, by a manually dispatched six-runner workflow.
- Native outputs receive a build receipt containing their SHA-256 values, sizes, source ref, and
  configure arguments. Verification and probes are mandatory before target-triple staging.
- Downloaded files, native binaries, receipts, and staging output are ignored. The workflow uploads
  private artifacts only and never publishes a release.

## Alternatives Considered

- Ambient `PATH` tools as the production default were rejected because they are neither
  reproducible nor guaranteed to exist.
- Shell command strings were rejected because quoting differs by platform and turns user-controlled
  values into an injection boundary.
- Killing only the direct child was rejected because yt-dlp and FFmpeg may create descendants.
- One third-party FFmpeg binary feed was rejected because coverage and provenance do not satisfy the
  six locked targets.
- Committing binaries was rejected because it obscures provenance, expands the repository, and
  bypasses a deliberate distribution review.

## Consequences

The engine has a small set of maintained process and verification dependencies, and tests use a
compiled child fixture. Native sidecar builds are slower and require platform-specific runners, but
their source inputs and output identities are auditable. Release work must still perform a separate
license and signing review; this decision does not claim project-wide license compatibility.
