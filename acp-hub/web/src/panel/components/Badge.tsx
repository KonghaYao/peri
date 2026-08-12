// 状态徽标（原 ui.js _badge/_badgeKind）：badge-ok 绿 / warn 黄 / err 红 / 默认灰。
// §3.8：高度 20px、padding 0 7px、圆角 999px、11px/500；语义色用对应 soft
// 底 + 深色文字（token），不使用全彩实心底。
// kind 为可选显式着色：StatusRail 连接行传 store 的 connState.kind（''/ok/
// warn/err，badgeKind 无法从中文 text 推断）；不传时按 status 文本推断
// （badgeKind 逻辑不变），既有调用点行为保持一致。

const OK = ['online', 'healthy', 'completed', 'accepting', 'allow'];
const WARN = ['degraded', 'active', 'streaming', 'pending', 'awaitingPermission', 'running', 'deny'];
const ERR = ['offline', 'crashed', 'error', 'restarting', 'failed'];

export function badgeKind(status: string | null | undefined): string {
  if (!status) return 'neutral';
  if (OK.includes(status)) return 'ok';
  if (WARN.includes(status)) return 'warn';
  if (ERR.includes(status)) return 'err';
  return 'neutral';
}

const KIND_CLASS: Record<string, string> = {
  ok: 'bg-[var(--success-soft)] text-[var(--success)]',
  warn: 'bg-[var(--warning-soft)] text-[var(--warning)]',
  err: 'bg-[var(--danger-soft)] text-[var(--danger)]',
  neutral: 'bg-[var(--surface-muted)] text-[var(--text-secondary)]',
};

export function Badge(props: { status: string | null | undefined; kind?: string }) {
  const kind = () => props.kind || badgeKind(props.status);
  return (
    <span
      class={`inline-flex h-5 items-center rounded-full px-[7px] text-[11px] font-medium ${KIND_CLASS[kind()] ?? KIND_CLASS.neutral}`}
    >
      {props.status}
    </span>
  );
}
