import html from 'solid-js/html';

// 面板区块头部：大写小标题 + 操作按钮槽
export function Header(props) {
  return html`
    <div class=${() => 'flex items-center justify-between h-9 px-3 border-b border-border shrink-0 select-none ' + (props.class || '')}>
      <h2 class="text-[11px] font-semibold uppercase tracking-wider text-text-secondary">${() => props.title}</h2>
      <div class="flex items-center gap-0.5">${props.children}</div>
    </div>
  `;
}
