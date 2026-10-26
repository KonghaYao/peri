import { cn } from '@/lib';
import type { ChatSummary } from '../runtime';

/** 相对时间：只做展示，不改变排序事实。 */
function formatRelativeTime(iso: string, now = Date.now()): string {
  const time = Date.parse(iso);
  if (Number.isNaN(time)) return '';
  const diff = Math.max(0, now - time);
  if (diff < 60_000) return '刚刚';
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)} 分钟前`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)} 小时前`;
  if (diff < 172_800_000) return '昨天';
  if (diff < 7 * 86_400_000) return `${Math.floor(diff / 86_400_000)} 天前`;
  return new Date(time).toLocaleDateString('zh-CN', { month: 'numeric', day: 'numeric' });
}

function startOfDay(time: number): number {
  const date = new Date(time);
  date.setHours(0, 0, 0, 0);
  return date.getTime();
}

/** 按日期分组；组内保持调用方顺序（列表已按 updatedAt 降序）。 */
export function groupChatsByDate(
  chats: readonly ChatSummary[], now = Date.now(),
): { label: string; chats: ChatSummary[] }[] {
  const today = startOfDay(now);
  const groups: { label: string; chats: ChatSummary[] }[] = [
    { label: '今天', chats: [] },
    { label: '昨天', chats: [] },
    { label: '最近 7 天', chats: [] },
    { label: '更早', chats: [] },
  ];
  for (const chat of chats) {
    const time = Date.parse(chat.updatedAt);
    if (Number.isNaN(time) || time < today - 6 * 86_400_000) {
      groups[3]!.chats.push(chat);
    } else if (time >= today) {
      groups[0]!.chats.push(chat);
    } else if (time >= today - 86_400_000) {
      groups[1]!.chats.push(chat);
    } else {
      groups[2]!.chats.push(chat);
    }
  }
  return groups.filter((group) => group.chats.length > 0);
}

interface SidebarThreadRowProps {
  chat: ChatSummary;
  selected: boolean;
  running: boolean;
  disabled: boolean;
  onSelect: () => void;
  now?: number;
}

/** 侧栏线程行：纯展示；运行点只代表当前实例的驻留事实。 */
export function SidebarThreadRow({
  chat, selected, running, disabled, onSelect, now = Date.now(),
}: SidebarThreadRowProps) {
  const title = chat.title || '新会话';
  return (
    <button
      type="button"
      data-slot="peri-sidebar-thread-row"
      data-running={running ? 'true' : 'false'}
      aria-current={selected ? 'page' : undefined}
      disabled={disabled}
      title={title}
      onClick={onSelect}
      className={cn(
        'group flex w-full min-w-0 items-center gap-2 rounded-lg px-2.5 py-2 text-left transition-colors',
        selected ? 'bg-accent text-accent-foreground' : 'text-foreground/80 hover:bg-accent/60',
      )}
    >
      <span className="min-w-0 flex-1 truncate text-[13px]">{title}</span>
      <span className="text-muted-foreground/70 shrink-0 text-[10px]">{formatRelativeTime(chat.updatedAt, now)}</span>
      {running && (
        <span
          className="size-1.5 shrink-0 animate-pulse rounded-full bg-[var(--chat-running)] motion-reduce:animate-none"
          aria-label="运行中"
        />
      )}
    </button>
  );
}
