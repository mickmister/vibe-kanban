import { describe, expect, it } from 'vitest';

import { formatKitchenTime } from './formatKitchenTime';

describe('formatKitchenTime', () => {
  it('formats an ISO timestamp as browser-local hour and minute', () => {
    const expected = new Intl.DateTimeFormat(undefined, {
      hour: 'numeric',
      minute: '2-digit',
    }).format(new Date('2026-06-04T21:19:28.000Z'));

    expect(formatKitchenTime('2026-06-04T21:19:28.000Z')).toBe(expected);
  });

  it('returns null for missing or invalid timestamps', () => {
    expect(formatKitchenTime(null)).toBeNull();
    expect(formatKitchenTime(undefined)).toBeNull();
    expect(formatKitchenTime('not-a-date')).toBeNull();
  });
});
