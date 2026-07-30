import { writable, type Readable } from 'svelte/store';

import { desktopClient, isIpcError, type DesktopClient } from '../ipc/client';
import type {
  BootstrapStateDto,
  DefaultDestinationUpdateDto,
  FormatOptionDto,
  IpcErrorDto,
  JobDto,
  JobEventEnvelopeDto,
  MediaInfoDto,
  OutputSelectionDto,
  SettingsDto,
  ToolStatusDto,
  UpdatePreferenceDto,
  UpdateSettingsRequestDto,
} from '../ipc/generated';

export type WorkspaceView = 'new-download' | 'queue' | 'history' | 'settings';
export type OutputKind = OutputSelectionDto['format'];

type ConnectionState = 'loading' | 'ready' | 'error';
type AnalysisStatus = 'idle' | 'loading' | 'ready' | 'error';
type SettingsStatus = 'idle' | 'saving' | 'error';

export interface AnalysisState {
  status: AnalysisStatus;
  media: MediaInfoDto | null;
  error: IpcErrorDto | null;
}

export interface DownloadDraft {
  url: string;
  destination: string;
  name: string;
  outputKind: OutputKind;
  selectedOutputs: Record<OutputKind, OutputSelectionDto | null>;
}

export interface WorkspaceState {
  connection: ConnectionState;
  connectionError: IpcErrorDto | null;
  view: WorkspaceView;
  jobs: JobDto[];
  settings: SettingsDto | null;
  tools: ToolStatusDto[];
  diagnostic: IpcErrorDto | null;
  lastEventSequence: string;
  analysis: AnalysisState;
  draft: DownloadDraft;
  settingsStatus: SettingsStatus;
  settingsError: IpcErrorDto | null;
  busyActions: readonly string[];
  shelfExpanded: boolean;
  announcement: string;
}

const internalError = (message: string): IpcErrorDto => ({
  code: 'internal',
  message,
  details: [],
});

const normalizeError = (value: unknown, fallback: string): IpcErrorDto =>
  isIpcError(value) ? value : internalError(fallback);

const outputMatchesFormat = (output: OutputSelectionDto, format: FormatOptionDto): boolean =>
  output.format === format.kind &&
  output.quality === (format.kind === 'mp3' ? format.bitrate_kbps : format.height);

const outputFromFormat = (format: FormatOptionDto): OutputSelectionDto =>
  format.kind === 'mp3'
    ? { format: 'mp3', quality: format.bitrate_kbps }
    : { format: 'mp4', quality: format.height };

const selectedFromSettings = (
  settings: SettingsDto | null,
  media: MediaInfoDto,
  kind: OutputKind,
): OutputSelectionDto | null => {
  const candidate = settings?.last_output;
  if (
    candidate === undefined ||
    candidate.format !== kind ||
    !media.formats.some((format) => outputMatchesFormat(candidate, format))
  ) {
    return null;
  }
  return candidate;
};

const defaultOutput = (
  settings: SettingsDto | null,
  media: MediaInfoDto,
  kind: OutputKind,
): OutputSelectionDto | null => {
  const saved = selectedFromSettings(settings, media, kind);
  if (saved !== null) {
    return saved;
  }
  const firstAvailable = media.formats.find((format) => format.kind === kind);
  return firstAvailable === undefined ? null : outputFromFormat(firstAvailable);
};

const replaceJob = (
  jobs: readonly JobDto[],
  incoming: JobDto,
  allowTerminalRestart = false,
): JobDto[] => {
  const existing = jobs.find((job) => job.id === incoming.id);
  if (existing?.is_terminal === true && !incoming.is_terminal && !allowTerminalRestart) {
    return [...jobs];
  }
  const updated = jobs.map((job) => (job.id === incoming.id ? incoming : job));
  return existing === undefined ? [...updated, incoming] : updated;
};

