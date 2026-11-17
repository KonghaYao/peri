import { AlertTriangle, Plus } from 'lucide-react';
import { Link } from 'react-router';
import { EmptyState } from '@/components/empty-state';
import { ErrorState } from '@/components/error-state';
import { PageHeader } from '@/components/page-header';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { Skeleton } from '@/components/ui/skeleton';
import { useCapabilities } from '@/features/agents/api';
import { useChatList } from './api';
import { ChatRuntimeNotice } from './runtime-notice';

const dateFormat = new Intl.DateTimeFormat('zh-CN', { dateStyle: 'medium', timeStyle: 'short' });
const SKELETON_ROWS = ['first', 'second', 'third'];

function formatDate(value: number): string {
  return dateFormat.format(new Date(value));
}

export function ChatListPage() {
  const chatsQuery = useChatList();
  const capabilities = useCapabilities();
  // A failed refresh must not hide chats that are already on screen; only a cold failure is fatal.
  const chats = chatsQuery.data;

  return (
    <div className="space-y-5">
      <PageHeader
        title="对话"
        description="每个对话对应一个 Agent 与一份持久会话；消息经本地引擎流式返回。"
        actions={
          <Button asChild>
            <Link to="/chats/new">
              <Plus />
              新建对话
            </Link>
          </Button>
        }
      />

      {capabilities.data !== undefined ? (
        <ChatRuntimeNotice
          mode={capabilities.data.chatRuntime}
          reasons={capabilities.data.chatRuntimeReasons}
        />
      ) : null}

      {chatsQuery.isPending ? (
        <div className="space-y-2 rounded-xl border bg-card p-4">
          {SKELETON_ROWS.map((row) => (
            <Skeleton key={row} className="h-12 w-full" />
          ))}
        </div>
      ) : null}

      {chatsQuery.isError && chats === undefined ? (
        <ErrorState
          title="无法加载对话列表"
          error={chatsQuery.error}
          onRetry={() => {
            void chatsQuery.refetch();
          }}
        />
      ) : null}

      {chatsQuery.isError && chats !== undefined ? (
        <Alert>
          <AlertTriangle />
          <AlertTitle>对话列表刷新失败</AlertTitle>
          <AlertDescription>
            <p>下面显示的是上一次加载的结果，当前页面仍可继续使用。</p>
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

      {chats !== undefined && chats.length === 0 ? (
        <EmptyState
          title="还没有对话"
          description="选择一个已配置好的 Agent 开始对话；会话 id 会保存在本地。"
          action={
            <Button asChild>
              <Link to="/chats/new">
                <Plus />
                新建对话
              </Link>
            </Button>
          }
        />
      ) : null}

      {chats !== undefined && chats.length > 0 ? (
        <div className="overflow-hidden rounded-xl border bg-card shadow">
          <div className="overflow-x-auto">
            <table className="w-full min-w-[640px] text-sm">
              <thead className="border-b bg-muted/40 text-left text-xs uppercase tracking-wide text-muted-foreground">
                <tr>
                  <th className="px-4 py-3 font-medium">Agent</th>
                  <th className="px-4 py-3 font-medium">会话</th>
                  <th className="px-4 py-3 font-medium">更新时间</th>
                  <th className="px-4 py-3 text-right font-medium">操作</th>
                </tr>
              </thead>
              <tbody>
                {chats.map((chat) => (
                  <tr key={chat.chatId} className="border-b last:border-b-0 hover:bg-muted/30">
                    <td className="max-w-[220px] truncate px-4 py-3 font-medium">
                      <Link className="hover:underline" to={`/chats/${chat.chatId}`}>
                        {chat.agentName}
                      </Link>
                    </td>
                    <td className="px-4 py-3 text-muted-foreground">
                      {chat.sessionId === null ? '新对话' : chat.sessionId.slice(0, 12)}
                    </td>
                    <td className="px-4 py-3 text-muted-foreground">{formatDate(chat.updatedAt)}</td>
                    <td className="px-4 py-3 text-right">
                      <Button asChild size="sm" variant="outline">
                        <Link to={`/chats/${chat.chatId}`}>继续对话</Link>
                      </Button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      ) : null}
    </div>
  );
}
