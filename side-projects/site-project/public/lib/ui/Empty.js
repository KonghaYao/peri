import html from 'solid-js/html';

export function Empty(props) {
  // props: text (string/accessor), icon (string, 可选), class
  return html`
    <div class=${() => 'flex-1 flex items-center justify-center text-text-muted p-5 ' + (props.class || '')}>
      <div class="text-center">
        ${() => props.icon ? html`<div class="text-2xl mb-2">${props.icon}</div>` : null}
        <div>${props.text}</div>
      </div>
    </div>
  `;
}
