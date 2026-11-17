import type { Agent, AgentCapabilities, AgentDraft, McpServerDraft } from '@shared/agent';
import { useMutation, useQueryClient } from '@tanstack/react-query';
import { Info, Save } from 'lucide-react';
import { useState } from 'react';
import { useParams } from 'react-router';
import { toast } from 'sonner';
import { ConfirmDialog } from '@/components/confirm-dialog';
import { ErrorState } from '@/components/error-state';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Skeleton } from '@/components/ui/skeleton';
import { CapabilitySwitches } from '@/features/mcp/capability-switches';
import { McpServersField } from '@/features/mcp/mcp-servers-field';
import { describeApiError } from '@/lib/api-client';
import { getAgent, updateAgent, useAgent, useCapabilities } from './api';

interface AgentToolsEditorProps {
  readonly agent: Agent;
  readonly capabilities: AgentCapabilities;
}

function AgentToolsEditor({ agent, capabilities }: AgentToolsEditorProps) {
  const queryClient = useQueryClient();
  const [baseline, setBaseline] = useState(agent);
  const [reload, setReload] = useState(false);
  const [mcpServers, setMcpServers] = useState<Record<string, McpServerDraft>>(agent.config.mcpServers);
  const [metaHarness, setMetaHarness] = useState<Record<string, boolean>>(agent.config.metaHarness);

  const dirty =
    JSON.stringify(mcpServers) !== JSON.stringify(baseline.config.mcpServers) ||
    JSON.stringify(metaHarness) !== JSON.stringify(baseline.config.metaHarness);

  const saveMutation = useMutation({
    mutationFn: async () => {
      const draft: AgentDraft = {
        name: baseline.name,
        description: baseline.description,
        config: {
          ...baseline.config,
          mcpServers,
          metaHarness,
        },
      };
      return updateAgent(baseline.id, draft, baseline.revision);
    },
    onSuccess: async (accepted) => {
      setBaseline(accepted);
      setMcpServers(accepted.config.mcpServers);
      setMetaHarness(accepted.config.metaHarness);
      queryClient.setQueryData(['agent', accepted.id], accepted);
      toast.success('工具配置已保存');
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ['agent', agent.id] }),
        queryClient.invalidateQueries({ queryKey: ['agents'] }),
      ]);
    },
    onError: (error) => {
      toast.error(describeApiError(error));
    },
  });

  return (
    <div className="space-y-4">
      <Card>
        <CardHeader>
          <CardTitle>MCP 服务器</CardTitle>
          <CardDescription>
            声明式配置只保存 http/stdio；添加或编辑时会即时检查重名、URL 与命令。
          </CardDescription>
        </CardHeader>
        <CardContent>
          <McpServersField value={mcpServers} onChange={setMcpServers} />
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>能力开关</CardTitle>
          <CardDescription>三态覆盖 MetaHarness 配置；键清单来自服务端 capabilities。</CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <CapabilitySwitches
            keys={capabilities.metaHarnessKeys}
            value={metaHarness}
            onChange={setMetaHarness}
          />
          <Alert>
            <Info />
            <AlertTitle>工具清单由运行时决定</AlertTitle>
            <AlertDescription>
              本页只声明 MCP 服务器与能力覆盖，不直接管理单个工具；实际可用工具在会话建立后由运行时返回。
            </AlertDescription>
          </Alert>
        </CardContent>
      </Card>

      {saveMutation.isError ? <ErrorState title="未保存工具配置" error={saveMutation.error} /> : null}
      <Button type="button" variant="outline" onClick={() => setReload(true)}>
        重新加载服务端版本
      </Button>
      <ConfirmDialog
        open={reload}
        onOpenChange={setReload}
        title="重新加载工具配置"
        description="将丢弃当前未保存的工具配置并读取最新版本。"
        confirmLabel="重新加载"
        onConfirm={() => {
          setReload(false);
          void getAgent(agent.id)
            .then((fresh) => {
              setBaseline(fresh);
              setMcpServers(fresh.config.mcpServers);
              setMetaHarness(fresh.config.metaHarness);
              queryClient.setQueryData(['agent', fresh.id], fresh);
              saveMutation.reset();
            })
            .catch((error) => toast.error(describeApiError(error)));
        }}
      />
      <div className="flex flex-wrap items-center justify-end gap-2">
        <Button
          type="button"
          variant="outline"
          disabled={!dirty || saveMutation.isPending}
          onClick={() => {
            setMcpServers(baseline.config.mcpServers);
            setMetaHarness(baseline.config.metaHarness);
          }}
        >
          重置
        </Button>
        <Button
          type="button"
          disabled={!dirty || saveMutation.isPending}
          onClick={() => saveMutation.mutate()}
        >
          <Save />
          {saveMutation.isPending ? '保存中…' : '保存工具配置'}
        </Button>
      </div>
    </div>
  );
}

export function AgentToolsPage() {
  const { agentId } = useParams();
  const agentQuery = useAgent(agentId);
  const capabilities = useCapabilities();

  if (agentQuery.isPending || capabilities.isPending) {
    return (
      <div className="space-y-4">
        <Skeleton className="h-48 w-full" />
        <Skeleton className="h-64 w-full" />
      </div>
    );
  }

  if (!agentQuery.data || !capabilities.data) {
    return (
      <ErrorState
        title="无法加载工具配置"
        error={agentQuery.error ?? capabilities.error ?? new Error('Agent not found')}
        onRetry={() => {
          void agentQuery.refetch();
          void capabilities.refetch();
        }}
      />
    );
  }

  return (
    <AgentToolsEditor key={agentQuery.data.id} agent={agentQuery.data} capabilities={capabilities.data} />
  );
}
