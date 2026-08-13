import { Show, splitProps, type JSX } from 'solid-js';
import { Tooltip } from './Tooltip';

type Props = JSX.ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: 'primary' | 'secondary' | 'ghost' | 'danger';
  size?: 'compact' | 'default';
  busy?: boolean;
};

export function Button(props: Props) {
  const [local, button] = splitProps(props, ['variant', 'size', 'busy', 'class', 'children', 'disabled']);
  return (
    <button
      {...button}
      type={button.type ?? 'button'}
      disabled={local.disabled || local.busy}
      aria-busy={local.busy || undefined}
      class={`ui-button ui-button--${local.variant ?? 'ghost'} ui-button--${local.size ?? 'default'} ${local.class ?? ''}`}
    >
      <Show when={local.busy}><span class="ui-spinner" aria-hidden="true" /><span class="sr-only">正在处理</span></Show>
      {local.children}
    </button>
  );
}

export function IconButton(props: Props & { label: string; tooltipPlacement?: 'start' | 'center' | 'end' }) {
  const [local, button] = splitProps(props, ['label', 'title', 'class', 'tooltipPlacement']);
  return <Tooltip content={local.title ?? local.label} placement={local.tooltipPlacement}>{(tooltipId) => <Button {...button} aria-label={local.label} aria-describedby={local.title && local.title !== local.label ? tooltipId : undefined} class={`ui-icon-button ${local.class ?? ''}`} />}</Tooltip>;
}
