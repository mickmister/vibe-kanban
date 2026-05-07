import { describe, expect, it } from 'vitest';
import { getWsRetryDecision, markWsStreamHealthy } from './wsStreamRetryPolicy';

describe('wsStreamRetryPolicy', () => {
  it('resets retry attempts only after a payload arrives', () => {
    const state = markWsStreamHealthy({
      retryAttempts: 4,
      hasReceivedPayload: false,
    });

    expect(state).toEqual({
      retryAttempts: 0,
      hasReceivedPayload: true,
    });
  });

  it('does not reset healthy streams repeatedly', () => {
    const state = {
      retryAttempts: 2,
      hasReceivedPayload: true,
    };

    expect(markWsStreamHealthy(state)).toBe(state);
  });

  it('bounds retries based on consecutive failures', () => {
    expect(
      getWsRetryDecision({ retryAttempts: 5, hasReceivedPayload: false }, 6)
    ).toEqual({
      attempt: 6,
      shouldRetry: true,
    });

    expect(
      getWsRetryDecision({ retryAttempts: 6, hasReceivedPayload: false }, 6)
    ).toEqual({
      attempt: 7,
      shouldRetry: false,
    });
  });
});
