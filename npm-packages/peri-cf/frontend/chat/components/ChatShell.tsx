import { useCallback, useEffect, useRef, useState } from 'react';
import { X } from 'lucide-react';
import { syncRetryAction, useChat } from '../runtime';
import type { ChatRetryAction } from '../runtime';
import { Timeline, type TimelineScrollSignal } from '../timeline';
import { parseOpenUIAction, type OpenUIActionEvent } from '../openui';
import {
  ChatComposer, adoptNewChatDraft, composerDraft, composerDraftKey, withComposerDraft,
  type ComposerDrafts,
} from '../composer';
import { ChatHeader } from './ChatHeader';
import { ChatRightPanel } from './ChatRightPanel';
import { PendingInteractions } from './PendingInteractions';
import { Sidebar } from './Sidebar';
import { Welcome } from './Welcome';
import type { ChatSummary } from '../runtime';

interface ChatShellProps {
  onOpenSettings: () => void;
}

/** `useChat` 的完整读面；DOM 测试可注入同形对象。 */
export type ChatShellChat = ReturnType<typeof useChat>;

interface ChatShellViewProps {
  chat: ChatShellChat;
  onOpenSettings: () => void;
}

const NAVIGATION_BLOCKED_PHASES = new Set(['cancelling', 'syncing']);

/** 三栏外壳：左侧线程列表、中间 Chat 列、右侧可收起的详情面板。 */
export function ChatShell({ onOpenSettings }: ChatShellProps) {
  const chat = useChat();
  return <ChatShellView chat={chat} onOpenSettings={onOpenSettings} />;
}

/**
 * 外壳视图：数据与命令全部经 `chat` 注入。
 * 窄屏 sidebar 变 drawer，右面板变 overlay；错误分列表/会话/实时同步三类展示。
 */
