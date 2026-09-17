import { useEffect, useMemo, useSyncExternalStore } from 'react';
import {
  ActivityClient,
  type ActivityClientFilters,
  type ActivityForegroundState,
} from '@/shared/lib/activityClient';

export function useActivityForeground(
  filters: ActivityClientFilters = {}
): ActivityForegroundState {
  const client = useMemo(
    () => new ActivityClient({ filters }),
    [filters.workspaceId, filters.sessionId]
  );

  useEffect(() => {
    client.start();
    return () => client.stop();
  }, [client]);

  return useSyncExternalStore(
    (listener) => client.subscribe(listener),
    () => client.getSnapshot(),
    () => client.getSnapshot()
  );
}
