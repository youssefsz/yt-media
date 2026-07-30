# Desktop Design Reference

`final-ui-reference.png` is the approved visual direction for the desktop application's analyzed
video state.

## What the Reference Establishes

- Dark editorial visual language with matte graphite surfaces
- Restrained amber accent and readable neutral typography
- Sidebar destinations for New Download, Queue, History, and Settings
- Integrated URL field with an explicit Analyze action
- Video metadata, MP3/MP4 selection, and format rows
- Compact Output panel with editable filename and destination
- Expandable global transfer shelf with honest processing stages

The reference is not an implementation specification for real media data, and its generated
thumbnails and names are placeholders.

## Native Platform Rule

Use normal Tauri system decorations.

- macOS owns traffic lights and the global application menu.
- Windows owns caption buttons and system window behavior.
- Linux decorations come from the active desktop environment or window manager.

The Svelte content begins inside the native window. Never reproduce another platform's title bar,
caption buttons, or menus in the webview.

## Responsive Validation

The configured minimum window size is `960x640`; the default is `1280x800`. UI work must also be
reviewed at `1600x1000`.

At narrower sizes, preserve the primary workflow and progressively reduce secondary detail. Do not
allow the format table, Output panel, or transfer shelf to overlap, disappear without an
alternative, or create horizontal page scrolling.

## UI Review Checklist

Before editing UI, open the reference image. After editing:

1. Run the desktop app.
2. Capture and inspect small, default, and large windows.
3. Compare the content region with the reference again.
4. Check empty, analyzing, ready, downloading, merging, converting, completed, cancelled, and
   failed states.
5. Check keyboard-only navigation, visible focus, screen-reader names, contrast, text scaling, and
   reduced motion.
6. Validate affected native behavior on macOS, Windows, and representative Linux environments.

Aim for pixel-faithful hierarchy and rhythm, not fake cross-platform chrome or brittle fixed
coordinates.

## Brand Assets

The approved product mark is a separated `YT` monogram: an amber `Y` ending in a download arrow
beside an off-white `T`. The letters must retain visible negative space and must never overlap.

- `apps/desktop/src-tauri/icon-source.svg` is the square production master for native app icons.
- `apps/desktop/public/brand-mark.svg` is the transparent in-app mark used by the sidebar, startup
  surface, and browser favicon.
- Run `pnpm --filter @yt-media/desktop icons:generate` after changing the master. The command uses
  Tauri's icon generator for Windows ICO and Store assets, macOS ICNS, and Linux PNG sizes, then
  removes unused Android and iOS output because this repository ships a desktop application.
- Do not edit generated PNG, ICO, or ICNS files manually. Change the vector master and regenerate
  the complete set so every platform stays visually consistent.
