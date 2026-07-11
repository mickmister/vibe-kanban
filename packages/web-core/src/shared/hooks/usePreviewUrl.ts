import { useEffect, useRef, useState } from 'react';
import { stripAnsi } from 'fancy-ansi';

export interface PreviewUrlInfo {
  url: string;
  port?: number;
  scheme: 'http' | 'https';
}

const urlPatterns = [
  // Full URL pattern. Candidate URLs are parsed and then accepted only if
  // they are local previews or match configured allowed dev server origins.
  /(https?:\/\/[^\s"'<>]+)/gi,
  // Host:port pattern (e.g., localhost:3000, 0.0.0.0:8080)
  /((?:localhost|127\.0\.0\.1|0\.0\.0\.0|\[[0-9a-f:]+\]|(?:\d{1,3}\.){3}\d{1,3})):(\d{2,5})/gi,
];
const LOG_SCAN_BUFFER_LIMIT = 16 * 1024;

const LOOPBACK_HOSTS = new Set([
  'localhost',
  '127.0.0.1',
  '0.0.0.0',
  '::',
  '[::]',
]);

const isIpv4Host = (host: string): boolean =>
  /^\d{1,3}(?:\.\d{1,3}){3}$/.test(host);

type AllowedPreviewOrigin = {
  scheme: 'http' | 'https';
  host: string;
  port: number;
  wildcard: boolean;
};

const defaultPort = (scheme: 'http' | 'https'): number =>
  scheme === 'https' ? 443 : 80;

const normalizeOriginHost = (host: string): string =>
  host.trim().replace(/^\[/, '').replace(/\]$/, '').toLowerCase();

const parseWildcardAllowedOrigin = (
  entry: string
): AllowedPreviewOrigin | null => {
  const [schemePart, remainder] = entry.split('://');
  if (
    !remainder ||
    (schemePart !== 'http' && schemePart !== 'https') ||
    /[/?#@]/.test(remainder.replace(/\/$/, ''))
  ) {
    return null;
  }

  const authority = remainder.endsWith('/')
    ? remainder.slice(0, -1)
    : remainder;
  const portSeparatorIndex = authority.lastIndexOf(':');
  const hostPattern =
    portSeparatorIndex >= 0
      ? authority.slice(0, portSeparatorIndex)
      : authority;
  const portText =
    portSeparatorIndex >= 0 ? authority.slice(portSeparatorIndex + 1) : '';
  const port = portText.length > 0 ? Number(portText) : defaultPort(schemePart);

  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    return null;
  }

  const host = normalizeOriginHost(hostPattern);
  const suffix = host.startsWith('*.') ? host.slice(2) : '';
  if (!validWildcardSuffix(suffix)) {
    return null;
  }

  return {
    scheme: schemePart,
    host: suffix,
    port,
    wildcard: true,
  };
};

const validOriginHost = (host: string): boolean =>
  host.length > 0 && !host.split('.').some((label) => label.length === 0);

const validWildcardSuffix = (suffix: string): boolean => {
  const labels = suffix.split('.');
  if (labels.length < 2 || labels.some((label) => label.length === 0)) {
    return false;
  }

  return labels.every(
    (label) =>
      !label.startsWith('-') &&
      !label.endsWith('-') &&
      /^[A-Za-z0-9-]+$/.test(label)
  );
};

const parseAllowedPreviewOrigin = (
  entry: string
): AllowedPreviewOrigin | null => {
  const trimmed = entry.trim();
  if (!trimmed) return null;

  if (trimmed.includes('*')) {
    return parseWildcardAllowedOrigin(trimmed);
  }

  try {
    const parsed = new URL(trimmed);
    if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') {
      return null;
    }
    if (
      parsed.username ||
      parsed.password ||
      parsed.pathname !== '/' ||
      parsed.search ||
      parsed.hash
    ) {
      return null;
    }

    const scheme = parsed.protocol === 'https:' ? 'https' : 'http';
    const host = normalizeOriginHost(parsed.hostname);
    if (!validOriginHost(host)) {
      return null;
    }

    return {
      scheme,
      host,
      port: parsed.port ? Number(parsed.port) : defaultPort(scheme),
      wildcard: false,
    };
  } catch {
    return null;
  }
};

const wildcardHostMatches = (suffix: string, host: string): boolean => {
  if (!host.endsWith(suffix)) return false;
  const prefix = host.slice(0, -suffix.length);
  if (!prefix.endsWith('.')) return false;
  const label = prefix.slice(0, -1);
  return label.length > 0 && !label.includes('.');
};

const matchesAllowedPreviewOrigin = (
  parsed: URL,
  allowedOrigins: string[]
): boolean => {
  const scheme = parsed.protocol === 'https:' ? 'https' : 'http';
  if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') {
    return false;
  }

  const host = normalizeOriginHost(parsed.hostname);
  const port = parsed.port ? Number(parsed.port) : defaultPort(scheme);

  return allowedOrigins.some((entry) => {
    const allowed = parseAllowedPreviewOrigin(entry);
    if (!allowed || allowed.scheme !== scheme || allowed.port !== port) {
      return false;
    }

    return allowed.wildcard
      ? wildcardHostMatches(allowed.host, host)
      : allowed.host === host;
  });
};

