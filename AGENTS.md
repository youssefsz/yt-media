# Repository Instructions for AI Agents

## Mission

Build a reliable, local-first media application with a reusable Rust engine, a command-line
interface, and a native Tauri desktop application. Optimize for long-term correctness,
maintainability, security, and clear ownership boundaries. Do not trade architecture for short-term
speed.

The repository may be private during development, but changes must be suitable for eventual
open-source review.

## Repository Map

```text
apps/
  cli/                  Rust CLI binary
  desktop/              Svelte UI and Tauri desktop shell
crates/
  engine/               Reusable, headless Rust engine
docs/
  architecture.md       Dependency rules and intended data flow
  design/               Approved UI reference and validation guidance
plans/
  README.md             Authoritative milestone registry and lifecycle
```

All Rust packages belong to the root Cargo workspace. The desktop frontend belongs to the root
pnpm workspace.

## Plan Lifecycle

Before implementing product work, read `plans/README.md` and inspect `plans/active/`.

- The plan registry and each plan's frontmatter are authoritative. Do not rely on chat memory to
  decide what is active or complete.
- Continue the single `in-progress` plan when one exists. Do not start or combine another milestone
  unless the user explicitly changes scope and the registry is updated first.
- Start only a `ready` plan whose dependencies are `completed`. Follow the documented move, status,
  timestamp, and commit workflow in `plans/README.md`.
- Implement only the active plan's objective and acceptance criteria. Record a material deviation
  in its Decisions and Deviations section before code depends on it.
- Checked boxes do not prove completion. Run the required tests, commit the implementation, record
  commit hashes and verification evidence, move the plan to `plans/completed/`, and unlock only its
  declared successor.
- Completed plans are historical records. Do not rewrite them except to correct factual
  documentation errors.
- If plan state, Git state, or implementation state disagree, stop product work and reconcile them
  explicitly rather than guessing.

## Non-Negotiable Architecture

- Dependency direction is `desktop -> engine` and `CLI -> engine`. The engine must never depend on
  either application.
- Business rules, media analysis, format selection, queue behavior, progress semantics, output
  naming, and conversion orchestration belong in the engine.
- Tauri commands are thin adapters. They validate transport-level input, call the engine, and map
  typed results to IPC-safe contracts.
- CLI code owns only argument parsing, terminal presentation, signals, and exit codes.
- Do not duplicate product logic between CLI and desktop.
- Keep the engine independent of Tauri, Svelte, browser APIs, terminal styling, and OS window APIs.
- Prefer explicit modules and typed interfaces over global state, service locators, or hidden
  singletons.
- Add a new crate only when an independently testable responsibility and dependency boundary
  exists. Do not create empty `domain`, `ports`, `adapters`, or `infrastructure` crates for
  appearance.
- The product is local-first. Do not introduce a hosted backend or telemetry without an explicit
  product decision.

## Rust Standards

- Use stable Rust, edition 2024, and the workspace lints.
- `unsafe` code is forbidden unless a narrowly scoped exception is documented and explicitly
  approved. Prefer safe APIs.
- Do not use `unwrap`, `expect`, `panic!`, `todo!`, or `unimplemented!` in production paths.
- Libraries expose typed errors. Preserve sources and actionable context; do not reduce failures to
  unstructured strings.
- Model state with enums and newtypes rather than boolean combinations or magic strings.
- Treat cancellation, partial files, cleanup, retries, and process termination as first-class
  behavior.
- Never block the UI thread with process or filesystem work. Use bounded concurrency and explicit
  ownership for long-running jobs.
- Avoid speculative abstractions and speculative dependencies. A dependency must serve current
  code, be actively maintained, and have compatible licensing.
- Public APIs require rustdoc. Non-obvious invariants and safety decisions require comments that
  explain why, not what the code already says.
- Keep functions focused and modules cohesive. Remove obsolete paths instead of leaving commented
  code, compatibility aliases, or dead feature flags.

## External Process and Media Boundaries

- `yt-dlp`, FFmpeg, FFprobe, and any JavaScript runtime are external adapters behind engine-owned
  interfaces.
- Never construct shell command strings from user input. Pass validated arguments directly to the
  process API.
- Do not parse unstable human-oriented console output. Use documented machine-readable output and
  versioned parsing contracts.
- Pin and verify distributed sidecars. Record their versions, checksums, licenses, and build
  provenance.
