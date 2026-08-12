import { describe, expect, it } from 'vitest';
import {
  ExecutorActionProvenanceKind,
  type ExecutorAction,
} from 'shared/types';
import {
  getExecutorActionProvenance,
  messageAuthorLabel,
} from './messageProvenance';

describe('message provenance labels', () => {
  it('labels workflow-authored prompts as workflow automation instead of You', () => {
    const action = {
      typ: {
        type: 'CodingAgentInitialRequest',
        prompt: 'Do workflow work',
        executor_config: {
          executor: BaseCodingAgent.CODEX,
          variant: null,
          model_id: null,
          agent_id: null,
          reasoning_id: null,
          permission_policy: null,
        },
        working_dir: null,
      },
      next_action: null,
      provenance: {
        kind: ExecutorActionProvenanceKind.workflow,
        label: 'Workflow automation',
        workflow_run_id: 'run-1',
        workflow_name: 'Dev Review Tester',
        workflow_design_id: 'design-drt',
        workflow_version: 2n,
      },
    } as ExecutorAction;

    const provenance = getExecutorActionProvenance(action);

    expect(messageAuthorLabel(provenance, 'You')).toBe(
      'Dev Review Tester workflow v2'
    );
  });

  it('keeps user-authored prompts labeled as You', () => {
    expect(messageAuthorLabel(null, 'You')).toBe('You');
    expect(
      messageAuthorLabel(
        {
          kind: ExecutorActionProvenanceKind.user,
          label: 'User',
          workflow_run_id: null,
          workflow_name: null,
          workflow_design_id: null,
          workflow_version: null,
        },
        'You'
      )
    ).toBe('You');
  });
});
