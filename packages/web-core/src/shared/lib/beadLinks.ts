export type BeadReferenceTarget = {
  prefix: string;
};

export type BeadReferenceClickMessage = {
  type: 'vk:bead-reference-clicked';
  beadId: string;
  workspaceId?: string;
  sessionId?: string;
  source: 'agent-message';
};

const DEFAULT_PREFIXES = ['vkvw', 'Vktest'];
const BEAD_HASH_PREFIX = '#vk-bead=';

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

export function buildBeadReferenceHref(beadId: string): string {
  return `${BEAD_HASH_PREFIX}${encodeURIComponent(beadId)}`;
}

export function parseBeadReferenceHref(href: string): string | null {
  const hashIndex = href.indexOf(BEAD_HASH_PREFIX);
  if (hashIndex === -1) return null;

  const encoded = href.slice(hashIndex + BEAD_HASH_PREFIX.length);
  if (!encoded) return null;

  try {
    const beadId = decodeURIComponent(encoded);
    return isValidBeadReference(beadId) ? beadId : null;
  } catch {
    return null;
  }
}

export function isValidBeadReference(beadId: string): boolean {
  return /^[A-Za-z][A-Za-z0-9_]*-[A-Za-z0-9][A-Za-z0-9._-]*$/.test(beadId);
}

function markdownAndUrlRanges(segment: string): Array<[number, number]> {
  const ranges: Array<[number, number]> = [];
  const markdownLinkPattern = /\[[^\]\n]+\]\([^\s)]+(?:\s+"[^"]*")?\)/g;
  const urlPattern = /https?:\/\/\S+/g;

  for (const pattern of [markdownLinkPattern, urlPattern]) {
    for (const match of segment.matchAll(pattern)) {
      if (match.index === undefined) continue;
      ranges.push([match.index, match.index + match[0].length]);
    }
  }

  return ranges;
}

function isInRanges(index: number, ranges: Array<[number, number]>): boolean {
  return ranges.some(([start, end]) => index >= start && index < end);
}

function splitInlineCode(line: string): Array<{ text: string; code: boolean }> {
  const parts: Array<{ text: string; code: boolean }> = [];
  let index = 0;

  while (index < line.length) {
    const start = line.indexOf('`', index);
    if (start === -1) {
      parts.push({ text: line.slice(index), code: false });
      break;
    }

    if (start > index)
      parts.push({ text: line.slice(index, start), code: false });

    const tickRun = line.slice(start).match(/^`+/)?.[0] ?? '`';
    const end = line.indexOf(tickRun, start + tickRun.length);
    if (end === -1) {
      parts.push({ text: line.slice(start), code: false });
      break;
    }

    parts.push({ text: line.slice(start, end + tickRun.length), code: true });
    index = end + tickRun.length;
  }

  return parts;
}

function linkifyTextSegment(
  segment: string,
  targets: BeadReferenceTarget[]
): string {
  const validTargets = targets.filter((target) => target.prefix.trim());
  if (validTargets.length === 0) return segment;

  const prefixes = validTargets
    .map((target) => target.prefix)
    .sort((a, b) => b.length - a.length)
    .map(escapeRegExp);
  const beadPattern = new RegExp(
    `(^|[^A-Za-z0-9_-])(${prefixes.join('|')})-([A-Za-z0-9][A-Za-z0-9._-]*)\\b`,
    'g'
  );
  const skipRanges = markdownAndUrlRanges(segment);

  return segment.replace(
    beadPattern,
    (
      match,
      leading: string,
      prefix: string,
      suffix: string,
      offset: number
    ) => {
      const beadStart = offset + leading.length;
      if (isInRanges(beadStart, skipRanges)) return match;

      const beadId = `${prefix}-${suffix}`;
      return `${leading}[${beadId}](${buildBeadReferenceHref(beadId)})`;
    }
  );
}

export function linkifyBeadReferencesMarkdown(
  content: string,
  targets: BeadReferenceTarget[]
): string {
  if (!content || targets.length === 0) return content;

  let inFence = false;
  let fenceMarker: string | null = null;

  return content
    .split(/(\r?\n)/)
    .map((part) => {
      if (part === '\n' || part === '\r\n') return part;

      const fence = part.match(/^\s*(```+|~~~+)/);
      if (fence) {
        if (!inFence) {
          inFence = true;
          fenceMarker = fence[1][0];
        } else if (fenceMarker === fence[1][0]) {
          inFence = false;
          fenceMarker = null;
        }
        return part;
      }

      if (inFence) return part;

      return splitInlineCode(part)
        .map((piece) =>
          piece.code ? piece.text : linkifyTextSegment(piece.text, targets)
        )
        .join('');
    })
    .join('');
}

function parsePrefixList(value: string | undefined): string[] {
  return (value || '')
    .split(',')
    .map((prefix) => prefix.trim())
    .filter(Boolean);
}

export function getConfiguredBeadReferenceTargets(): BeadReferenceTarget[] {
  const prefixes = parsePrefixList(
    import.meta.env.VITE_BEAD_REFERENCE_PREFIXES
  );
  return (prefixes.length > 0 ? prefixes : DEFAULT_PREFIXES).map((prefix) => ({
    prefix,
  }));
}
