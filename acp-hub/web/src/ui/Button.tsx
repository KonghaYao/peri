import type { JSX } from 'solid-js';

type Props = JSX.ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: 'primary' | 'ghost' | 'danger';
  busy?: boolean;
};

export function Button(props: Props) {
  const variant = () => props.variant ?? 'ghost';
  return (
    <button
      {...props}
      disabled={props.disabled || props.busy}
      aria-busy={props.busy}
      class={`ui-button ui-button--${variant()} ${props.class ?? ''}`}
    >
      {props.busy ? <span class="ui-spinner" aria-hidden="true" /> : null}
      {props.children}
    </button>
  );
}

export function IconButton(props: Props & { label: string }) {
  return <Button {...props} aria-label={props.label} title={props.title ?? props.label} class={`ui-icon-button ${props.class ?? ''}`} />;
}
