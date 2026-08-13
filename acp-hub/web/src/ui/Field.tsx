import type { JSX } from 'solid-js';

export function TextField(props: JSX.InputHTMLAttributes<HTMLInputElement> & { label?: string; hint?: string }) {
  return (
    <label class="ui-field">
      {props.label ? <span class="ui-field__label">{props.label}</span> : null}
      <input {...props} class={`ui-input ${props.class ?? ''}`} />
      {props.hint ? <span class="ui-field__hint">{props.hint}</span> : null}
    </label>
  );
}
