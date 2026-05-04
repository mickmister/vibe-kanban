import { useEffect, useState, useRef } from 'react';
import { produce } from 'immer';
import type { Operation } from 'rfc6902';
import { applyUpsertPatch } from '@/shared/lib/jsonPatch';
import { openLocalApiWebSocket } from '@/shared/lib/localApiTransport';

type WsJsonPatchMsg = { JsonPatch: Operation[] };
type WsReadyMsg = { Ready: true };
type WsFinishedMsg = { finished: boolean };
type WsMsg = WsJsonPatchMsg | WsReadyMsg | WsFinishedMsg;

interface UseJsonPatchStreamOptions<T> {
  /**
   * Called once when the stream starts to inject initial data
   */
  injectInitialEntry?: (data: T) => void;
  /**
   * Filter/deduplicate patches before applying them
   */
  deduplicatePatches?: (patches: Operation[]) => Operation[];
  /**
   * Reconnect after a clean close. Defaults to true for long-lived streams.
   */
  reconnectOnCleanClose?: boolean;
}

interface UseJsonPatchStreamResult<T> {
  data: T | undefined;
  isConnected: boolean;
  isInitialized: boolean;
  isReconnecting: boolean;
  error: string | null;
}

/**
 * Generic hook for consuming WebSocket streams that send JSON messages with patches
 */
