import { useEffect, useMemo, useRef, useState } from 'react';
import { ShieldCheckIcon } from '@phosphor-icons/react';
import { Button } from '@vibe/ui/components/Button';
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@vibe/ui/components/Popover';
import { Switch } from '@vibe/ui/components/Switch';
import type { AgentSandboxConfig, SandboxNetworkMode } from 'shared/types';
import {
  buildAgentSandboxConfig,
  parseReadonlyRepoPaths,
  readonlyRepoPathsToInput,
} from '@/shared/lib/agentSandboxConfig';

interface AgentSandboxControlsProps {
  value: AgentSandboxConfig | null | undefined;
  onChange: (config: AgentSandboxConfig | undefined) => void;
  disabled?: boolean;
}

export function AgentSandboxControls({
  value,
  onChange,
  disabled = false,
}: AgentSandboxControlsProps) {
  const [enabled, setEnabled] = useState(() => value?.enabled ?? false);
  const [network, setNetwork] = useState<SandboxNetworkMode>(
    () => value?.network ?? 'inherit'
  );
  const [readonlyRepoPathsInput, setReadonlyRepoPathsInput] = useState(() =>
    readonlyRepoPathsToInput(value)
  );
  const lastEmittedSandboxRef = useRef<AgentSandboxConfig | undefined>(
    undefined
  );

  useEffect(() => {
    if (value === lastEmittedSandboxRef.current) return;
    setEnabled(value?.enabled ?? false);
    setNetwork(value?.network ?? 'inherit');
    setReadonlyRepoPathsInput(readonlyRepoPathsToInput(value));
  }, [value]);

  const parsedReadonlyRepoPaths = useMemo(
    () => parseReadonlyRepoPaths(readonlyRepoPathsInput),
    [readonlyRepoPathsInput]
  );

  const emitChange = (next: {
    enabled?: boolean;
    network?: SandboxNetworkMode;
    readonlyRepoPathsInput?: string;
  }) => {
    const nextEnabled = next.enabled ?? enabled;
    const nextNetwork = next.network ?? network;
    const nextReadonlyRepoPathsInput =
      next.readonlyRepoPathsInput ?? readonlyRepoPathsInput;

    const nextConfig = buildAgentSandboxConfig({
      enabled: nextEnabled,
      network: nextNetwork,
      readonlyRepoPathsInput: nextReadonlyRepoPathsInput,
    });
    lastEmittedSandboxRef.current = nextConfig;
    onChange(nextConfig);
  };

  const handleEnabledChange = (checked: boolean) => {
    setEnabled(checked);
    emitChange({ enabled: checked });
  };

  const handleNetworkChange = (nextNetwork: SandboxNetworkMode) => {
    setNetwork(nextNetwork);
    emitChange({ network: nextNetwork });
  };

  const handleReadonlyRepoPathsChange = (nextInput: string) => {
    setReadonlyRepoPathsInput(nextInput);
    emitChange({ readonlyRepoPathsInput: nextInput });
  };

  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button
          type="button"
          variant="outline"
          size="xs"
          disabled={disabled}
          aria-label="Agent sandbox settings"
          title="Agent sandbox settings"
          className={enabled ? 'border-brand text-brand' : undefined}
        >
          <ShieldCheckIcon className="mr-1 size-icon-base" />
          Sandbox {enabled ? 'on' : 'off'}
        </Button>
      </PopoverTrigger>
      <PopoverContent align="start" className="w-96 space-y-base">
        <div className="space-y-half">
          <div className="flex items-center justify-between gap-base">
            <div>
              <h3 className="text-sm font-medium text-high">Agent sandbox</h3>
              <p className="text-xs text-low">Advanced per-turn isolation.</p>
            </div>
            <Switch
              checked={enabled}
              onCheckedChange={handleEnabledChange}
              disabled={disabled}
              aria-label="Enable agent sandbox"
            />
          </div>
          <p className="text-xs leading-5 text-low">
            The workspace remains writable. Listed repo paths are overlaid
            read-only. Host commands still require explicit approval, and VK
            automatically selects the platform sandbox implementation.
          </p>
        </div>

        {enabled && (
          <div className="space-y-base border-t border-border pt-base">
            <label className="block space-y-half">
              <span className="text-xs font-medium text-normal">Network</span>
              <select
                value={network}
                onChange={(event) =>
                  handleNetworkChange(event.target.value as SandboxNetworkMode)
                }
                className="h-8 w-full rounded-sm border border-border bg-primary px-2 text-sm text-normal"
                disabled={disabled}
              >
                <option value="inherit">Allow network</option>
                <option value="none">No network</option>
              </select>
            </label>

            <label className="block space-y-half">
              <span className="text-xs font-medium text-normal">
                Read-only repo paths
              </span>
              <textarea
                value={readonlyRepoPathsInput}
                onChange={(event) =>
                  handleReadonlyRepoPathsChange(event.target.value)
                }
                placeholder="node_modules
target
.venv"
                disabled={disabled}
                className="min-h-20 w-full rounded-sm border border-border bg-transparent px-3 py-2 text-sm text-normal focus-visible:outline-none disabled:cursor-not-allowed disabled:opacity-50"
              />
              <p className="text-xs text-low">
                Enter relative repo paths separated by commas or new lines. Use
                explicit paths only; glob patterns and absolute paths are not
                supported here.
              </p>
              {parsedReadonlyRepoPaths.invalid.length > 0 && (
                <p className="text-xs text-error">
                  Ignoring unsupported entries:{' '}
                  {parsedReadonlyRepoPaths.invalid.join(', ')}
                </p>
              )}
            </label>
          </div>
        )}
      </PopoverContent>
    </Popover>
  );
}
