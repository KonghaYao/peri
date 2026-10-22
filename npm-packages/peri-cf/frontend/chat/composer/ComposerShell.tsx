import { useEffect, useRef, type ReactNode } from 'react';

export interface ComposerShellProps {
  children: ReactNode;
  onHeightChange?: (height: number) => void;
}

/** 贴底浮层外壳：向时间线发布实际高度，作为底部留白。 */
export function ComposerShell({ children, onHeightChange }: ComposerShellProps) {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const element = ref.current;
    if (!element) return;
    const publish = () => onHeightChange?.(element.offsetHeight);
    publish();
    if (typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver(publish);
    observer.observe(element);
    return () => observer.disconnect();
  }, [onHeightChange]);

  return (
    <div
      ref={ref}
      data-slot="composer-shell"
      className="bg-card/95 supports-[backdrop-filter]:bg-card/80 rounded-3xl border shadow-lg backdrop-blur"
    >
      {children}
    </div>
  );
}
