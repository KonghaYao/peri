import { Window } from 'happy-dom';

let installed = false;

class NoopResizeObserver {
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
}

/**
 * Bun has no DOM. Install happy-dom explicitly; every test file that renders React calls this
 * before importing Testing Library.
 */
export function installDom(): void {
  if (installed || typeof globalThis.document !== 'undefined') return;

  const window = new Window({ url: 'http://localhost/' });
  const globals: Record<string, unknown> = {
    window,
    document: window.document,
    navigator: window.navigator,
    location: window.location,
    HTMLElement: window.HTMLElement,
    HTMLButtonElement: window.HTMLButtonElement,
    HTMLTextAreaElement: window.HTMLTextAreaElement,
    HTMLInputElement: window.HTMLInputElement,
    HTMLFormElement: window.HTMLFormElement,
    HTMLAnchorElement: window.HTMLAnchorElement,
    HTMLDivElement: window.HTMLDivElement,
    HTMLSpanElement: window.HTMLSpanElement,
    HTMLParagraphElement: window.HTMLParagraphElement,
    HTMLHeadingElement: window.HTMLHeadingElement,
    HTMLLabelElement: window.HTMLLabelElement,
    HTMLSelectElement: window.HTMLSelectElement,
    HTMLOptionElement: window.HTMLOptionElement,
    HTMLUListElement: window.HTMLUListElement,
    HTMLLIElement: window.HTMLLIElement,
    HTMLTableElement: window.HTMLTableElement,
    HTMLTableRowElement: window.HTMLTableRowElement,
    HTMLTableCellElement: window.HTMLTableCellElement,
    HTMLPreElement: window.HTMLPreElement,
    SVGElement: window.SVGElement,
    Element: window.Element,
    Node: window.Node,
    NodeFilter: window.NodeFilter,
    DocumentFragment: window.DocumentFragment,
    Event: window.Event,
    KeyboardEvent: window.KeyboardEvent,
    MouseEvent: window.MouseEvent,
    FocusEvent: window.FocusEvent,
    InputEvent: window.InputEvent,
    CustomEvent: window.CustomEvent,
    getComputedStyle: window.getComputedStyle.bind(window),
    requestAnimationFrame: (callback: FrameRequestCallback) => setTimeout(() => callback(Date.now()), 0),
    cancelAnimationFrame: (handle: number) => clearTimeout(handle),
    ResizeObserver: NoopResizeObserver,
    MutationObserver: window.MutationObserver,
    IntersectionObserver: window.IntersectionObserver,
    DOMRect: window.DOMRect,
  };

  for (const [key, value] of Object.entries(globals)) {
    if (value !== undefined) {
      Object.defineProperty(globalThis, key, { value, configurable: true, writable: true });
    }
  }
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  installed = true;
}
