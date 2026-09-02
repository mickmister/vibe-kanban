import { describe, expect, it } from 'vitest';
import type { ActivityForegroundState } from '@/shared/lib/activityClient';
import { buildWorkflowActivityIndicatorModel } from './activityForegroundModel';

const now = '2026-08-28T12:00:00.000Z';

function state(
  overrides: Partial<ActivityForegroundState> = {}
): ActivityForegroundState {
  return {
    snapshot: null,
    connection: {
      status: 'connected',
      lastCursor: null,
      lastHeartbeatAt: null,
      lastSnapshotAt: null,
      lastError: null,
      usingPollingFallback: false,
    },
    summary: {
      activeTurnCount: 0,
      pendingTurnCount: 0,
      callbackWaitingCount: 0,
      recentCallbackCount: 0,
    },
    callbacks: [],
    ...overrides,
  };
}

describe('buildWorkflowActivityIndicatorModel', () => {
  it('dedupes workflow callbacks by stable callback id', () => {
    const model = buildWorkflowActivityIndicatorModel(
      state({
        summary: {
          activeTurnCount: 0,
          pendingTurnCount: 0,
          callbackWaitingCount: 1,
          recentCallbackCount: 2,
        },
        callbacks: [
          {
            callback_id: 'callback-1',
            kind: 'workflow_completion',
            status: 'waiting',
            summary_text: 'Old pending callback',
            workflow: {
              run_id: 'run-1',
              name: 'Workflow',
              design_id: null,
              version: null,
            },
            links: [
              { rel: 'workflow_run', href: '/dashboard/workflows/run-1' },
            ],
            created_at: now,
            updated_at: '2026-08-28T12:00:00.000Z',
          },
          {
            callback_id: 'callback-1',
            kind: 'workflow_completion',
            status: 'delivered',
            summary_text: 'Workflow completion response delivered',
            workflow: {
              run_id: 'run-1',
              name: 'Workflow',
              design_id: null,
              version: null,
            },
            links: [
              { rel: 'workflow_run', href: '/dashboard/workflows/run-1' },
            ],
            created_at: now,
            updated_at: '2026-08-28T12:01:00.000Z',
          },
        ],
      })
    );

    expect(model.visible).toBe(true);
    expect(model.items).toHaveLength(1);
    expect(model.items[0]).toMatchObject({
      id: 'callback-1',
      status: 'delivered',
      summaryText: 'Workflow completion response delivered',
      workflowHref: '/dashboard/workflows/run-1',
    });
  });

  it('shows polling fallback connection copy', () => {
    const model = buildWorkflowActivityIndicatorModel(
      state({
        connection: {
          status: 'polling',
          lastCursor: null,
          lastHeartbeatAt: null,
          lastSnapshotAt: null,
          lastError: 'Activity stream unavailable',
          usingPollingFallback: true,
        },
      })
    );

    expect(model.visible).toBe(true);
    expect(model.isDegraded).toBe(true);
    expect(model.connectionText).toBe(
      'Activity updates are using safe polling.'
    );
  });

  it('scrubs product-visible callback strings and links', () => {
    const model = buildWorkflowActivityIndicatorModel(
      state({
        summary: {
          activeTurnCount: 0,
          pendingTurnCount: 0,
          callbackWaitingCount: 1,
          recentCallbackCount: 1,
        },
        callbacks: [
          {
            callback_id: 'callback-1',
            kind: 'workflow_completion',
            status: 'failed',
            summary_text:
              'webhook queue_item /Users/me raw XML provider diagnostics shell bd show git status trigger delivery ID execution process ID',
            workflow: {
              run_id: 'run-1',
              name: 'Workflow /tmp/raw JSON',
              design_id: null,
              version: null,
            },
            links: [
              { rel: 'workflow_run', href: '/dashboard/workflows/run-1' },
              { rel: 'session', href: '/api/sessions/session-1' },
            ],
            created_at: now,
            updated_at: now,
          },
        ],
      })
    );

    const serialized = JSON.stringify(model).toLowerCase();
    for (const forbidden of [
      'webhook',
      'queue_item',
      '/users/',
      '/tmp/',
      'raw xml',
      'raw json',
      'provider diagnostics',
      'shell',
      'bd show',
      'git status',
      'trigger',
      'delivery id',
      'execution process id',
    ]) {
      expect(serialized).not.toContain(forbidden);
    }
  });
});
