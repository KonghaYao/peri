import { createUniqueId, Show, splitProps, type JSX } from 'solid-js';

type Props = JSX.SelectHTMLAttributes<HTMLSelectElement> & {
  label: string;
  hint?: string;
  error?: string;
};

export function SelectField(props: Props) {
  const [local, select] = splitProps(props, ['label', 'hint', 'error', 'id', 'class', 'children', 'aria-describedby']);
  const generated = createUniqueId();
  const id = () => local.id || `select-${generated}`;
  const hintId = () => `${id()}-hint`;
  const errorId = () => `${id()}-error`;
  const describedBy = () => [local['aria-describedby'], local.hint ? hintId() : '', local.error ? errorId() : ''].filter(Boolean).join(' ') || undefined;
  return <div class="ui-field">
    <label class="ui-field__label" for={id()}>{local.label}</label>
    <select {...select} id={id()} aria-invalid={local.error ? 'true' : undefined} aria-describedby={describedBy()} class={`ui-input ui-select ${local.class ?? ''}`}>{local.children}</select>
    <Show when={local.hint}><span id={hintId()} class="ui-field__hint">{local.hint}</span></Show>
    <Show when={local.error}><span id={errorId()} class="ui-error">{local.error}</span></Show>
  </div>;
}
