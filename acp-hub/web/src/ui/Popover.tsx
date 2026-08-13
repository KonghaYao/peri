import { createEffect, onCleanup, Show, type JSX } from 'solid-js';
import { acquireOverlay } from './overlay-state.mjs';

interface PopoverProps {
  open: boolean;
  id: string;
  label: string;
  trigger: () => HTMLElement | undefined;
  onClose: () => void;
  children: JSX.Element;
  dismissible?: boolean;
}

const FOCUSABLE = 'input,textarea,select,button,a[href],[tabindex]:not([tabindex="-1"])';

export function Popover(props: PopoverProps) {
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
    queueMicrotask(() => panel?.querySelector<HTMLElement>(FOCUSABLE)?.focus());
    const closeFromOutside = (event: PointerEvent) => {
      const target = event.target as Node;
      if (props.dismissible !== false && !panel?.contains(target) && !props.trigger()?.contains(target)) props.onClose();
    };
    const closeFromKeyboard = (event: KeyboardEvent) => {
      if (!overlay.isTop() || event.key !== 'Escape' || props.dismissible === false) return;
      event.preventDefault();
      props.onClose();
    };
    document.addEventListener('pointerdown', closeFromOutside);
    document.addEventListener('keydown', closeFromKeyboard);
    onCleanup(() => {
      document.removeEventListener('pointerdown', closeFromOutside);
      document.removeEventListener('keydown', closeFromKeyboard);
      overlay.release();
    });
  });

  return <Show when={props.open}>
    <div ref={panel} id={props.id} class="ui-popover" role="dialog" aria-label={props.label}>{props.children}</div>
  </Show>;
}
