import { get } from 'svelte/store';
import { describe, expect, it, vi } from 'vitest';

import type { JobEventEnvelopeDto, JobStateDto } from '../ipc/generated';
import {
  activeActions,
  activeJobFixture,
  bootstrapFixture,
  failedActions,
  FixtureDesktopClient,
  analysisFixture,
  fixtureError,
  pausedActions,
  settingsFixture,
} from '../testing/fixtures';
import { createWorkspaceController } from './workspace';

const eventFixture = (sequence: string, state: JobStateDto): JobEventEnvelopeDto => ({
  schema_version: 1,
  sequence,
  job_id: activeJobFixture().id,
  timestamp_ms: (1_785_312_020_000n + BigInt(sequence)).toString(),
  state,
  progress:
    state === 'downloading'
      ? activeJobFixture().progress
      : state === 'paused'
        ? {
            stage: 'paused',
            completed: '111592000',
            total: '192400000',
            percent: 58,
            bytes_per_second: null,
            eta_seconds: null,
          }
        : null,
  result: null,
  error:
    state === 'failed' ? { class: 'process', message: 'Fixture process failed safely.' } : null,
  activity: null,
  is_terminal: state === 'failed' || state === 'cancelled' || state === 'completed',
  available_actions:
    state === 'paused' || state === 'interrupted'
      ? pausedActions
      : state === 'failed' || state === 'cancelled' || state === 'completed'
        ? failedActions
        : activeActions,
});

