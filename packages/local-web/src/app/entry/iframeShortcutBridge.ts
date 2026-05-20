type IframeShortcut = 'session-next' | 'session-prev';

const INSTALL_FLAG = '__vkIframeShortcutBridgeInstalled';

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

function getIframeShortcut(event: KeyboardEvent): IframeShortcut | null {
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
    return 'session-next';
  }

  if (event.key === '[' || event.code === 'BracketLeft') {
    return 'session-prev';
  }

  return null;
}

export function installIframeShortcutBridge() {
  if (window[INSTALL_FLAG] || !isInIframe()) return;
  window[INSTALL_FLAG] = true;

  window.addEventListener('keydown', (event) => {
    const shortcut = getIframeShortcut(event);
    if (!shortcut) return;

    window.parent.postMessage({ type: 'vk-iframe-shortcut', shortcut }, '*');
  });
}
