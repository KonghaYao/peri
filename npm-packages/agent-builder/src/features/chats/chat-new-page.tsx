import { isReferenceProvider } from '@shared/agent';
import type { ChatSummary } from '@shared/chat';
import { useMutation, useQueryClient } from '@tanstack/react-query';
import { MessageSquarePlus, Plus } from 'lucide-react';
import { Link, useNavigate } from 'react-router';
import { EmptyState } from '@/components/empty-state';
import { ErrorState } from '@/components/error-state';
import { PageHeader } from '@/components/page-header';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Skeleton } from '@/components/ui/skeleton';
import { useAgent, useAgentList, useCapabilities } from '@/features/agents/api';
import { useProviders } from '@/features/providers/api';
import { cacheChat, createChat } from './api';
import {
  ChatRuntimeNotice,
  isRetryableRuntime,
  runtimeReasonsFromError,
  SandboxStatusNotice,
} from './runtime-notice';

function AgentProviderNotice({ agentId }: { readonly agentId: string }) {
  const agent = useAgent(agentId);
  const providers = useProviders();
  if (!agent.data)
    return (
      <p className="text-xs text-muted-foreground">Provider 状态加载中；服务端将在首次运行时检查凭据。</p>
    );
  const provider = agent.data.config.provider;
  const configured =
    provider &&
    isReferenceProvider(provider) &&
    providers.data?.find((item) => item.id === provider.providerId)?.hasCredential;
  return (
    <p className="text-xs text-muted-foreground">
      {!provider
        ? '未选择 Provider，需先编辑 Agent。'
        : !isReferenceProvider(provider)
          ? '旧内嵌配置不能启动新运行；请编辑 Agent 选择共享 Provider，并在 Providers 管理页填写 SQLite API key。'
          : configured
            ? 'Provider 凭据已存在（未认证），基础设施就绪不等于模型已连通。'
            : '此 Provider 待填写 SQLite API key 或状态不可用；请检查 Providers 管理页，缺凭据时服务端会拒绝。'}
    </p>
  );
}

export function ChatNewPage() {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const agentsQuery = useAgentList();
  const capabilities = useCapabilities();
  // A store-only outage is retryable: the POST itself performs the lazy re-probe.
  const runtimeReady =
    capabilities.data?.chatRuntime === 'wasm' ||
    isRetryableRuntime(capabilities.data?.chatRuntimeReasons ?? []);

  const createMutation = useMutation({
    mutationFn: (agentId: string) => createChat(agentId),
    onSuccess: async (chat) => {
      // Write first so the chat page can render immediately, then invalidate so the list refetches
      // instead of serving a 30s-stale empty array (the "对话不存在" regression).
      queryClient.setQueryData<ChatSummary[]>(['chats'], (current) => {
        if (current === undefined) return [chat];
        return current.some((item) => item.chatId === chat.chatId) ? current : [chat, ...current];
      });
      // The list is eventually consistent (read replicas); the single-chat cache is authoritative.
      cacheChat(queryClient, chat);
      // A successful create proves the Store answers again; refresh the runtime notice.
      void queryClient.invalidateQueries({ queryKey: ['capabilities'] });
      await queryClient.invalidateQueries({ queryKey: ['chats'] });
      void navigate(`/chats/${chat.chatId}`);
    },
  });

  const errorReasons = runtimeReasonsFromError(createMutation.error);

  return (
    <div className="space-y-5">
      <PageHeader
        title="新建对话"
        description="选择 Agent 后创建对话；会话在第一次发送时建立并持久化到 Store。"
        actions={
          <Button asChild variant="outline">
            <Link to="/chats">返回列表</Link>
          </Button>
        }
      />

      {capabilities.data !== undefined ? (
        <ChatRuntimeNotice
          mode={capabilities.data.chatRuntime}
          reasons={capabilities.data.chatRuntimeReasons}
        />
      ) : null}

      {capabilities.data !== undefined ? <SandboxStatusNotice status={capabilities.data.sandbox} /> : null}

      {/* Disabled buttons alone do not explain a cold capabilities failure: show the error + retry. */}
      {capabilities.isError && capabilities.data === undefined ? (
        <ErrorState
          title="无法加载运行时状态"
          error={capabilities.error}
          onRetry={() => {
            void capabilities.refetch();
          }}
        />
      ) : null}

      {createMutation.isError && errorReasons.length > 0 ? (
        <ChatRuntimeNotice mode="not_configured" reasons={errorReasons} />
      ) : null}

      {createMutation.isError ? <ErrorState title="无法创建对话" error={createMutation.error} /> : null}

      {agentsQuery.isPending ? (
        <div className="grid gap-3 sm:grid-cols-2">
          {['first', 'second'].map((row) => (
            <Skeleton key={row} className="h-32 w-full" />
          ))}
        </div>
      ) : null}

      {agentsQuery.isError ? (
        <ErrorState
          title="无法加载 Agent 列表"
          error={agentsQuery.error}
          onRetry={() => {
            void agentsQuery.refetch();
          }}
        />
      ) : null}

      {agentsQuery.isSuccess && agentsQuery.data.length === 0 ? (
        <EmptyState
          title="还没有可用的 Agent"
          description="先创建一个 Agent 并配置 provider，再回来开始对话。"
          action={
            <Button asChild>
              <Link to="/agents/new">
                <Plus />
                新建 Agent
              </Link>
            </Button>
          }
        />
      ) : null}

      {agentsQuery.isSuccess && agentsQuery.data.length > 0 ? (
        <div className="grid gap-3 sm:grid-cols-2">
          {agentsQuery.data.map((agent) => (
            <Card key={agent.id}>
              <CardHeader className="pb-2">
                <CardTitle className="text-base">{agent.name}</CardTitle>
              </CardHeader>
              <CardContent className="space-y-3">
                <p className="min-h-10 text-sm text-muted-foreground">
                  {agent.description.trim().length > 0 ? agent.description : '未填写描述'}
                </p>
                <AgentProviderNotice agentId={agent.id} />
                <Button
                  className="w-full"
                  disabled={!runtimeReady || createMutation.isPending}
                  onClick={() => createMutation.mutate(agent.id)}
                >
                  <MessageSquarePlus />
                  {createMutation.isPending ? '创建中…' : '开始对话'}
                </Button>
              </CardContent>
            </Card>
          ))}
        </div>
      ) : null}
    </div>
  );
}
