import type { ChatCloseResponse, ChatSummary } from '@shared/chat';
import { useMutation, useQueryClient } from '@tanstack/react-query';
import { AlertTriangle } from 'lucide-react';
import { useCallback, useEffect, useRef, useState } from 'react';
import { Link, useNavigate, useParams } from 'react-router';
import { toast } from 'sonner';
import { ConfirmDialog } from '@/components/confirm-dialog';
import { EmptyState } from '@/components/empty-state';
import { ErrorState } from '@/components/error-state';
import { PageHeader } from '@/components/page-header';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Skeleton } from '@/components/ui/skeleton';
import { useCapabilities } from '@/features/agents/api';
import { describeApiError } from '@/lib/api-client';
import { cancelChat, chatQueryKey, closeChat, deleteChat, sendChat, useCachedChat, useChatList } from './api';
import { Composer } from './composer';
import { MessageList } from './message-list';
import { ChatRuntimeNotice, isRetryableRuntime } from './runtime-notice';
import { useChatStream } from './use-chat-stream';

function ChatLoading() {
  return (
    <div className="space-y-3">
      <p className="text-sm text-muted-foreground" role="status">
        正在加载对话…
      </p>
      <Skeleton className="h-10 w-64" />
      <Skeleton className="h-64 w-full" />
    </div>
  );
}

function connectionLabel(state: string): string {
  if (state === 'open') return '已连接';
  if (state === 'closed') return '对话已关闭';
  if (state === 'reconnecting') return '连接中断，正在重连…';
  if (state === 'error') return '连接失败';
  return '连接中…';
}

function closeMessage(result: ChatCloseResponse): void {
  if (result.status === 'closed') {
    toast.success('对话与 Sandbox 已关闭');
    return;
  }
  toast.error(`关闭未完成：${result.errors[0] ?? '未知原因'}；可重试关闭，未确认前不会释放 Sandbox。`);
}

function sandboxLabel(state: ChatSummary['sandbox']['state']): string {
  switch (state) {
    case 'ready':
      return 'Sandbox 已就绪';
    case 'starting':
      return 'Sandbox 启动中…';
    case 'releasing':
      return 'Sandbox 关闭中…';
    case 'incomplete':
      return 'Sandbox 关闭未确认';
    case 'error':
      return 'Sandbox 启动失败';
    case 'closed':
      return 'Sandbox 已停止';
    default:
      return '未启用 Sandbox';
  }
}

