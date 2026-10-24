import { useCallback, useMemo, useSyncExternalStore } from 'react';

/**
 * 订阅 CSS 媒体查询。SSR/无 matchMedia 环境固定返回 false（宽屏/内联）。
 * 只用于交互语义（dialog/焦点陷阱），布局仍由 Tailwind 断点负责。
 */
export function useMediaQuery(query: string): boolean {
  const mql = useMemo(() => (
    typeof window !== 'undefined' && typeof window.matchMedia === 'function'
      ? window.matchMedia(query)
      : null
  ), [query]);

  const subscribe = useCallback((onChange: () => void) => {
    if (!mql) return () => {};
    mql.addEventListener('change', onChange);
    return () => mql.removeEventListener('change', onChange);
  }, [mql]);

  const getSnapshot = useCallback(() => mql?.matches ?? false, [mql]);
  return useSyncExternalStore(subscribe, getSnapshot, () => false);
}
