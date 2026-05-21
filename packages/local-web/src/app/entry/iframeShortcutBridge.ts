type IframeShortcutAction = 'cycle-next' | 'cycle-prev';

const INSTALL_FLAG = '__vkIframeShortcutBridgeInstalled';
const CAPTURE_PHASE = { capture: true } as const;

declare global {
  interface Window {
    [INSTALL_FLAG]?: boolean;
  }
}

function isInIframe() {
  try {
    return window.self !== window.top;
  } catch {
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

function getIframeShortcutAction(
  event: KeyboardEvent
): IframeShortcutAction | null {
  if (
    event.defaultPrevented ||
    event.isComposing ||
    !event.ctrlKey ||
    event.metaKey ||
    event.altKey ||
    event.shiftKey ||
    isTextEditingTarget(event.target)
  ) {
    return null;
  }

  if (event.key === ']' || event.code === 'BracketRight') {
    return 'cycle-next';
  }

  if (event.key === '[' || event.code === 'BracketLeft') {
    return 'cycle-prev';
  }

  return null;
}

export function installIframeShortcutBridge() {
  if (window[INSTALL_FLAG] || !isInIframe()) return;
  window[INSTALL_FLAG] = true;

  const handledEvents = new WeakSet<KeyboardEvent>();
  const onKeyDown = (event: KeyboardEvent) => {
    if (handledEvents.has(event)) return;
    handledEvents.add(event);

    const action = getIframeShortcutAction(event);
    if (!action) return;

    window.parent.postMessage({ type: 'vk-iframe-shortcut', action }, '*');
  };

  window.addEventListener('keydown', onKeyDown, CAPTURE_PHASE);
  document.addEventListener('keydown', onKeyDown, CAPTURE_PHASE);
}
