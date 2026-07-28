# 0002: Normalize Analysis Behind a Versioned Engine Contract

- Status: Accepted
- Date: 2026-07-28

## Context

yt-dlp exposes a large, evolving JSON document whose strings, collections, numeric values, and
format records originate outside the application. The CLI and future desktop adapter need the same
stable interpretation without depending on yt-dlp field layout or reproducing selection policy.
Analysis also needs to remain cancellable through the process ownership boundary established by
ADR 0001.

## Decision

- The engine validates and canonicalizes a bounded single-video YouTube URL before any process is
  started.
- A private yt-dlp adapter invokes explicit, identity-checked yt-dlp, FFmpeg, and Deno paths through
  the existing `ProcessRunner`; it uses isolated configuration, no cookies, no playlist traversal,
  simulation, and one bounded JSON document.
- Raw extractor records are private. Unknown JSON fields are ignored, while required structure,
  collections, strings, dates, numeric ranges, content availability, and stream records are
  validated before conversion to public types.
- The public model uses newtypes and enums for media identity, duration, format identity, codecs,
  containers, output kinds, and compatibility work. MP3 and MP4 selection is deterministic and
  engine-owned.
- The CLI serializes a schema-versioned wrapper around the public engine model. That schema is
  stable within version `1` even if yt-dlp adds or reorders fields.

## Alternatives Considered

- Exposing yt-dlp JSON directly was rejected because it would make every application adapter depend
  on an unstable external contract.
- Selecting formats in the CLI was rejected because desktop integration would duplicate product
  rules.
- Following redirects during URL validation was rejected because it would expand the trust boundary
  before the engine has established a supported media identity.
- Parsing human-oriented yt-dlp console output was rejected because it is not a versioned protocol.

## Consequences

New yt-dlp fields do not affect consumers until the engine deliberately adopts them. Inputs outside
the v1 public, on-demand, single-video boundary fail with typed errors. The engine must maintain
fixture coverage whenever its public schema, bounds, or deterministic selection policy changes.
