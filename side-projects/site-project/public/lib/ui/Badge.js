import html from 'solid-js/html';

// Badge 变体映射：传入 variant 返回对应 class
const VARIANT_CLASS = {
  modified: 'bg-warning/15 text-warning',
  added: 'bg-success/15 text-success',
  deleted: 'bg-error/15 text-error',
  unknown: 'bg-text-muted/15 text-text-muted',
  // 简短别名
  m: 'bg-warning/15 text-warning',
  a: 'bg-success/15 text-success',
  d: 'bg-error/15 text-error',
  u: 'bg-text-muted/15 text-text-muted',
};

export function Badge(props) {
  // props: variant (key of VARIANT_CLASS), children
  return html`
    <span class=${() => 'inline-flex items-center justify-center w-4 h-4 rounded text-[10px] font-bold font-mono leading-none ' + (VARIANT_CLASS[props.variant] || VARIANT_CLASS.unknown)}>
      ${props.children}
    </span>
  `;
}

// 也导出 VARIANT_CLASS 供外部使用
export { VARIANT_CLASS as BADGE_VARIANTS };
