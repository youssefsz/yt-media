import { cleanup, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

import App from './App.svelte';

vi.mock('./lib/ipc/client', () => ({
  connectJobEvents: vi.fn(
    (
      onSnapshot: (snapshot: {
        schema_version: number;
        health: 'healthy';
        last_event_sequence: string;
        jobs: [];
        settings: null;
        tools: Array<{ tool: 'yt-dlp'; ready: true; source: 'bundled-baseline'; message: null }>;
        diagnostic: null;
      }) => void,
    ) => {
      onSnapshot({
        schema_version: 1,
        health: 'healthy',
        last_event_sequence: '0',
        jobs: [],
        settings: null,
        tools: [{ tool: 'yt-dlp', ready: true, source: 'bundled-baseline', message: null }],
        diagnostic: null,
      });
      return Promise.resolve(() => undefined);
    },
  ),
  isIpcError: vi.fn(() => false),
}));

afterEach(() => {
  cleanup();
});

describe('bootstrap scaffold', () => {
  it('announces recovered native state with semantic diagnostics', async () => {
    render(App);

    expect(await screen.findByRole('heading', { level: 1, name: 'YT Media' })).toBeDefined();
    expect(await screen.findByText('Desktop integration ready')).toBeDefined();
    expect(screen.getByText('Recovered jobs')).toBeDefined();
    expect(screen.getByText('Verified tools')).toBeDefined();
  });
});
