import html from 'solid-js/html';

// Badge 变体映射：传入 variant 返回对应 class
const VARIANT_CLASS = {
  modified: 'bg-[rgba(210,153,34,0.2)] text-warning',
  added: 'bg-[rgba(63,185,80,0.2)] text-success',
  deleted: 'bg-[rgba(248,81,73,0.2)] text-error',
  unknown: 'bg-[rgba(139,148,158,0.2)] text-text-muted',
  // 简短别名
  m: 'bg-[rgba(210,153,34,0.2)] text-warning',
  a: 'bg-[rgba(63,185,80,0.2)] text-success',
  d: 'bg-[rgba(248,81,73,0.2)] text-error',
  u: 'bg-[rgba(139,148,158,0.2)] text-text-muted',
};

export function Badge(props) {
  // props: variant (key of VARIANT_CLASS), children
  return html`
    <span class=${() => 'inline-block px-1 py-0 rounded text-[10px] font-semibold leading-4 min-w-[16px] text-center ' + (VARIANT_CLASS[props.variant] || VARIANT_CLASS.unknown)}>
      ${props.children}
    </span>
  `;
}

// 也导出 VARIANT_CLASS 供外部使用
export { VARIANT_CLASS as BADGE_VARIANTS };
