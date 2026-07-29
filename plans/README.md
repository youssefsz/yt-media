# Implementation Plans

This directory is the repository's authoritative implementation roadmap. It is designed to make
work resumable by humans and AI agents without relying on chat history.

## Required Reading Order

Before implementing product work:

1. Read the root `AGENTS.md`.
2. Read this registry.
3. Inspect `plans/active/`.
4. Read the complete active plan, or the first `ready` plan when no plan is active.
5. Verify the plan's dependencies and the current Git state before editing.

Do not start a later milestone while another plan is active. An agent may identify the next ready
plan, but must not begin it unless the user asked for that plan to be implemented.

## Status Definitions

- `draft`: decisions are missing; implementation must not start.
- `ready`: decision-complete, dependencies satisfied, and available to start.
- `in-progress`: currently being implemented. Only one plan may normally have this status.
- `blocked`: decision-complete but waiting for listed dependencies or an explicit blocker.
- `completed`: acceptance criteria and quality gates passed, with implementation commits recorded.
- `superseded`: deliberately replaced by another identified plan.

The registry and plan frontmatter must agree. If they differ, stop product work and repair the plan
state first.

## Plan Registry

| ID  | Milestone                                        | Status        | Depends on | Plan                                                     |
| --- | ------------------------------------------------ | ------------- | ---------- | -------------------------------------------------------- |
| 01  | Toolchain and engine foundations                 | `completed`   | —          | [Plan 01](completed/01-toolchain-engine-foundations.md)  |
| 02  | Analysis engine and CLI slice                    | `completed`   | 01         | [Plan 02](completed/02-analysis-cli-slice.md)            |
| 03  | Download and conversion CLI slice                | `completed`   | 02         | [Plan 03](completed/03-download-conversion-cli-slice.md) |
| 04  | Persistent jobs, recovery, and history           | `completed`   | 03         | [Plan 04](completed/04-persistent-jobs-recovery.md)      |
| 05  | Desktop integration and typed IPC                | `completed`   | 04         | [Plan 05](completed/05-desktop-integration.md)           |
| 06  | Desktop UI, accessibility, and motion            | `in-progress` | 05         | [Plan 06](active/06-desktop-ui.md)                       |
| 07  | Verified tool updates and cross-platform release | `blocked`     | 06         | [Plan 07](backlog/07-updates-packaging-release.md)       |

## Locked Product Decisions

- The first release supports public, on-demand YouTube videos only. It does not import browser
  cookies, accept cookie files, authenticate accounts, download private content, or download live
  streams and playlists.
- MP3 and MP4 are the supported outputs. MP4 prioritizes H.264 video and AAC audio for broad
  compatibility, remuxing when possible and transcoding only when required.
- Every desktop release works out of the box with bundled baseline copies of yt-dlp, FFmpeg,
  FFprobe, and Deno. Users are not required to install external tools.
- Tool updates are checksum-verified, signature-verified, installed outside the signed application
  bundle, health-checked before activation, and rollback-capable. A bundled baseline always
  remains available.
- Required release targets are Windows x64, Windows ARM64, macOS Intel, macOS Apple Silicon, Linux
  x64, and Linux ARM64.
- Queue, history, and settings persist. Unfinished jobs reopen as `interrupted` and require an
  explicit user resume; the application never starts consuming bandwidth merely because it opened.

## Starting a Plan

1. Confirm the working tree is clean and every dependency is `completed`.
2. Move the plan from `plans/backlog/` to `plans/active/`.
3. Change its status to `in-progress`, add `started_at` in UTC, and update this registry link and
   status.
4. Commit only that state transition with `docs(plans): start plan NN`.
5. Implement only the active plan. Record material deviations in its Decisions section before
   depending on them.

## Completing a Plan

1. Satisfy every acceptance criterion and run the plan-specific tests plus the root quality gates.
2. Commit the implementation in one or more reviewable commits.
3. Fill in the plan's completion evidence: UTC date, implementation commits, commands, and results.
4. Change its status to `completed`, move it to `plans/completed/`, and update this registry.
5. Change only the directly unlocked successor from `blocked` to `ready`.
6. Commit the closeout with `docs(plans): complete plan NN`.

Checked boxes alone never establish completion. A plan is completed only when its acceptance
criteria pass and the repository records the implementation commits and verification evidence.

## Required Quality Gates

Every milestone runs its narrower tests while developing and these complete gates before closeout:

```text
pnpm format:check
pnpm lint
pnpm check
pnpm test
pnpm build
```

Plans that change packaging, sidecars, native integration, or platform behavior must also run their
declared target-matrix checks.
