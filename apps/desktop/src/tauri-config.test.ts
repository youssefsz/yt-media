import { describe, expect, it } from 'vitest';

import tauriConfig from '../src-tauri/tauri.conf.json';

describe('packaged desktop security', () => {
  it('permits the YouTube thumbnail CDN without permitting arbitrary remote images', () => {
    const imageDirective = tauriConfig.app.security.csp
      .split(';')
      .map((directive) => directive.trim())
      .find((directive) => directive.startsWith('img-src '));
    const sources = imageDirective?.split(/\s+/).slice(1);

    expect(sources).toContain('https://i.ytimg.com');
    expect(sources).not.toContain('https:');
    expect(sources).not.toContain('http:');
  });
});
