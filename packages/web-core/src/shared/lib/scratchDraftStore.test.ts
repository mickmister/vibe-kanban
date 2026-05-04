import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ScratchType } from '../../../../../shared/types';
import {
  areScratchDraftValuesEqual,
  clearStoredScratchDraft,
  readStoredScratchDraft,
  writeStoredScratchDraft,
} from './scratchDraftStore';

function createStorage(): Storage {
  const values = new Map<string, string>();

  return {
    length: 0,
    clear() {
      values.clear();
    },
    getItem(key: string) {
      return values.get(key) ?? null;
    },
    key(index: number) {
      return [...values.keys()][index] ?? null;
    },
    removeItem(key: string) {
      values.delete(key);
    },
    setItem(key: string, value: string) {
      values.set(key, value);
    },
  };
}

describe('scratchDraftStore', () => {
  beforeEach(() => {
    vi.stubGlobal('window', {
      localStorage: createStorage(),
    });
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('round-trips a stored draft with dirty state', () => {
    writeStoredScratchDraft(ScratchType.DRAFT_TASK, 'issue-1', 'hello', true);

    expect(
      readStoredScratchDraft<string>(ScratchType.DRAFT_TASK, 'issue-1')
    ).toMatchObject({
      value: 'hello',
      dirty: true,
    });
  });

  it('clears a stored draft', () => {
    writeStoredScratchDraft(
      ScratchType.WORKSPACE_NOTES,
      'workspace-1',
      { content: 'notes' },
      false
    );

    clearStoredScratchDraft(ScratchType.WORKSPACE_NOTES, 'workspace-1');

    expect(
      readStoredScratchDraft(ScratchType.WORKSPACE_NOTES, 'workspace-1')
    ).toBeNull();
  });

  it('compares structured values by JSON shape', () => {
    expect(
      areScratchDraftValuesEqual(
        { content: 'same', nested: { ok: true } },
        { content: 'same', nested: { ok: true } }
      )
    ).toBe(true);
    expect(
      areScratchDraftValuesEqual({ content: 'left' }, { content: 'right' })
    ).toBe(false);
  });
});
