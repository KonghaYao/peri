import html from 'solid-js/html';

// solid-js/html 的 ${expr} 对函数值会自调，所以 props 可能是值也可能是 getter
function resolve(prop) {
  return typeof prop === 'function' ? prop() : prop;
}

// 主按钮
export function Button(props) {
  // props: variant ('primary' | 'ghost', 默认 'ghost'), disabled (bool | () => bool), onClick, children, class
  return html`
    <button
      class=${() => [
        'px-3 py-1 text-[13px] rounded border border-border cursor-pointer',
        props.variant === 'primary' ? 'bg-accent text-bg' : 'bg-transparent text-text-muted hover:bg-bg-hover hover:text-text',
        props.class || '',
      ].filter(Boolean).join(' ')}
      onClick=${props.onClick}
      disabled=${() => !!resolve(props.disabled)}
    >
      ${props.children}
    </button>
  `;
}

// 图标按钮（方形，无边框）
export function IconButton(props) {
  // props: title (tooltip), onClick, children (icon char), class, disabled (bool | () => bool)
  return html`
    <button
      title=${props.title}
      class=${() => 'bg-transparent border-none text-text-muted cursor-pointer px-1.5 py-0.5 text-sm hover:bg-bg-hover hover:text-text rounded ' + (props.class || '')}
      onClick=${props.onClick}
      disabled=${() => !!resolve(props.disabled)}
    >
      ${props.children}
    </button>
  `;
}
