import { useEffect, type RefObject } from 'react';

/** 常见可聚焦元素；disabled 由选择器排除，hidden 元素由浏览器自身跳过。 */
const FOCUSABLE = [
  'a[href]', 'button:not([disabled])', 'textarea:not([disabled])', 'input:not([disabled])',
  'select:not([disabled])', '[tabindex]:not([tabindex="-1"])',
].join(',');

/**
 * 窄屏 drawer/sheet 的焦点陷阱：打开时把焦点移入容器，Tab/Shift+Tab 在容器内循环。
 * 关闭后的焦点归还由触发按钮的调用方负责（ChatShell 的 closeSidebar/closeDetails）。
 */
export function useFocusTrap(active: boolean, containerRef: RefObject<HTMLElement | null>): void {
  useEffect(() => {
    const container = containerRef.current;
    if (!active || !container) return;

    const focusable = (): HTMLElement[] => (
      Array.from(container.querySelectorAll<HTMLElement>(FOCUSABLE))
        .filter((element) => element.getAttribute('aria-hidden') !== 'true')
    );

    if (!container.contains(document.activeElement)) focusable()[0]?.focus();

    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Tab') return;
      const items = focusable();
      if (items.length === 0) {
        event.preventDefault();
        return;
      }
      const current = items.indexOf(document.activeElement as HTMLElement);
      const next = event.shiftKey
        ? (current <= 0 ? items.length - 1 : current - 1)
        : (current === -1 || current === items.length - 1 ? 0 : current + 1);
      event.preventDefault();
      items[next]?.focus();
    };

    container.addEventListener('keydown', onKeyDown);
    return () => container.removeEventListener('keydown', onKeyDown);
  }, [active, containerRef]);
}
