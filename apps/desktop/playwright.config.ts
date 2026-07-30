import { defineConfig, devices } from '@playwright/test';

export default defineConfig({
  testDir: './tests/visual',
  timeout: 30_000,
  expect: {
    timeout: 8_000,
    toHaveScreenshot: {
      animations: 'disabled',
      caret: 'hide',
      maxDiffPixelRatio: 0.015,
    },
  },
  fullyParallel: false,
  forbidOnly: true,
  retries: 0,
  reporter: 'line',
  snapshotPathTemplate: '../../docs/design/plan-06-captures/{arg}{ext}',
  use: {
    ...devices['Desktop Chrome'],
    baseURL: 'http://127.0.0.1:1420',
    colorScheme: 'dark',
    locale: 'en-US',
    timezoneId: 'UTC',
  },
  webServer: {
    command: 'pnpm dev --host 127.0.0.1',
    url: 'http://127.0.0.1:1420/visual.html',
    reuseExistingServer: false,
    timeout: 120_000,
  },
});
