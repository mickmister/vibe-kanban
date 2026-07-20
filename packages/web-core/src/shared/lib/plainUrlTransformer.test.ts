import { describe, expect, it } from 'vitest';
import { $convertFromMarkdownString } from '@lexical/markdown';
import { $isLinkNode, LinkNode } from '@lexical/link';
import { $getRoot, createEditor } from 'lexical';
import {
  CHAT_MARKDOWN_TRANSFORMERS,
  trimPlainUrlMatch,
} from './plainUrlTransformer';

function getFirstInlineNode(markdown: string) {
  const editor = createEditor({
    nodes: [LinkNode],
    onError: (error) => {
      throw error;
    },
  });

  editor.update(
    () => {
      $convertFromMarkdownString(markdown, CHAT_MARKDOWN_TRANSFORMERS);
    },
    { discrete: true }
  );

  return editor.getEditorState().read(() => {
    const paragraph = $getRoot().getFirstChildOrThrow();
    const node = paragraph.getFirstChildOrThrow();

    return {
      isLink: $isLinkNode(node),
      text: node.getTextContent(),
      url: $isLinkNode(node) ? node.getURL() : null,
    };
  });
}

describe('plain URL Markdown transformer', () => {
  it('imports a plain HTTPS URL as a link node', () => {
    const node = getFirstInlineNode('https://example.com/path?x=1#top');

    expect(node).toEqual({
      isLink: true,
      text: 'https://example.com/path?x=1#top',
      url: 'https://example.com/path?x=1#top',
    });
  });

  it('leaves trailing sentence punctuation outside the URL', () => {
    expect(trimPlainUrlMatch('https://example.com/path.')).toBe(
      'https://example.com/path'
    );
    expect(trimPlainUrlMatch('(https://example.com/path)')).toBe(
      '(https://example.com/path)'
    );
    expect(trimPlainUrlMatch('https://example.com/path)')).toBe(
      'https://example.com/path'
    );
    expect(trimPlainUrlMatch('https://example.com/path.)')).toBe(
      'https://example.com/path'
    );
  });

  it('keeps explicit Markdown link syntax working', () => {
    const node = getFirstInlineNode('[docs](https://example.com/docs)');

    expect(node).toEqual({
      isLink: true,
      text: 'docs',
      url: 'https://example.com/docs',
    });
  });
});
