// ========== Dock 组件 — macOS 风格底部应用栏 ==========

import html from 'solid-js/html';

export function Dock(props) {
  // props.apps: AppDef[]（6 个 app 定义）
  // props.openIds: Set | string[]（当前打开的 app id 集合）
  // props.onActivate(id): 点击 Dock 图标触发

  return html`
    <div class="flex items-end justify-center gap-1 h-12 px-4 pb-1 bg-bg-secondary/80 backdrop-blur-sm border-t border-border shrink-0">
      ${() => (props.apps ?? []).map(app => {
        const isOpen = (props.openIds ?? []).some(id => id === app.id);
        return html`
          <${DockIcon}
            app=${app}
            isOpen=${isOpen}
            onClick=${() => props.onActivate?.(app.id)}
          />
        `;
      })}
    </div>
  `;
}

function DockIcon(props) {
  const tooltipId = 'dock-tt-' + props.app.id;
  return html`
    <button
      class="relative flex flex-col items-center gap-0.5 bg-transparent border-none cursor-pointer px-2 pt-1 pb-0.5 hover:scale-110 transition-transform"
      title=${props.app.name}
      onClick=${props.onClick}
      onMouseEnter=${(e) => showTooltip(e.currentTarget, props.app.name, tooltipId)}
      onMouseLeave=${() => hideTooltip(tooltipId)}>
      <span class="text-lg leading-none">${props.app.icon}</span>
      <span class=${() => 'w-1 h-1 rounded-full ' + (props.isOpen ? 'bg-accent' : 'bg-transparent')} />
    </button>
  `;
}

// 简易 tooltip
function showTooltip(el, text, id) {
  let tt = document.getElementById(id);
  if (!tt) {
    tt = document.createElement('div');
    tt.id = id;
    tt.className = 'fixed bg-bg-secondary text-text text-[10px] px-2 py-0.5 rounded border border-border shadow pointer-events-none z-[9999] whitespace-nowrap';
    document.body.appendChild(tt);
  }
  const r = el.getBoundingClientRect();
  tt.textContent = text;
  tt.style.left = (r.left + r.width / 2 - tt.offsetWidth / 2) + 'px';
  tt.style.top = (r.top - 20) + 'px';
  tt.style.display = 'block';
}
function hideTooltip(id) {
  const tt = document.getElementById(id);
  if (tt) tt.style.display = 'none';
}
