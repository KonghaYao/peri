// 状态徽标（原 ui.js _badge/_badgeKind）：badge-ok 绿 / warn 黄 / err 红 / 默认灰蓝。

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
  ok: 'bg-emerald-100 text-emerald-700',
  warn: 'bg-amber-100 text-amber-700',
  err: 'bg-red-100 text-red-700',
  neutral: 'bg-slate-200 text-slate-600',
};

export function Badge(props: { status: string | null | undefined }) {
  const kind = () => badgeKind(props.status);
  return (
    <span
      class={`inline-block rounded px-1.5 py-0.5 align-middle text-[11px] leading-4 ${KIND_CLASS[kind()]}`}
    >
      {props.status}
    </span>
  );
}
