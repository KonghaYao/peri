import { createEffect, onCleanup, Show, type JSX } from 'solid-js';
export function Dialog(props: { open: boolean; title: string; onClose: () => void; children: JSX.Element }) {
  let panel: HTMLDivElement | undefined;
  createEffect(() => {
    if (!props.open) return;
    const previous = document.activeElement as HTMLElement | null;
    queueMicrotask(() => panel?.querySelector<HTMLElement>('input,button')?.focus());
    const key = (e: KeyboardEvent) => {
      if (e.key === 'Escape') props.onClose();
      if (e.key !== 'Tab' || !panel) return;
      const items = [...panel.querySelectorAll<HTMLElement>('input,button,[tabindex]:not([tabindex="-1"])')];
      const edge = e.shiftKey ? items[0] : items.at(-1);
      if (items.length && document.activeElement === edge) { e.preventDefault(); (e.shiftKey ? items.at(-1) : items[0])?.focus(); }
    };
    document.addEventListener('keydown', key);
    onCleanup(() => { document.removeEventListener('keydown', key); previous?.focus(); });
  });
  return <Show when={props.open}><div class="ui-dialog-backdrop" onMouseDown={(e) => e.target === e.currentTarget && props.onClose()}><div ref={panel} class="ui-dialog" role="dialog" aria-modal="true" aria-label={props.title}>{props.children}</div></div></Show>;
}
