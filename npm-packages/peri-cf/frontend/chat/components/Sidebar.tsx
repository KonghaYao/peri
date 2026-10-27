import { useMemo, useRef, useState } from 'react';
import { PanelLeftClose, Plus, RefreshCw, Search, Sparkles } from 'lucide-react';
import { cn } from '@/lib';
import { useFocusTrap, useMediaQuery } from '@/hooks';
import type { ChatSummary } from '../runtime';
import { groupChatsByDate, SidebarThreadRow } from './SidebarThreadRow';

/** 与 Tailwind `md` 断点互补：小于 768px 时侧栏是 drawer。 */
const SIDEBAR_SHEET_MEDIA_QUERY = '(max-width: 767px)';

export interface SidebarProps {
  chats: readonly ChatSummary[];
  selectedId?: string | undefined;
  selectedRunning: boolean;
  loading: boolean;
  disabled: boolean;
  open: boolean;
  onClose: () => void;
  onNew: () => void;
  onRefresh: () => void;
  onSelect: (chat: ChatSummary) => void;
}

function SidebarSkeleton() {
  return <div className="flex flex-col gap-1.5 px-1 pt-1" aria-hidden="true">
    {[0, 1, 2, 3, 4].map((index) => <div key={index} className="bg-muted/70 h-8 animate-pulse rounded-lg" />)}
  </div>;
}

/** 左侧线程列表：本地搜索、日期分组、运行点；窄屏由外壳切换为 drawer。 */
export function Sidebar({
  chats, selectedId, selectedRunning, loading, disabled, open, onClose, onNew, onRefresh, onSelect,
}: SidebarProps) {
  const [query, setQuery] = useState('');
  const asideRef = useRef<HTMLElement>(null);
  const sheet = useMediaQuery(SIDEBAR_SHEET_MEDIA_QUERY);
  useFocusTrap(open && sheet, asideRef);
  const filtered = useMemo(() => {
    const keyword = query.trim().toLowerCase();
    if (!keyword) return chats;
    return chats.filter((chat) => (chat.title || '新会话').toLowerCase().includes(keyword));
  }, [chats, query]);
  const groups = useMemo(() => groupChatsByDate(filtered), [filtered]);

  return (
    <>
      {open && <div className="fixed inset-0 z-30 bg-black/30 backdrop-blur-sm md:hidden" onClick={onClose} aria-hidden="true" />}
      <aside
        ref={asideRef}
        id="peri-sidebar"
        data-slot="peri-sidebar"
        role={sheet ? 'dialog' : 'complementary'}
        aria-modal={sheet && open ? true : undefined}
        aria-label="会话导航"
        aria-hidden={sheet && !open ? true : undefined}
        inert={sheet && !open ? true : undefined}
        className={cn(
          'bg-sidebar text-sidebar-foreground fixed inset-y-0 left-0 z-40 flex w-[min(290px,85vw)] shrink-0 flex-col border-r shadow-xl transition-transform duration-200 motion-reduce:transition-none',
          'md:static md:z-auto md:w-[272px] md:translate-x-0 md:shadow-none',
          open ? 'visible translate-x-0' : 'invisible -translate-x-full md:visible',
        )}
      >
        <div className="flex items-center gap-2 px-4 pt-4 pb-3">
          <span className="bg-primary text-primary-foreground grid size-8 place-items-center rounded-xl">
            <Sparkles size={18} aria-hidden="true" />
          </span>
          <span className="text-[22px] leading-none font-semibold tracking-tight">
            peri<span className="text-[var(--green)]">.</span>
          </span>
          <button
            type="button"
            className="text-muted-foreground hover:bg-accent hover:text-foreground ml-auto inline-flex size-8 items-center justify-center rounded-lg md:hidden"
            aria-label="关闭会话列表"
            onClick={onClose}
          >
            <PanelLeftClose size={18} />
          </button>
        </div>

        <div className="px-3">
          <button
            type="button"
            disabled={disabled}
            onClick={onNew}
            className="bg-foreground text-background hover:bg-foreground/85 flex w-full items-center gap-2 rounded-xl px-3 py-2 text-[13px] font-medium transition-colors disabled:opacity-45"
          >
            <Plus size={16} aria-hidden="true" />
            新建会话
          </button>
        </div>

        <label className="bg-background/70 focus-within:ring-ring/40 mx-3 mt-3 flex items-center gap-2 rounded-lg border px-2.5 py-1.5 focus-within:ring-2">
          <Search size={14} className="text-muted-foreground shrink-0" aria-hidden="true" />
          <input
            type="search"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="搜索会话"
            aria-label="搜索会话"
            className="text-foreground placeholder:text-muted-foreground/70 min-w-0 flex-1 bg-transparent text-xs outline-none"
          />
        </label>

        <div className="text-muted-foreground/70 flex items-center justify-between px-4 pt-4 pb-1 text-[10px] tracking-[0.12em] uppercase">
          <span>你的对话</span>
          <button
            type="button"
            className="text-muted-foreground hover:text-foreground inline-flex size-6 items-center justify-center rounded-md"
            aria-label="刷新会话列表"
            disabled={loading}
            onClick={onRefresh}
          >
            <RefreshCw size={13} className={loading ? 'animate-spin motion-reduce:animate-none' : undefined} />
          </button>
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto px-2 pb-3">
          {loading && chats.length === 0 ? (
            <SidebarSkeleton />
          ) : groups.length === 0 ? (
            <p className="text-muted-foreground/70 px-2.5 py-8 text-center text-xs">
              {query.trim() ? '没有匹配的会话。' : '还没有会话，新建一个开始吧。'}
            </p>
          ) : groups.map((group) => (
            <section key={group.label} className="mb-2" aria-label={group.label}>
              <h2 className="text-muted-foreground/60 px-2.5 py-1 text-[10px] font-medium">{group.label}</h2>
              <div className="flex flex-col gap-0.5">
                {group.chats.map((chat) => (
                  <SidebarThreadRow
                    key={chat.sessionId}
                    chat={chat}
                    selected={chat.sessionId === selectedId}
                    running={chat.sessionId === selectedId ? selectedRunning : false}
                    disabled={disabled}
                    onSelect={() => onSelect(chat)}
                  />
                ))}
              </div>
            </section>
          ))}
        </div>
      </aside>
    </>
  );
}
