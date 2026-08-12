import type { ExecutorAction, ExecutorActionProvenance } from 'shared/types';

export type ConversationMessageProvenance = ExecutorActionProvenance;

export function getExecutorActionProvenance(
  action: ExecutorAction | null | undefined
): ConversationMessageProvenance | null {
  let current = action;
  while (current) {
    if (current.provenance) return current.provenance;
    current = current.next_action;
  }
  return null;
}

export function messageAuthorLabel(
  provenance: ConversationMessageProvenance | null | undefined,
  fallback: string
): string {
  if (!provenance || provenance.kind === 'user') return fallback;
  if (provenance.workflow_name && provenance.workflow_version != null) {
    return `${provenance.workflow_name} workflow v${provenance.workflow_version}`;
  }
  if (provenance.workflow_name) return `${provenance.workflow_name} workflow`;
  return provenance.label?.trim() || 'Automation';
}
