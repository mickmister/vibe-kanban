import { useEffect, useState } from 'react';
import { createFileRoute } from '@tanstack/react-router';
import { sessionsApi } from '@/shared/lib/api';
import { useAppNavigation } from '@/shared/hooks/useAppNavigation';

function SessionDeepLinkRoute() {
  const { sessionId } = Route.useParams();
  const appNavigation = useAppNavigation();
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;

    sessionsApi
      .getById(sessionId)
      .then((session) => {
        if (cancelled) return;
        appNavigation.goToWorkspace(session.workspace_id, session.id, {
          replace: true,
        });
      })
      .catch((caught) => {
        if (cancelled) return;
        setError(caught instanceof Error ? caught.message : String(caught));
      });

    return () => {
      cancelled = true;
    };
  }, [appNavigation, sessionId]);

  return (
    <div className="flex h-full items-center justify-center p-6 text-sm text-normal">
      {error ? `Unable to open VK session: ${error}` : 'Opening VK session…'}
    </div>
  );
}

export const Route = createFileRoute('/_app/sessions/$sessionId')({
  component: SessionDeepLinkRoute,
});