const initialState = (): WorkspaceState => ({
  connection: 'loading',
  connectionError: null,
  view: 'new-download',
  jobs: [],
  settings: null,
  tools: [],
  diagnostic: null,
  lastEventSequence: '0',
  analysis: {
    status: 'idle',
    media: null,
    error: null,
  },
  draft: {
    url: '',
    destination: '',
    name: '',
    outputKind: 'mp4',
    selectedOutputs: {
      mp3: null,
      mp4: null,
    },
  },
  settingsStatus: 'idle',
  settingsError: null,
  busyActions: [],
  shelfExpanded: true,
  announcement: 'Starting YT Media.',
});

export interface WorkspaceController {
  readonly state: Readable<WorkspaceState>;
  connect(): Promise<void>;
  disconnect(): void;
  reconnect(): Promise<void>;
  navigate(view: WorkspaceView): void;
  setUrl(url: string): void;
  setName(name: string): void;
  setOutputKind(kind: OutputKind): void;
  selectOutput(output: OutputSelectionDto): void;
  analyze(): Promise<void>;
  cancelAnalysis(): void;
  chooseDestination(): Promise<void>;
  enqueue(): Promise<void>;
  pause(jobId: string): Promise<void>;
  resume(jobId: string): Promise<void>;
  cancel(jobId: string): Promise<void>;
  retry(jobId: string): Promise<void>;
  reveal(jobId: string): Promise<void>;
  deleteHistory(jobId: string): Promise<void>;
  setConcurrency(value: number): Promise<void>;
  setUpdatePreference(value: UpdatePreferenceDto): Promise<void>;
  chooseDefaultDestination(): Promise<void>;
  clearDefaultDestination(): Promise<void>;
  refreshTools(): Promise<void>;
  checkForToolUpdates(): Promise<void>;
  resetToolUpdates(): Promise<void>;
  toggleShelf(): void;
}

