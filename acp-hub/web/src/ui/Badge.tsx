import { splitProps, type JSX } from 'solid-js';

export type BadgeTone = 'neutral' | 'ok' | 'warn' | 'err';

const TONE_CLASS: Record<BadgeTone, string> = {
  ok: 'ui-badge--ok',
  warn: 'ui-badge--warn',
  err: 'ui-badge--err',
  neutral: 'ui-badge--neutral',
};

export function Badge(props: JSX.HTMLAttributes<HTMLSpanElement> & { tone?: BadgeTone }) {
  const [local, span] = splitProps(props, ['tone', 'class', 'children']);
  return <span {...span} class={`ui-badge ${TONE_CLASS[local.tone ?? 'neutral']} ${local.class || ''}`.trim()}>{local.children}</span>;
}
