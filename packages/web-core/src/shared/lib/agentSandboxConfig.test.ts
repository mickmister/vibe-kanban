import { describe, expect, it } from 'vitest';
import {
  buildAgentSandboxConfig,
  parseReadonlyRepoPaths,
} from './agentSandboxConfig';

describe('agent sandbox config builder', () => {
  it('omits sandbox config when disabled', () => {
    expect(
      buildAgentSandboxConfig({
        enabled: false,
        network: 'inherit',
        readonlyRepoPathsInput: 'node_modules',
      })
    ).toBeUndefined();
  });

  it('builds the common DSL payload when enabled', () => {
    const config = buildAgentSandboxConfig({
      enabled: true,
      network: 'none',
      readonlyRepoPathsInput: 'node_modules\ntarget, .venv\nnode_modules',
    });

    expect(config).toMatchObject({
      enabled: true,
      network: 'none',
      readonly_paths: [],
      writable_paths: [],
      readonly_repo_paths: ['node_modules', 'target', '.venv'],
      auth_mounts: [],
      sandbox_home: null,
    });
    expect(config).not.toHaveProperty('backend');
  });

  it('defaults readonly repo paths to node_modules', () => {
    const config = buildAgentSandboxConfig({
      enabled: true,
      network: 'inherit',
      readonlyRepoPathsInput: '',
    });

    expect(config?.readonly_repo_paths).toEqual(['node_modules']);
  });

  it('reports unsupported absolute and glob repo path entries', () => {
    expect(
      parseReadonlyRepoPaths('node_modules\n/tmp/cache\npackages/*')
    ).toEqual({
      paths: ['node_modules'],
      invalid: ['/tmp/cache', 'packages/*'],
    });
  });
});