export const createWorkspaceController = (
  client: DesktopClient = desktopClient,
): WorkspaceController => {
  let current = initialState();
  const store = writable(current);
  let unlisten: (() => void) | undefined;
  let disposed = false;
  let lastSequence = 0n;
  let analysisGeneration = 0;
  let analysisCancellation = Promise.resolve();
  const jobSequences = new Map<string, bigint>();

  const update = (change: (state: WorkspaceState) => WorkspaceState): void => {
    current = change(current);
    store.set(current);
  };

  const setBusy = (key: string, busy: boolean): void => {
    update((state) => ({
      ...state,
      busyActions: busy
        ? state.busyActions.includes(key)
          ? state.busyActions
          : [...state.busyActions, key]
        : state.busyActions.filter((value) => value !== key),
    }));
  };

  const applySnapshot = (snapshot: BootstrapStateDto): void => {
    lastSequence = BigInt(snapshot.last_event_sequence);
    jobSequences.clear();
    for (const job of snapshot.jobs) {
      jobSequences.set(job.id, lastSequence);
    }
    update((state) => ({
      ...state,
      connection: 'ready',
      connectionError: null,
      jobs: snapshot.jobs,
      settings: snapshot.settings,
      tools: snapshot.tools,
      diagnostic: snapshot.diagnostic,
      lastEventSequence: snapshot.last_event_sequence,
      draft: {
        ...state.draft,
        destination: state.draft.destination || snapshot.settings?.default_destination || '',
        outputKind: snapshot.settings?.last_output.format ?? state.draft.outputKind,
      },
      announcement:
        snapshot.health === 'healthy'
          ? 'Local jobs and media tools are ready.'
          : (snapshot.diagnostic?.message ??
            'Local data is ready, but some media tools need attention.'),
    }));
  };

  const refreshUnknownJob = async (event: JobEventEnvelopeDto, sequence: bigint): Promise<void> => {
    try {
      const job = await client.getJob({ job_id: event.job_id });
      if (disposed || jobSequences.get(event.job_id) !== sequence) {
        return;
      }
      update((state) => ({ ...state, jobs: replaceJob(state.jobs, job) }));
    } catch {
      // A later snapshot will reconcile jobs that disappear between event delivery and lookup.
    }
  };

  const applyEvent = (event: JobEventEnvelopeDto): void => {
    const sequence = BigInt(event.sequence);
    if (sequence <= lastSequence) {
      return;
    }
    lastSequence = sequence;
    jobSequences.set(event.job_id, sequence);
    const existing = current.jobs.find((job) => job.id === event.job_id);
    if (existing === undefined) {
      update((state) => ({
        ...state,
        lastEventSequence: event.sequence,
      }));
      void refreshUnknownJob(event, sequence);
      return;
    }
    if (existing.is_terminal && !event.is_terminal) {
      update((state) => ({
        ...state,
        lastEventSequence: event.sequence,
      }));
      return;
    }
    const updated: JobDto = {
      ...existing,
      state: event.state,
      progress: event.progress,
      error: event.error,
      updated_at_ms: event.timestamp_ms,
      final_output: event.result ?? existing.final_output,
      output_availability: event.result === null ? existing.output_availability : 'present',
      is_terminal: event.is_terminal,
      available_actions: event.available_actions,
    };
    update((state) => ({
      ...state,
      jobs: replaceJob(state.jobs, updated),
      lastEventSequence: event.sequence,
      announcement:
        event.activity?.event === 'warning'
          ? event.activity.message
          : `${updated.name ?? 'Job'} is ${event.state}.`,
    }));
  };

  const connect = async (): Promise<void> => {
    disposed = false;
    update((state) => ({
      ...state,
      connection: 'loading',
      connectionError: null,
      announcement: 'Recovering local jobs and checking media tools.',
    }));
    try {
      unlisten = await client.connectJobEvents(applySnapshot, applyEvent);
    } catch (error: unknown) {
      if (disposed) {
        return;
      }
      const connectionError = normalizeError(
        error,
        'The native application service did not respond. Restart YT Media.',
      );
      update((state) => ({
        ...state,
        connection: 'error',
        connectionError,
        announcement: connectionError.message,
      }));
    }
  };

  const requestAnalysisCancellation = (): void => {
    analysisCancellation = analysisCancellation
      .then(async () => {
        await client.cancelAnalysis();
      })
      .catch((error: unknown) => {
        if (disposed) {
          return;
        }
        const cancellationError = normalizeError(
          error,
          'The active analysis could not be stopped cleanly.',
        );
        update((state) => ({
          ...state,
          connectionError: cancellationError,
          announcement: cancellationError.message,
        }));
      });
  };

  const cancelCurrentAnalysis = (announce: boolean): void => {
    if (current.analysis.status !== 'loading') {
      return;
    }
    analysisGeneration += 1;
    update((state) => ({
      ...state,
      analysis: { status: 'idle', media: null, error: null },
      announcement: announce ? 'Video analysis stopped.' : state.announcement,
    }));
    requestAnalysisCancellation();
  };

  const disconnect = (): void => {
    cancelCurrentAnalysis(false);
    disposed = true;
    unlisten?.();
    unlisten = undefined;
  };

  const reconnect = async (): Promise<void> => {
    disconnect();
    disposed = false;
    await connect();
  };

  const analyzeCurrent = async (): Promise<void> => {
    if (current.analysis.status === 'loading') {
      return;
    }
    const url = current.draft.url.trim();
    if (url.length === 0) {
      const error: IpcErrorDto = {
        code: 'invalid-request',
        message: 'Enter a public video URL to analyze.',
        details: [],
      };
      update((state) => ({
        ...state,
        analysis: { status: 'error', media: null, error },
        announcement: error.message,
      }));
      return;
    }
    const generation = ++analysisGeneration;
    update((state) => ({
      ...state,
      analysis: { status: 'loading', media: null, error: null },
      announcement: 'Analyzing the video URL.',
    }));
    await analysisCancellation;
    if (disposed || generation !== analysisGeneration) {
      return;
    }
    try {
      const response = await client.analyze({ url });
      if (disposed || generation !== analysisGeneration) {
        return;
      }
      const selectedOutputs: DownloadDraft['selectedOutputs'] = {
        mp3: defaultOutput(current.settings, response.media, 'mp3'),
        mp4: defaultOutput(current.settings, response.media, 'mp4'),
      };
      update((state) => ({
        ...state,
        analysis: { status: 'ready', media: response.media, error: null },
        draft: {
          ...state.draft,
          url: response.media.url,
          name: response.media.title,
          selectedOutputs,
        },
        announcement: `${response.media.title} is ready. Choose an available format.`,
      }));
    } catch (error: unknown) {
      if (disposed || generation !== analysisGeneration) {
        return;
      }
      const analysisError = normalizeError(
        error,
        'Analysis failed. Check the URL and local tool status, then try again.',
      );
      if (analysisError.code === 'analysis-cancelled') {
        update((state) => ({
          ...state,
          analysis: { status: 'idle', media: null, error: null },
          announcement: 'Video analysis stopped.',
        }));
        return;
      }
      update((state) => ({
        ...state,
        analysis: { status: 'error', media: null, error: analysisError },
        announcement: analysisError.message,
      }));
    }
  };

  const chooseDestination = async (): Promise<void> => {
    setBusy('choose-destination', true);
    try {
      const selection = await client.chooseDestination();
      if (selection.path !== null) {
        update((state) => ({
          ...state,
          draft: { ...state.draft, destination: selection.path ?? '' },
          announcement: 'Download destination selected.',
        }));
      }
    } catch (error: unknown) {
      const destinationError = normalizeError(error, 'The destination picker could not be opened.');
      update((state) => ({
        ...state,
        analysis: { ...state.analysis, error: destinationError },
        announcement: destinationError.message,
      }));
    } finally {
      setBusy('choose-destination', false);
    }
  };

  const enqueueCurrent = async (): Promise<void> => {
    const media = current.analysis.media;
    const output = current.draft.selectedOutputs[current.draft.outputKind];
    if (media === null || output === null) {
      const error = internalError('Choose an available output format before starting.');
      update((state) => ({
        ...state,
        analysis: { ...state.analysis, error },
        announcement: error.message,
      }));
      return;
    }
    const destination = current.draft.destination.trim();
    if (destination.length === 0) {
      const error = internalError('Choose a destination before starting the download.');
      update((state) => ({
        ...state,
        analysis: { ...state.analysis, error },
        announcement: error.message,
      }));
      return;
    }
    setBusy('enqueue', true);
    try {
      const name = current.draft.name.trim();
      const job = await client.enqueue({
        url: media.url,
        output,
        destination,
        name: name.length === 0 ? null : name,
      });
      update((state) => ({
        ...state,
        jobs: replaceJob(state.jobs, job),
        shelfExpanded: true,
        analysis: { ...state.analysis, error: null },
        announcement: `${job.name ?? 'Download'} was added to the queue.`,
      }));
    } catch (error: unknown) {
      const enqueueError = normalizeError(
        error,
        'The download could not be queued. Review the destination and tool status.',
      );
      update((state) => ({
        ...state,
        analysis: { ...state.analysis, error: enqueueError },
        announcement: enqueueError.message,
      }));
    } finally {
      setBusy('enqueue', false);
    }
  };

  type JobAction = 'pause' | 'resume' | 'cancel' | 'retry';

  const runJobAction = async (action: JobAction, jobId: string): Promise<void> => {
    const key = `${action}:${jobId}`;
    setBusy(key, true);
    try {
      const request = { job_id: jobId };
      const job =
        action === 'pause'
          ? await client.pauseJob(request)
          : action === 'resume'
            ? await client.resumeJob(request)
            : action === 'cancel'
              ? await client.cancelJob(request)
              : await client.retryJob(request);
      update((state) => ({
        ...state,
        jobs: replaceJob(state.jobs, job, action === 'retry'),
        announcement: `${job.name ?? 'Job'} is ${job.state}.`,
      }));
    } catch (error: unknown) {
      const actionError = normalizeError(
        error,
        `The job could not be ${action === 'retry' ? 'retried' : `${action}d`}. Refresh and try again.`,
      );
      update((state) => ({
        ...state,
        connectionError: actionError,
        announcement: actionError.message,
      }));
    } finally {
      setBusy(key, false);
    }
  };

  const reveal = async (jobId: string): Promise<void> => {
    const key = `reveal:${jobId}`;
    setBusy(key, true);
    try {
      await client.revealOutput({ job_id: jobId });
      update((state) => ({
        ...state,
        announcement: 'Output revealed in the system file manager.',
      }));
    } catch (error: unknown) {
      const revealError = normalizeError(
        error,
        'The completed output could not be revealed. It may have been moved or deleted.',
      );
      update((state) => ({
        ...state,
        connectionError: revealError,
        announcement: revealError.message,
      }));
    } finally {
      setBusy(key, false);
    }
  };

  const deleteHistory = async (jobId: string): Promise<void> => {
    const key = `delete:${jobId}`;
    setBusy(key, true);
    try {
      await client.deleteHistory({ job_id: jobId });
      update((state) => ({
        ...state,
        jobs: state.jobs.filter((job) => job.id !== jobId),
        announcement: 'Completed history entry deleted. The output file was not removed.',
      }));
    } catch (error: unknown) {
      const deleteError = normalizeError(error, 'The history entry could not be deleted.');
      update((state) => ({
        ...state,
        connectionError: deleteError,
        announcement: deleteError.message,
      }));
    } finally {
      setBusy(key, false);
    }
  };

  const updateSettings = async (request: UpdateSettingsRequestDto): Promise<void> => {
    update((state) => ({
      ...state,
      settingsStatus: 'saving',
      settingsError: null,
    }));
    try {
      const settings = await client.updateSettings(request);
      update((state) => ({
        ...state,
        settings,
        draft:
          request.default_destination.action === 'unchanged'
            ? state.draft
            : {
                ...state.draft,
                destination: settings.default_destination ?? '',
              },
        settingsStatus: 'idle',
        announcement: 'Settings saved.',
      }));
    } catch (error: unknown) {
      const settingsError = normalizeError(error, 'Settings could not be saved.');
      update((state) => ({
        ...state,
        settingsStatus: 'error',
        settingsError,
        announcement: settingsError.message,
      }));
    }
  };

  const settingsRequest = (
    defaultDestination: DefaultDestinationUpdateDto = {
      action: 'unchanged',
    },
  ): UpdateSettingsRequestDto => ({
    default_destination: defaultDestination,
    queue_concurrency: null,
    update_preference: null,
    last_output: null,
  });

  const chooseDefaultDestination = async (): Promise<void> => {
    setBusy('choose-default-destination', true);
    try {
      const selection = await client.chooseDestination();
      if (selection.path !== null) {
        await updateSettings({
          ...settingsRequest({ action: 'set', value: selection.path }),
        });
      }
    } catch (error: unknown) {
      const settingsError = normalizeError(error, 'The destination picker could not be opened.');
      update((state) => ({
        ...state,
        settingsStatus: 'error',
        settingsError,
        announcement: settingsError.message,
      }));
    } finally {
      setBusy('choose-default-destination', false);
    }
  };

  const refreshTools = async (): Promise<void> => {
    setBusy('refresh-tools', true);
    try {
      const tools = await client.requestToolStatus();
      update((state) => ({
        ...state,
        tools,
        announcement: 'Tool health refreshed.',
      }));
    } catch (error: unknown) {
      const toolError = normalizeError(error, 'Tool health could not be refreshed.');
      update((state) => ({
        ...state,
        settingsError: toolError,
        announcement: toolError.message,
      }));
    } finally {
      setBusy('refresh-tools', false);
    }
  };

  const checkForToolUpdates = async (): Promise<void> => {
    setBusy('check-tool-updates', true);
    try {
      const result = await client.checkForToolUpdates();
      const version = result.version === null ? '' : ` ${result.version}`;
      const announcement =
        result.status === 'installed'
          ? `Verified tool update${version} installed. Restart YT Media to use it.`
          : result.status === 'available'
            ? `Verified tool update${version} is available.`
            : result.status === 'current'
              ? `Verified tools${version} are current.`
              : 'A recent background update check already completed.';
      update((state) => ({ ...state, settingsError: null, announcement }));
    } catch (error: unknown) {
      const updateError = normalizeError(error, 'Verified tool updates could not be checked.');
      update((state) => ({
        ...state,
        settingsError: updateError,
        announcement: updateError.message,
      }));
    } finally {
      setBusy('check-tool-updates', false);
    }
  };

  const resetToolUpdates = async (): Promise<void> => {
    setBusy('reset-tool-updates', true);
    try {
      await client.resetToolUpdates();
      update((state) => ({
        ...state,
        settingsError: null,
        announcement: 'Managed tools were removed. Restart YT Media to use the bundled baseline.',
      }));
    } catch (error: unknown) {
      const updateError = normalizeError(error, 'Managed tools could not be reset.');
      update((state) => ({
        ...state,
        settingsError: updateError,
        announcement: updateError.message,
      }));
    } finally {
      setBusy('reset-tool-updates', false);
    }
  };

  return {
    state: { subscribe: store.subscribe },
    connect,
    disconnect,
    reconnect,
    navigate: (view) => {
      if (view !== current.view && current.analysis.status === 'loading') {
        cancelCurrentAnalysis(true);
      }
      update((state) => ({ ...state, view }));
    },
    setUrl: (url) => {
      cancelCurrentAnalysis(false);
      update((state) => ({
        ...state,
        draft: { ...state.draft, url },
        analysis:
          state.analysis.status === 'error'
            ? { status: 'idle', media: null, error: null }
            : state.analysis,
      }));
    },
    setName: (name) => {
      update((state) => ({
        ...state,
        draft: { ...state.draft, name },
      }));
    },
    setOutputKind: (kind) => {
      update((state) => ({
        ...state,
        draft: {
          ...state.draft,
          outputKind: kind,
        },
      }));
    },
    selectOutput: (output) => {
      const media = current.analysis.media;
      if (media === null || !media.formats.some((format) => outputMatchesFormat(output, format))) {
        return;
      }
      update((state) => ({
        ...state,
        draft: {
          ...state.draft,
          outputKind: output.format,
          selectedOutputs: {
            ...state.draft.selectedOutputs,
            [output.format]: output,
          },
        },
        analysis: { ...state.analysis, error: null },
      }));
    },
    analyze: analyzeCurrent,
    cancelAnalysis: () => {
      cancelCurrentAnalysis(true);
    },
    chooseDestination,
    enqueue: enqueueCurrent,
    pause: (jobId) => runJobAction('pause', jobId),
    resume: (jobId) => runJobAction('resume', jobId),
    cancel: (jobId) => runJobAction('cancel', jobId),
    retry: (jobId) => runJobAction('retry', jobId),
    reveal,
    deleteHistory,
    setConcurrency: (value) =>
      updateSettings({
        ...settingsRequest(),
        queue_concurrency: value,
      }),
    setUpdatePreference: (value) =>
      updateSettings({
        ...settingsRequest(),
        update_preference: value,
      }),
    chooseDefaultDestination,
    clearDefaultDestination: () => updateSettings(settingsRequest({ action: 'clear' })),
    refreshTools,
    checkForToolUpdates,
    resetToolUpdates,
    toggleShelf: () => {
      update((state) => ({ ...state, shelfExpanded: !state.shelfExpanded }));
    },
  };
};
