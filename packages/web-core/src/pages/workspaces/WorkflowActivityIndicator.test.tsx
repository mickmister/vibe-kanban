/**
 * @vitest-environment jsdom
 */

import React, { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, describe, expect, it } from 'vitest';

(
  globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;
import { WorkflowActivityIndicatorView } from './WorkflowActivityIndicator';
import type { WorkflowActivityIndicatorModel } from '@/shared/lib/activityForegroundModel';

let root: Root | null = null;
let container: HTMLDivElement | null = null;

afterEach(() => {
  if (root) {
    act(() => root?.unmount());
  }
  container?.remove();
  root = null;
  container = null;
});

function render(model: WorkflowActivityIndicatorModel) {
  container = document.createElement('div');
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root?.render(<WorkflowActivityIndicatorView model={model} />);
  });
  return container;
}

describe('WorkflowActivityIndicatorView', () => {
  it('renders product-safe callback summaries and clean source links', () => {
    const node = render({
      visible: true,
      badgeCount: 1,
      title: 'Workflow activity',
      connectionText: 'Live activity connected.',
      isDegraded: false,
      items: [
        {
          id: 'callback-1',
          status: 'delivered',
          summaryText: 'Workflow completion response delivered',
          workflowName: 'Ask teammate',
          workflowHref: '/dashboard/workflows/run-1',
          sessionHref: '/api/sessions/session-1',
          updatedAt: '2026-08-28T12:00:00.000Z',
        },
      ],
    });

    expect(node.textContent).toContain('Workflow activity');
    expect(node.textContent).toContain(
      'Workflow completion response delivered'
    );
    expect(node.textContent).toContain('Ask teammate');
    expect(node.querySelector('a')?.getAttribute('href')).toBe(
      '/dashboard/workflows/run-1'
    );
    expect(node.textContent?.toLowerCase()).not.toContain('queue_item');
    expect(node.textContent?.toLowerCase()).not.toContain('webhook');
  });

  it('renders fallback health copy without requiring a callback', () => {
    const node = render({
      visible: true,
      badgeCount: 0,
      title: 'Activity stream status',
      connectionText: 'Activity updates are using safe polling.',
      isDegraded: true,
      items: [],
    });

    expect(node.textContent).toContain(
      'Activity updates are using safe polling.'
    );
  });
});
