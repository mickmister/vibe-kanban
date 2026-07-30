import type { AgentSandboxConfig, SandboxNetworkMode } from 'shared/types';

export const DEFAULT_SANDBOX_READONLY_REPO_PATHS = ['node_modules'];

export const DEFAULT_SANDBOX_ENV_ALLOWLIST = [
  'PATH',
  'TERM',
  'LANG',
  'LC_ALL',
  'SSL_CERT_FILE',
  'SSL_CERT_DIR',
  'HTTP_PROXY',
  'HTTPS_PROXY',
  'NO_PROXY',
  'http_proxy',
  'https_proxy',
  'no_proxy',
];

export interface ParsedReadonlyRepoPaths {
  paths: string[];
  invalid: string[];
}

export function parseReadonlyRepoPaths(input: string): ParsedReadonlyRepoPaths {
  const seen = new Set<string>();
  const paths: string[] = [];
  const invalid: string[] = [];

  for (const raw of input.split(/[\n,]/)) {
    const path = raw.trim();
    if (!path) continue;

    const isAbsolute = path.startsWith('/') || /^[A-Za-z]:[\\/]/.test(path);
    const hasGlob = /[*?{\[\]]/.test(path);
    if (isAbsolute || hasGlob) {
      invalid.push(path);
      continue;
    }

    if (!seen.has(path)) {
      seen.add(path);
      paths.push(path);
    }
  }

  return { paths, invalid };
}

export function buildAgentSandboxConfig(params: {
  enabled: boolean;
  network: SandboxNetworkMode;
  readonlyRepoPathsInput: string;
}): AgentSandboxConfig | undefined {
  if (!params.enabled) return undefined;

  const { paths } = parseReadonlyRepoPaths(params.readonlyRepoPathsInput);

  return {
    enabled: true,
    network: params.network,
    readonly_paths: [],
    writable_paths: [],
    readonly_repo_paths:
      paths.length > 0 ? paths : DEFAULT_SANDBOX_READONLY_REPO_PATHS,
    auth_mounts: [],
    env_allowlist: DEFAULT_SANDBOX_ENV_ALLOWLIST,
    sandbox_home: null,
  };
}

export function readonlyRepoPathsToInput(
  config: AgentSandboxConfig | null | undefined
): string {
  const paths = config?.readonly_repo_paths?.length
    ? config.readonly_repo_paths
    : DEFAULT_SANDBOX_READONLY_REPO_PATHS;
  return paths.join('\n');
}