export const useJsonPatchWsStream = <T extends object>(
  endpoint: string | undefined,
  enabled: boolean,
  initialData: () => T,
  options?: UseJsonPatchStreamOptions<T>
): UseJsonPatchStreamResult<T> => {
  const [data, setData] = useState<T | undefined>(undefined);
  const [isConnected, setIsConnected] = useState(false);
  const [isInitialized, setIsInitialized] = useState(false);
  const [isReconnecting, setIsReconnecting] = useState(false);
  const initializedForEndpointRef = useRef<string | undefined>(undefined);
  const [error, setError] = useState<string | null>(null);
  const wsRef = useRef<WebSocket | null>(null);
  const dataRef = useRef<T | undefined>(undefined);
  const retryTimerRef = useRef<number | null>(null);
  const retryAttemptsRef = useRef<number>(0);
  const [retryNonce, setRetryNonce] = useState(0);
  const finishedRef = useRef<boolean>(false);
  const endpointIdentityRef = useRef<string | undefined>(undefined);
  const manualCloseRef = useRef(false);

  const injectInitialEntry = options?.injectInitialEntry;
  const deduplicatePatches = options?.deduplicatePatches;
  const reconnectOnCleanClose = options?.reconnectOnCleanClose ?? true;

  function scheduleReconnect() {
    if (retryTimerRef.current) return; // already scheduled
    if (initializedForEndpointRef.current === endpointIdentityRef.current) {
      setIsReconnecting(true);
    }
    // Exponential backoff with cap: 1s, 2s, 4s, 8s (max), then stay at 8s
    const attempt = retryAttemptsRef.current;
    const delay = Math.min(8000, 1000 * Math.pow(2, attempt));
    retryTimerRef.current = window.setTimeout(() => {
      retryTimerRef.current = null;
      setRetryNonce((n) => n + 1);
    }, delay);
  }

  useEffect(() => {
    if (!enabled || !endpoint) {
      manualCloseRef.current = true;
      if (wsRef.current) {
        wsRef.current.close();
        wsRef.current = null;
      }
      if (retryTimerRef.current) {
        window.clearTimeout(retryTimerRef.current);
        retryTimerRef.current = null;
      }
      retryAttemptsRef.current = 0;
      finishedRef.current = false;
      setData(undefined);
      setIsConnected(false);
      setIsInitialized(false);
      setIsReconnecting(false);
      setError(null);
      dataRef.current = undefined;
      initializedForEndpointRef.current = undefined;
      endpointIdentityRef.current = undefined;
      return;
    }

    const endpointChanged = endpointIdentityRef.current !== endpoint;
    endpointIdentityRef.current = endpoint;

    if (endpointChanged) {
      manualCloseRef.current = true;
      if (wsRef.current) {
        wsRef.current.close();
        wsRef.current = null;
      }
      if (retryTimerRef.current) {
        window.clearTimeout(retryTimerRef.current);
        retryTimerRef.current = null;
      }
      retryAttemptsRef.current = 0;
      finishedRef.current = false;
      initializedForEndpointRef.current = undefined;
      dataRef.current = initialData();
      setData(undefined);
      setIsConnected(false);
      setIsInitialized(false);
      setIsReconnecting(false);
      setError(null);
      if (injectInitialEntry) {
        injectInitialEntry(dataRef.current);
      }
    } else if (!dataRef.current) {
      dataRef.current = initialData();
      if (injectInitialEntry) {
        injectInitialEntry(dataRef.current);
      }
    }

    let cancelled = false;

    // Create WebSocket if it doesn't exist
    if (!wsRef.current) {
      // Reset finished flag for new connection
      finishedRef.current = false;

      void (async () => {
        try {
          manualCloseRef.current = false;
          const ws = await openLocalApiWebSocket(endpoint);

          if (cancelled) {
            ws.close();
            return;
          }

          const handleOpen = () => {
            setError(null);
            setIsConnected(true);
            setIsReconnecting(false);
            // Reset backoff on successful connection
            retryAttemptsRef.current = 0;
            if (retryTimerRef.current) {
              window.clearTimeout(retryTimerRef.current);
              retryTimerRef.current = null;
            }
          };
          ws.onopen = handleOpen;

          ws.onmessage = (event) => {
            try {
              const msg: WsMsg = JSON.parse(event.data);

              // Handle JsonPatch messages (same as SSE json_patch event)
              if ('JsonPatch' in msg) {
                const patches: Operation[] = msg.JsonPatch;
                const filtered = deduplicatePatches
                  ? deduplicatePatches(patches)
                  : patches;

                const current = dataRef.current;
                if (!filtered.length || !current) return;

                // Use Immer for structural sharing - only modified parts get new references
                const next = produce(current, (draft) => {
                  applyUpsertPatch(draft, filtered);
                });

                dataRef.current = next;
                setData(next);
              }

              // Handle Ready messages (initial data has been sent)
              if ('Ready' in msg) {
                initializedForEndpointRef.current = endpoint;
                setIsInitialized(true);
                setIsReconnecting(false);
                setError(null);
              }

              // Handle finished messages ({finished: true})
              // Treat finished as terminal - do NOT reconnect
              if ('finished' in msg) {
                finishedRef.current = true;
                manualCloseRef.current = true;
                ws.close(1000, 'finished');
                wsRef.current = null;
                setIsConnected(false);
                setIsReconnecting(false);
              }
            } catch (err) {
              console.error('Failed to process WebSocket message:', err);
              setError('Failed to process stream update');
            }
          };

          ws.onerror = () => {
            // Don't set error here — onclose always fires after onerror
            // and handles retry logic. Setting error eagerly hides data
            // that was already received.
          };

          ws.onclose = (evt) => {
            setIsConnected(false);
            wsRef.current = null;

            const cleanTerminalClose =
              evt?.code === 1000 && evt?.wasClean && !reconnectOnCleanClose;

            if (cancelled || manualCloseRef.current || finishedRef.current) {
              setIsReconnecting(false);
              return;
            }

            if (cleanTerminalClose) {
              setIsReconnecting(false);
              return;
            }

            // Otherwise, reconnect on unexpected/error closures
            retryAttemptsRef.current += 1;
            if (
              initializedForEndpointRef.current !== endpoint &&
              retryAttemptsRef.current > 6
            ) {
              setError('Connection failed');
            }
            scheduleReconnect();
          };

          wsRef.current = ws;
          if (ws.readyState === WebSocket.OPEN) {
            handleOpen();
          }
        } catch (error) {
          if (cancelled) {
            return;
          }

          console.error('Failed to open WebSocket stream:', error);
          retryAttemptsRef.current += 1;
          if (initializedForEndpointRef.current === endpoint) {
            setIsReconnecting(true);
          }
          if (
            initializedForEndpointRef.current !== endpoint &&
            retryAttemptsRef.current > 6
          ) {
            setError('Connection failed');
          }
          scheduleReconnect();
        }
      })();
    }

    return () => {
      cancelled = true;
      if (wsRef.current) {
        manualCloseRef.current = true;
        const ws = wsRef.current;

        // Clear all event handlers first to prevent callbacks after cleanup
        ws.onopen = null;
        ws.onmessage = null;
        ws.onerror = null;
        ws.onclose = null;

        // Close regardless of state
        ws.close();
        wsRef.current = null;
      }
      if (retryTimerRef.current) {
        window.clearTimeout(retryTimerRef.current);
        retryTimerRef.current = null;
      }
    };
  }, [
    endpoint,
    enabled,
    initialData,
    injectInitialEntry,
    deduplicatePatches,
    reconnectOnCleanClose,
    retryNonce,
  ]);

  const isInitializedForCurrentEndpoint =
    isInitialized && initializedForEndpointRef.current === endpoint;

  return {
    data,
    isConnected,
    isInitialized: isInitializedForCurrentEndpoint,
    isReconnecting,
    error,
  };
};
