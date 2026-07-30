---
id: '06'
title: Desktop UI, accessibility, and motion
status: in-progress
depends_on:
  - '05'
unlocks:
  - '07'
started_at: '2026-07-29T09:12:50Z'
completed_at: null
implementation_commits: []
---

# Plan 06: Desktop UI, Accessibility, and Motion

## Objective

Implement the approved dark editorial desktop experience against the real typed IPC layer, with
responsive layouts, accessible interaction, restrained motion, and native platform chrome.

## Information Architecture

- Implement sidebar destinations for New Download, Queue, History, and Settings. Navigation changes
  content only; macOS, Windows, and Linux retain their native title bars and system controls.
- New Download states:
  - empty URL entry with integrated Analyze button and Enter shortcut
  - validation error
  - analyzing/loading
  - analyzed metadata with MP3/MP4 tabs and selectable compatible formats
  - output filename and native destination selection
  - enqueue/start action and actionable failure
- Add a global transfer shelf showing active/queued jobs with truthful engine stages, percent when
  known, transferred bytes, speed/ETA when supplied, and pause/resume/cancel controls.
- Queue shows ordered non-terminal jobs and recovery actions for interrupted work. History shows
  completed/failed/cancelled entries, missing-file state, retry, reveal, and explicit deletion.
- Settings covers default destination, concurrency from one through four, tool update preference,
  reduced-motion/system-theme behavior, and tool/version health. It does not expose arbitrary
  executable arguments.

## Visual and Interaction System

- Open `docs/design/final-ui-reference.png` before implementation and after every major UI pass.
- Establish tokens for graphite surfaces, neutral text, restrained amber accent, spacing, type,
  borders, radii, elevation, focus, and motion. Do not introduce generic dashboard cards,
  glassmorphism, glow, gratuitous gradients, or fake OS chrome.
- Use semantic controls, visible keyboard focus, logical focus restoration, accessible names and
  descriptions, status announcements, sufficient contrast, and no color-only status signals.
- Motion communicates continuity only: approximately 120–220 ms transitions, interruptible,
  transform/opacity where practical, and disabled under `prefers-reduced-motion`.
- At `960x640`, collapse secondary detail and convert the output column into a non-overlapping
  stacked region while preserving analyze, format selection, destination, and start controls. At
  `1280x800`, match the approved hierarchy. At `1600x1000`, constrain line lengths and content width
  instead of stretching controls.
- Long titles, localized numbers, large text, slow analysis, unknown sizes, unavailable qualities,
  and zero active jobs must remain usable without horizontal page scrolling.

## Frontend Architecture

- Keep feature state in explicit stores/controllers backed only by the generated IPC client. Svelte
  components render state and emit intent; they do not select formats or infer job transitions.
- Split reusable accessible primitives from feature components without creating a generic component
  library beyond current needs.
- Treat thumbnails and external metadata as untrusted: constrain URLs to expected protocols, provide
  fallbacks, prevent layout shifts, and never render supplied HTML.
- Reconcile event updates with authoritative snapshots by sequence number and job ID. Terminal state
  from the engine always wins over optimistic UI state.

## Tests and Visual QA

- Add Vitest, Svelte Testing Library, user-event, and axe-based accessibility checks. Cover keyboard
  navigation, disabled/loading behavior, focus restoration, announcements, and all named states.
- Add deterministic browser-level visual regression fixtures for New Download, Queue, History,
  Settings, active shelf, errors, and interrupted recovery at `960x640`, `1280x800`, and
  `1600x1000`, in normal and reduced-motion modes.
- Test IPC-controller behavior with generated typed fixtures, including out-of-order/duplicate
  events, reconnect snapshots, command failures, and cancellation races.
- Run native desktop smoke checks on Windows, macOS, and Linux and manually compare captured content
  regions to the approved reference. Validate native dialogs and system chrome on each OS.

## Acceptance Criteria

- All approved workflows are usable with mouse, keyboard, screen-reader semantics, and reduced
  motion at the three required sizes.
- The rendered content is visually faithful to the reference while preserving native platform
  decorations.
- No horizontal page scrolling, overlapping panels, inaccessible hidden actions, or fake progress
  appears in tested states.
- Component, accessibility, controller, visual regression, native smoke, and root quality gates
  pass.
- Captures for small, default, and large windows are attached to the plan completion evidence.

## Decisions and Deviations

- Native analyze-flow validation exposed a prerequisite engine compatibility defect: pinned
  `yt-dlp` legitimately reports zero for bitrate fields that do not apply to a format, while the
  Plan 02 normalizer rejected every present zero. Plan 06 includes the narrow parser correction
  required to exercise the real desktop workflow. Zero is treated as unavailable for scoring;
  negative, non-finite, and over-limit values remain invalid. No format-selection rule or broader
  engine boundary changes.
- Native navigation testing exposed two desktop integration defects hidden by frontend fixtures:
  JSON IPC delivered Rust 64-bit integers as JavaScript numbers although generated types claimed
  `bigint`, and analysis created a cancellation token with no cancellation owner. Plan 06 corrects
  output-only 64-bit IPC fields to lossless decimal strings, adds one thin cancel-analysis command,
  gives the service explicit ownership of the active analysis token, and rejects stale frontend
  completions by operation identity. Production resolver and engine analysis rules are unchanged.
- The desktop and CLI packages previously emitted the same `yt-media` binary into Cargo's shared
  target directory. Build order could therefore make CLI tests launch the Tauri application and
  hang indefinitely. The desktop binary is now uniquely named `yt-media-app`; its bundle and
  window retain the user-facing `YT Media` product name.
- The native shell resolves the operating system's Downloads directory and exposes it as the
  desktop's effective destination only when the engine has no persisted user override. Clearing a
  custom desktop destination therefore restores the native Downloads directory without storing a
  platform-specific path as an engine default. The folder picker is explicitly parented to the
  Tauri webview and its blocking native wait runs on a blocking worker, so selection and
  cancellation both return without freezing the UI. CLI settings semantics remain unchanged.
- The transfer shelf treats external progress as stage-local evidence, not an invented whole-job
  estimate. Split-stream downloads seed their aggregate denominator from the combined source-size
  estimate produced during analysis, then replace it with the sum of current yt-dlp totals after
  every source reports. Estimate-only progress is capped below completion, displayed percentage
  never moves backward when totals are revised, and genuinely unknown totals remain indeterminate.
  ETA uses aggregate remaining bytes and requires a short stable sample window; network metrics
  disappear outside the downloading stage. FFmpeg time units are never presented as bytes.
- Native Windows installation testing exposed visible console windows for startup health probes and
  media operations. The engine-owned process runner now adds the Win32 `CREATE_NO_WINDOW` creation
  flag to every grouped child process on Windows. The existing Job Object ownership, piped output,
  cancellation, and CLI presentation contracts are unchanged; macOS and Linux process spawning is
  unaffected.
- Installed-app pause/resume testing exposed a scheduler handoff race: a resume request could be
  durably requeued while the current scheduler was between its final empty check and clearing its
  running flag, losing the only wake-up and leaving the job queued. Scheduler kicks now carry a
  monotonic generation so an in-flight shutdown either retains ownership or hands off to a newly
  spawned scheduler. Active process ownership is also cleared before the paused state becomes
  observable, and integration coverage verifies that the resumed yt-dlp attempt sees its retained
  partial file.

Record any accepted deviation here before code depends on it.

## Completion Evidence

- Completed at:
- Implementation commits:
- UI capture paths:
- Verification commands and results:
