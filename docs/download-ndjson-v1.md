# Download NDJSON schema v1

`yt-media download --json` writes newline-delimited JSON to stdout. Every non-empty line is one
complete JSON object with `schema_version: 1`. No terminal decoration is written to stdout.
Actionable diagnostics may also be written to stderr, and the process exit code remains
authoritative.

The engine model is independent of yt-dlp and FFmpeg output. Raw progress templates, format IDs,
diagnostics, and FFprobe JSON are private adapter contracts.

## Event records

All engine events contain:

```json
{
  "schema_version": 1,
  "job_id": "1a2b-...",
  "sequence": 0,
  "event": "stage",
  "stage": "analyzing"
}
```

`sequence` is monotonic within one job. Event variants are:

- `stage`: `stage` is one of `analyzing`, `downloading`, `merging`, `converting`, `finalizing`,
  `completed`, `paused`, `cancelled`, or `failed`.
- `progress`: `progress` contains `stage`, `completed`, optional `total`, optional `percent`,
  optional `bytes_per_second`, and optional `eta_seconds`.
- `warning`: `message` is a bounded non-fatal adapter diagnostic.

A renderer that falls behind the bounded event stream receives a CLI-owned record:

```json
{
  "schema_version": 1,
  "event": "stream-lagged",
  "job_id": "1a2b-...",
  "dropped_events": 3
}
```

The completion handle and final record remain authoritative when coalescible progress events are
dropped.

## Final success

The final line on success is:

```json
{
  "schema_version": 1,
  "event": "result",
  "result": {
    "job_id": "1a2b-...",
    "path": "/selected/output/Example.mp3",
    "size_bytes": 123456,
    "output": {
      "format": "mp3",
      "quality": 192
    }
  }
}
```

For MP4, `output.format` is `mp4` and `quality` is the selected source height. `path` names a
non-empty output that passed final FFprobe validation and no-clobber publication.

## Final failure

Once a job has started, JSON mode emits a final error record before returning a non-zero status:

```json
{
  "schema_version": 1,
  "event": "result",
  "job_id": "1a2b-...",
  "error": {
    "code": 8,
    "message": "invalid output destination ..."
  }
}
```

Argument, URL, or tool-resolution failures that occur before a job exists have no job record and
are reported through stderr and the process exit code.

## Exit codes

| Code | Meaning                                                                    |
| ---: | -------------------------------------------------------------------------- |
|  `0` | Verified output published successfully                                     |
|  `2` | Invalid arguments, URL, format, or quality                                 |
|  `3` | Unsupported content                                                        |
|  `4` | A required tool is unavailable or has the wrong identity/version           |
|  `5` | Immediate re-analysis failed                                               |
|  `6` | Cancelled after owned process-tree cleanup                                 |
|  `7` | Download, conversion, bounded protocol, or final media validation failed   |
|  `8` | Destination, reservation, filesystem, collision, or publication failed     |
|  `9` | Paused after process cleanup; documented resumable partials may remain     |
| `70` | CLI runtime, completion channel, serialization, or standard-stream failure |

Schema v1 may add new optional fields or new warning text. It will not change the meaning or type of
an existing field. Incompatible record-shape or semantic changes require a new schema version.
