import { Window } from 'happy-dom';

let installed = false;

/**
 * Bun 没有内置 DOM：显式安装 happy-dom 全局，供真实事件/焦点测试使用。
 * 每个测试文件独立调用；重复调用是 no-op。
 */
export function installDom(): void {
  if (installed || typeof globalThis.document !== 'undefined') return;
  const window = new Window({ url: 'http://localhost/' });
  const globals: Record<string, unknown> = {
    window, document: window.document, navigator: window.navigator, location: window.location,
    HTMLElement: window.HTMLElement, HTMLButtonElement: window.HTMLButtonElement,
    HTMLTextAreaElement: window.HTMLTextAreaElement, HTMLInputElement: window.HTMLInputElement,
    Element: window.Element, Node: window.Node, DocumentFragment: window.DocumentFragment,
    Event: window.Event, KeyboardEvent: window.KeyboardEvent, MouseEvent: window.MouseEvent,
    FocusEvent: window.FocusEvent, InputEvent: window.InputEvent, CustomEvent: window.CustomEvent,
    getComputedStyle: window.getComputedStyle.bind(window),
    requestAnimationFrame: (callback: FrameRequestCallback) => setTimeout(() => callback(Date.now()), 0),
    cancelAnimationFrame: (handle: number) => clearTimeout(handle),
    ResizeObserver: window.ResizeObserver,
    MutationObserver: window.MutationObserver,
    IntersectionObserver: window.IntersectionObserver,
    DOMRect: window.DOMRect,
  };
  for (const [key, value] of Object.entries(globals)) {
    if (value !== undefined) Object.defineProperty(globalThis, key, { value, configurable: true, writable: true });
  }
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  installed = true;
}