- Sanitize output names and constrain filesystem access to explicit user-selected destinations.
- Progress must distinguish analyzing, downloading, merging, converting, completed, cancelled, and
  failed states.
- Child processes must be cancellable and must not outlive their owning job or the application.

## TypeScript and Svelte Standards

- Keep TypeScript strict. Do not use `any`, unchecked casts, or suppression comments as shortcuts.
- UI components render state and emit user intent; they do not own download or conversion rules.
- Keep IPC contracts centralized and typed. Validate untrusted data at the Rust boundary.
- Prefer small, accessible components and explicit state transitions over monolithic pages.
- Use semantic HTML, full keyboard operation, visible focus, sufficient contrast, reduced-motion
  support, and meaningful accessible names.
- Use design tokens for repeated color, spacing, type, radius, and motion values. Avoid scattered
  magic values.
- Animation must communicate state or continuity. Keep it subtle and interruptible; never delay an
  operation for decoration.

## Native Desktop and UI Reference

The approved reference is `docs/design/final-ui-reference.png`. Before changing the product UI,
open and inspect that image and read `docs/design/README.md`.

- The reference governs the application content region: information hierarchy, density, graphite
  palette, restrained amber accent, sidebar, output panel, format table, and transfer shelf.
- Use normal Tauri system decorations. Never draw fake traffic lights, Windows caption buttons, or
  Linux window-manager controls in the web UI.
- macOS, Windows, and Linux own their title bars, menus, system controls, shadows, and platform
  behaviors. Linux appearance may differ by desktop environment.
- The URL field includes an explicit integrated `Analyze` action. Enter triggers the same action.
- Do not imitate the generated mockup blindly where real data, accessibility, or platform behavior
  requires a better solution.
- Avoid generic AI-dashboard styling: no gratuitous gradients, glow, glassmorphism, excessive
  cards, pill-shaped everything, giant headings, decorative charts, or flashy motion.

After a UI change:

1. Run the desktop app and inspect the rendered result, not only source code.
2. Compare it with the reference image again.
3. Validate at minimum `960x640`, default `1280x800`, and large `1600x1000` content sizes.
4. Check overflow, truncation, focus order, keyboard use, hover/pressed/disabled states, loading,
   error, empty, and reduced-motion behavior.
5. Inspect on every affected operating system when platform chrome or native integration changes.
6. Keep the content pixel-faithful where practical while preserving native platform behavior and
   responsive usability.

## Testing and Quality Gates

Tests follow the dependency boundary:

- Engine: deterministic unit tests and adapter contract tests.
- CLI: argument, exit-code, cancellation, and machine-readable output tests.
- Desktop Rust: command mapping and capability tests.
- Svelte: component behavior and accessibility tests.
- End-to-end: critical analyze-to-download flows with controlled fixtures, never live third-party
  media in routine CI.

Before considering work complete, run:

```text
pnpm format:check
pnpm lint
pnpm check
pnpm test
pnpm build
```

Run narrower checks during development, then the complete relevant set before handoff. Do not
disable a lint or test merely to make CI pass; address the cause or document a narrowly scoped,
reviewable exception.

## Change Discipline

- Read surrounding code and documentation before editing.
- Keep each change focused. Separate refactors from behavior changes when practical.
- Add tests with behavior. Add or update an architecture decision record for lasting changes to
  boundaries, persistence, process management, security, distribution, or public interfaces.
- Preserve user changes and avoid unrelated rewrites.
- Do not commit generated build output, local secrets, downloaded media, or external binaries.
- No dead code, abandoned TODOs, temporary aliases, demo features, or commented-out
  implementations may remain in a finished change.
- Update README, architecture, design guidance, and commands whenever behavior or structure makes
  them inaccurate.

## Git Standards

- When creating a branch, use a work-type prefix followed by a short, descriptive kebab-case name.
  Use `feat/<name>` for features and `fix/<name>` for bug fixes. Use an appropriate equivalent for
  other work, such as `docs/<name>`, `refactor/<name>`, `test/<name>`, or `chore/<name>`.
- Use clear, imperative commit subjects.
- Keep commits reviewable and leave the repository passing its quality gates.
- Do not rewrite shared history, force-push, or publish releases without explicit authorization.
- Do not add a software license or claim third-party license compatibility without an explicit
  project decision and dependency review.
