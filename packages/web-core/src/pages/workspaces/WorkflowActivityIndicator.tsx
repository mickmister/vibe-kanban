import { BellIcon } from '@phosphor-icons/react';
import { cn } from '@vibe/ui/lib/cn';
import { Tooltip } from '@vibe/ui/components/Tooltip';
import { useActivityForeground } from '@/shared/hooks/useActivityForeground';
import {
  buildWorkflowActivityIndicatorModel,
  type WorkflowActivityIndicatorModel,
} from '@/shared/lib/activityForegroundModel';

export function WorkflowActivityIndicatorContainer() {
  const activity = useActivityForeground();
  return (
    <WorkflowActivityIndicatorView
      model={buildWorkflowActivityIndicatorModel(activity)}
    />
  );
}

export function WorkflowActivityIndicatorView({
  model,
}: {
  model: WorkflowActivityIndicatorModel;
}) {
  if (!model.visible) return null;

  const primaryHref =
    model.items.find((item) => item.workflowHref)?.workflowHref ??
    model.items.find((item) => item.sessionHref)?.sessionHref ??
    '/notifications';
  const label = [
    model.title,
    model.connectionText,
    ...model.items.slice(0, 2).map((item) => item.summaryText),
  ].join('. ');

  return (
    <div className="relative group" data-testid="workflow-activity-indicator">
      <Tooltip content={label} side="right">
        <a
          href={primaryHref}
          className={cn(
            'relative flex h-10 w-10 items-center justify-center rounded-lg',
            'bg-panel text-normal transition-colors hover:opacity-80',
            'focus:outline-none focus-visible:ring-2 focus-visible:ring-brand',
            model.isDegraded && 'text-warning'
          )}
          aria-label={label}
        >
          <BellIcon className="h-5 w-5" weight="bold" />
          {model.badgeCount > 0 && (
            <span className="absolute -right-1 -top-2 flex h-[18px] min-w-[18px] items-center justify-center rounded-full bg-brand-secondary px-1 text-[10px] font-medium text-white">
              {model.badgeCount > 99 ? '99+' : model.badgeCount}
            </span>
          )}
        </a>
      </Tooltip>
      <div
        role="status"
        className={cn(
          'pointer-events-none absolute bottom-0 left-12 z-50 hidden w-72 rounded-lg border border-border bg-primary p-base text-sm shadow-xl',
          'group-hover:block group-focus-within:block'
        )}
      >
        <p className="font-medium text-high">{model.title}</p>
        <p className="mt-1 text-low">{model.connectionText}</p>
        {model.items.length > 0 && (
          <ul className="mt-2 space-y-2">
            {model.items.slice(0, 3).map((item) => (
              <li key={item.id} className="text-normal">
                <p className="font-medium text-high">{item.workflowName}</p>
                <p>{item.summaryText}</p>
                {(item.workflowHref || item.sessionHref) && (
                  <p className="mt-1 text-xs text-low">
                    {item.workflowHref ? 'Open workflow' : 'Open session'}
                  </p>
                )}
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
