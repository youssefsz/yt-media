import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { BootstrapStateDto, JobEventEnvelopeDto } from './generated';
import { activeActions, activeJobFixture, bootstrapFixture } from '../testing/fixtures';

type EventCallback = (event: { payload: JobEventEnvelopeDto }) => void;

interface MockState {
  invoke: ReturnType<typeof vi.fn>;
  listen: ReturnType<typeof vi.fn>;
  unlisten: ReturnType<typeof vi.fn>;
  callback: EventCallback | undefined;
}

const mocks = vi.hoisted<MockState>(() => ({
  invoke: vi.fn(),
  listen: vi.fn(),
  unlisten: vi.fn(),
  callback: undefined,
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: mocks.invoke,
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: mocks.listen,
}));

import { cancelAnalysis, connectJobEvents } from './client';

const eventFixture = (sequence: string): JobEventEnvelopeDto => ({
  schema_version: 1,
  sequence,
  job_id: activeJobFixture().id,
  timestamp_ms: (1_785_312_020_000n + BigInt(sequence)).toString(),
  state: 'downloading',
  progress: activeJobFixture().progress,
  result: null,
  error: null,
  activity: null,
  is_terminal: false,
  available_actions: activeActions,
});

describe('typed IPC event connection', () => {
  beforeEach(() => {
    mocks.invoke.mockReset();
    mocks.listen.mockReset();
    mocks.unlisten.mockReset();
    mocks.callback = undefined;
    mocks.listen.mockImplementation((_name: string, callback: EventCallback) => {
      mocks.callback = callback;
      return Promise.resolve(mocks.unlisten);
    });
  });

  it('buffers through bootstrap and drops duplicate, stale, and out-of-order envelopes', async () => {
    let resolveBootstrap: ((snapshot: BootstrapStateDto) => void) | undefined;
    mocks.invoke.mockImplementation(
      () =>
        new Promise<BootstrapStateDto>((resolve) => {
          resolveBootstrap = resolve;
        }),
    );
    const snapshots: BootstrapStateDto[] = [];
    const events: JobEventEnvelopeDto[] = [];

    const connection = connectJobEvents(
      (snapshot) => snapshots.push(snapshot),
      (event) => events.push(event),
    );
    await vi.waitFor(() => {
      expect(mocks.callback).toBeDefined();
    });
    mocks.callback?.({ payload: eventFixture('5') });
    resolveBootstrap?.({
      ...bootstrapFixture(),
      last_event_sequence: '2',
    });
    const disconnect = await connection;

    mocks.callback?.({ payload: eventFixture('5') });
    mocks.callback?.({ payload: eventFixture('4') });
    mocks.callback?.({ payload: eventFixture('6') });

    expect(snapshots).toHaveLength(1);
    expect(events.map((event) => event.sequence)).toEqual(['5', '6']);
    disconnect();
    expect(mocks.unlisten).toHaveBeenCalledOnce();
  });

  it('unlistens when bootstrap fails', async () => {
    mocks.invoke.mockRejectedValue(new Error('fixture bootstrap failure'));

    await expect(
      connectJobEvents(
        () => undefined,
        () => undefined,
      ),
    ).rejects.toThrow('fixture bootstrap failure');
    expect(mocks.unlisten).toHaveBeenCalledOnce();
  });

  it('invokes the explicit native analysis cancellation command', async () => {
    mocks.invoke.mockResolvedValue({ schema_version: 1 });

    await expect(cancelAnalysis()).resolves.toEqual({ schema_version: 1 });
    expect(mocks.invoke).toHaveBeenCalledWith('cancel_analysis');
  });
});