const normalizeDetectedHost = (host: string): string => {
  const normalized = host.toLowerCase();
  if (LOOPBACK_HOSTS.has(normalized)) {
    return 'localhost';
  }

  // Dev servers often print network/private IP addresses in addition to Local.
  // We keep preview stable by preferring localhost for these cases.
  if (isIpv4Host(normalized)) {
    return 'localhost';
  }

  return host;
};

const getUrlParts = (
  url: string
): { hostname: string; port: string } | null => {
  try {
    const parsed = new URL(url);
    return {
      hostname: parsed.hostname,
      port: parsed.port,
    };
  } catch {
    return null;
  }
};

const isLocalPreviewUrl = (url: string): boolean => {
  const parsed = getUrlParts(url);
  if (!parsed) return false;
  return normalizeDetectedHost(parsed.hostname) === 'localhost';
};

const isBetterPreviewUrlCandidate = (
  candidate: PreviewUrlInfo,
  current: PreviewUrlInfo
): boolean => {
  if (candidate.url === current.url) {
    return false;
  }

  const candidateIsLocal = isLocalPreviewUrl(candidate.url);
  const currentIsLocal = isLocalPreviewUrl(current.url);
  if (candidateIsLocal && !currentIsLocal) {
    return true;
  }
  if (!candidateIsLocal && currentIsLocal) {
    return false;
  }

  return false;
};

const getVibeKanbanPort = (): string | null => {
  if (typeof window !== 'undefined' && window.location.port) {
    return window.location.port;
  }
  return null;
};

const isStandaloneHostPortMatch = (
  source: string,
  startIndex: number,
  matchedText: string
): boolean => {
  const before = startIndex > 0 ? source[startIndex - 1] : '';
  const afterIndex = startIndex + matchedText.length;
  const after = afterIndex < source.length ? source[afterIndex] : '';

  // Ignore embedded matches such as "4000.localhost:3009" where the detected
  // "localhost:3009" is just a suffix of a larger hostname.
  if (before && /[A-Za-z0-9_.-]/.test(before)) {
    return false;
  }

  // Reject if token keeps going with hostname-safe chars.
  if (after && /[A-Za-z0-9_.-]/.test(after)) {
    return false;
  }

  return true;
};

const trimMatchedUrlCandidate = (raw: string): string => {
  let candidate = raw.trim();

  while (
    candidate.length > 0 &&
    ['"', "'", '`', '<', '(', '[', '{'].includes(candidate[0])
  ) {
    candidate = candidate.slice(1).trimStart();
  }

  while (
    candidate.length > 0 &&
    ['"', "'", '`', '>', ')', ']', '}', ',', ';'].includes(
      candidate[candidate.length - 1]
    )
  ) {
    candidate = candidate.slice(0, -1).trimEnd();
  }

  return candidate;
};

const toOriginUrlInfo = (
  parsed: URL,
  scheme: 'http' | 'https'
): PreviewUrlInfo => {
  const originOnly = new URL(parsed.origin);
  originOnly.pathname = '/';
  originOnly.search = '';
  originOnly.hash = '';
  return {
    url: originOnly.toString(),
    port: parsed.port ? Number(parsed.port) : undefined,
    scheme,
  };
};

export const detectPreviewUrl = (
  line: string,
  allowedDevServerOrigins: string[] = []
): PreviewUrlInfo | null => {
  const cleaned = stripAnsi(line);
  // Some dev servers split terminal output into chunks, which can break
  // ports as `:40\n00`. Collapse whitespace inside the port before matching.
  const normalized = cleaned.replace(
    /:(\d(?:[\d\s]{0,8}\d))(?=\/|\s|$)/g,
    (_match, rawPort) => `:${rawPort.replace(/\s+/g, '')}`
  );
  const vibeKanbanPort = getVibeKanbanPort();

  const fullUrlPattern = new RegExp(urlPatterns[0]);
  let fullUrlMatch: RegExpExecArray | null;

  while ((fullUrlMatch = fullUrlPattern.exec(normalized)) !== null) {
    try {
      const candidateUrl = trimMatchedUrlCandidate(fullUrlMatch[1]);
      const parsed = new URL(candidateUrl);
      const normalizedHost = normalizeDetectedHost(parsed.hostname);
      const isLocalhost = normalizedHost === 'localhost';

      if (isLocalhost && !parsed.port) {
        // Fall through to host:port pattern detection
      } else {
        if (
          !isLocalhost &&
          !matchesAllowedPreviewOrigin(parsed, allowedDevServerOrigins)
        ) {
          continue;
        }

        parsed.hostname = normalizedHost;

        if (vibeKanbanPort && parsed.port === vibeKanbanPort) {
          continue;
        }

        const scheme = parsed.protocol === 'https:' ? 'https' : 'http';
        return toOriginUrlInfo(parsed, scheme);
      }
    } catch {
      // Ignore invalid URLs and keep scanning.
    }
  }

  const hostPortPattern = new RegExp(urlPatterns[1]);
  let hostPortMatch: RegExpExecArray | null;

  while ((hostPortMatch = hostPortPattern.exec(normalized)) !== null) {
    if (
      !isStandaloneHostPortMatch(
        normalized,
        hostPortMatch.index,
        hostPortMatch[0]
      )
    ) {
      continue;
    }

    const host = normalizeDetectedHost(hostPortMatch[1]);
    const port = Number(hostPortMatch[2]);

    if (vibeKanbanPort && String(port) === vibeKanbanPort) {
      continue;
    }

    const scheme = /https/i.test(normalized) ? 'https' : 'http';
    const originOnly = new URL(`${scheme}://${host}:${port}`);
    originOnly.pathname = '/';
    originOnly.search = '';
    originOnly.hash = '';
    return {
      url: originOnly.toString(),
      port,
      scheme: scheme as 'http' | 'https',
    };
  }

  return null;
};

