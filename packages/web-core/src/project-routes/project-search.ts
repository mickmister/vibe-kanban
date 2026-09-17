import { zodValidator } from '@tanstack/zod-adapter';
import { z } from 'zod';

export const projectSearchSchema = z.object({
  sessionId: z.string().optional(),
});

export type ProjectSearch = z.infer<typeof projectSearchSchema>;

export const projectSearchValidator = zodValidator(projectSearchSchema);
