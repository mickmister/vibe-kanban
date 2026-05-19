import { describe, expect, it } from 'vitest';

import { detectPreviewUrl } from './usePreviewUrl';

describe('detectPreviewUrl', () => {
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
});
