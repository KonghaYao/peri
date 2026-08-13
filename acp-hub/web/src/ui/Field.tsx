import { createUniqueId, Show, splitProps, type JSX } from 'solid-js';

export function TextField(props: JSX.InputHTMLAttributes<HTMLInputElement> & { label?: string; hint?: string; error?: string }) {
  const [local, input] = splitProps(props, ['label', 'hint', 'error', 'class', 'id', 'aria-describedby']);
  const generated = createUniqueId();
  const id = () => local.id || `field-${generated}`;
  const hintId = () => local.hint ? `${id()}-hint` : undefined;
  const errorId = () => local.error ? `${id()}-error` : undefined;
  const describedBy = () => [local['aria-describedby'], hintId(), errorId()].filter(Boolean).join(' ') || undefined;
  return (
    <div class="ui-field">
      <Show when={local.label}><label class="ui-field__label" for={id()}>{local.label}</label></Show>
      <input {...input} id={id()} aria-invalid={local.error ? 'true' : undefined} aria-describedby={describedBy()} class={`ui-input ${local.class ?? ''}`} />
      <Show when={local.hint}><span id={hintId()} class="ui-field__hint">{local.hint}</span></Show>
      <Show when={local.error}><span id={errorId()} class="ui-error">{local.error}</span></Show>
    </div>
  );
}
