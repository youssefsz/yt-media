import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import type {
  ActionResultDto,
  AnalyzeRequestDto,
  AnalyzeResponseDto,
  BootstrapStateDto,
  DestinationSelectionDto,
  EnqueueRequestDto,
  IpcErrorDto,
  JobDto,
  JobEventEnvelopeDto,
  JobIdRequestDto,
  SettingsDto,
  ToolStatusDto,
  UpdateCheckResultDto,
  UpdateSettingsRequestDto,
} from './generated';

export const commandNames = [
  'bootstrap',
  'analyze',
  'cancel_analysis',
  'enqueue',
  'list_jobs',
  'get_job',
  'pause_job',
  'resume_job',
  'cancel_job',
  'retry_job',
  'list_history',
  'delete_history',
  'read_settings',
  'update_settings',
  'choose_destination',
  'reveal_output',
  'tool_status',
  'check_for_tool_updates',
  'reset_tool_updates',
] as const;

export const bootstrap = (): Promise<BootstrapStateDto> => invoke<BootstrapStateDto>('bootstrap');

export const analyze = (request: AnalyzeRequestDto): Promise<AnalyzeResponseDto> =>
  invoke<AnalyzeResponseDto>('analyze', { request });

export const cancelAnalysis = (): Promise<ActionResultDto> =>
  invoke<ActionResultDto>('cancel_analysis');

export const enqueue = (request: EnqueueRequestDto): Promise<JobDto> =>
  invoke<JobDto>('enqueue', { request });

export const listJobs = (): Promise<JobDto[]> => invoke<JobDto[]>('list_jobs');

export const getJob = (request: JobIdRequestDto): Promise<JobDto> =>
  invoke<JobDto>('get_job', { request });

export const pauseJob = (request: JobIdRequestDto): Promise<JobDto> =>
  invoke<JobDto>('pause_job', { request });

export const resumeJob = (request: JobIdRequestDto): Promise<JobDto> =>
  invoke<JobDto>('resume_job', { request });

export const cancelJob = (request: JobIdRequestDto): Promise<JobDto> =>
  invoke<JobDto>('cancel_job', { request });

export const retryJob = (request: JobIdRequestDto): Promise<JobDto> =>
  invoke<JobDto>('retry_job', { request });

export const listHistory = (): Promise<JobDto[]> => invoke<JobDto[]>('list_history');

export const deleteHistory = (request: JobIdRequestDto): Promise<ActionResultDto> =>
  invoke<ActionResultDto>('delete_history', { request });

export const readSettings = (): Promise<SettingsDto> => invoke<SettingsDto>('read_settings');

export const updateSettings = (request: UpdateSettingsRequestDto): Promise<SettingsDto> =>
  invoke<SettingsDto>('update_settings', { request });

export const chooseDestination = (): Promise<DestinationSelectionDto> =>
  invoke<DestinationSelectionDto>('choose_destination');

export const revealOutput = (request: JobIdRequestDto): Promise<ActionResultDto> =>
  invoke<ActionResultDto>('reveal_output', { request });

export const requestToolStatus = (): Promise<ToolStatusDto[]> =>
  invoke<ToolStatusDto[]>('tool_status');

export const checkForToolUpdates = (): Promise<UpdateCheckResultDto> =>
  invoke<UpdateCheckResultDto>('check_for_tool_updates');

export const resetToolUpdates = (): Promise<ActionResultDto> =>
  invoke<ActionResultDto>('reset_tool_updates');

export interface DesktopClient {
  connectJobEvents(
    onSnapshot: (snapshot: BootstrapStateDto) => void,
    onEvent: (event: JobEventEnvelopeDto) => void,
  ): Promise<UnlistenFn>;
  analyze(request: AnalyzeRequestDto): Promise<AnalyzeResponseDto>;
  cancelAnalysis(): Promise<ActionResultDto>;
  enqueue(request: EnqueueRequestDto): Promise<JobDto>;
  getJob(request: JobIdRequestDto): Promise<JobDto>;
  pauseJob(request: JobIdRequestDto): Promise<JobDto>;
  resumeJob(request: JobIdRequestDto): Promise<JobDto>;
  cancelJob(request: JobIdRequestDto): Promise<JobDto>;
  retryJob(request: JobIdRequestDto): Promise<JobDto>;
  deleteHistory(request: JobIdRequestDto): Promise<ActionResultDto>;
  updateSettings(request: UpdateSettingsRequestDto): Promise<SettingsDto>;
  chooseDestination(): Promise<DestinationSelectionDto>;
  revealOutput(request: JobIdRequestDto): Promise<ActionResultDto>;
  requestToolStatus(): Promise<ToolStatusDto[]>;
  checkForToolUpdates(): Promise<UpdateCheckResultDto>;
  resetToolUpdates(): Promise<ActionResultDto>;
}

const isErrorCode = (value: unknown): value is IpcErrorDto['code'] => {
  switch (value) {
    case 'invalid-request':
    case 'invalid-job-id':
    case 'job-not-found':
    case 'invalid-job-state':
    case 'tools-unavailable':
    case 'analysis-failed':
    case 'analysis-cancelled':
    case 'persistence-unavailable':
    case 'shutting-down':
    case 'destination-selection-failed':
    case 'reveal-failed':
    case 'update-unavailable':
    case 'internal':
      return true;
    default:
      return false;
  }
};

export const isIpcError = (value: unknown): value is IpcErrorDto => {
  if (typeof value !== 'object' || value === null) {
    return false;
  }
  if (
    !('code' in value) ||
    !isErrorCode(value.code) ||
    !('message' in value) ||
    typeof value.message !== 'string' ||
    !('details' in value) ||
    !Array.isArray(value.details)
  ) {
    return false;
  }
  return value.details.every(
    (detail: unknown) =>
      typeof detail === 'object' &&
      detail !== null &&
      'key' in detail &&
      typeof detail.key === 'string' &&
      'value' in detail &&
      typeof detail.value === 'string',
  );
};

export const connectJobEvents = async (
  onSnapshot: (snapshot: BootstrapStateDto) => void,
  onEvent: (event: JobEventEnvelopeDto) => void,
): Promise<UnlistenFn> => {
  let boundary = 0n;
  const buffered: JobEventEnvelopeDto[] = [];
  let connected = false;
  const unlisten = await listen<JobEventEnvelopeDto>('job-event-v1', ({ payload }) => {
    if (!connected) {
      buffered.push(payload);
      return;
    }
    const sequence = BigInt(payload.sequence);
    if (sequence > boundary) {
      boundary = sequence;
      onEvent(payload);
    }
  });
  try {
    const snapshot = await bootstrap();
    boundary = BigInt(snapshot.last_event_sequence);
    onSnapshot(snapshot);
    connected = true;
    for (const event of buffered) {
      const sequence = BigInt(event.sequence);
      if (sequence > boundary) {
        boundary = sequence;
        onEvent(event);
      }
    }
    return unlisten;
  } catch (error: unknown) {
    unlisten();
    throw error;
  }
};

export const desktopClient: DesktopClient = {
  connectJobEvents,
  analyze,
  cancelAnalysis,
  enqueue,
  getJob,
  pauseJob,
  resumeJob,
  cancelJob,
  retryJob,
  deleteHistory,
  updateSettings,
  chooseDestination,
  revealOutput,
  requestToolStatus,
  checkForToolUpdates,
  resetToolUpdates,
};
