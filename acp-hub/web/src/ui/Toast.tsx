import { createSignal, For, onMount, type JSX } from 'solid-js';

export interface ToastRecord {
  id: string | number;
  content: JSX.Element;
}

function ToastItem(props: { children: JSX.Element }) {
  const [shown, setShown] = createSignal(false);
  onMount(() => requestAnimationFrame(() => setShown(true)));
  return <div class={`ui-toast ${shown() ? 'is-visible' : ''}`}>{props.children}</div>;
}

/** Polite, non-blocking viewport for short-lived success/information feedback. */
export function ToastViewport(props: { items: readonly ToastRecord[]; label?: string }) {
  return <div class="ui-toast-viewport" role="region" aria-label={props.label ?? '通知'} aria-live="polite" aria-relevant="additions">
    <For each={props.items}>{(item) => <ToastItem>{item.content}</ToastItem>}</For>
  </div>;
}
