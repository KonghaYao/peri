import { createEffect, splitProps, type JSX } from 'solid-js';

type Props = JSX.TextareaHTMLAttributes<HTMLTextAreaElement> & {
  autoResize?: boolean;
  maxHeight?: number;
};

/** Controlled textarea with the same finite auto-growth contract as Composer. */
export function Textarea(props: Props) {
  const [local, textarea] = splitProps(props, ['autoResize', 'maxHeight', 'class', 'ref']);
  let element: HTMLTextAreaElement | undefined;
  const resize = () => {
    if (!local.autoResize || !element) return;
    element.style.height = 'auto';
    element.style.height = `${Math.min(element.scrollHeight, local.maxHeight ?? 180)}px`;
  };
  createEffect(() => { textarea.value; queueMicrotask(resize); });
  return <textarea {...textarea} ref={(node) => {
    element = node;
    if (typeof local.ref === 'function') local.ref(node);
  }} onInput={(event) => {
    resize();
    const handler = textarea.onInput;
    if (typeof handler === 'function') handler(event);
  }} class={`ui-textarea ${local.class ?? ''}`} />;
}
