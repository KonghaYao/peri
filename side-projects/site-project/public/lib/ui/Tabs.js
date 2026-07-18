import html from 'solid-js/html';
import { For } from 'solid-js';

// props:
//   tabs: Array<{ id: string, label: string }> | (() => Array<{ id: string, label: string }>)
//   activeId: string | (() => string)
//   onChange: (id) => void
//   class
// 注意：solid-js/html 的 ${expr} 对函数值会自调，所以 props.tabs / props.activeId 可能是值也可能是 getter。
function resolve(prop) {
  return typeof prop === 'function' ? prop() : prop;
}

export function Tabs(props) {
  return html`
    <div class=${() => 'inline-flex items-center gap-0.5 p-0.5 rounded-md bg-bg-secondary ' + (props.class || '')}>
      <${For} each=${() => resolve(props.tabs) || []}>
        ${(tab) => html`
          <button
            class=${() => [
              'px-2.5 h-6 text-[11px] font-medium rounded cursor-pointer border-none transition-colors duration-150',
              resolve(props.activeId) === tab.id
                ? 'bg-bg-tertiary text-text shadow-sm'
                : 'bg-transparent text-text-muted hover:text-text',
            ].join(' ')}
            onClick=${() => props.onChange?.(tab.id)}
          >
            ${tab.label}
          </button>
        `}
      <//>
    </div>
  `;
}
