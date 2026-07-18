import html from 'solid-js/html';
import { Icon } from '/lib/ui/icons.js';

export function Empty(props) {
  // props: text (string/accessor), icon (string, 可选), hint (string/accessor, 可选), class
  return html`
    <div class=${() => 'flex-1 flex items-center justify-center text-text-muted p-5 select-none ' + (props.class || '')}>
      <div class="text-center flex flex-col items-center gap-2">
        ${() => props.icon
          ? html`<span class="text-text-muted opacity-60"><${Icon} name=${props.icon} class="w-7 h-7" /></span>`
          : null}
        <div class="text-[12px]">${props.text}</div>
        ${() => props.hint ? html`<div class="text-[11px] text-text-muted opacity-75">${props.hint}</div>` : null}
      </div>
    </div>
  `;
}
