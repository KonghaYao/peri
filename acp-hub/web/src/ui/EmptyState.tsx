import type { JSX } from 'solid-js';

export function EmptyState(props: { title: string; description: string; action?: JSX.Element }) {
  return (
    <div class="ui-empty">
      <div class="ui-empty__mark" aria-hidden="true">✦</div>
      <h2>{props.title}</h2>
      <p>{props.description}</p>
      {props.action}
    </div>
  );
}
