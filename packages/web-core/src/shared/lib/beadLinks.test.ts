import { describe, expect, it } from 'vitest';
import {
  buildBeadReferenceHref,
  linkifyBeadReferencesMarkdown,
  parseBeadReferenceHref,
} from './beadLinks';

const targets = [{ prefix: 'vkvw' }, { prefix: 'Vktest' }];

describe('linkifyBeadReferencesMarkdown', () => {
  it('links plain bead references to postMessage intent anchors', () => {
    expect(
      linkifyBeadReferencesMarkdown('See vkvw-f9p1 please.', targets)
    ).toBe('See [vkvw-f9p1](#vk-bead=vkvw-f9p1) please.');
  });

  it('does not rewrite inline code or fenced code blocks', () => {
    const content = [
      'Use `vkvw-code` here.',
      '',
      '```',
      'vkvw-fenced',
      '```',
      'Then vkvw-real',
    ].join('\n');

    expect(linkifyBeadReferencesMarkdown(content, targets)).toBe(
      [
        'Use `vkvw-code` here.',
        '',
        '```',
        'vkvw-fenced',
        '```',
        'Then [vkvw-real](#vk-bead=vkvw-real)',
      ].join('\n')
    );
  });

  it('does not rewrite existing markdown links or URLs', () => {
    const content =
      '[vkvw-linked](/already) https://example.test/vkvw-url vkvw-new';

    expect(linkifyBeadReferencesMarkdown(content, targets)).toBe(
      '[vkvw-linked](/already) https://example.test/vkvw-url [vkvw-new](#vk-bead=vkvw-new)'
    );
  });
});

describe('bead reference hrefs', () => {
  it('round trips valid bead IDs', () => {
    expect(parseBeadReferenceHref(buildBeadReferenceHref('vkvw-f9p1'))).toBe(
      'vkvw-f9p1'
    );
  });

  it('rejects malformed bead IDs', () => {
    expect(parseBeadReferenceHref('#vk-bead=../bad')).toBeNull();
  });
});
