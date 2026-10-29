import { memo } from 'react';
import { X } from 'lucide-react';
import type { ChatRetryAction } from '../../runtime';

/**
 * 会话区横幅：列表错误、执行阻断/会话错误、实时同步中断与首次同步提示。
 *
 * 只负责展示与动作转发；阻断原因、重试路由由 `useChat`/控制器决定，文案与
 * `data-slot` 保持原有契约。
 */

export interface ChatBannerSpec {
  message: string;
  action: ChatRetryAction;
  /** 读面失败必须保留重试入口：关闭横幅后仍显示失败态。 */
  dismissible: boolean;
}

export interface ChatBannersProps {
  listBanner?: ChatBannerSpec;
  /** 阻断态优先展示专用提示，错误横幅让位，避免重复。 */
  blocked: boolean;
  blockedMessage: string;
  chatBanner?: ChatBannerSpec;
  /** 实时同步中断：已有内容时也保留重连入口。 */
  syncError: boolean;
  /** 首次同步中：只在没有其他横幅时提示。 */
  syncPending: boolean;
  onAction(action: ChatRetryAction): void;
  onDismissList(): void;
  onDismissChat(): void;
}

export const ChatBanners = memo(function ChatBanners({
  listBanner, blocked, blockedMessage, chatBanner, syncError, syncPending,
  onAction, onDismissList, onDismissChat,
}: ChatBannersProps) {
  return (
    <>
      {listBanner && (
        <div
          role="alert"
          data-slot="peri-list-error-banner"
          className="border-destructive/30 bg-destructive/5 text-destructive mx-3 mt-2 flex items-start gap-2 rounded-lg border px-3 py-2 text-xs leading-5 md:mx-5"
        >
          <span className="min-w-0 flex-1 [overflow-wrap:anywhere]">{listBanner.message}</span>
          {listBanner.action !== 'none' && (
            <button
              type="button"
              className="hover:bg-destructive/10 shrink-0 rounded-md px-1.5 py-0.5"
              onClick={() => onAction(listBanner.action)}
            >
              刷新列表
            </button>
          )}
          <button
            type="button"
            className="hover:bg-destructive/10 inline-flex size-5 shrink-0 items-center justify-center rounded-md"
            aria-label="关闭列表错误提示"
            onClick={onDismissList}
          >
            <X size={14} />
          </button>
        </div>
      )}

      {blocked ? (
        <div
          role="alert"
          data-slot="peri-chat-blocked"
          className="border-destructive/30 bg-destructive/5 text-destructive mx-3 mt-2 flex flex-wrap items-start gap-2 rounded-lg border px-3 py-2 text-xs leading-5 md:mx-5"
        >
          <span className="min-w-0 flex-1 [overflow-wrap:anywhere]">{blockedMessage}</span>
          <button
            type="button"
            className="hover:bg-destructive/10 shrink-0 rounded-md px-1.5 py-0.5"
            onClick={() => onAction('reload-chat')}
          >
            重新加载
          </button>
          <button
            type="button"
            className="hover:bg-destructive/10 shrink-0 rounded-md px-1.5 py-0.5"
            onClick={() => onAction('rebuild-chat')}
          >
            关闭并新建
          </button>
        </div>
      ) : chatBanner && (
        <div
          role="alert"
          data-slot="peri-error-banner"
          className="border-destructive/30 bg-destructive/5 text-destructive mx-3 mt-2 flex items-start gap-2 rounded-lg border px-3 py-2 text-xs leading-5 md:mx-5"
        >
          <span className="min-w-0 flex-1 [overflow-wrap:anywhere]">{chatBanner.message}</span>
          {chatBanner.action !== 'none' && (
            <button
              type="button"
              className="hover:bg-destructive/10 shrink-0 rounded-md px-1.5 py-0.5"
              onClick={() => onAction(chatBanner.action)}
            >
              {chatBanner.action === 'reload-chat' ? '重新加载' : '刷新列表'}
            </button>
          )}
          {chatBanner.dismissible && (
            <button
              type="button"
              className="hover:bg-destructive/10 inline-flex size-5 shrink-0 items-center justify-center rounded-md"
              aria-label="关闭错误提示"
              onClick={onDismissChat}
            >
              <X size={14} />
            </button>
          )}
        </div>
      )}

      {syncError && (
        <div
          role="status"
          data-slot="peri-sync-error-banner"
          className="text-muted-foreground bg-muted/60 mx-3 mt-2 flex items-center gap-2 rounded-lg px-3 py-2 text-xs md:mx-5"
        >
          <span className="min-w-0 flex-1">实时同步已中断，消息可能不是最新。</span>
          <button
            type="button"
            className="border-border hover:bg-muted shrink-0 rounded-md border px-2 py-0.5"
            onClick={() => onAction('reload-chat')}
          >
            重新连接
          </button>
        </div>
      )}

      {syncPending && (
        <div
          role="status"
          data-slot="peri-sync-banner"
          className="text-muted-foreground bg-muted/60 mx-3 mt-2 rounded-lg px-3 py-2 text-xs md:mx-5"
        >
          正在同步会话，连接就绪后可以发送消息。
        </div>
      )}
    </>
  );
});
