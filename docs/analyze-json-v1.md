# Analyze JSON Schema Version 1

`yt-media analyze <URL> --json [--tool-dir <PATH>]` writes exactly one compact JSON document to
stdout followed by one newline. Successful JSON mode writes no headings, colors, progress,
warnings, or logging to either terminal stream. Bounded yt-dlp warnings are data in
`media.warnings`.

The top-level contract is:

```json
{
  "schema_version": 1,
  "media": {
    "id": "dQw4w9WgXcQ",
    "url": "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
    "title": "Example video",
    "uploader": "Example channel",
    "duration": 212400,
    "view_count": 123456,
    "upload_date": "2026-07-01",
    "thumbnails": [
      {
        "url": "https://i.example.invalid/thumb.jpg",
        "width": 1280,
        "height": 720
      }
    ],
    "formats": [
      {
        "kind": "mp3",
        "bitrate_kbps": 128,
        "source": {
          "format_id": "140",
          "container": { "name": "m4a", "family": "m4a" },
          "video_codec": null,
          "audio_codec": { "name": "mp4a.40.2", "family": "aac" }
        }
      },
      {
        "kind": "mp4",
        "height": 1080,
        "width": 1920,
        "fps": 30.0,
        "estimated_size_bytes": 13500000,
        "video_source": {
          "format_id": "137",
          "container": { "name": "mp4", "family": "mp4" },
          "video_codec": { "name": "avc1.640028", "family": "h264" },
          "audio_codec": null
        },
        "audio_source": {
          "format_id": "140",
          "container": { "name": "m4a", "family": "m4a" },
          "video_codec": null,
          "audio_codec": { "name": "mp4a.40.2", "family": "aac" }
        },
        "compatibility": "merge"
      }
    ],
    "warnings": []
  }
}
```

## Fields

- `schema_version` is the integer `1`.
- `media.id` is the validated eleven-character YouTube video identity.
- `media.url` is the canonical HTTPS single-video watch URL.
- `media.title` is a required bounded string.
- `media.uploader` is a bounded string or `null`.
- `media.duration` is a positive integer count of milliseconds.
- `media.view_count` is a non-negative integer or `null`.
- `media.upload_date` is a valid `YYYY-MM-DD` string or `null`.
- `media.thumbnails` contains at most twenty validated HTTP(S) records. Width and height may be
  `null`.
- `media.formats` starts with MP3 choices in ascending bitrate order, followed by MP4 choices at
  distinct source heights in descending order. No synthetic or upscaled height is emitted.
- MP3 `bitrate_kbps` is one of `128`, `192`, `256`, or `320`. `source` identifies the
  audio-bearing input retained for later conversion; analysis does not create an MP3.
- MP4 `fps`, `width`, and `estimated_size_bytes` may be `null` when yt-dlp has no trustworthy
  value. Separate video and audio source IDs are always explicit.
- Codec `family` values are stable: video uses `h264`, `vp9`, `av1`, or `other`; audio uses `aac`,
  `opus`, `vorbis`, `mp3`, or `other`.
- Container `family` values are stable: `mp4`, `m4a`, `webm`, or `other`.
- MP4 `compatibility` is one of `none`, `merge`, `video-transcode`, `audio-transcode`, or
  `video-and-audio-transcode`. It describes required future work; Plan 02 does not perform it.
  When selected IDs differ, a transcode classification also implies combining those sources.
- `media.warnings` contains at most 32 bounded diagnostic lines from a successful yt-dlp run.

## Compatibility

Consumers must first require `schema_version == 1`. Within schema version 1:

- existing field names, value types, units, ordering rules, enum values, and nullability are stable;
- new optional object fields may be added, so consumers should ignore unknown fields;
- required-field removal, semantic changes, new required fields, or enum changes require a new
  schema version.

This is the application contract, not a copy of yt-dlp JSON. Raw extractor fields remain private to
the engine adapter.
