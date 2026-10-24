import type { RefObject } from 'react';
import { Menu, PanelRight, Settings } from 'lucide-react';

export interface ChatHeaderProps {
  title: string;
  detailsOpen: boolean;
  sidebarOpen: boolean;
  sidebarTrigger?: RefObject<HTMLButtonElement | null>;
  detailsTrigger?: RefObject<HTMLButtonElement | null>;
  syncLabel?: string | undefined;
  onOpenSidebar: () => void;
  onToggleDetails: () => void;
  onOpenSettings: () => void;
}

/** 中间列 header：Peri / 会话标题面包屑 + 右面板开关与设置入口。 */
export function ChatHeader({
  title, detailsOpen, sidebarOpen, sidebarTrigger, detailsTrigger, syncLabel,
  onOpenSidebar, onToggleDetails, onOpenSettings,
}: ChatHeaderProps) {
  return (
    <header
      data-slot="peri-chat-header"
      className="bg-background flex h-14 shrink-0 items-center gap-2 border-b px-3 md:px-5"
    >
      <button
        ref={sidebarTrigger}
        type="button"
        aria-expanded={sidebarOpen}
        aria-controls="peri-sidebar"
        aria-haspopup="dialog"
        aria-label="打开会话列表"
        className="text-muted-foreground hover:bg-accent hover:text-foreground inline-flex size-8 items-center justify-center rounded-lg md:hidden"
        onClick={onOpenSidebar}
      >
        <Menu size={19} />
      </button>

      <nav className="flex min-w-0 items-center gap-2 text-sm" aria-label="位置">
        <span className="text-muted-foreground shrink-0">Peri</span>
        <span className="text-muted-foreground/40 shrink-0">/</span>
        <span className="min-w-0 truncate font-medium">{title}</span>
      </nav>

      {syncLabel && (
        <span role="status" className="text-muted-foreground/70 ml-1 shrink-0 text-[11px]">{syncLabel}</span>
      )}

      <div className="ml-auto flex shrink-0 items-center gap-1">
        <button
          ref={detailsTrigger}
          type="button"
          aria-expanded={detailsOpen}
          aria-controls="peri-right-panel"
          className="text-muted-foreground hover:bg-accent hover:text-foreground inline-flex size-8 items-center justify-center rounded-lg"
          aria-label="详情面板"
          title="详情"
          onClick={onToggleDetails}
        >
          <PanelRight size={17} />
        </button>
        <button
          type="button"
          className="text-muted-foreground hover:bg-accent hover:text-foreground inline-flex size-8 items-center justify-center rounded-lg"
          aria-label="服务端信息"
          aria-haspopup="dialog"
          title="服务端配置"
          onClick={onOpenSettings}
        >
          <Settings size={17} />
        </button>
      </div>
    </header>
  );
}
