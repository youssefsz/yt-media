# 0004: Persist Engine Jobs in a Single-Writer SQLite Queue

- Status: Accepted
- Date: 2026-07-29

## Context

Plan 03 owns one download from validated request through process cleanup and no-clobber
publication, but its identity, lifecycle, and controls are process-local. Desktop and CLI clients
need the same durable queue, settings, history, recovery, concurrency, and partial-file ownership
rules. Merely opening the application must never restart interrupted network work.

## Decision

- The engine owns a `JobQueue` service and a numbered SQLite schema. Applications translate input
  and render engine snapshots; they do not implement queue or recovery policy.
- SQLite uses bundled `rusqlite`, WAL journaling, foreign keys, bounded busy timeouts, integrity
  checks, and atomic forward-only migrations. Filesystem and database operations run through
  blocking task boundaries rather than an async executor or UI thread.
- One process holds an exclusive sibling lock file for the lifetime of a writable store. A second
  writer fails with an actionable lock error instead of racing queue mutations.
- Job IDs are UUIDv7 values. FIFO scheduling uses a persisted monotonic queue sequence rather than
  wall-clock ordering, and one shared concurrency limit covers download and post-processing work.
- Opening a queue performs transactional recovery only: process-active states become
  `interrupted`, destinations are rechecked, and exact owner-manifest paths are reconciled into the
  database. No scheduler starts until an explicit enqueue, resume, or retry operation requests work.
- Resumable workspace names are job-specific. A bounded owner manifest records the job identity and
  exact engine-owned paths before tools write them. Pause and crash recovery retain compatible
  yt-dlp partials; cancellation deletes only validated, recorded owned paths.
- Completed records persist. The engine derives whether a recorded final output is present or
  missing without deleting history when users move or remove files externally.

## Consequences

The CLI and future Tauri adapter share one queue state machine, recovery transaction, settings
contract, cleanup policy, and scheduler. Queue construction can fail for a corrupt, future-version,
or already-locked database without mutating user media. Explicit resume and retry re-enter the FIFO
queue and always use Plan 03's fresh format analysis.

The database lock intentionally prevents multiple process-local schedulers from mutating one queue.
Read-only tooling can be added later if concurrent inspection becomes a product requirement; it
must not weaken the single-writer guarantee.
