import { useQuery } from '@tanstack/react-query';
import { Plus } from 'lucide-react';
import { Link, NavLink, Outlet, useLocation } from 'react-router';
import { ThemeToggle } from '@/components/theme-toggle';
import { Button } from '@/components/ui/button';
import { runtimeSummary } from '@/features/chats/runtime-notice';
import { fetchHealth } from '@/lib/api-client';
import { cn } from '@/lib/utils';

function breadcrumbFor(pathname: string): string[] {
  const parts = pathname.split('/').filter(Boolean);
  if (parts[0] === 'chats') {
    const crumbs = ['对话'];
    if (parts[1] === 'new') {
      crumbs.push('新建对话');
    } else if (parts[1]) {
      crumbs.push(`对话 ${parts[1].slice(0, 8)}`);
    }
    return crumbs;
  }
  if (parts[0] === 'providers') return ['Providers'];
  if (parts[0] !== 'agents') return ['Agents'];
  const crumbs = ['Agents'];
  if (parts[1] === 'new') {
    crumbs.push('新建 Agent');
  } else if (parts[1]) {
    crumbs.push(`Agent ${parts[1].slice(0, 8)}`);
    if (parts[2] === 'edit') crumbs.push('编辑');
    if (parts[2] === 'preview') crumbs.push('预览');
    if (parts[2] === 'tools') crumbs.push('工具');
  }
  return crumbs;
}

const navLinkClass = ({ isActive }: { isActive: boolean }): string =>
  cn(
    'rounded-md px-3 py-2 text-sm transition-colors',
    isActive
      ? 'bg-sidebar-accent font-medium text-sidebar-accent-foreground'
      : 'text-muted-foreground hover:bg-sidebar-accent/60 hover:text-sidebar-accent-foreground',
  );

export function AppShell() {
  const location = useLocation();
  const health = useQuery({
    queryKey: ['health'],
    queryFn: fetchHealth,
    retry: false,
    refetchInterval: 30_000,
  });

  const engine = health.data?.engine;
  const summary = health.isPending
    ? { label: 'API 连接中', detail: null }
    : health.isError || engine === undefined
      ? { label: 'API 不可用', detail: null }
      : runtimeSummary(engine.status, engine.reasons ?? []);
  const statusTone = health.isError
    ? 'bg-destructive'
    : health.isPending
      ? 'bg-muted-foreground'
      : engine?.status === 'ready'
        ? 'bg-emerald-500'
        : 'bg-amber-500';

  const crumbs = breadcrumbFor(location.pathname);

  return (
    <div className="flex min-h-screen w-full bg-background text-foreground">
      <aside className="hidden w-60 shrink-0 flex-col border-r bg-sidebar text-sidebar-foreground md:flex">
        <Link to="/agents" className="block border-b px-5 py-4">
          <p className="text-sm font-semibold tracking-tight">Peri Agent Builder</p>
          <p className="mt-1 text-xs text-muted-foreground">本地 Agent 配置与验证</p>
        </Link>

        <div className="p-3">
          <Button asChild className="w-full justify-start" size="sm">
            <Link to="/agents/new">
              <Plus />
              新建 Agent
            </Link>
          </Button>
        </div>

        <nav className="flex flex-1 flex-col gap-1 px-3">
          <NavLink to="/agents" end className={navLinkClass}>
            Agents
          </NavLink>
          <NavLink to="/providers" className={navLinkClass}>
            Providers
          </NavLink>
          <NavLink to="/chats" className={navLinkClass}>
            对话
          </NavLink>
        </nav>

        <div className="border-t p-4 text-xs text-muted-foreground">
          <div className="flex items-center gap-2">
            <span className={cn('size-2 rounded-full', statusTone)} aria-hidden="true" />
            <span>{summary.label}</span>
          </div>
          {summary.detail !== null ? <p className="mt-1 opacity-70">{summary.detail}</p> : null}
          <p className="mt-2 opacity-70">仅监听 127.0.0.1</p>
        </div>
      </aside>

      <div className="flex min-w-0 flex-1 flex-col">
        <header className="flex h-14 shrink-0 items-center gap-3 border-b px-4 sm:px-6">
          <Link to="/agents" className="text-sm font-semibold md:hidden">
            Peri
          </Link>
          <nav aria-label="面包屑" className="hidden min-w-0 items-center gap-2 text-sm md:flex">
            {crumbs.map((crumb, index) => (
              <span key={crumb} className="flex min-w-0 items-center gap-2">
                {index > 0 ? <span className="text-muted-foreground">/</span> : null}
                <span
                  className={cn(
                    'truncate',
                    index === crumbs.length - 1 ? 'font-medium' : 'text-muted-foreground',
                  )}
                >
                  {crumb}
                </span>
              </span>
            ))}
          </nav>
          <div className="ml-auto flex items-center gap-1">
            <Button asChild variant="ghost" size="sm" className="md:hidden">
              <Link to="/agents/new">新建</Link>
            </Button>
            <Button asChild variant="ghost" size="sm" className="md:hidden">
              <Link to="/providers">Providers</Link>
            </Button>
            <ThemeToggle />
          </div>
        </header>
        <main className="min-w-0 flex-1 p-4 sm:p-6">
          <Outlet />
        </main>
      </div>
    </div>
  );
}
