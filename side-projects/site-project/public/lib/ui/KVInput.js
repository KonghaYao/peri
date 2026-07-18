import html from 'solid-js/html';
import { For } from 'solid-js';
import { IconButton } from '/lib/ui/Button.js';
import { Icon } from '/lib/ui/icons.js';

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

  const inputCls = 'flex-1 min-w-0 h-7 bg-bg-tertiary border border-border text-text px-2 text-[12px] rounded-md transition-colors duration-150 focus:border-accent placeholder:text-text-muted';

  return html`
    <div class="flex flex-col gap-1.5">
      <${For} each=${getEntries}>
        ${(entry, i) => html`
          <div class="flex gap-1.5 items-center group">
            <input type="text"
              class=${inputCls}
              placeholder=${props.keyPlaceholder || 'key'}
              value=${entry.key}
              onInput=${e => update(i(), 'key', e.target.value)}
            />
            <input type="text"
              class=${inputCls}
              placeholder=${props.valuePlaceholder || 'value'}
              value=${entry.value}
              onInput=${e => update(i(), 'value', e.target.value)}
            />
            <${IconButton} title="删除" class="opacity-0 group-hover:opacity-100" onClick=${() => remove(i())}>
              <${Icon} name="x" class="w-3.5 h-3.5" />
            <//>
          </div>
        `}
      <//>
      <button
        class="self-start inline-flex items-center gap-1 bg-transparent border border-dashed border-border-strong text-text-muted cursor-pointer px-2.5 h-6 rounded-md text-[11px] transition-colors duration-150 hover:text-text hover:border-text-muted"
        onClick=${add}
      >
        <${Icon} name="plus" class="w-3 h-3" />
        ${() => props.addLabel || '添加'}
      </button>
    </div>
  `;
}
