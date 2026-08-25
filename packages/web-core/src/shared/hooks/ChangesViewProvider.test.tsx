/**
 * @vitest-environment jsdom
 */

import React, { act } from 'react';
import { createRoot } from 'react-dom/client';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ChangesViewProvider } from './ChangesViewProvider';
import {
  useChangesView,
  type ScrollToFileCallback,
} from '@/shared/hooks/useChangesView';
import {
  RIGHT_MAIN_PANEL_MODES,
  useUiPreferencesStore,
} from '@/shared/stores/useUiPreferencesStore';

(
  globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

afterEach(() => {
  vi.clearAllMocks();
  useUiPreferencesStore.setState({
    mobileActiveTab: 'chat',
    workspacePanelStates: {},
    isLeftSidebarVisible: true,
  });
});

describe('ChangesViewProvider', () => {
  it('replays a pending selected file once when a scroll callback registers', async () => {
    const { getApi, unmount } = await renderProviderProbe();
    const scrollToFile = vi.fn<ScrollToFileCallback>();

    await act(async () => {
      getApi().scrollToFile('src/file-a.ts', 12);
    });

    expect(scrollToFile).not.toHaveBeenCalled();

    await act(async () => {
      getApi().registerScrollToFile(scrollToFile);
    });

    expect(scrollToFile).toHaveBeenCalledTimes(1);
    expect(scrollToFile).toHaveBeenLastCalledWith('src/file-a.ts', 12);

    await act(async () => {
      getApi().registerScrollToFile(scrollToFile);
    });

    expect(scrollToFile).toHaveBeenCalledTimes(1);

    await unmount();
  });

  it('queues the same file again when it is selected while unregistered', async () => {
    const { getApi, unmount } = await renderProviderProbe();
    const scrollToFile = vi.fn<ScrollToFileCallback>();

    await act(async () => {
      getApi().scrollToFile('src/file-a.ts');
      getApi().registerScrollToFile(scrollToFile);
      getApi().registerScrollToFile(null);
      getApi().scrollToFile('src/file-a.ts');
      getApi().registerScrollToFile(scrollToFile);
    });

    expect(scrollToFile).toHaveBeenCalledTimes(2);
    expect(scrollToFile).toHaveBeenNthCalledWith(1, 'src/file-a.ts', undefined);
    expect(scrollToFile).toHaveBeenNthCalledWith(2, 'src/file-a.ts', undefined);

    await unmount();
  });

  it('replays a file opened from chat after the changes panel mounts', async () => {
    mockMatchMedia(true);

    const { getApi, unmount } = await renderProviderProbe({
      workspaceId: 'workspace-1',
    });
    const scrollToFile = vi.fn<ScrollToFileCallback>();

    await act(async () => {
      getApi().viewFileInChanges('src/from-chat.ts');
    });

    expect(scrollToFile).not.toHaveBeenCalled();

    await act(async () => {
      getApi().registerScrollToFile(scrollToFile);
    });

    expect(scrollToFile).toHaveBeenCalledTimes(1);
    expect(scrollToFile).toHaveBeenCalledWith('src/from-chat.ts', undefined);
    expect(
      useUiPreferencesStore.getState().getWorkspacePanelState('workspace-1')
        .rightMainPanelMode
    ).toBe(RIGHT_MAIN_PANEL_MODES.CHANGES);
    expect(useUiPreferencesStore.getState().mobileActiveTab).toBe('changes');

    await unmount();
  });

  it('does not switch panels when opening a file without a workspace id', async () => {
    mockMatchMedia(true);

    const { getApi, unmount } = await renderProviderProbe();

    await act(async () => {
      getApi().viewFileInChanges('src/from-chat.ts');
    });

    expect(
      useUiPreferencesStore.getState().getWorkspacePanelState('workspace-1')
        .rightMainPanelMode
    ).not.toBe(RIGHT_MAIN_PANEL_MODES.CHANGES);
    expect(useUiPreferencesStore.getState().mobileActiveTab).toBe('chat');

    await unmount();
  });
});

function mockMatchMedia(matches: boolean) {
  Object.defineProperty(window, 'matchMedia', {
    writable: true,
    value: vi.fn().mockImplementation((query: string) => ({
      matches,
      media: query,
      onchange: null,
      addListener: vi.fn(),
      removeListener: vi.fn(),
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      dispatchEvent: vi.fn(),
    })),
  });
}

async function renderProviderProbe({
  workspaceId,
}: { workspaceId?: string } = {}) {
  const container = document.createElement('div');
  document.body.appendChild(container);
  const root = createRoot(container);
  let api: ReturnType<typeof useChangesView> | null = null;

  function Probe() {
    api = useChangesView();
    return null;
  }

  await act(async () => {
    root.render(
      <ChangesViewProvider workspaceId={workspaceId}>
        <Probe />
      </ChangesViewProvider>
    );
  });

  return {
    getApi: () => {
      if (!api) throw new Error('ChangesViewProvider probe did not render');
      return api;
    },
    unmount: async () => {
      await act(async () => {
        root.unmount();
      });
      container.remove();
    },
  };
}
