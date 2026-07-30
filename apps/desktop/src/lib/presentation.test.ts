import { describe, expect, it } from 'vitest';

import {
  formatBytes,
  formatCount,
  formatDate,
  formatDuration,
  formatEta,
  progressDetail,
} from './presentation';

describe('IPC decimal presentation', () => {
  it('formats the decimal strings emitted by native JSON IPC', () => {
    expect(formatBytes('192400000')).toContain('192.4');
    expect(formatCount('998120')).toContain('views');
    expect(formatDuration('277000')).toBe('4:37');
    expect(formatEta('120')).toBe('2 min left in download');
    expect(formatDate('1785312000000')).not.toBe('Unknown date');
  });

  it('fails safely for malformed or unbounded decimal fields', () => {
    expect(formatBytes('-1')).toBe('Unknown size');
    expect(formatBytes('1e9')).toBe('Unknown size');
    expect(formatCount('not-a-count')).toBe('View count unavailable');
    expect(formatDuration('Infinity')).toBe('Unknown duration');
    expect(formatEta('9'.repeat(400))).toBeNull();
    expect(formatDate('not-a-date')).toBe('Unknown date');
  });

  it('does not present FFmpeg time units as downloaded bytes', () => {
    expect(
      progressDetail({
        stage: 'converting',
        completed: '120000000',
        total: '240000000',
        percent: 50,
        bytes_per_second: null,
        eta_seconds: null,
      }),
    ).toBe('Encoding selected format');
  });
});
