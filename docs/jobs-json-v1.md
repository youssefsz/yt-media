# Durable Jobs JSON v1

`yt-media jobs`, `history`, and `settings` use schema version `1`. Machine-readable list and
snapshot commands emit exactly one JSON document on stdout; diagnostics remain on stderr.

## Commands

```text
yt-media [--data-dir PATH] jobs list --json
yt-media [--data-dir PATH] jobs get <UUIDV7> --json
yt-media [--data-dir PATH] jobs pause <UUIDV7> --json
yt-media [--data-dir PATH] jobs cancel <UUIDV7> --json
yt-media [--data-dir PATH] history list --json
yt-media [--data-dir PATH] settings show --json
yt-media [--data-dir PATH] settings set ... --json
```

Resume and retry reuse the versioned download NDJSON stream from
[`download-ndjson-v1.md`](download-ndjson-v1.md) and finish with the same result document.

## Job snapshot

Every job snapshot contains:

- `id`: canonical UUIDv7 string;
- `request`: canonical URL, normalized MP3/MP4 quality, destination, and optional name;
- `state`: `queued`, `analyzing`, `downloading`, `merging`, `converting`, `paused`,
  `interrupted`, `completed`, `cancelled`, or `failed`;
- latest bounded `progress` and classified `error`, when present;
- Unix epoch millisecond creation, update, and optional completion times;
- `attempt_count`;
- verified `final_output`, when completed;
- `output_availability`: `not-applicable`, `present`, or `missing`;
- exact retained `owned_partial_paths`;
- `destination_available`.

`jobs list` and `history list` wrap snapshots in `{"schema_version":1,"jobs":[...]}`. `jobs get`,
pause, and cancel use `{"schema_version":1,"job":{...}}`.

Opening the database may change process-active states to `interrupted`, but never starts work.
Explicit resume or retry preserves the job ID and increments `attempt_count`.

## Settings snapshot

Settings use `{"schema_version":1,"settings":{...}}` with:

- `default_destination`, which may be `null`;
- `queue_concurrency`, from one through four;
- `update_preference`: `notify`, `automatic`, or `disabled`;
- `last_output`, using the normalized download output selection contract.

## Stability and safety

The database does not store raw tool output, credentials, cookies, or authentication material.
Paths exposed here are user-facing destinations, final outputs, or exact engine-owned partial paths.
Unknown fields may be added in a backward-compatible revision; consumers must use
`schema_version` before assuming field semantics.
