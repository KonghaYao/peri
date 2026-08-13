import { createEffect, createUniqueId, onCleanup, Show, type JSX } from 'solid-js';
import { Portal } from 'solid-js/web';
import { acquireOverlay } from './overlay-state.mjs';
export function Dialog(props: { open: boolean; title: string; onClose: () => void; children: JSX.Element; dismissible?: boolean; showHeader?: boolean }) {
  let panel: HTMLDivElement | undefined;
  const titleId = `dialog-title-${createUniqueId()}`;
  createEffect(() => {
    if (!props.open) return;
    const previous = document.activeElement as HTMLElement | null;
    const app = document.getElementById('app');
    const overlay = acquireOverlay(app);
    queueMicrotask(() => panel?.querySelector<HTMLElement>('input,textarea,select,button,a[href],[tabindex]:not([tabindex="-1"])')?.focus());
    const key = (e: KeyboardEvent) => {
      if (!overlay.isTop()) return;
      if (e.key === 'Escape' && props.dismissible !== false) props.onClose();
      if (e.key !== 'Tab' || !panel) return;
      const items = [...panel.querySelectorAll<HTMLElement>('input,textarea,select,button,a[href],[tabindex]:not([tabindex="-1"])')];
      const edge = e.shiftKey ? items[0] : items.at(-1);
      if (items.length && document.activeElement === edge) { e.preventDefault(); (e.shiftKey ? items.at(-1) : items[0])?.focus(); }
    };
    document.addEventListener('keydown', key);
    onCleanup(() => { document.removeEventListener('keydown', key); overlay.release(); previous?.focus(); });
  });
  return <Show when={props.open}><Portal><div class="ui-dialog-backdrop" onMouseDown={(e) => e.target === e.currentTarget && props.dismissible !== false && props.onClose()}><div ref={panel} class="ui-dialog" role="dialog" aria-modal="true" aria-labelledby={titleId}><Show when={props.showHeader} fallback={<h2 id={titleId} class="sr-only">{props.title}</h2>}><header class="ui-dialog__header"><h2 id={titleId}>{props.title}</h2><Show when={props.dismissible !== false}><button type="button" class="ui-dialog__close" aria-label={`关闭${props.title}`} onClick={props.onClose}>×</button></Show></header></Show>{props.children}</div></div></Portal></Show>;
}
