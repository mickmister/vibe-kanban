import { describe, expect, it } from 'vitest';
import { updateHistoricReplayFailures } from './historyReplayState';

describe('updateHistoricReplayFailures', () => {
  it('ignores stale-scope updates', () => {
    const previous = new Set(['process-1']);

    const next = updateHistoricReplayFailures(previous, {
      isCurrentScope: false,
      processId: 'process-2',
      failed: true,
    });

    expect(next).toBe(previous);
    expect([...next]).toEqual(['process-1']);
  });

  it('adds and clears failures for the current scope', () => {
    const withFailure = updateHistoricReplayFailures(new Set<string>(), {
      isCurrentScope: true,
      processId: 'process-1',
      failed: true,
    });

    expect([...withFailure]).toEqual(['process-1']);

    const cleared = updateHistoricReplayFailures(withFailure, {
      isCurrentScope: true,
      processId: 'process-1',
      failed: false,
    });

    expect([...cleared]).toEqual([]);
  });
});
