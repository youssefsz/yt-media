import process from 'node:process';

import { svelte } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vite';

const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  clearScreen: false,
  plugins: [svelte()],
  server: {
    host: host ?? false,
    port: 1420,
    strictPort: true,
    hmr:
      host === undefined
        ? undefined
        : {
            host,
            port: 1421,
            protocol: 'ws',
          },
    watch: {
      ignored: ['**/src-tauri/**'],
    },
  },
});
