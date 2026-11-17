import type { Agent, AgentCapabilities } from '@shared/agent';
import { useQueryClient } from '@tanstack/react-query';
import { useState } from 'react';
import { useParams } from 'react-router';
import { ErrorState } from '@/components/error-state';
import { Skeleton } from '@/components/ui/skeleton';
import { AgentForm } from './agent-form';
import { getAgent, updateAgent, useAgent, useCapabilities } from './api';
import { draftFromAgent } from './draft';

function AgentEditor({
  initial,
  capabilities,
}: {
  readonly initial: Agent;
  readonly capabilities: AgentCapabilities;
}) {
  const client = useQueryClient();
  // 查询刷新不能悄悄推进草稿的 CAS baseline。
  const [baseline, setBaseline] = useState(initial);
  return (
    <AgentForm
      defaultValues={draftFromAgent(initial)}
      submitLabel="保存修改"
      capabilities={capabilities}
      onReload={async () => {
        const fresh = await getAgent(initial.id);
        setBaseline(fresh);
        client.setQueryData(['agent', fresh.id], fresh);
        return draftFromAgent(fresh);
      }}
      onSubmit={async (draft) => {
        const accepted = await updateAgent(baseline.id, draft, baseline.revision);
        setBaseline(accepted);
        client.setQueryData(['agent', accepted.id], accepted);
        await Promise.all([
          client.invalidateQueries({ queryKey: ['agents'] }),
          client.invalidateQueries({ queryKey: ['providers'] }),
          client.invalidateQueries({ queryKey: ['agent', accepted.id, 'preview'] }),
        ]);
        return draftFromAgent(accepted);
      }}
    />
  );
}

export function AgentEditPage() {
  const { agentId } = useParams();
  const agent = useAgent(agentId);
  const capabilities = useCapabilities();
  if (agent.isPending || capabilities.isPending) return <Skeleton className="h-40 w-full" />;
  if (!agent.data || !capabilities.data)
    return (
      <ErrorState
        title="无法加载 Agent 配置"
        error={agent.error ?? capabilities.error}
        onRetry={() => {
          void agent.refetch();
          void capabilities.refetch();
        }}
      />
    );
  return <AgentEditor key={agent.data.id} initial={agent.data} capabilities={capabilities.data} />;
}
