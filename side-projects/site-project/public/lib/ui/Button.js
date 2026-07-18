import html from 'solid-js/html';

// solid-js/html 的 ${expr} 对函数值会自调，所以 props 可能是值也可能是 getter
function resolve(prop) {
  return typeof prop === 'function' ? prop() : prop;
}

// 主按钮
export function Button(props) {
  // props: variant ('primary' | 'ghost', 默认 'ghost'), disabled (bool | () => bool), onClick, children, class, title
  return html`
    <button
      title=${props.title}
      class=${() => [
        'inline-flex items-center justify-center gap-1.5 px-3 h-7 text-[12px] font-medium rounded-md border cursor-pointer transition-colors duration-150 select-none',
        'disabled:opacity-45 disabled:cursor-not-allowed',
        props.variant === 'primary'
          ? 'bg-accent text-accent-contrast border-transparent hover:bg-accent-hover'
          : 'bg-transparent text-text-secondary border-border hover:bg-bg-hover hover:text-text',
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
  // props: title (tooltip), onClick, children (icon), class, disabled (bool | () => bool), active (bool | () => bool)
  return html`
    <button
      title=${props.title}
      class=${() => [
        'inline-flex items-center justify-center w-6 h-6 rounded-md border-none bg-transparent cursor-pointer transition-colors duration-150 select-none',
        'disabled:opacity-40 disabled:cursor-not-allowed',
        resolve(props.active) ? 'text-accent' : 'text-text-muted hover:bg-bg-hover hover:text-text',
        props.class || '',
      ].filter(Boolean).join(' ')}
      onClick=${props.onClick}
      disabled=${() => !!resolve(props.disabled)}
    >
      ${props.children}
    </button>
  `;
}