export function ChatShellView({ chat, onOpenSettings }: ChatShellViewProps) {
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const [detailsOpen, setDetailsOpen] = useState(false);
  const [drafts, setDrafts] = useState<ComposerDrafts>({});
  const [creatingChat, setCreatingChat] = useState(false);
  const [focusSignal, setFocusSignal] = useState(0);
  const [composerHeight, setComposerHeight] = useState(0);
  const [scrollSignal, setScrollSignal] = useState<TimelineScrollSignal | null>(null);
  const sidebarTrigger = useRef<HTMLButtonElement>(null);
  const detailsTrigger = useRef<HTMLButtonElement>(null);
  const navigationRequest = useRef(0);
  /** 本次导航是否在创建新会话；创建成功后即使读面失败也要迁移草稿。 */
  const pendingNewChat = useRef(false);

  const running = chat.state?.running === true || chat.phase === 'cancelling';
  const canSend = chat.historyReady && chat.phase === 'idle';
  const navigationDisabled = NAVIGATION_BLOCKED_PHASES.has(chat.phase);
  const pendingInteractions = chat.state?.pendingInteractions ?? [];
  const chatBanner = chat.banners.chat;
  const listBanner = chat.banners.list;
  const syncRetry = syncRetryAction({ loadState: chat.loadState, liveSync: chat.liveSync });
  // 阻断态必须给出原因与可执行入口；错误横幅在这时让位给专用提示，避免重复。
  const blocked = chat.phase === 'blocked';
  const blockedMessage = chat.chatError
    || (chat.state?.executionBlocked
      ? '宿主执行被阻断，当前会话暂时不能发送消息。'
      : '会话被阻断，需要重新加载后才能继续。');
  const composerDisabledReason = chat.phase === 'blocked'
    ? (chat.state?.executionBlocked ? '会话执行被阻断，请先重新加载。' : '操作状态未确认，请先重新加载。')
    : undefined;
  // 新建期间固定使用占位草稿槽，创建成功后再迁到新会话，加载期间输入不丢。
  const draftKey = composerDraftKey(chat.selected?.id ?? null, creatingChat);
  const draft = composerDraft(drafts, draftKey);

  const adoptCreatedDraft = useCallback(() => {
    if (!pendingNewChat.current) return;
    const createdId = chat.currentChatId();
    if (!createdId) return;
    pendingNewChat.current = false;
    setDrafts((previous) => adoptNewChatDraft(previous, createdId));
  }, [chat.currentChatId]);

  const closeSidebar = useCallback(() => {
    setSidebarOpen(false);
    sidebarTrigger.current?.focus();
  }, []);

  const closeDetails = useCallback(() => {
    setDetailsOpen(false);
    detailsTrigger.current?.focus();
  }, []);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return;
      if (sidebarOpen) closeSidebar();
      if (detailsOpen) closeDetails();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [sidebarOpen, detailsOpen, closeSidebar, closeDetails]);

  const select = useCallback(async (target: ChatSummary | null) => {
    setSidebarOpen(false);
    const request = ++navigationRequest.current;
    if (target === null) pendingNewChat.current = true;
    else pendingNewChat.current = false;
    setCreatingChat(target === null);
    const opened = await chat.selectChat(target);
    // 被更晚的导航取代时，由后一次选择负责草稿槽与焦点。
    if (request !== navigationRequest.current) return;
    setCreatingChat(false);
    // 创建成功但 read/watch 失败时 selected 已是新会话，草稿同样要迁移。
    adoptCreatedDraft();
    if (!opened) return;
    setFocusSignal((value) => value + 1);
  }, [adoptCreatedDraft, chat.selectChat]);

  const changeDraft = useCallback((value: string) => {
    setDrafts((previous) => withComposerDraft(previous, draftKey, value));
  }, [draftKey]);

  const send = useCallback(async (content: string) => {
    const selectedBefore = chat.currentChatId();
    const navigation = navigationRequest.current;
    if (!selectedBefore) pendingNewChat.current = true;
    // 以实际提交会话为准：隐式创建期间切走，草稿仍迁到真正收到消息的会话。
    const outcome = await chat.send(content);
    const deliveredId = outcome.delivered ? outcome.sessionId : null;
    // 创建成功即使用户没切走、prompt 失败/被撤回，也要把占位草稿迁到实际会话；切走时不猜测目标。
    const createdId = !deliveredId && !selectedBefore && navigation === navigationRequest.current
      ? chat.currentChatId()
      : null;
    const draftTarget = deliveredId ?? createdId;
    if (!selectedBefore && draftTarget) {
      pendingNewChat.current = false;
      setDrafts((previous) => {
        const adopted = adoptNewChatDraft(previous, draftTarget);
        if (!deliveredId) return adopted;
        // 成功后 Composer 会用旧占位槽清空；这里把刚提交的内容从新会话槽同步清掉。
        return composerDraft(adopted, draftTarget).trim() === content.trim()
          ? withComposerDraft(adopted, draftTarget, '')
          : adopted;
      });
    } else if (!deliveredId && !chat.currentChatId()) {
      pendingNewChat.current = false;
    }
    // 信号携带实际接收会话；迟到结算由 Timeline 按当前会话过滤，不能移动别的会话。
    if (deliveredId) {
      setScrollSignal((previous) => ({ token: (previous?.token ?? 0) + 1, chatId: deliveredId }));
    }
    return outcome;
  }, [chat.currentChatId, chat.send]);

  const stop = useCallback(() => chat.stop(), [chat.stop]);

  /** Renderer action 路由：复用 send 的草稿迁移语义；外链只接受已白名单的 http(s)。 */
  const onOpenUIAction = useCallback((event: OpenUIActionEvent) => {
    const action = parseOpenUIAction(event);
    if (action.kind === 'send') {
      void send(action.text);
    } else if (action.kind === 'open_url' && typeof window !== 'undefined') {
      window.open(action.url, '_blank', 'noopener,noreferrer');
    }
  }, [send]);

  const selectPrompt = useCallback((prompt: string) => {
    setDrafts((previous) => withComposerDraft(previous, draftKey, prompt));
    setFocusSignal((value) => value + 1);
  }, [draftKey]);

  const runAction = useCallback((action: ChatRetryAction) => {
    if (action === 'refresh-list') void chat.refreshList();
    else if (action === 'reload-chat') void chat.reloadChat().then(adoptCreatedDraft);
    else if (action === 'rebuild-chat') {
      // 关闭未确认的逃生入口：显式关闭句柄后新建会话，关闭失败保留当前会话与原因。
      pendingNewChat.current = true;
      setCreatingChat(true);
      void chat.rebuildChat().then((opened) => {
        setCreatingChat(false);
        adoptCreatedDraft();
        if (opened) setFocusSignal((value) => value + 1);
      });
    }
  }, [adoptCreatedDraft, chat.rebuildChat, chat.reloadChat, chat.refreshList]);

  return (
    <div className="bg-background text-foreground flex h-dvh min-h-[320px] overflow-hidden">
      <Sidebar
        chats={chat.chats}
        selectedId={chat.selected?.id}
        selectedRunning={chat.state?.running === true}
        loading={chat.listLoading}
        disabled={navigationDisabled}
        open={sidebarOpen}
        onClose={closeSidebar}
        onNew={() => void select(null)}
        onRefresh={() => void chat.refreshList()}
        onSelect={(target) => void select(target)}
      />

      <main className="flex min-w-0 flex-1 flex-col">
        <ChatHeader
          title={chat.selected?.title || '新会话'}
          detailsOpen={detailsOpen}
          sidebarOpen={sidebarOpen}
          sidebarTrigger={sidebarTrigger}
          detailsTrigger={detailsTrigger}
          syncLabel={
            chat.loadState === 'unsupported'
              ? '版本不支持'
              : chat.loadState === 'failed' || chat.liveSync === 'failed'
                ? '同步失败'
                : chat.selected && !chat.historyReady ? '同步中…' : undefined
          }
          onOpenSidebar={() => setSidebarOpen(true)}
          onToggleDetails={() => setDetailsOpen((value) => !value)}
          onOpenSettings={onOpenSettings}
        />

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
                onClick={() => runAction(listBanner.action)}
              >
                刷新列表
              </button>
            )}
            <button
              type="button"
              className="hover:bg-destructive/10 inline-flex size-5 shrink-0 items-center justify-center rounded-md"
              aria-label="关闭列表错误提示"
              onClick={chat.dismissListError}
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
              onClick={() => runAction('reload-chat')}
            >
              重新加载
            </button>
            <button
              type="button"
              className="hover:bg-destructive/10 shrink-0 rounded-md px-1.5 py-0.5"
              onClick={() => runAction('rebuild-chat')}
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
                onClick={() => runAction(chatBanner.action)}
              >
                {chatBanner.action === 'reload-chat' ? '重新加载' : '刷新列表'}
              </button>
            )}
            {chatBanner.dismissible && (
              <button
                type="button"
                className="hover:bg-destructive/10 inline-flex size-5 shrink-0 items-center justify-center rounded-md"
                aria-label="关闭错误提示"
                onClick={chat.dismissChatError}
              >
                <X size={14} />
              </button>
            )}
          </div>
        )}

        {syncRetry === 'reload-chat' && chat.loadState !== 'failed' && (
          <div
            role="status"
            data-slot="peri-sync-error-banner"
            className="text-muted-foreground bg-muted/60 mx-3 mt-2 flex items-center gap-2 rounded-lg px-3 py-2 text-xs md:mx-5"
          >
            <span className="min-w-0 flex-1">实时同步已中断，消息可能不是最新。</span>
            <button
              type="button"
              className="border-border hover:bg-muted shrink-0 rounded-md border px-2 py-0.5"
              onClick={() => runAction('reload-chat')}
            >
              重新连接
            </button>
          </div>
        )}

        {!blocked && !chatBanner && syncRetry === 'none' && chat.selected && !chat.historyReady
          && chat.phase !== 'loading' && chat.loadState !== 'unsupported' && (
          <div
            role="status"
            data-slot="peri-sync-banner"
            className="text-muted-foreground bg-muted/60 mx-3 mt-2 rounded-lg px-3 py-2 text-xs md:mx-5"
          >
            正在同步会话，连接就绪后可以发送消息。
          </div>
        )}

        <div className="relative flex min-h-0 flex-1 flex-col">
          <Timeline
            state={chat.state}
            loadState={chat.loadState}
            error={chat.chatError}
            onRetry={syncRetry === 'reload-chat' ? () => runAction('reload-chat') : undefined}
            emptyContent={<Welcome disabled={!canSend} onSelectPrompt={selectPrompt} />}
            bottomInset={composerHeight}
            scrollSignal={scrollSignal}
            payloadReader={chat.payloadReader}
            onOpenUIAction={onOpenUIAction}
            actionsEnabled={canSend}
          />

          <div className="pointer-events-none absolute inset-x-0 bottom-0 z-20 bg-gradient-to-t from-background via-background/90 to-transparent pt-8">
            <div className="pointer-events-auto mx-auto w-full max-w-[46rem] px-4 pb-4">
              <ChatComposer
                key={draftKey}
                draft={draft}
                onDraftChange={changeDraft}
                canSend={canSend}
                canStop={running}
                stopping={chat.phase === 'cancelling'}
                disabledReason={composerDisabledReason}
                pendingContent={
                  <PendingInteractions
                    interactions={pendingInteractions}
                    onAnswer={chat.historyReady ? chat.answerInteraction : undefined}
                  />
                }
                onSend={send}
                onStop={stop}
                onHeightChange={setComposerHeight}
                focusSignal={focusSignal}
              />
            </div>
          </div>
        </div>
      </main>

      <ChatRightPanel open={detailsOpen} state={chat.state} onClose={closeDetails} />
    </div>
  );
}
