import html from 'solid-js/html';
import { For } from 'solid-js';
import { IconButton } from '/lib/ui/Button.js';

// solid-js/html 的 ${expr} 对函数值会自调，所以 props 可能是值也可能是 getter
function resolve(prop) {
  return typeof prop === 'function' ? prop() : prop;
}

// props:
//   entries: Array<{ key: string, value: string }> | (() => Array<{ key: string, value: string }>)
//   onChange: (next) => void  // 接收新数组
//   keyPlaceholder, valuePlaceholder
//   addLabel: string (默认 '+ add')
export function KVInput(props) {
  const getEntries = () => resolve(props.entries) || [];
  const update = (i, field, value) => {
    const next = getEntries().map((e, idx) => idx === i ? { ...e, [field]: value } : e);
    props.onChange?.(next);
  };
  const remove = (i) => {
    props.onChange?.(getEntries().filter((_, idx) => idx !== i));
  };
  const add = () => {
    props.onChange?.([...getEntries(), { key: '', value: '' }]);
  };

  return html`
    <div class="flex flex-col gap-1">
      <${For} each=${getEntries}>
        ${(entry, i) => html`
          <div class="flex gap-1 items-center">
            <input type="text"
              class="flex-1 min-w-0 bg-bg-secondary border border-border text-text px-2 py-1 text-[13px] rounded focus:border-accent"
              placeholder=${props.keyPlaceholder || 'key'}
              value=${entry.key}
              onInput=${e => update(i(), 'key', e.target.value)}
            />
            <input type="text"
              class="flex-1 min-w-0 bg-bg-secondary border border-border text-text px-2 py-1 text-[13px] rounded focus:border-accent"
              placeholder=${props.valuePlaceholder || 'value'}
              value=${entry.value}
              onInput=${e => update(i(), 'value', e.target.value)}
            />
            <${IconButton} title="remove" onClick=${() => remove(i())}>✕<//>
          </div>
        `}
      <//>
      <button
        class="self-start bg-transparent border border-dashed border-border text-text-muted cursor-pointer px-3 py-1 rounded text-[11px] hover:text-text hover:border-text-muted"
        onClick=${add}
      >
        ${() => props.addLabel || '+ add'}
      </button>
    </div>
  `;
}
