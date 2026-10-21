/** Web 页面共用的外壳与转义工具。 */

const PAGE_STYLE = `
  :root { color-scheme: light dark; }
  * { box-sizing: border-box; }
  body { margin: 0; min-height: 100vh; display: grid; place-items: center; padding: 24px;
    font: 16px/1.6 ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif;
    background: #f6f7f9; color: #1c1e21; }
  main { width: min(560px, 100%); background: #fff; border: 1px solid #e3e5e8; border-radius: 16px;
    padding: 28px; box-shadow: 0 8px 28px rgb(0 0 0 / 6%); }
  h1 { margin: 0 0 4px; font-size: 24px; line-height: 1.3; }
  .muted { color: #6b7280; margin: 0 0 18px; }
  body.wide { display: block; min-height: 100dvh; height: 100dvh; padding: 0; overflow: hidden;
    background: #ededed; }
  body.wide main { width: 100%; max-width: none; height: 100dvh; padding: 0; border: 0;
    border-radius: 0; box-shadow: none; background: transparent; }
  dl { display: grid; grid-template-columns: auto 1fr; gap: 8px 12px; margin: 0 0 20px; }
  dt { color: #6b7280; }
  dd { margin: 0; overflow-wrap: anywhere; }
  code { font: 13px/1.5 ui-monospace, SFMono-Regular, Menlo, monospace;
    background: #f2f4f7; border-radius: 6px; padding: 2px 6px; }
  button { font: inherit; border-radius: 10px; border: 1px solid #d0d4da; background: #fff;
    padding: 9px 14px; cursor: pointer; }
  button.primary { background: #1c64f2; border-color: #1c64f2; color: #fff; }
  button:disabled { opacity: .55; cursor: not-allowed; }
  .row { display: flex; gap: 10px; flex-wrap: wrap; align-items: center; }
  #status { min-height: 1.6em; margin: 14px 0 0; overflow-wrap: anywhere; }
  details { margin-top: 18px; color: #4b5563; }
  pre { overflow-x: auto; background: #f2f4f7; border-radius: 8px; padding: 10px; font-size: 13px; }
  @media (prefers-color-scheme: dark) {
    body { background: #111315; color: #e8eaed; }
    main { background: #191c1f; border-color: #2a2f34; box-shadow: none; }
    .muted, dt, details { color: #9aa1a9; }
    code, pre { background: #22272c; }
    button { background: #22272c; border-color: #3a4148; color: #e8eaed; }
    button.primary { background: #3b82f6; border-color: #3b82f6; color: #fff; }
  }
`;

export function escapeHtml(value: string): string {
  return value.replace(/[&<>"']/gu, (char) => {
    switch (char) {
      case '&':
        return '&amp;';
      case '<':
        return '&lt;';
      case '>':
        return '&gt;';
      case '"':
        return '&quot;';
      default:
        return '&#39;';
    }
  });
}

/** 内联到 `<script>` 的 JSON 必须转义 HTML 敏感字符与行分隔符。 */
export function jsonForScript(value: unknown): string {
  return JSON.stringify(value)
    .replaceAll('<', '\\u003c')
    .replaceAll('>', '\\u003e')
    .replaceAll('&', '\\u0026')
    .replaceAll('\u2028', '\\u2028')
    .replaceAll('\u2029', '\\u2029');
}

export interface LayoutOptions {
  /** 全屏应用布局：去掉卡片宽度与内边距，由页面自行接管视口。 */
  wide?: boolean;
  /** 附加到 `<body>` 的类名，用于页面级布局分支（如移动端单栏切换）。 */
  bodyClass?: string;
}

export function layout(
  title: string,
  body: string,
  extraStyle = '',
  options: LayoutOptions = {},
): string {
  const bodyClass = [options.wide ? 'wide' : '', options.bodyClass ?? '']
    .filter(Boolean)
    .join(' ');
  return `<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${escapeHtml(title)}</title>
<style>${PAGE_STYLE}${extraStyle}</style>
</head>
<body${bodyClass ? ` class="${escapeHtml(bodyClass)}"` : ''}>
<main>${body}</main>
</body>
</html>`;
}
