import { useRef, useState, type KeyboardEvent } from 'react';
import { X } from 'lucide-react';
import { cn } from '@/lib';
import { useFocusTrap, useMediaQuery } from '@/hooks';
import type { ChatDisplayState } from '../runtime';
import { DetailsPanel, type DetailsPanelMode } from './DetailsPanel';

/**
 * 与 CSS `min-[981px]` 使用同一断点：981px 起内联，其余宽度（含 980–981 的小数像素）
 * 保持 overlay。JS 若用 `max-width: 980px`，小数宽度会同时落空，产生定位缝隙。
 */
export const RIGHT_PANEL_INLINE_MEDIA_QUERY = '(min-width: 981px)';

export interface ChatRightPanelProps {
  open: boolean;
  state: ChatDisplayState | null;
  onClose: () => void;
}

const TABS: ReadonlyArray<{ id: DetailsPanelMode; label: string }> = [
  { id: 'details', label: '详情' },
  { id: 'plan', label: '计划' },
  { id: 'tasks', label: '任务' },
];

/**
 * 右面板：宽屏内联，980px 以下 overlay（同一 DOM，断点切换定位）。
 * 三个 tab 复用现有会话/计划/任务事实；窄屏打开时焦点进入并在面板内循环。
 */
export function ChatRightPanel({ open, state, onClose }: ChatRightPanelProps) {
  const panelRef = useRef<HTMLElement>(null);
  const inline = useMediaQuery(RIGHT_PANEL_INLINE_MEDIA_QUERY);
  const sheet = !inline;
  const [tab, setTab] = useState<DetailsPanelMode>('details');

  useFocusTrap(open && sheet, panelRef);

  const onTabKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
    const buttons = Array.from(
      event.currentTarget.querySelectorAll<HTMLButtonElement>('[role="tab"]'),
    );
    const current = TABS.findIndex((item) => item.id === tab);
    const next = event.key === 'Home' ? 0
      : event.key === 'End' ? TABS.length - 1
        : event.key === 'ArrowLeft' ? (current - 1 + TABS.length) % TABS.length
          : (current + 1) % TABS.length;
    event.preventDefault();
    setTab(TABS[next]!.id);
    buttons[next]?.focus();
  };

  const active = TABS.find((item) => item.id === tab) ?? TABS[0]!;

  return (
    <>
      {open && sheet && (
        <div
          className="fixed inset-0 z-30 bg-black/30 backdrop-blur-sm"
          onClick={onClose}
          aria-hidden="true"
        />
      )}
      <aside
        ref={panelRef}
        id="peri-right-panel"
        data-slot="peri-right-panel"
        role={sheet ? 'dialog' : 'complementary'}
        aria-modal={sheet && open ? true : undefined}
        aria-label="详情"
        aria-hidden={!open}
        inert={!open || undefined}
        className={cn(
          'bg-background fixed inset-y-0 right-0 z-40 flex w-[min(360px,92vw)] flex-col border-l shadow-xl transition-transform duration-200 motion-reduce:transition-none',
          'min-[981px]:static min-[981px]:z-auto min-[981px]:w-[340px] min-[981px]:translate-x-0 min-[981px]:shadow-none',
          open ? 'visible translate-x-0' : 'invisible translate-x-full min-[981px]:hidden',
        )}
      >
        <header className="flex h-14 shrink-0 items-center gap-2 border-b px-2">
          <div
            role="tablist"
            aria-label="右面板视图"
            className="flex min-w-0 items-center gap-0.5"
            onKeyDown={onTabKeyDown}
          >
            {TABS.map((item) => (
              <button
                key={item.id}
                type="button"
                role="tab"
                id={`peri-right-tab-${item.id}`}
                aria-selected={tab === item.id}
                aria-controls="peri-right-panel-content"
                tabIndex={tab === item.id ? 0 : -1}
                className={cn(
                  'rounded-md px-2.5 py-1 text-xs transition-colors',
                  tab === item.id
                    ? 'bg-muted text-foreground font-medium'
                    : 'text-muted-foreground hover:text-foreground',
                )}
                onClick={() => setTab(item.id)}
              >
                {item.label}
              </button>
            ))}
          </div>
          <button
            type="button"
            className="text-muted-foreground hover:bg-accent hover:text-foreground ml-auto inline-flex size-8 shrink-0 items-center justify-center rounded-lg"
            aria-label="关闭详情面板"
            onClick={onClose}
          >
            <X size={17} />
          </button>
        </header>
        <div
          id="peri-right-panel-content"
          role="tabpanel"
          aria-labelledby={`peri-right-tab-${active.id}`}
          className="min-h-0 flex-1 overflow-y-auto"
        >
          <DetailsPanel state={state} mode={active.id} />
        </div>
      </aside>
    </>
  );
}
