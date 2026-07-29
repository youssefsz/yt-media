# Desktop IPC v1

Plan 05 exposes one versioned desktop boundary. Rust types in
`apps/desktop/src-tauri/src/ipc.rs` are authoritative; `pnpm ipc:generate` writes the checked-in
TypeScript declarations and `pnpm ipc:check` verifies drift.

## Commands

| Command              | Purpose                                                   |
| -------------------- | --------------------------------------------------------- |
| `bootstrap`          | Recovered jobs, settings, tool status, and event boundary |
| `analyze`            | Analyze one validated public video URL                    |
| `enqueue`            | Persist and explicitly schedule one normalized job        |
| `list_jobs`          | List all durable jobs                                     |
| `get_job`            | Read one UUIDv7 job                                       |
| `pause_job`          | Pause queued or active work                               |
| `resume_job`         | Explicitly resume paused or interrupted work              |
| `cancel_job`         | Cancel work and clean only engine-owned paths             |
| `retry_job`          | Explicitly retry failed or cancelled work                 |
| `list_history`       | List terminal history                                     |
| `delete_history`     | Delete completed history without deleting output          |
| `read_settings`      | Read engine settings                                      |
| `update_settings`    | Validate and persist an engine settings patch             |
| `choose_destination` | Open the native folder picker                             |
| `reveal_output`      | Reveal an engine-validated completed output               |
| `tool_status`        | Return current verified tool status                       |

Every failure uses a stable `IpcErrorCodeDto`, a bounded user-facing message, and optional explicit
safe details. Engine error chains, tool output, database paths, and non-user-facing paths remain in
native diagnostics.

## Event reconnect

The native shell emits `job-event-v1`. Each envelope contains schema version `1`, a lossless
decimal sequence string, UUIDv7 job ID, timestamp, state, progress, optional terminal result or
error, and optional transient activity.

The typed client:

1. installs the event listener;
2. requests `bootstrap`;
3. renders that authoritative snapshot;
4. discards buffered events through `last_event_sequence`; and
5. consumes only later sequences.

If a listener lags or reconnects, it repeats this process. It never reconstructs authoritative
queue state from an incomplete event stream.
