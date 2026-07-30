import type {
  FormatOptionDto,
  JobDto,
  JobProgressDto,
  JobStageDto,
  OutputSelectionDto,
} from './ipc/generated';

const byteFormatter = new Intl.NumberFormat(undefined, {
  maximumFractionDigits: 1,
});

const countFormatter = new Intl.NumberFormat(undefined, {
  notation: 'compact',
  maximumFractionDigits: 1,
});

const dateFormatter = new Intl.DateTimeFormat(undefined, {
  dateStyle: 'medium',
  timeStyle: 'short',
});

const parseUnsignedDecimal = (value: string): number | null => {
  if (!/^(0|[1-9]\d*)$/.test(value)) {
    return null;
  }
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : null;
};

export const formatBytes = (bytes: string | null): string => {
  if (bytes === null) {
    return 'Unknown size';
  }
  const value = parseUnsignedDecimal(bytes);
  if (value === null) {
    return 'Unknown size';
  }
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let size = value;
  let unit = units[0] ?? 'B';
  for (let index = 1; index < units.length && size >= 1000; index += 1) {
    size /= 1000;
    unit = units[index] ?? unit;
  }
  return `${byteFormatter.format(size)} ${unit}`;
};

export const formatCount = (value: string | null): string => {
  const count = value === null ? null : parseUnsignedDecimal(value);
  return count === null ? 'View count unavailable' : `${countFormatter.format(count)} views`;
};

export const formatDuration = (milliseconds: string): string => {
  const numeric = parseUnsignedDecimal(milliseconds);
  if (numeric === null) {
    return 'Unknown duration';
  }
  const totalSeconds = Math.floor(numeric / 1000);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  return hours > 0
    ? `${hours}:${String(minutes).padStart(2, '0')}:${String(seconds).padStart(2, '0')}`
    : `${minutes}:${String(seconds).padStart(2, '0')}`;
};

export const formatEta = (seconds: string | null): string | null => {
  if (seconds === null) {
    return null;
  }
  const numeric = parseUnsignedDecimal(seconds);
  if (numeric === null) {
    return null;
  }
  if (numeric < 60) {
    return `${Math.round(numeric)} sec left in download`;
  }
  const minutes = Math.ceil(numeric / 60);
  return `${minutes} min left in download`;
};

export const formatDate = (milliseconds: string): string => {
  const numeric = parseUnsignedDecimal(milliseconds);
  if (numeric === null) {
    return 'Unknown date';
  }
  const date = new Date(numeric);
  return Number.isNaN(date.getTime()) ? 'Unknown date' : dateFormatter.format(date);
};

export const formatOutput = (output: OutputSelectionDto): string =>
  output.format === 'mp3' ? `MP3 · ${output.quality} kbps` : `MP4 · ${output.quality}p`;

export const formatFormat = (format: FormatOptionDto): string => {
  if (format.kind === 'mp3') {
    return `${format.bitrate_kbps} kbps · ${format.source.audio_codec?.name ?? 'audio'}`;
  }
  const codec = format.video_source.video_codec?.name ?? 'video';
  const fps = format.fps === null ? '' : ` · ${byteFormatter.format(format.fps)} fps`;
  return `${format.height}p · ${codec}${fps}`;
};

export const formatEstimatedSize = (format: FormatOptionDto): string =>
  format.kind === 'mp4'
    ? formatBytes(format.estimated_size_bytes)
    : 'Size available during transfer';

export const jobDisplayName = (job: JobDto): string =>
  job.name === null
    ? `Download ${job.id.slice(0, 8)}.${job.output.format}`
    : job.name.toLowerCase().endsWith(`.${job.output.format}`)
      ? job.name
      : `${job.name}.${job.output.format}`;

export const progressDetail = (progress: JobProgressDto | null): string => {
  if (progress === null) {
    return 'Preparing transfer';
  }
  if (progress.stage === 'merging') {
    return 'Processing selected streams';
  }
  if (progress.stage === 'converting') {
    return 'Encoding selected format';
  }
  if (progress.stage === 'finalizing') {
    return 'Checking completed output';
  }
  if (progress.stage !== 'downloading') {
    return stateLabel(progress.stage);
  }
  const transferred = formatBytes(progress.completed);
  const total = progress.total === null ? null : formatBytes(progress.total);
  return total === null ? transferred : `${transferred} of ${total}`;
};

export const stateLabel = (state: JobDto['state'] | JobStageDto): string => {
  if (state === 'merging') {
    return 'Merging audio and video';
  }
  if (state === 'converting') {
    return 'Converting media';
  }
  if (state === 'finalizing') {
    return 'Finalizing output';
  }
  return state.replaceAll('-', ' ').replace(/^\w/, (character) => character.toUpperCase());
};
