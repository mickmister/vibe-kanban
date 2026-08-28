import type { ActivityForegroundState } from '@/shared/lib/activityClient';
import { productSafeText } from '@/shared/lib/activityClient';
import type { ActivityV1Callback, ActivityV1Link } from 'shared/types';

export interface WorkflowCallbackActivityItem {
  id: string;
  status: ActivityV1Callback['status'];
  summaryText: string;
  workflowName: string;
  workflowHref: string | null;
  sessionHref: string | null;
  updatedAt: string;
}

export interface WorkflowActivityIndicatorModel {
  visible: boolean;
  badgeCount: number;
  title: string;
  connectionText: string;
  isDegraded: boolean;
  items: WorkflowCallbackActivityItem[];
}

export function buildWorkflowActivityIndicatorModel(
  state: ActivityForegroundState
): WorkflowActivityIndicatorModel {
  const items = dedupeCallbacks(state.callbacks).map(callbackToItem);
  const hasActivity =
    state.summary.activeTurnCount > 0 ||
    state.summary.pendingTurnCount > 0 ||
    state.summary.callbackWaitingCount > 0 ||
    items.length > 0;
  const isDegraded =
    state.connection.usingPollingFallback ||
    state.connection.status === 'error' ||
    state.connection.status === 'reconnecting';
  const badgeCount =
    state.summary.activeTurnCount +
    state.summary.pendingTurnCount +
    state.summary.callbackWaitingCount;

  return {
    visible: hasActivity || isDegraded,
    badgeCount,
    title: hasActivity
      ? productSafeText('Workflow activity')
      : productSafeText('Activity stream status'),
    connectionText: connectionText(state.connection.status, isDegraded),
    isDegraded,
    items,
  };
}

function callbackToItem(
  callback: ActivityV1Callback
): WorkflowCallbackActivityItem {
  return {
    id: productSafeText(callback.callback_id),
    status: callback.status,
    summaryText: productSafeText(callback.summary_text),
    workflowName: productSafeText(callback.workflow?.name ?? 'Workflow'),
    workflowHref: linkHref(callback.links, 'workflow_run'),
    sessionHref: linkHref(callback.links, 'session'),
    updatedAt: callback.updated_at,
  };
}

function dedupeCallbacks(
  callbacks: ActivityV1Callback[]
): ActivityV1Callback[] {
  const byId = new Map<string, ActivityV1Callback>();
  for (const callback of callbacks) {
    const existing = byId.get(callback.callback_id);
    if (!existing || callback.updated_at >= existing.updated_at) {
      byId.set(callback.callback_id, callback);
    }
  }
  return [...byId.values()].sort((a, b) =>
    b.updated_at.localeCompare(a.updated_at)
  );
}

function linkHref(links: ActivityV1Link[], rel: string): string | null {
  const href = links.find((link) => link.rel === rel)?.href;
  return href ? productSafeText(href) : null;
}

function connectionText(
  status: ActivityForegroundState['connection']['status'],
  isDegraded: boolean
): string {
  if (isDegraded) return 'Activity updates are using safe polling.';
  if (status === 'connected') return 'Live activity connected.';
  if (status === 'connecting') return 'Connecting activity updates.';
  return 'Activity updates available.';
}
