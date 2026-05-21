type IframeShortcutAction = 'cycle-next' | 'cycle-prev';

type ShortcutDecision =
  | { action: IframeShortcutAction }
  | { action: null; reason: string };

const INSTALL_FLAG = '__vkIframeShortcutBridgeInstalled';
const CAPTURE_PHASE = { capture: true } as const;
const LOG_PREFIX = '[VK iframe shortcuts]';

declare global {
  interface Window {
    [INSTALL_FLAG]?: boolean;
  }
}

function debugLog(message: string, details?: Record<string, unknown>) {
  if (details) {
    console.debug(LOG_PREFIX, message, details);
  } else {
    console.debug(LOG_PREFIX, message);
  }
}

function describeTarget(target: EventTarget | null) {
  if (!(target instanceof Element)) return String(target);

  return {
    tagName: target.tagName,
    id: target.id || undefined,
    className:
      typeof target.className === 'string' && target.className
        ? target.className
        : undefined,
    isContentEditable: (target as HTMLElement).isContentEditable,
  };
}

function describeKeyEvent(event: KeyboardEvent) {
  return {
    key: event.key,
    code: event.code,
    ctrlKey: event.ctrlKey,
    metaKey: event.metaKey,
    altKey: event.altKey,
    shiftKey: event.shiftKey,
    repeat: event.repeat,
    defaultPrevented: event.defaultPrevented,
    isComposing: event.isComposing,
    target: describeTarget(event.target),
  };
}

function isInIframe() {
  try {
    const inIframe = window.self !== window.top;
    debugLog('iframe check completed', {
      inIframe,
      origin: window.location.origin,
      href: window.location.href,
    });
    return inIframe;
  } catch (error) {
    debugLog('iframe check treated as embedded after window.top access error', {
      error,
      origin: window.location.origin,
      href: window.location.href,
    });
    return true;
  }
}

function isTextEditingTarget(target: EventTarget | null) {
  if (!(target instanceof Element)) return false;

  const tagName = target.tagName;
  if (tagName === 'INPUT' || tagName === 'TEXTAREA' || tagName === 'SELECT') {
    return true;
  }

  return Boolean(
    target.closest('[contenteditable]:not([contenteditable="false"])')
  );
}

function isShortcutCandidate(event: KeyboardEvent) {
  return (
    event.ctrlKey ||
    event.metaKey ||
    event.altKey ||
    event.shiftKey ||
    event.key === '[' ||
    event.key === ']' ||
    event.code === 'BracketLeft' ||
    event.code === 'BracketRight'
  );
}

function getIframeShortcutAction(event: KeyboardEvent): ShortcutDecision {
  if (event.defaultPrevented) {
    return { action: null, reason: 'default already prevented' };
  }
  if (event.isComposing) {
    return { action: null, reason: 'IME composition in progress' };
  }
  if (!event.ctrlKey) {
    return { action: null, reason: 'Ctrl key is not pressed' };
  }
  if (event.metaKey) {
    return { action: null, reason: 'Meta key is pressed' };
  }
  if (event.altKey) {
    return { action: null, reason: 'Alt key is pressed' };
  }
  if (event.shiftKey) {
    return { action: null, reason: 'Shift key is pressed' };
  }
  if (isTextEditingTarget(event.target)) {
    return { action: null, reason: 'target is text-editing element' };
  }

  if (event.key === ']' || event.code === 'BracketRight') {
    return { action: 'cycle-next' };
  }

  if (event.key === '[' || event.code === 'BracketLeft') {
    return { action: 'cycle-prev' };
  }

  return { action: null, reason: 'key is not a bracket shortcut' };
}

export function installIframeShortcutBridge() {
  debugLog('install requested', {
    alreadyInstalled: Boolean(window[INSTALL_FLAG]),
    origin: window.location.origin,
    href: window.location.href,
  });

  if (window[INSTALL_FLAG]) {
    debugLog('install skipped because bridge is already installed');
    return;
  }

  if (!isInIframe()) {
    debugLog('install skipped because window is not in an iframe');
    return;
  }

  window[INSTALL_FLAG] = true;
  debugLog('installing keydown listeners');

  const handledEvents = new WeakSet<KeyboardEvent>();
  const onKeyDown = (event: KeyboardEvent) => {
    if (handledEvents.has(event)) {
      if (isShortcutCandidate(event)) {
        debugLog('duplicate keydown ignored', describeKeyEvent(event));
      }
      return;
    }
    handledEvents.add(event);

    const decision = getIframeShortcutAction(event);
    if (!decision.action) {
      if (isShortcutCandidate(event)) {
        debugLog('shortcut candidate rejected', {
          reason: decision.reason,
          event: describeKeyEvent(event),
        });
      }
      return;
    }

    debugLog('shortcut matched; posting message to parent', {
      action: decision.action,
      event: describeKeyEvent(event),
    });

    window.parent.postMessage(
      { type: 'vk-iframe-shortcut', action: decision.action },
      '*'
    );
  };

  window.addEventListener('keydown', onKeyDown, CAPTURE_PHASE);
  document.addEventListener('keydown', onKeyDown, CAPTURE_PHASE);
  debugLog('keydown listeners installed');
}
