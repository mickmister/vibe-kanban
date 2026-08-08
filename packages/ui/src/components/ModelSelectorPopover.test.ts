import { describe, expect, it } from 'vitest';
import {
  getProviderFilterState,
  providerMatchesSearch,
} from './ModelSelectorPopover';
import { modelMatchesSearch, type ModelListModel } from './ModelList';

const openRouterModel: ModelListModel = {
  id: 'anthropic/claude-future',
  name: 'Claude Future',
  provider_id: 'openrouter',
  reasoning_options: [],
};

describe('model selector filtering', () => {
  it('matches provider-prefixed model IDs for discovered OpenRouter models', () => {
    expect(
      modelMatchesSearch(openRouterModel, 'openrouter/anthropic/claude')
    ).toBe(true);
  });

  it('matches provider names and IDs', () => {
    expect(
      providerMatchesSearch(
        { id: 'openrouter', name: 'OpenRouter' },
        'openrouter'
      )
    ).toBe(true);
  });

  it('auto-expands the first provider matching the active search', () => {
    const state = getProviderFilterState(
      {
        providers: [
          { id: 'anthropic', name: 'Anthropic' },
          { id: 'openrouter', name: 'OpenRouter' },
        ],
        models: [
          {
            id: 'opus',
            name: 'Opus',
            provider_id: 'anthropic',
            reasoning_options: [],
          },
          openRouterModel,
        ],
      },
      'claude future',
      'anthropic'
    );

    expect(state.visibleProviderIds).toEqual(['openrouter']);
    expect(state.activeProviderId).toBe('openrouter');
  });

  it('keeps canonical provider ids visible for explicit OpenRouter models', () => {
    const state = getProviderFilterState(
      {
        providers: [{ id: 'openrouter', name: 'OpenRouter' }],
        models: [
          {
            ...openRouterModel,
            provider_id: 'openrouter',
          },
        ],
      },
      'openrouter/anthropic/claude',
      ''
    );

    expect(state.visibleProviderIds).toEqual(['openrouter']);
    expect(state.activeProviderId).toBe('openrouter');
  });
});
