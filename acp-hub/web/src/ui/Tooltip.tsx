import { createEffect, createSignal, createUniqueId, onCleanup, Show, type JSX } from 'solid-js';
import { Portal } from 'solid-js/web';

interface TooltipProps {
  content: string;
  children: JSX.Element | ((tooltipId: string) => JSX.Element);
  delay?: number;
  class?: string;
  placement?: 'start' | 'center' | 'end';
}

/** Visible supplemental help for already-accessibly-named controls. */
export function Tooltip(props: TooltipProps) {
  const [open, setOpen] = createSignal(false);
  const tooltipId = `tooltip-${createUniqueId()}`;
  const [position, setPosition] = createSignal({ x: 0, y: 0, above: false });
  let anchor: HTMLSpanElement | undefined;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const clear = () => { if (timer) clearTimeout(timer); timer = undefined; };
  const showPointer = (event: PointerEvent) => {
    if (event.pointerType === 'touch') return;
    clear();
    timer = setTimeout(() => setOpen(true), props.delay ?? 450);
  };
  const hide = () => { clear(); setOpen(false); };
  const place = () => {
    const rect = anchor?.getBoundingClientRect();
    if (!rect) return;
    const placement = props.placement ?? 'center';
    const x = placement === 'start' ? rect.left : placement === 'end' ? rect.right : rect.left + rect.width / 2;
    const above = window.innerHeight - rect.bottom < 56 && rect.top > 56;
    setPosition({ x, y: above ? rect.top - 7 : rect.bottom + 7, above });
  };
  createEffect(() => {
    if (!open()) return;
    place();
    window.addEventListener('resize', place);
    window.addEventListener('scroll', place, true);
    onCleanup(() => {
      window.removeEventListener('resize', place);
      window.removeEventListener('scroll', place, true);
    });
  });
  onCleanup(clear);
  return <span
    ref={anchor}
    class={`ui-tooltip-anchor ${props.class || ''}`.trim()}
    onPointerEnter={showPointer}
    onPointerLeave={hide}
    onClick={hide}
    onFocusIn={() => { clear(); setOpen(true); }}
    onFocusOut={(event) => { if (!event.currentTarget.contains(event.relatedTarget as Node | null)) hide(); }}
    onKeyDown={(event) => { if (event.key === 'Escape') hide(); }}
  >
    {typeof props.children === 'function' ? props.children(tooltipId) : props.children}
    <Show when={open()}><Portal><span id={tooltipId} role="tooltip" style={{ left: `${position().x}px`, top: `${position().y}px` }} class={`ui-tooltip ui-tooltip--${props.placement ?? 'center'} ${position().above ? 'is-above' : ''}`}>{props.content}</span></Portal></Show>
  </span>;
}