function detectPreviewUrlFromBuffer(
  buffer: string,
  blockedPort?: number,
  allowedDevServerOrigins: string[] = []
): PreviewUrlInfo | null {
  const lines = buffer.split(/\r?\n/);
  let best: PreviewUrlInfo | null = null;

  // Prefer the newest entries first so stale older matches don't block detection.
  for (let i = lines.length - 1; i >= 0; i -= 1) {
    const line = lines[i];
    if (!line) continue;

    const detected = detectPreviewUrl(line, allowedDevServerOrigins);
    if (!detected || (blockedPort && detected.port === blockedPort)) {
      continue;
    }

    if (!best || isBetterPreviewUrlCandidate(detected, best)) {
      best = detected;
    }
  }
  if (best) return best;

  // Fallback for URLs split across chunk boundaries where line-by-line matching fails.
  const fallback = detectPreviewUrl(buffer, allowedDevServerOrigins);
  if (fallback && blockedPort && fallback.port === blockedPort) {
    return null;
  }
  return fallback;
}

export function usePreviewUrl(
  logs: Array<{ content: string }> | undefined,
  previewProxyPort?: number,
  allowedDevServerOrigins: string[] = []
): PreviewUrlInfo | undefined {
  const [urlInfo, setUrlInfo] = useState<PreviewUrlInfo | undefined>();
  const lastIndexRef = useRef(0);
  const logBufferRef = useRef('');
  const allowedDevServerOriginsKey = allowedDevServerOrigins.join('\0');
  const lastAllowedDevServerOriginsKeyRef = useRef(allowedDevServerOriginsKey);

  useEffect(() => {
    const allowedOriginsChanged =
      lastAllowedDevServerOriginsKeyRef.current !== allowedDevServerOriginsKey;
    lastAllowedDevServerOriginsKeyRef.current = allowedDevServerOriginsKey;

    if (!logs) {
      setUrlInfo(undefined);
      lastIndexRef.current = 0;
      logBufferRef.current = '';
      return;
    }

    // Reset if logs were cleared (new process started)
    if (logs.length < lastIndexRef.current) {
      lastIndexRef.current = 0;
      setUrlInfo(undefined);
      logBufferRef.current = '';
    }

    const hasBlockedUrl =
      Boolean(previewProxyPort) && urlInfo?.port === previewProxyPort;
    if (hasBlockedUrl) {
      setUrlInfo(undefined);
      lastIndexRef.current = 0;
      logBufferRef.current = '';
    }

    // Scan new log entries for URL
    let detectedUrl: PreviewUrlInfo | undefined;
    const newEntries = logs.slice(lastIndexRef.current);
    if (newEntries.length > 0) {
      const chunk = newEntries.map((entry) => entry.content).join('');
      const merged = `${logBufferRef.current}${chunk}`;
      logBufferRef.current =
        merged.length > LOG_SCAN_BUFFER_LIMIT
          ? merged.slice(-LOG_SCAN_BUFFER_LIMIT)
          : merged;
      detectedUrl =
        detectPreviewUrlFromBuffer(
          logBufferRef.current,
          previewProxyPort,
          allowedDevServerOrigins
        ) ?? undefined;
    }

    if (
      !detectedUrl &&
      !urlInfo &&
      allowedOriginsChanged &&
      logBufferRef.current
    ) {
      detectedUrl =
        detectPreviewUrlFromBuffer(
          logBufferRef.current,
          previewProxyPort,
          allowedDevServerOrigins
        ) ?? undefined;
    }

    if (detectedUrl) {
      setUrlInfo((prev) => {
        if (!prev) return detectedUrl;
        return isBetterPreviewUrlCandidate(detectedUrl, prev)
          ? detectedUrl
          : prev;
      });
    }

    lastIndexRef.current = logs.length;
  }, [
    logs,
    urlInfo,
    previewProxyPort,
    allowedDevServerOrigins,
    allowedDevServerOriginsKey,
  ]);

  return urlInfo;
}
