import process from 'node:process';

import { svelte } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vitest/config';

const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  clearScreen: false,
  plugins: [svelte()],
  resolve: {
    conditions: ['browser'],
  },
  test: {
    environment: 'jsdom',
  },
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
