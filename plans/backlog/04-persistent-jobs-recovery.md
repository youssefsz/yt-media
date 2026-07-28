---
id: '04'
title: Persistent jobs, recovery, and history
status: blocked
depends_on:
  - '03'
unlocks:
  - '05'
started_at: null
completed_at: null
implementation_commits: []
---

# Plan 04: Persistent Jobs, Recovery, and History

## Objective

Add a durable engine-owned job queue, settings, history, explicit pause/resume, and crash recovery.
Unfinished work must reopen as interrupted and never consume bandwidth until the user explicitly
resumes it.

## Public Interfaces and Storage

- Extend job state with `queued`, `analyzing`, `downloading`, `merging`, `converting`, `paused`,
  `interrupted`, `completed`, `cancelled`, and `failed`, with validated transitions.
- Add queue operations for enqueue, list, subscribe, pause, resume, cancel, retry, remove completed
  history, and bounded shutdown.
- Persist through SQLite using `rusqlite` with bundled SQLite and transactional numbered migrations.
  Run database work on a dedicated blocking boundary; never block async executors or the UI thread.
- Store stable UUIDv7 job IDs, canonical URL, normalized request, destination, owned partial paths,
  state, progress snapshot, error classification, created/updated/completed times, attempt count, and
  final output metadata. Do not store raw tool output or credentials.
- Persist settings needed by the engine: default destination, queue concurrency, update preference,
  and last selected output defaults. Desktop-only appearance state remains in the desktop layer.

## Behavior

- Default concurrency is two jobs and is configurable from one through four. Per-job post-processing
  counts against the same limit to avoid unbounded CPU and memory use.
- Queue ordering is FIFO with explicit retry appended to the queue. A resumed paused/interrupted job
  keeps its original identity and increments its attempt count.
- On startup, transactionally convert process-active states to `interrupted`, preserve resumable
  partials, verify that destinations still exist, and wait for explicit resume.
- Pause terminates the owned process tree, waits for cleanup, keeps resumable partials, then commits
  `paused`. Cancel terminates children and removes only paths recorded as engine-owned.
- Retry re-analyzes formats and fails clearly when the previous selection is no longer available.
- Completed history persists until explicit deletion. Missing or externally deleted outputs are
  shown as missing rather than silently removed.
- Migrations are forward-only, atomic, backed up before destructive changes, and fail without
  corrupting the last usable database. Only one process instance may mutate a database at a time.
- The CLI uses a platform app-data database by default and adds commands to list jobs/history,
  inspect one job, pause/cancel an active job, and explicitly resume/retry. `--data-dir` enables
  isolated automation and tests.

## Tests

- Unit-test every valid and invalid state transition, concurrency scheduling, FIFO order, retry
  semantics, startup recovery, and partial-file ownership.
- Migration-test a fresh database, each historical schema fixture, interrupted migration rollback,
  unsupported future versions, locking, and corrupted database reporting.
- Integration-test two concurrent fixture downloads, queued cancellation, pause/resume, crash-style
  restart, unavailable destination, changed formats on retry, cleanup, and graceful shutdown.
- Black-box test CLI persistence across separate invocations with isolated temporary data
  directories and stable JSON output.
- Property-test state-machine command sequences to ensure terminal jobs never restart implicitly and
  active count never exceeds the configured limit.

## Acceptance Criteria

- Restarting during every active stage produces an `interrupted` job and no automatic network or
  process activity.
- Explicit resume continues compatible partials or safely restarts without corrupting final output.
- Queue concurrency and lifecycle invariants hold under cancellation and simultaneous completion.
- Database migrations and corruption errors are actionable and never erase user data silently.
- All persistence, recovery, concurrency, CLI integration, and root quality gates pass.

## Decisions and Deviations

Record any accepted deviation here before code depends on it.

## Completion Evidence

- Completed at:
- Implementation commits:
- Verification commands and results:
