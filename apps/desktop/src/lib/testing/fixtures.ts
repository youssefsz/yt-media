import type { DesktopClient } from '../ipc/client';
import type {
  ActionResultDto,
  AnalyzeResponseDto,
  BootstrapStateDto,
  DestinationSelectionDto,
  IpcErrorDto,
  JobActionDto,
  JobDto,
  JobEventEnvelopeDto,
  JobIdRequestDto,
  SettingsDto,
  ToolStatusDto,
  UpdateSettingsRequestDto,
} from '../ipc/generated';

export const pausedActions: JobActionDto[] = ['resume', 'cancel'];

export const activeActions: JobActionDto[] = ['pause', 'cancel'];

export const failedActions: JobActionDto[] = ['retry'];

export const completedActions: JobActionDto[] = ['reveal', 'delete-history'];

export const settingsFixture: SettingsDto = {
  default_destination: 'C:\\Users\\Local\\Downloads',
  queue_concurrency: 2,
  update_preference: 'notify',
  last_output: { format: 'mp4', quality: 1080 },
};

export const toolsFixture: ToolStatusDto[] = [
  {
    tool: 'yt-dlp',
    ready: true,
    source: 'bundled-baseline',
    message: null,
  },
  {
    tool: 'ffmpeg',
    ready: true,
    source: 'bundled-baseline',
    message: null,
  },
  {
    tool: 'ffprobe',
    ready: true,
    source: 'bundled-baseline',
    message: null,
  },
  {
    tool: 'deno',
    ready: true,
    source: 'bundled-baseline',
    message: null,
  },
];

export const analysisFixture: AnalyzeResponseDto = {
  schema_version: 1,
  media: {
    id: 'dQw4w9WgXcQ',
    url: 'https://www.youtube.com/watch?v=dQw4w9WgXcQ',
    title: 'City Lights Timelapse',
    uploader: 'Local Fixture Studio',
    duration_ms: '277000',
    view_count: '998120',
    upload_date: '2026-06-14',
    thumbnails: [
      {
        url: 'http://127.0.0.1:1420/fixture-thumbnail.svg',
        width: 320,
        height: 180,
      },
    ],
    formats: [
      {
        kind: 'mp3',
        bitrate_kbps: 128,
        source: {
          format_id: 'audio-128',
          container: { name: 'm4a', family: 'm4a' },
          video_codec: null,
          audio_codec: { name: 'aac', family: 'aac' },
        },
      },
      {
        kind: 'mp3',
        bitrate_kbps: 192,
        source: {
          format_id: 'audio-192',
          container: { name: 'm4a', family: 'm4a' },
          video_codec: null,
          audio_codec: { name: 'aac', family: 'aac' },
        },
      },
      {
        kind: 'mp4',
        height: 1080,
        width: 1920,
        fps: 60,
        estimated_size_bytes: '192400000',
        video_source: {
          format_id: 'video-1080',
          container: { name: 'mp4', family: 'mp4' },
          video_codec: { name: 'H.264', family: 'h264' },
          audio_codec: null,
        },
        audio_source: {
          format_id: 'audio-aac',
          container: { name: 'm4a', family: 'm4a' },
          video_codec: null,
          audio_codec: { name: 'AAC', family: 'aac' },
        },
        compatibility: 'merge',
      },
      {
        kind: 'mp4',
        height: 720,
        width: 1280,
        fps: null,
        estimated_size_bytes: '82700000',
        video_source: {
          format_id: 'video-720',
          container: { name: 'mp4', family: 'mp4' },
          video_codec: { name: 'H.264', family: 'h264' },
          audio_codec: null,
        },
        audio_source: {
          format_id: 'audio-aac',
          container: { name: 'm4a', family: 'm4a' },
          video_codec: null,
          audio_codec: { name: 'AAC', family: 'aac' },
        },
        compatibility: 'none',
      },
      {
        kind: 'mp4',
        height: 480,
        width: 854,
        fps: null,
        estimated_size_bytes: '42600000',
        video_source: {
          format_id: 'video-480',
          container: { name: 'mp4', family: 'mp4' },
          video_codec: { name: 'H.264', family: 'h264' },
          audio_codec: null,
        },
        audio_source: {
          format_id: 'audio-aac',
          container: { name: 'm4a', family: 'm4a' },
          video_codec: null,
          audio_codec: { name: 'AAC', family: 'aac' },
        },
        compatibility: 'none',
      },
      {
        kind: 'mp4',
        height: 360,
        width: 640,
        fps: null,
        estimated_size_bytes: '25100000',
        video_source: {
          format_id: 'video-360',
          container: { name: 'mp4', family: 'mp4' },
          video_codec: { name: 'H.264', family: 'h264' },
          audio_codec: null,
        },
        audio_source: {
          format_id: 'audio-aac',
          container: { name: 'm4a', family: 'm4a' },
          video_codec: null,
          audio_codec: { name: 'AAC', family: 'aac' },
        },
        compatibility: 'none',
      },
      {
        kind: 'mp4',
        height: 240,
        width: 426,
        fps: null,
        estimated_size_bytes: '15300000',
        video_source: {
          format_id: 'video-240',
          container: { name: 'mp4', family: 'mp4' },
          video_codec: { name: 'H.264', family: 'h264' },
          audio_codec: null,
        },
        audio_source: {
          format_id: 'audio-aac',
          container: { name: 'm4a', family: 'm4a' },
          video_codec: null,
          audio_codec: { name: 'AAC', family: 'aac' },
        },
        compatibility: 'none',
      },
    ],
    warnings: [],
  },
};

const baseJob = (): JobDto => ({
  id: '0198-0000-7000-8000-000000000001',
  canonical_url: analysisFixture.media.url,
  output: { format: 'mp4', quality: 1080 },
  destination: settingsFixture.default_destination ?? '',
  name: 'Wilderness Escape Documentary',
  state: 'downloading',
  progress: {
    stage: 'downloading',
    completed: '111592000',
    total: '192400000',
    percent: 58,
    bytes_per_second: '24100000',
    eta_seconds: '120',
  },
  error: null,
  created_at_ms: '1785312000000',
  updated_at_ms: '1785312020000',
  completed_at_ms: null,
  attempt_count: 1,
  final_output: null,
  output_availability: 'not-applicable',
  destination_available: true,
  is_terminal: false,
  available_actions: activeActions,
});

export const activeJobFixture = (overrides: Partial<JobDto> = {}): JobDto => ({
  ...baseJob(),
  ...overrides,
});

export const interruptedJobFixture = activeJobFixture({
  id: '0198-0000-7000-8000-000000000002',
  name: 'Interrupted field recording',
  state: 'interrupted',
  progress: {
    stage: 'paused',
    completed: '42000000',
    total: null,
    percent: null,
    bytes_per_second: null,
    eta_seconds: null,
  },
  available_actions: pausedActions,
});

export const queuedJobFixture = activeJobFixture({
  id: '0198-0000-7000-8000-000000000003',
  name: 'Queued architecture lecture',
  state: 'queued',
  progress: null,
});

export const completedJobFixture = activeJobFixture({
  id: '0198-0000-7000-8000-000000000004',
  name: 'Completed city walk',
  state: 'completed',
  progress: {
    stage: 'completed',
    completed: '82700000',
    total: '82700000',
    percent: 100,
    bytes_per_second: null,
    eta_seconds: null,
  },
  completed_at_ms: '1785312120000',
  final_output: {
    path: 'C:\\Users\\Local\\Downloads\\Completed city walk.mp4',
    size_bytes: '82700000',
    output: { format: 'mp4', quality: 720 },
  },
  output_availability: 'present',
  is_terminal: true,
  available_actions: completedActions,
});

export const missingJobFixture = activeJobFixture({
  ...completedJobFixture,
  id: '0198-0000-7000-8000-000000000005',
  name: 'Moved interview archive',
  final_output: {
    path: 'C:\\Users\\Local\\Downloads\\Moved interview archive.mp3',
    size_bytes: '18100000',
    output: { format: 'mp3', quality: 192 },
  },
  output_availability: 'missing',
  available_actions: ['delete-history'],
});

export const failedJobFixture = activeJobFixture({
  id: '0198-0000-7000-8000-000000000006',
  name: 'Failed unavailable format',
  state: 'failed',
  progress: {
    stage: 'failed',
    completed: '0',
    total: null,
    percent: null,
    bytes_per_second: null,
    eta_seconds: null,
  },
  error: {
    class: 'format-unavailable',
    message: 'The selected quality is no longer available. Analyze again or retry.',
  },
  completed_at_ms: '1785312140000',
  is_terminal: true,
  available_actions: failedActions,
});

export const cancelledJobFixture = activeJobFixture({
  id: '0198-0000-7000-8000-000000000007',
  name: 'Cancelled soundtrack',
  state: 'cancelled',
  progress: {
    stage: 'cancelled',
    completed: '21000000',
    total: null,
    percent: null,
    bytes_per_second: null,
    eta_seconds: null,
  },
  completed_at_ms: '1785312160000',
  is_terminal: true,
  available_actions: failedActions,
});

export const bootstrapFixture = (
  jobs: JobDto[] = [
    activeJobFixture(),
    queuedJobFixture,
    interruptedJobFixture,
    completedJobFixture,
    missingJobFixture,
    failedJobFixture,
    cancelledJobFixture,
  ],
): BootstrapStateDto => ({
  schema_version: 1,
  health: 'healthy',
  last_event_sequence: '40',
  jobs,
  settings: settingsFixture,
  tools: toolsFixture,
  diagnostic: null,
});

export const fixtureError = (
  message = 'The fixture command failed with an actionable safe message.',
): IpcErrorDto => ({
  code: 'internal',
  message,
  details: [],
});

export class FixtureDesktopClient implements DesktopClient {
  readonly calls: string[] = [];
  snapshot: BootstrapStateDto;
  analysisResponse: AnalyzeResponseDto = analysisFixture;
  analysisError: IpcErrorDto | null = null;
  analysisPromise: Promise<AnalyzeResponseDto> | null = null;
  commandError: IpcErrorDto | null = null;
  systemDownloadsDestination: string | null = settingsFixture.default_destination;
  destination: DestinationSelectionDto = {
    path: settingsFixture.default_destination,
  };
  private onEvent: ((event: JobEventEnvelopeDto) => void) | undefined;

  constructor(snapshot: BootstrapStateDto = bootstrapFixture()) {
    this.snapshot = snapshot;
  }

  connectJobEvents(
    onSnapshot: (snapshot: BootstrapStateDto) => void,
    onEvent: (event: JobEventEnvelopeDto) => void,
  ): Promise<() => void> {
    this.calls.push('connect');
    this.onEvent = onEvent;
    onSnapshot(this.snapshot);
    return Promise.resolve(() => {
      this.onEvent = undefined;
    });
  }

  emit(event: JobEventEnvelopeDto): void {
    this.onEvent?.(event);
  }

  analyze(): Promise<AnalyzeResponseDto> {
    this.calls.push('analyze');
    if (this.analysisPromise !== null) {
      return this.analysisPromise;
    }
    return this.analysisError === null
      ? Promise.resolve(this.analysisResponse)
      : Promise.reject(this.analysisError);
  }

  cancelAnalysis(): Promise<ActionResultDto> {
    this.calls.push('cancel-analysis');
    return this.commandError === null
      ? Promise.resolve({ schema_version: 1 })
      : Promise.reject(this.commandError);
  }

  enqueue(): Promise<JobDto> {
    this.calls.push('enqueue');
    if (this.commandError !== null) {
      return Promise.reject(this.commandError);
    }
    return Promise.resolve(queuedJobFixture);
  }

  getJob(request: JobIdRequestDto): Promise<JobDto> {
    this.calls.push(`get:${request.job_id}`);
    const job = this.snapshot.jobs.find((candidate) => candidate.id === request.job_id);
    return job === undefined
      ? Promise.reject(fixtureError('Fixture job not found.'))
      : Promise.resolve(job);
  }

  pauseJob(request: JobIdRequestDto): Promise<JobDto> {
    this.calls.push(`pause:${request.job_id}`);
    return this.actionJob(request);
  }

  resumeJob(request: JobIdRequestDto): Promise<JobDto> {
    this.calls.push(`resume:${request.job_id}`);
    return this.actionJob(request);
  }

  cancelJob(request: JobIdRequestDto): Promise<JobDto> {
    this.calls.push(`cancel:${request.job_id}`);
    return this.actionJob(request);
  }

  retryJob(request: JobIdRequestDto): Promise<JobDto> {
    this.calls.push(`retry:${request.job_id}`);
    return this.actionJob(request);
  }

  deleteHistory(request: JobIdRequestDto): Promise<ActionResultDto> {
    this.calls.push(`delete:${request.job_id}`);
    if (this.commandError !== null) {
      return Promise.reject(this.commandError);
    }
    this.snapshot = {
      ...this.snapshot,
      jobs: this.snapshot.jobs.filter((job) => job.id !== request.job_id),
    };
    return Promise.resolve({ schema_version: 1 });
  }

  updateSettings(request: UpdateSettingsRequestDto): Promise<SettingsDto> {
    this.calls.push('update-settings');
    if (this.commandError !== null || this.snapshot.settings === null) {
      return Promise.reject(this.commandError ?? fixtureError());
    }
    const defaultDestination =
      request.default_destination.action === 'set'
        ? request.default_destination.value
        : request.default_destination.action === 'clear'
          ? this.systemDownloadsDestination
          : this.snapshot.settings.default_destination;
    const settings: SettingsDto = {
      ...this.snapshot.settings,
      default_destination: defaultDestination,
      queue_concurrency: request.queue_concurrency ?? this.snapshot.settings.queue_concurrency,
      update_preference: request.update_preference ?? this.snapshot.settings.update_preference,
      last_output: request.last_output ?? this.snapshot.settings.last_output,
    };
    this.snapshot = { ...this.snapshot, settings };
    return Promise.resolve(settings);
  }

  chooseDestination(): Promise<DestinationSelectionDto> {
    this.calls.push('choose-destination');
    return this.commandError === null
      ? Promise.resolve(this.destination)
      : Promise.reject(this.commandError);
  }

  revealOutput(request: JobIdRequestDto): Promise<ActionResultDto> {
    this.calls.push(`reveal:${request.job_id}`);
    return this.commandError === null
      ? Promise.resolve({ schema_version: 1 })
      : Promise.reject(this.commandError);
  }

  requestToolStatus(): Promise<ToolStatusDto[]> {
    this.calls.push('tool-status');
    return this.commandError === null
      ? Promise.resolve(this.snapshot.tools)
      : Promise.reject(this.commandError);
  }

  private actionJob(request: JobIdRequestDto): Promise<JobDto> {
    if (this.commandError !== null) {
      return Promise.reject(this.commandError);
    }
    return this.getJob(request);
  }
}
