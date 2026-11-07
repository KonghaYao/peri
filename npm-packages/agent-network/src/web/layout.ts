/** Web 页面共用的外壳与转义工具。 */

const PAGE_STYLE = `
  :root { color-scheme: light dark; }
  * { box-sizing: border-box; }
  body { margin: 0; min-height: 100vh; display: grid; place-items: center; padding: 24px;
    font: 14px/1.6 -apple-system, BlinkMacSystemFont, "PingFang SC", "Helvetica Neue",
      "Microsoft YaHei", sans-serif; background: #ededed; color: #1a1a1a;
    -webkit-font-smoothing: antialiased; }
  main { width: min(460px, 100%); background: #fff; border: 1px solid #e5e5e5;
    border-radius: 12px; padding: 32px 28px; box-shadow: 0 6px 24px rgb(0 0 0 / 6%); }
  h1 { margin: 0 0 6px; font-size: 22px; line-height: 1.3; font-weight: 600; }
  .muted { color: #999; margin: 0 0 20px; font-size: 13px; }
  body.wide { display: block; min-height: 100dvh; height: 100dvh; padding: 0; overflow: hidden;
    background: #ededed; }
  body.wide main { width: 100%; max-width: none; height: 100dvh; padding: 0; border: 0;
    border-radius: 0; box-shadow: none; background: transparent; }
  dl { display: grid; grid-template-columns: auto 1fr; gap: 8px 12px; margin: 0 0 20px; }
  dt { color: #999; font-size: 12px; }
  dd { margin: 0; overflow-wrap: anywhere; }
  code { font: 12px/1.5 ui-monospace, SFMono-Regular, Menlo, monospace;
    background: #f2f2f2; border-radius: 4px; padding: 2px 6px; }
  button { font: inherit; border-radius: 4px; border: 1px solid #dcdcdc; background: #fff;
    color: #1a1a1a; padding: 9px 14px; cursor: pointer; }
  button:hover { background: #f2f2f2; }
  button.primary { background: #07c160; border-color: #07c160; color: #fff; }
  button.primary:hover { background: #06ad56; border-color: #06ad56; }
  button:disabled { opacity: .55; cursor: not-allowed; }
  button:focus-visible, a:focus-visible, input:focus-visible, textarea:focus-visible,
  summary:focus-visible { outline: 2px solid #07c160; outline-offset: 2px; }
  .row { display: flex; gap: 10px; flex-wrap: wrap; align-items: center; }
  #status { min-height: 1.6em; margin: 12px 0 0; font-size: 13px; color: #576b95;
    overflow-wrap: anywhere; text-align: center; }
  #status[data-error] { color: #fa5151; }
  details { margin-top: 16px; color: #666; border-top: 1px solid #eee; padding-top: 12px;
    font-size: 12px; }
  summary { cursor: pointer; }
  pre { overflow-x: auto; background: #f2f2f2; border-radius: 6px; padding: 10px;
    font-size: 12px; }
  a { color: #576b95; }
  .share-head { text-align: center; margin-bottom: 16px; }
  .share-avatar { width: 64px; height: 64px; margin: 0 auto 14px; border-radius: 10px;
    background: #07c160; color: #fff; display: grid; place-items: center;
    font-size: 26px; font-weight: 500; }
  .share-head h1 { margin-bottom: 4px; overflow-wrap: anywhere; }
  .share-head .muted { margin: 0; }
  .share-lead { margin: 0 0 20px; font-size: 13px; color: #666; line-height: 1.7; }
  .share-actions { flex-direction: column; align-items: stretch; }
  .share-actions button { height: 42px; }
  .share-actions .primary { order: -1; }
  @media (prefers-color-scheme: dark) {
    body { background: #191919; color: #e5e5e5; }
    main { background: #242424; border-color: #333; box-shadow: none; }
    .muted, dt, details { color: #8c8c8c; }
    code, pre { background: #2c2c2c; }
    button { background: #2b2b2b; border-color: #3a3a3a; color: #e5e5e5; }
    button:hover { background: #333; }
    button.primary { background: #07c160; border-color: #07c160; color: #fff; }
    button.primary:hover { background: #06ad56; border-color: #06ad56; }
    details { border-top-color: #333; }
    .share-lead { color: #a3a3a3; }
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
