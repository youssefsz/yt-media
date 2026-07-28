# Contributing

Thank you for improving YT Media. The repository is being developed privately before its planned
open-source release, but all work should already meet public-review standards.

## Before Editing

1. Read `AGENTS.md`.
2. Read `docs/architecture.md` for dependency boundaries.
3. For UI work, inspect `docs/design/final-ui-reference.png` and read `docs/design/README.md`.
4. Keep the change focused and avoid unrelated cleanup.

## Development

Install dependencies:

```bash
pnpm install
```

Run the relevant checks while working, then run the full quality gate before handoff:

```bash
pnpm format:check
pnpm lint
pnpm check
pnpm test
pnpm build
```

Windows contributors must use the MSVC Rust toolchain required by Tauri.

## Pull Requests

- Explain the user-visible outcome and architectural impact.
- Include tests for behavior changes.
- Include screenshots for UI changes at small, default, and large window sizes.
- Call out dependency additions and their purpose.
- Keep generated output, downloaded media, sidecar binaries, and secrets out of commits.
