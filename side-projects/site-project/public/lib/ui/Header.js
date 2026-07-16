import html from 'solid-js/html';

export function Header(props) {
  return html`
    <div class=${() => 'flex items-center justify-between px-2 py-1.5 border-b border-border shrink-0 ' + (props.class || '')}>
      <h2 class="text-[13px] font-semibold">${() => props.title}</h2>
      <div class="flex items-center gap-1">${props.children}</div>
    </div>
  `;
}
