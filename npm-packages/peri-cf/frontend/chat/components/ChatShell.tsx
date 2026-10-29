import { memo, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { syncRetryAction, useChat } from '../runtime';
import type { ChatSummary } from '../runtime';
import { Timeline } from '../timeline';
import { parseOpenUIAction, type OpenUIActionEvent } from '../openui';
import { ChatComposer } from '../composer';
import { ChatHeader } from './ChatHeader';
import { ChatRightPanel } from './ChatRightPanel';
import { PendingInteractions } from './PendingInteractions';
import { Sidebar } from './Sidebar';
import { Welcome } from './Welcome';
import { ChatBanners } from './shell/ChatBanners';
import { useComposerDrafts } from './shell/useComposerDrafts';
import { useChatNavigation } from './shell/useChatNavigation';

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

/**
 * 键入只改草稿槽：Timeline 的 props 全部保持引用稳定，避免整段会话列表跟着
 * 每次输入重渲染。Timeline 内部滚动实现不变。
 */
const MemoTimeline = memo(Timeline);

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
  const [composerHeight, setComposerHeight] = useState(0);
  const sidebarTrigger = useRef<HTMLButtonElement>(null);
  const detailsTrigger = useRef<HTMLButtonElement>(null);

  const drafts = useComposerDrafts(chat.selected?.id ?? null, chat.currentChatId);
  const navigation = useChatNavigation(chat, drafts);
  const { draftKey, draft, changeDraft, selectPrompt: setPromptDraft } = drafts;
  const { select, runAction, send, focusComposer, focusSignal, scrollSignal } = navigation;

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
  // 实时同步中断：已有内容时也保留重连；首次同步提示只在没有其他横幅时出现。
  const syncError = syncRetry === 'reload-chat' && chat.loadState !== 'failed';
  const syncPending = !blocked && !chatBanner && syncRetry === 'none' && chat.selected !== null
    && !chat.historyReady && chat.phase !== 'loading' && chat.loadState !== 'unsupported';

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

  const selectFromSidebar = useCallback((target: ChatSummary | null) => {
    setSidebarOpen(false);
    void select(target);
  }, [select]);

  const newChat = useCallback(() => selectFromSidebar(null), [selectFromSidebar]);

  const refreshList = useCallback(() => {
    void chat.refreshList();
  }, [chat.refreshList]);

  /** 断开连接的恢复入口：重开当前会话；成功后才迁移草稿由导航 hook 负责。 */
  const onRetry = useMemo(
    () => (syncRetry === 'reload-chat' ? () => runAction('reload-chat') : undefined),
    [runAction, syncRetry],
  );

  const selectPrompt = useCallback((prompt: string) => {
    setPromptDraft(prompt);
    focusComposer();
  }, [focusComposer, setPromptDraft]);

  /** Renderer action 路由：复用 send 的草稿迁移语义；外链只接受已白名单的 http(s)。 */
  const onOpenUIAction = useCallback((event: OpenUIActionEvent) => {
    const action = parseOpenUIAction(event);
    if (action.kind === 'send') {
      void send(action.text);
    } else if (action.kind === 'open_url' && typeof window !== 'undefined') {
      window.open(action.url, '_blank', 'noopener,noreferrer');
    }
  }, [send]);

  const stop = useCallback(() => chat.stop(), [chat.stop]);

  const emptyContent = useMemo(
    () => <Welcome disabled={!canSend} onSelectPrompt={selectPrompt} />,
    [canSend, selectPrompt],
  );

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
        onNew={newChat}
        onRefresh={refreshList}
        onSelect={selectFromSidebar}
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

        <ChatBanners
          listBanner={listBanner}
          blocked={blocked}
          blockedMessage={blockedMessage}
          chatBanner={chatBanner}
          syncError={syncError}
          syncPending={syncPending}
          onAction={runAction}
          onDismissList={chat.dismissListError}
          onDismissChat={chat.dismissChatError}
        />

        <div className="relative flex min-h-0 flex-1 flex-col">
          <MemoTimeline
            state={chat.state}
            loadState={chat.loadState}
            error={chat.chatError}
            onRetry={onRetry}
            emptyContent={emptyContent}
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
