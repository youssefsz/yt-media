import { rm } from 'node:fs/promises';
import { URL } from 'node:url';

import { run } from '@tauri-apps/cli';

await run(['icon', 'src-tauri/icon-source.svg']);

const iconDirectory = new URL('../src-tauri/icons/', import.meta.url);

// The Tauri generator also emits mobile assets. This repository ships a desktop app only, so
// remove those unused outputs while retaining the Windows, macOS, and Linux icon formats.
await Promise.all(
  ['android', 'ios'].map((platform) =>
    rm(new URL(`${platform}/`, iconDirectory), {
      force: true,
      recursive: true,
    }),
  ),
);