export function ChatPage() {
  const { chatId } = useParams<{ chatId: string }>();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const chatsQuery = useChatList();
  const capabilities = useCapabilities();
  // The create response wins over the list: a read replica may not include the new chat yet, and a
  // transient list failure must not hide a chat we already know about.
  const cachedChat = useCachedChat(chatId);
  // The list carries live sandbox state; the create response stays as a fallback for a lagging list.
  const chat = chatsQuery.data?.find((item) => item.chatId === chatId) ?? cachedChat;
  const { refetch: refetchChats } = chatsQuery;
  const [lookupError, setLookupError] = useState<{ chatId: string | undefined; error: unknown } | null>(null);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const lookedUpFor = useRef<string | undefined>(undefined);

  const lookupChat = useCallback(async () => {
    const result = await refetchChats();
    setLookupError({ chatId, error: result.error });
  }, [chatId, refetchChats]);

  const missingFromCache = chatsQuery.isSuccess && chat === undefined;
  // Ignore a failure recorded for a different chat id; otherwise a stale error would hide the
  // loading/404 states of the chat currently in the URL.
  const activeLookupError = lookupError !== null && lookupError.chatId === chatId ? lookupError.error : null;

  useEffect(() => {
    if (!missingFromCache) {
      lookedUpFor.current = undefined;
      setLookupError(null);
      return;
    }
    // The cached list can be stale (created on another page or tab). Look once at the server
    // before claiming the chat does not exist; the second render after the lookup can trust [].
    if (lookedUpFor.current === chatId) return;
    lookedUpFor.current = chatId;
    void lookupChat();
  }, [missingFromCache, chatId, lookupChat]);

  // The single-chat cache never expires on its own; a 404 from the stream is the server telling us
  // the local chat row was deleted, which is the only signal strong enough to drop it.
  const forgetMissingChat = useCallback(() => {
    if (chatId === undefined) return;
    queryClient.removeQueries({ queryKey: chatQueryKey(chatId), exact: true });
    void refetchChats();
  }, [chatId, queryClient, refetchChats]);

  const stream = useChatStream({
    chatId,
    // A list refresh is not an existence check: keep the stream alive while the chat is cached.
    enabled: chat !== undefined,
    onNotFound: forgetMissingChat,
  });

  const sendMutation = useMutation({
    mutationFn: (request: { requestId: string; text: string }) => {
      if (chatId === undefined) throw new Error('chatId is required');
      return sendChat(chatId, request);
    },
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ['chats'] });
      // A delivered send proves the Store answers again; drop a stale store_unreachable notice.
      void queryClient.invalidateQueries({ queryKey: ['capabilities'] });
    },
    onError: (error) => {
      toast.error(describeApiError(error));
    },
  });

  const cancelMutation = useMutation({
    mutationFn: () => {
      if (chatId === undefined) throw new Error('chatId is required');
      return cancelChat(chatId);
    },
    onSuccess: () => {
      toast.success('已请求停止当前回复');
    },
    onError: (error) => {
      toast.error(describeApiError(error));
    },
  });

  const closeMutation = useMutation({
    mutationFn: () => {
      if (chatId === undefined) throw new Error('chatId is required');
      return closeChat(chatId);
    },
    onSuccess: async (result) => {
      closeMessage(result);
      await queryClient.invalidateQueries({ queryKey: ['chats'] });
    },
    onError: (error) => {
      toast.error(describeApiError(error));
    },
  });

  const deleteMutation = useMutation({
    mutationFn: () => {
      if (chatId === undefined) throw new Error('chatId is required');
      return deleteChat(chatId);
    },
    onSuccess: async () => {
      toast.success('对话已删除；Store 中的历史仍保留');
      if (chatId !== undefined) {
        queryClient.removeQueries({ queryKey: chatQueryKey(chatId), exact: true });
      }
      await queryClient.invalidateQueries({ queryKey: ['chats'] });
      setConfirmOpen(false);
      navigate('/chats');
    },
    onError: (error) => {
      // A 409 close_incomplete keeps the row and the sandbox holder; the user can retry close.
      toast.error(describeApiError(error));
      void queryClient.invalidateQueries({ queryKey: ['chats'] });
    },
  });

  // A retryable Store outage keeps the composer enabled so a send can trigger the lazy re-probe.
  const notConfigured =
    capabilities.data !== undefined &&
    capabilities.data.chatRuntimeReady === false &&
    !isRetryableRuntime(capabilities.data.chatRuntimeReasons);

  if (chat === undefined) {
    // Only a cache miss may show a full-page error or a 404; a cached chat always renders below.
    if (chatsQuery.isPending) {
      return <ChatLoading />;
    }
    if (chatsQuery.isError) {
      return (
        <ErrorState
          title="无法加载对话"
          error={chatsQuery.error}
          onRetry={() => {
            void chatsQuery.refetch();
          }}
        />
      );
    }
    if (activeLookupError !== null) {
      return (
        <ErrorState
          title="无法加载对话"
          error={activeLookupError}
          onRetry={() => {
            lookedUpFor.current = chatId;
            void lookupChat();
          }}
        />
      );
    }
    // Neither the first lookup nor its result has landed yet: still loading, not a 404.
    if (chatsQuery.isFetching || lookedUpFor.current !== chatId) {
      return <ChatLoading />;
    }
    return (
      <EmptyState
        title="对话不存在"
        description="这个对话可能已被删除，或链接中的 chatId 不正确。"
        action={
          <Button asChild>
            <Link to="/chats">返回对话列表</Link>
          </Button>
        }
      />
    );
  }

  return (
    <div className="flex min-h-[calc(100vh-7rem)] flex-col gap-4">
      <PageHeader
        title={`与 ${chat.agentName} 的对话`}
        description={
          stream.sessionId === null
            ? // Lazy binding: the session row appears on the first SSE subscribe or first send.
              '会话将在首次订阅事件流或首次发送消息后建立'
            : `会话 ${stream.sessionId}`
        }
        actions={
          <div className="flex gap-2">
            <Button
              variant="outline"
              disabled={closeMutation.isPending || stream.closed}
              onClick={() => closeMutation.mutate()}
            >
              {closeMutation.isPending ? '关闭中…' : '关闭对话'}
            </Button>
            <Button variant="destructive" onClick={() => setConfirmOpen(true)}>
              删除
            </Button>
          </div>
        }
      />

      <div className="flex flex-wrap items-center gap-2">
        <Badge variant={chat.sandbox.state === 'ready' ? 'secondary' : 'outline'}>
          {sandboxLabel(chat.sandbox.state)}
        </Badge>
        {chat.sandbox.state === 'disabled' ? null : (
          <span className="text-xs text-muted-foreground">
            {chat.sandbox.reason === null
              ? '环境与 MCP 凭据仅在服务端使用'
              : `原因代码：${chat.sandbox.reason}`}
          </span>
        )}
      </div>

      {chat.sandbox.state === 'incomplete' || chat.sandbox.state === 'error' ? (
        <Alert variant="destructive">
          <AlertTriangle />
          <AlertTitle>{sandboxLabel(chat.sandbox.state)}</AlertTitle>
          <AlertDescription>
            <p>
              {chat.sandbox.state === 'incomplete'
                ? '上一次关闭没有确认完成；在关闭成功前，Sandbox 不会被释放。'
                : 'Sandbox 启动失败；可以重试发送或关闭后重新创建对话。'}
            </p>
            <div className="mt-3 flex gap-2">
              <Button
                size="sm"
                variant="outline"
                disabled={closeMutation.isPending}
                onClick={() => closeMutation.mutate()}
              >
                重试关闭
              </Button>
              <Button size="sm" variant="outline" onClick={() => void refetchChats()}>
                刷新状态
              </Button>
            </div>
          </AlertDescription>
        </Alert>
      ) : null}

      {capabilities.data !== undefined ? (
        <ChatRuntimeNotice
          mode={capabilities.data.chatRuntime}
          ready={capabilities.data.chatRuntimeReady}
          reasons={capabilities.data.chatRuntimeReasons}
        />
      ) : null}

      {chatsQuery.isError ? (
        <Alert>
          <AlertTriangle />
          <AlertTitle>对话列表暂时不可用</AlertTitle>
          <AlertDescription>
            <p>当前对话仍可继续使用；列表恢复后可刷新查看全部对话。</p>
            <Button
              className="mt-3"
              size="sm"
              variant="outline"
              onClick={() => {
                void chatsQuery.refetch();
              }}
            >
              刷新列表
            </Button>
          </AlertDescription>
        </Alert>
      ) : null}

      {stream.closed ? (
        <Alert>
          <AlertTriangle />
          <AlertTitle>对话已关闭</AlertTitle>
          <AlertDescription>
            <p>当前对话不再接受消息；历史仍保存在 Store 中。</p>
            <Button asChild className="mt-3" size="sm" variant="outline">
              <Link to="/chats">返回对话列表</Link>
            </Button>
          </AlertDescription>
        </Alert>
      ) : null}

      {stream.connection === 'error' ? (
        <Alert variant="destructive">
          <AlertTriangle />
          <AlertTitle>对话流连接失败</AlertTitle>
          <AlertDescription>
            <p>{stream.error ?? '未知错误'}</p>
            <Button className="mt-3" size="sm" variant="outline" onClick={stream.reconnect}>
              重试
            </Button>
          </AlertDescription>
        </Alert>
      ) : null}

      <div className="min-h-0 flex-1 overflow-y-auto rounded-xl border bg-background p-4">
        <MessageList messages={stream.messages} running={stream.running} />
      </div>

      <p className="text-xs text-muted-foreground" role="status">
        {connectionLabel(stream.connection)}
      </p>

      <Composer
        disabled={notConfigured || stream.closed || stream.connection !== 'open'}
        sending={sendMutation.isPending}
        running={stream.running}
        cancelling={cancelMutation.isPending}
        onSend={(text) => sendMutation.mutate({ requestId: crypto.randomUUID(), text })}
        onCancel={() => cancelMutation.mutate()}
      />

      <ConfirmDialog
        open={confirmOpen}
        onOpenChange={setConfirmOpen}
        title="删除这个对话？"
        description="将先关闭对话与 Sandbox，确认成功后只删除本地映射；Store 中的历史不会被删除。关闭未确认时会保留该行以便重试。"
        pending={deleteMutation.isPending}
        onConfirm={() => deleteMutation.mutate()}
      />
    </div>
  );
}
