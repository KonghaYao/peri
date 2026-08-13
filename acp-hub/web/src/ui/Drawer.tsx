import { createEffect, onCleanup, Show, type JSX } from 'solid-js';
import { acquireOverlay } from './overlay-state.mjs';

const FOCUSABLE = 'button:not(:disabled),input:not(:disabled),textarea:not(:disabled),select:not(:disabled),a[href],[tabindex]:not([tabindex="-1"])';

interface DrawerProps {
  open: boolean;
  modal: boolean;
  label: string;
  onClose: () => void;
  background: () => HTMLElement | undefined;
  children: JSX.Element;
  class?: string;
  ref?: (element: HTMLElement) => void;
}

/** Responsive navigation container: persistent on wide layouts, modal on compact ones. */
export function Drawer(props: DrawerProps) {
  let panel: HTMLElement | undefined;

  createEffect(() => {
    if (!props.modal || !props.open) return;
    const previous = document.activeElement as HTMLElement | null;
    const overlay = acquireOverlay(props.background() ?? null);
    queueMicrotask(() => panel?.querySelector<HTMLElement>(FOCUSABLE)?.focus());
    const keyboard = (event: KeyboardEvent) => {
      if (!overlay.isTop()) return;
      if (event.key === 'Escape') {
        event.preventDefault();
        props.onClose();
        return;
      }
      if (event.key !== 'Tab' || !panel) return;
      const items = [...panel.querySelectorAll<HTMLElement>(FOCUSABLE)];
      if (!items.length) return;
      const edge = event.shiftKey ? items[0] : items.at(-1);
      if (document.activeElement !== edge) return;
      event.preventDefault();
      (event.shiftKey ? items.at(-1) : items[0])?.focus();
    };
    document.addEventListener('keydown', keyboard);
    onCleanup(() => {
      document.removeEventListener('keydown', keyboard);
      overlay.release();
      previous?.focus();
    });
  });

  return <>
    <aside
      ref={(element) => { panel = element; props.ref?.(element); }}
      class={`${props.class || ''} ${props.open ? 'is-open' : ''}`.trim()}
      inert={props.modal && !props.open}
      role={props.modal && props.open ? 'dialog' : undefined}
      aria-modal={props.modal && props.open ? 'true' : undefined}
      aria-label={props.modal && props.open ? props.label : undefined}
    >{props.children}</aside>
    <Show when={props.modal && props.open}>
      <button type="button" class="drawer-scrim" aria-label={`关闭${props.label}`} onClick={props.onClose} />
    </Show>
  </>;
}
