import type { AgentDraft } from '@shared/agent';
import { useMutation, useQueryClient } from '@tanstack/react-query';
import { useNavigate } from 'react-router';
import { ErrorState } from '@/components/error-state';
import { PageHeader } from '@/components/page-header';
import { Skeleton } from '@/components/ui/skeleton';
import { AgentForm } from './agent-form';
import { createAgent, useCapabilities } from './api';
import { EMPTY_AGENT_DRAFT } from './draft';

export function AgentCreatePage() {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const capabilities = useCapabilities();
  const createMutation = useMutation({
    mutationFn: (draft: AgentDraft) => createAgent(draft),
    onSuccess: async () => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ['agents'] }),
        queryClient.invalidateQueries({ queryKey: ['providers'] }),
      ]);
    },
  });

  return (
    <div className="space-y-5">
      <PageHeader title="新建 Agent" description="保存后可以继续编辑、预览运行目标并配置 MCP 工具。" />

      {capabilities.isPending ? (
        <div className="space-y-3">
          <Skeleton className="h-40 w-full" />
          <Skeleton className="h-40 w-full" />
        </div>
      ) : null}

      {capabilities.isError && capabilities.data === undefined ? (
        <ErrorState
          title="无法加载能力清单"
          error={capabilities.error}
          onRetry={() => {
            void capabilities.refetch();
          }}
        />
      ) : null}

      {capabilities.data !== undefined ? (
        <AgentForm
          defaultValues={EMPTY_AGENT_DRAFT}
          submitLabel="创建 Agent"
          successMessage="Agent 已创建"
          resetOnSuccess={false}
          capabilities={capabilities.data}
          showTemplate
          onSaved={() => {
            if (createMutation.data) void navigate(`/agents/${createMutation.data.id}/edit`);
          }}
          onSubmit={async (draft) => {
            return createMutation.mutateAsync(draft);
          }}
        />
      ) : null}
    </div>
  );
}
