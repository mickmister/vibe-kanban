import { afterEach, describe, expect, it, vi } from 'vitest';

import { detectPreviewUrl } from './usePreviewUrl';

describe('detectPreviewUrl', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('detects configured exact dev server origins', () => {
    expect(
      detectPreviewUrl('ready at https://preview.example.com', [
        'https://preview.example.com',
      ])?.url
    ).toBe('https://preview.example.com/');
  });

  it('detects configured single-label wildcard dev server origins', () => {
    expect(
      detectPreviewUrl('ready at https://app.preview.example.com', [
        'https://*.preview.example.com',
      ])?.url
    ).toBe('https://app.preview.example.com/');
  });

  it('does not match root or nested hosts for wildcard origins', () => {
    const allowed = ['https://*.preview.example.com'];

    expect(
      detectPreviewUrl('ready at https://preview.example.com', allowed)
    ).toBeNull();
    expect(
      detectPreviewUrl('ready at https://deep.app.preview.example.com', allowed)
    ).toBeNull();
  });

  it('keeps scanning when an earlier unconfigured URL is present', () => {
    expect(
      detectPreviewUrl(
        'docs https://vite.dev and local http://localhost:5173',
        []
      )?.url
    ).toBe('http://localhost:5173/');
  });

  it('keeps scanning when an earlier URL uses the Vibe Kanban port', () => {
    vi.stubGlobal('window', {
      location: { port: '3000' },
    });

    expect(
      detectPreviewUrl(
        'dashboard http://localhost:3000 and app https://preview.example.com',
        ['https://preview.example.com']
      )?.url
    ).toBe('https://preview.example.com/');
  });

  it('does not detect URLs for invalid allowed wildcard origins', () => {
    const invalidCases = [
      ['https://*.com', 'https://foo.com'],
      ['https://*.bad-.example.com', 'https://foo.bad-.example.com'],
      ['https://*.-bad.example.com', 'https://foo.-bad.example.com'],
      ['https://foo.*.example.com', 'https://foo.bar.example.com'],
      ['https://*.*.example.com', 'https://foo.bar.example.com'],
    ];

    for (const [allowed, candidate] of invalidCases) {
      expect(detectPreviewUrl(`ready at ${candidate}`, [allowed])).toBeNull();
    }
  });

  it('does not detect URLs for allowed origins with paths, query strings, fragments, or userinfo', () => {
    const invalidAllowedOrigins = [
      'https://preview.example.com/path',
      'https://preview.example.com?preview=1',
      'https://preview.example.com#fragment',
      'https://user:password@preview.example.com',
      'https://*.example.com/path',
      'https://*.example.com?preview=1',
      'https://*.example.com#fragment',
      'https://user:password@*.example.com',
    ];

    for (const allowed of invalidAllowedOrigins) {
      expect(
        detectPreviewUrl('ready at https://preview.example.com', [allowed])
      ).toBeNull();
    }
  });
});
