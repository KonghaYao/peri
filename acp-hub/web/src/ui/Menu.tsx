import { createEffect, onCleanup, Show, type JSX } from 'solid-js';
import { acquireOverlay } from './overlay-state.mjs';

interface MenuProps {
  open: boolean;
  id: string;
  label: string;
  trigger: () => HTMLElement | undefined;
  onClose: () => void;
  children: JSX.Element;
}

const ITEMS = '[role="menuitem"]:not(:disabled)';

export function Menu(props: MenuProps) {
  let panel: HTMLDivElement | undefined;
  let wasOpen = false;

  createEffect(() => {
    if (!props.open) {
      if (wasOpen) queueMicrotask(() => props.trigger()?.focus());
      wasOpen = false;
      return;
    }
    wasOpen = true;
    const overlay = acquireOverlay(null);
    queueMicrotask(() => panel?.querySelector<HTMLElement>(ITEMS)?.focus());
    const outside = (event: PointerEvent) => {
      const target = event.target as Node;
      if (!panel?.contains(target) && !props.trigger()?.contains(target)) props.onClose();
    };
    const keyboard = (event: KeyboardEvent) => {
      if (!panel || !overlay.isTop()) return;
      if (event.key === 'Escape' || event.key === 'Tab') {
        if (event.key === 'Escape') event.preventDefault();
        props.onClose();
        return;
      }
      if (!['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) return;
      const items = [...panel.querySelectorAll<HTMLElement>(ITEMS)];
      if (!items.length) return;
      event.preventDefault();
      const current = items.indexOf(document.activeElement as HTMLElement);
      const next = event.key === 'Home' ? 0
        : event.key === 'End' ? items.length - 1
          : event.key === 'ArrowDown' ? (current + 1 + items.length) % items.length
            : (current - 1 + items.length) % items.length;
      items[next]?.focus();
    };
    document.addEventListener('pointerdown', outside);
    document.addEventListener('keydown', keyboard);
    onCleanup(() => {
      document.removeEventListener('pointerdown', outside);
      document.removeEventListener('keydown', keyboard);
      overlay.release();
    });
  });

  return <Show when={props.open}>
    <div ref={panel} id={props.id} class="ui-menu" role="menu" aria-label={props.label}>{props.children}</div>
  </Show>;
}
