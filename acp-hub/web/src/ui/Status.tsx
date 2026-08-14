import { splitProps, type JSX } from 'solid-js';

export type StatusTone = 'idle' | 'ok' | 'warn' | 'err';

type Props = JSX.HTMLAttributes<HTMLDivElement> & {
  tone?: StatusTone;
  live?: boolean;
};

/** Compact semantic status used by navigation and connection surfaces. */
export function Status(props: Props) {
  const [local, div] = splitProps(props, ['tone', 'live', 'class', 'children']);
  return <div {...div} role="status" aria-live={local.live ? 'polite' : undefined} class={`ui-status ui-status--${local.tone ?? 'idle'} ${local.class ?? ''}`}><span class="ui-status__dot" aria-hidden="true" /><span class="ui-status__label">{local.children}</span></div>;
}
