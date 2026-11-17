import type { AgentSummary } from '@shared/agent';
import { useMutation, useQueryClient } from '@tanstack/react-query';
import { Pencil, Plus, ScanEye, Trash2 } from 'lucide-react';
import { useState } from 'react';
import { Link } from 'react-router';
import { toast } from 'sonner';
import { ConfirmDialog } from '@/components/confirm-dialog';
import { EmptyState } from '@/components/empty-state';
import { ErrorState } from '@/components/error-state';
import { PageHeader } from '@/components/page-header';
import { Button } from '@/components/ui/button';
import { Skeleton } from '@/components/ui/skeleton';
import { describeApiError } from '@/lib/api-client';
import { deleteAgent, useAgentList } from './api';

const dateFormat = new Intl.DateTimeFormat('zh-CN', { dateStyle: 'medium', timeStyle: 'short' });

function formatDate(value: number): string {
  return dateFormat.format(new Date(value));
}

const SKELETON_ROWS = ['first', 'second', 'third', 'fourth', 'fifth'];

function ListSkeleton() {
  return (
    <div className="space-y-2 rounded-xl border bg-card p-4">
      {SKELETON_ROWS.map((row) => (
        <Skeleton key={row} className="h-12 w-full" />
      ))}
    </div>
  );
}

export function AgentListPage() {
  const queryClient = useQueryClient();
  const agentsQuery = useAgentList();
  const [deleteTarget, setDeleteTarget] = useState<AgentSummary | null>(null);

  const deleteMutation = useMutation({
    mutationFn: (target: AgentSummary) => deleteAgent(target.id, target.revision),
    onSuccess: async (_result, target) => {
      setDeleteTarget(null);
      toast.success('Agent 已删除');
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ['agents'] }),
        queryClient.invalidateQueries({ queryKey: ['providers'] }),
      ]);
      queryClient.removeQueries({ queryKey: ['agent', target.id] });
    },
    onError: (error) => {
      toast.error(describeApiError(error));
    },
  });

  return (
    <div className="space-y-5">
      <PageHeader
        title="Agents"
        description="配置指令、provider、MCP 服务器与能力开关；配置只保存在本地。"
        actions={
          <Button asChild>
            <Link to="/agents/new">
              <Plus />
              新建 Agent
            </Link>
          </Button>
        }
      />

      {deleteMutation.isError ? (
        <ErrorState
          title="未删除 Agent"
          error={deleteMutation.error}
          onRetry={() => {
            setDeleteTarget(null);
            deleteMutation.reset();
            void agentsQuery.refetch();
          }}
        />
      ) : null}
      {agentsQuery.isPending ? <ListSkeleton /> : null}

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
          title="还没有 Agent"
          description="创建第一个 Agent，配置 provider 与 MCP 服务器后即可预览运行时目标。"
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
        <div className="overflow-hidden rounded-xl border bg-card shadow">
          <div className="overflow-x-auto">
            <table className="w-full min-w-[680px] text-sm">
              <thead className="border-b bg-muted/40 text-left text-xs uppercase tracking-wide text-muted-foreground">
                <tr>
                  <th className="px-4 py-3 font-medium">名称</th>
                  <th className="px-4 py-3 font-medium">描述</th>
                  <th className="px-4 py-3 font-medium">更新时间</th>
                  <th className="px-4 py-3 text-right font-medium">操作</th>
                </tr>
              </thead>
              <tbody>
                {agentsQuery.data.map((agent) => (
                  <tr key={agent.id} className="border-b last:border-b-0 hover:bg-muted/30">
                    <td className="max-w-[220px] truncate px-4 py-3 font-medium">
                      <Link className="hover:underline" to={`/agents/${agent.id}/edit`}>
                        {agent.name}
                      </Link>
                    </td>
                    <td className="max-w-[320px] truncate px-4 py-3 text-muted-foreground">
                      {agent.description || '—'}
                    </td>
                    <td className="whitespace-nowrap px-4 py-3 text-muted-foreground">
                      {formatDate(agent.updatedAt)}
                    </td>
                    <td className="px-4 py-3">
                      <div className="flex justify-end gap-1">
                        <Button asChild variant="ghost" size="sm">
                          <Link to={`/agents/${agent.id}/edit`}>
                            <Pencil />
                            编辑
                          </Link>
                        </Button>
                        <Button asChild variant="ghost" size="sm">
                          <Link to={`/agents/${agent.id}/preview`}>
                            <ScanEye />
                            预览
                          </Link>
                        </Button>
                        <Button
                          variant="ghost"
                          size="sm"
                          className="text-destructive hover:text-destructive"
                          onClick={() => setDeleteTarget(agent)}
                        >
                          <Trash2 />
                          删除
                        </Button>
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      ) : null}

      <ConfirmDialog
        open={deleteTarget !== null}
        onOpenChange={(open) => {
          if (!open) setDeleteTarget(null);
        }}
        title="删除 Agent"
        description={`删除「${deleteTarget?.name ?? ''}」后无法恢复；已有对话的 Agent 会拒绝删除。`}
        pending={deleteMutation.isPending}
        onConfirm={() => {
          if (deleteTarget) deleteMutation.mutate(deleteTarget);
        }}
      />
    </div>
  );
}
