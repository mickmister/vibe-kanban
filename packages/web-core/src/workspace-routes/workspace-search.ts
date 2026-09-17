import { zodValidator } from '@tanstack/zod-adapter';
import { z } from 'zod';

export const workspaceSearchSchema = z.object({
  sessionId: z.string().optional(),
});

export type WorkspaceSearch = z.infer<typeof workspaceSearchSchema>;

export const workspaceSearchValidator = zodValidator(workspaceSearchSchema);
