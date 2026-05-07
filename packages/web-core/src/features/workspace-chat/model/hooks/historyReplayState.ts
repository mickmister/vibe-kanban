export function updateHistoricReplayFailures(
  previous: Set<string>,
  options: {
    isCurrentScope: boolean;
    processId: string;
    failed: boolean;
  }
): Set<string> {
  if (!options.isCurrentScope) {
    return previous;
  }

  const alreadyFailed = previous.has(options.processId);
  if (options.failed === alreadyFailed) {
    return previous;
  }

  const next = new Set(previous);
  if (options.failed) {
    next.add(options.processId);
  } else {
    next.delete(options.processId);
  }

  return next;
}
