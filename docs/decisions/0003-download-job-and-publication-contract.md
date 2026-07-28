# 0003: Own Download Jobs and Publish Verified Outputs Without Clobbering

- Status: Accepted
- Date: 2026-07-28

## Context

Downloads cross several trust and failure boundaries: fresh extractor data, network transfer,
optional merging or transcoding, filesystem naming, final media validation, cancellation, and
pause. The CLI and future desktop adapter need the same lifecycle without owning subprocesses or
duplicating product rules. A final filename can also collide with an existing user file or another
job between its initial check and publication.

## Decision

- The engine starts each job and returns a bounded broadcast event stream, a separate authoritative
  completion handle, and explicit pause and cancel controls. Slow event readers may receive an
  explicit lag indication but cannot block subprocess pipe drainage.
- Every job re-analyzes the URL immediately before resolving its normalized MP3 bitrate or MP4
  source height. Raw extractor format IDs never enter the public download request.
- yt-dlp and FFmpeg progress remain private bounded line protocols. The adapters use yt-dlp
  progress templates and `-progress pipe:1`, and the engine converts those records into typed
  progress snapshots.
- Pause terminates and reaps the active process tree while preserving only engine-owned yt-dlp
  resumable partials and their small versioned ownership marker. Cancel removes all owned temporary,
  partial, and ownership files. Durable recovery and automatic restart remain Plan 04
  responsibilities.
- Output stems are sanitized to a portable bounded subset. Candidate names are reserved with
  exclusive hidden lock files and deterministic numeric suffixes.
- Tools write only same-directory engine-owned temporary paths. FFprobe validates the completed
  container, codecs, dimensions, pixel format, duration, and non-empty content before publication.
- Publication creates a no-clobber hard link from the verified temporary to the reserved final
  path, then removes the temporary and reservation. An existing final path always makes publication
  fail instead of replacing user data.

## Alternatives Considered

- Streaming raw tool output to applications was rejected because it exposes unstable external
  contracts and makes every renderer parse untrusted records.
- Letting yt-dlp choose a final filename was rejected because naming, collisions, and partial-file
  ownership would no longer be deterministic engine rules.
- Checking for existence and then using a normal rename was rejected because POSIX rename can
  replace a file created during the race window.
- Creating an empty final placeholder was rejected because failure or cancellation could expose a
  false completed output.
- Suspending an arbitrary process tree in place was rejected for this slice because the semantics
  are platform-specific and do not provide durable recovery. Controlled stop plus documented
  yt-dlp partial retention is portable.

## Consequences

Applications consume one stable job model and never own media rules or child processes. Final
publication requires same-directory hard-link support; a filesystem that cannot provide the
no-clobber guarantee returns a typed output failure instead of weakening safety. Plan 04 may persist
requests and resumable-partial ownership but must preserve this lifecycle and publication contract.
