import { type RefObject, useEffect, useState } from 'react';

const DEFAULT_COMPACT_HEIGHT_THRESHOLD = 720;

/**
 * Returns true when the referenced container's available height is small
 * enough that surrounding UI chrome should collapse to prioritize content.
 */
export function useCompactHeight<T extends HTMLElement>(
  containerRef: RefObject<T | null>,
  threshold = DEFAULT_COMPACT_HEIGHT_THRESHOLD
): boolean {
  const [isCompactHeight, setIsCompactHeight] = useState(false);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const update = (nextHeight?: number) => {
      const height = nextHeight ?? container.getBoundingClientRect().height;
      setIsCompactHeight(height <= threshold);
    };

    update();

    if (typeof ResizeObserver === 'undefined') return;

    const observer = new ResizeObserver((entries) => {
      update(entries[0]?.contentRect.height);
    });

    observer.observe(container);
    return () => observer.disconnect();
  }, [containerRef, threshold]);

  return isCompactHeight;
}
