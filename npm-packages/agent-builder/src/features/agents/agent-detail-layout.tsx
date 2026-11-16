import { Link, NavLink, Outlet, useParams } from 'react-router';
import { ErrorState } from '@/components/error-state';
import { PageHeader } from '@/components/page-header';
import { Badge } from '@/components/ui/badge';
import { buttonVariants } from '@/components/ui/button';
import { Skeleton } from '@/components/ui/skeleton';
import { cn } from '@/lib/utils';
import { useAgent } from './api';

const TABS = [
  { to: 'edit', label: '编辑' },
  { to: 'preview', label: '预览' },
  { to: 'tools', label: '工具' },
] as const;

export function AgentDetailLayout() {
  const { agentId } = useParams();
  const agentQuery = useAgent(agentId);

  if (agentQuery.isPending) {
    return (
      <div className="space-y-5">
        <Skeleton className="h-16 w-full" />
        <Skeleton className="h-10 w-64" />
        <Skeleton className="h-96 w-full" />
      </div>
    );
  }

  if (!agentQuery.data) {
    return (
      <div className="space-y-4">
        <PageHeader title="Agent 不可用" description="该 Agent 可能已被删除，或者 API 暂时不可用。" />
        <ErrorState
          error={agentQuery.error ?? new Error('Agent not found')}
          onRetry={() => {
            void agentQuery.refetch();
          }}
        />
        <Link className={buttonVariants({ variant: 'outline', size: 'sm' })} to="/agents">
          返回列表
        </Link>
      </div>
    );
  }

  const agent = agentQuery.data;

  return (
    <div className="space-y-5">
      <PageHeader
        title={agent.name}
        description={agent.description || '未填写描述'}
        actions={
          <Badge variant="secondary">更新时间 {new Date(agent.updatedAt).toLocaleString('zh-CN')}</Badge>
        }
      />

      <nav aria-label="Agent 页面" className="flex flex-wrap gap-1 border-b pb-2">
        {TABS.map((tab) => (
          <NavLink
            key={tab.to}
            to={tab.to}
            className={({ isActive }) =>
              cn(
                buttonVariants({ variant: isActive ? 'secondary' : 'ghost', size: 'sm' }),
                isActive && 'font-semibold',
              )
            }
          >
            {tab.label}
          </NavLink>
        ))}
      </nav>

      {agentQuery.isError ? (
        <ErrorState
          title="刷新 Agent 失败；保留当前草稿"
          error={agentQuery.error}
          onRetry={() => void agentQuery.refetch()}
        />
      ) : null}
      <Outlet />
    </div>
  );
}