describe('workspace controller event reconciliation', () => {
  it('drops duplicate and out-of-order events while retaining the newest sequence', async () => {
    const client = new FixtureDesktopClient(bootstrapFixture([activeJobFixture()]));
    const controller = createWorkspaceController(client);
    await controller.connect();

    client.emit(eventFixture('42', 'paused'));
    client.emit(eventFixture('41', 'downloading'));
    client.emit(eventFixture('42', 'failed'));

    const state = get(controller.state);
    expect(state.jobs[0]?.state).toBe('paused');
    expect(state.lastEventSequence).toBe('42');
    controller.disconnect();
  });

  it('gives terminal state precedence over a later non-terminal cancellation race', async () => {
    const client = new FixtureDesktopClient(bootstrapFixture([activeJobFixture()]));
    const controller = createWorkspaceController(client);
    await controller.connect();

    client.emit(eventFixture('43', 'cancelled'));
    client.emit(eventFixture('44', 'downloading'));

    const state = get(controller.state);
    expect(state.jobs[0]?.state).toBe('cancelled');
    expect(state.lastEventSequence).toBe('44');
    controller.disconnect();
  });

  it('replaces local event state with an authoritative reconnect snapshot', async () => {
    const client = new FixtureDesktopClient(bootstrapFixture([activeJobFixture()]));
    const controller = createWorkspaceController(client);
    await controller.connect();
    client.emit(eventFixture('43', 'failed'));
    expect(get(controller.state).jobs[0]?.state).toBe('failed');

    client.snapshot = {
      ...client.snapshot,
      last_event_sequence: '51',
      jobs: [activeJobFixture({ state: 'queued', progress: null })],
    };
    await controller.reconnect();

    expect(get(controller.state).jobs[0]?.state).toBe('queued');
    expect(get(controller.state).lastEventSequence).toBe('51');
    controller.disconnect();
  });

  it('keeps command failures non-optimistic and announces the safe error', async () => {
    const client = new FixtureDesktopClient(bootstrapFixture([activeJobFixture()]));
    client.commandError = fixtureError('Cancellation lost a fixture race.');
    const controller = createWorkspaceController(client);
    await controller.connect();

    await controller.cancel(activeJobFixture().id);

    const state = get(controller.state);
    expect(state.jobs[0]?.state).toBe('downloading');
    expect(state.announcement).toBe('Cancellation lost a fixture race.');
    expect(state.busyActions).toHaveLength(0);
    controller.disconnect();
  });

  it('selects only a generated engine format and reports validation failures', async () => {
    const client = new FixtureDesktopClient(bootstrapFixture([]));
    const controller = createWorkspaceController(client);
    await controller.connect();

    await controller.analyze();
    expect(get(controller.state).analysis.status).toBe('error');
    expect(client.calls).not.toContain('analyze');

    controller.setUrl('https://www.youtube.com/watch?v=dQw4w9WgXcQ');
    await controller.analyze();
    const ready = get(controller.state);
    expect(ready.analysis.status).toBe('ready');
    expect(ready.draft.selectedOutputs).toEqual({
      mp3: { format: 'mp3', quality: 128 },
      mp4: { format: 'mp4', quality: 1080 },
    });
    controller.disconnect();
  });

  it('falls back to the first available format and remembers one selection per output tab', async () => {
    const snapshot = bootstrapFixture([]);
    const client = new FixtureDesktopClient({
      ...snapshot,
      settings: {
        ...settingsFixture,
        last_output: { format: 'mp4', quality: 1440 },
      },
    });
    const controller = createWorkspaceController(client);
    await controller.connect();
    controller.setUrl(analysisFixture.media.url);
    await controller.analyze();

    expect(get(controller.state).draft.selectedOutputs).toEqual({
      mp3: { format: 'mp3', quality: 128 },
      mp4: { format: 'mp4', quality: 1080 },
    });

    controller.selectOutput({ format: 'mp4', quality: 720 });
    controller.setOutputKind('mp3');
    controller.selectOutput({ format: 'mp3', quality: 192 });
    controller.setOutputKind('mp4');

    const ready = get(controller.state);
    expect(ready.draft.outputKind).toBe('mp4');
    expect(ready.draft.selectedOutputs).toEqual({
      mp3: { format: 'mp3', quality: 192 },
      mp4: { format: 'mp4', quality: 720 },
    });
    controller.disconnect();
  });

  it('cancels analysis on navigation and ignores its late completion', async () => {
    const client = new FixtureDesktopClient(bootstrapFixture([]));
    let resolveAnalysis: ((response: typeof analysisFixture) => void) | undefined;
    client.analysisPromise = new Promise((resolve) => {
      resolveAnalysis = resolve;
    });
    const controller = createWorkspaceController(client);
    await controller.connect();
    controller.setUrl(analysisFixture.media.url);

    const pending = controller.analyze();
    await vi.waitFor(() => {
      expect(get(controller.state).analysis.status).toBe('loading');
    });
    controller.navigate('queue');

    expect(get(controller.state).view).toBe('queue');
    expect(get(controller.state).analysis.status).toBe('idle');
    await vi.waitFor(() => {
      expect(client.calls).toContain('cancel-analysis');
    });

    resolveAnalysis?.(analysisFixture);
    await pending;
    expect(get(controller.state).view).toBe('queue');
    expect(get(controller.state).analysis.status).toBe('idle');

    controller.navigate('new-download');
    client.analysisPromise = Promise.resolve(analysisFixture);
    await controller.analyze();
    expect(get(controller.state).analysis.status).toBe('ready');
    controller.disconnect();
  });

  it('supports explicit analysis cancellation without surfacing a failure', async () => {
    const client = new FixtureDesktopClient(bootstrapFixture([]));
    let resolveAnalysis: ((response: typeof analysisFixture) => void) | undefined;
    client.analysisPromise = new Promise((resolve) => {
      resolveAnalysis = resolve;
    });
    const controller = createWorkspaceController(client);
    await controller.connect();
    controller.setUrl(analysisFixture.media.url);

    const pending = controller.analyze();
    await vi.waitFor(() => {
      expect(get(controller.state).analysis.status).toBe('loading');
    });
    controller.cancelAnalysis();
    expect(get(controller.state).analysis).toEqual({
      status: 'idle',
      media: null,
      error: null,
    });
    expect(get(controller.state).announcement).toBe('Video analysis stopped.');

    resolveAnalysis?.(analysisFixture);
    await pending;
    expect(get(controller.state).analysis.status).toBe('idle');
    controller.disconnect();
  });

  it('keeps the draft aligned with a custom destination and the system Downloads fallback', async () => {
    const client = new FixtureDesktopClient(bootstrapFixture([]));
    const controller = createWorkspaceController(client);
    await controller.connect();
    expect(get(controller.state).draft.destination).toBe(client.systemDownloadsDestination);

    client.destination = { path: 'D:\\Media' };
    await controller.chooseDefaultDestination();
    expect(get(controller.state).settings?.default_destination).toBe('D:\\Media');
    expect(get(controller.state).draft.destination).toBe('D:\\Media');

    await controller.clearDefaultDestination();
    expect(get(controller.state).settings?.default_destination).toBe(
      client.systemDownloadsDestination,
    );
    expect(get(controller.state).draft.destination).toBe(client.systemDownloadsDestination);
    controller.disconnect();
  });
});
