import { createFileRoute } from '@tanstack/react-router';
import { Workspaces } from '@/pages/workspaces/Workspaces';
import { workspaceSearchValidator } from '@vibe/web-core/workspace-search';

export const Route = createFileRoute(
  '/_app/hosts/$hostId/workspaces_/$workspaceId'
)({
  validateSearch: workspaceSearchValidator,
  component: Workspaces,
});
