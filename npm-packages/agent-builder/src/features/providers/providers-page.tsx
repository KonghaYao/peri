import type { Provider } from '@shared/provider';
import { useQueryClient } from '@tanstack/react-query';
import { useState } from 'react';
import { toast } from 'sonner';
import { ConfirmDialog } from '@/components/confirm-dialog';
import { EmptyState } from '@/components/empty-state';
import { ErrorState } from '@/components/error-state';
import { PageHeader } from '@/components/page-header';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Skeleton } from '@/components/ui/skeleton';
import { describeApiError } from '@/lib/api-client';
import { deleteProvider, invalidateProviders, useProviders } from './api';
import { ProviderDialog } from './provider-form';

export function ProvidersPage() {
  const query = useProviders();
  const client = useQueryClient();
  const [open, setOpen] = useState(false);
  const [editing, setEditing] = useState<Provider>();
  const [removing, setRemoving] = useState<Provider | null>(null);
  const [pending, setPending] = useState(false);
  const [failure, setFailure] = useState<unknown>(null);
  const remove = async () => {
    if (!removing) return;
    setPending(true);
    setFailure(null);
    try {
      await deleteProvider(removing);
      setRemoving(null);
      await invalidateProviders(client);
      toast.success('Provider 已删除');
    } catch (error) {
      setFailure(error);
      toast.error(describeApiError(error));
    } finally {
      setPending(false);
    }
  };
  const add = () => {
    setEditing(undefined);
    setOpen(true);
  };
  return (
    <div className="space-y-5">
      <PageHeader
        title="Providers"
        description="共享连接与凭据，仅影响后续新运行；凭据存在不表示已认证。"
        actions={<Button onClick={add}>添加 Provider</Button>}
      />
      <Button variant="outline" disabled={query.isFetching} onClick={() => void query.refetch()}>
        刷新 Providers
      </Button>
      {query.isPending ? <Skeleton className="h-40 w-full" /> : null}
      {query.isError ? (
        <ErrorState title="无法加载 Providers" error={query.error} onRetry={() => void query.refetch()} />
      ) : null}
      {failure ? (
        <ErrorState
          title="未删除 Provider"
          error={failure}
          onRetry={() => {
            setFailure(null);
            setRemoving(null);
            void query.refetch();
          }}
        />
      ) : null}
      {query.isSuccess && query.data.length === 0 ? (
        <EmptyState
          title="还没有 Provider"
          description="添加 Anthropic 或 OpenAI 连接并填写 SQLite API key，再在 Agent 中选择。"
          action={<Button onClick={add}>添加 Provider</Button>}
        />
      ) : null}
      <div className="grid gap-4 lg:grid-cols-2">
        {query.data?.map((provider) => (
          <Card key={provider.id}>
            <CardHeader>
              <CardTitle>{provider.name}</CardTitle>
            </CardHeader>
            <CardContent className="space-y-3">
              <p className="text-sm">
                {provider.type} · {provider.hasCredential ? '凭据已存在（未认证）' : '待填写 SQLite API key'}
              </p>
              <p className="break-all text-xs text-muted-foreground">
                {provider.baseUrl ?? '协议默认 endpoint'}
              </p>
              <p className="text-xs text-muted-foreground">
                Agent 引用 {provider.agentCount} · 运行引用 {provider.runtimeCount} · revision{' '}
                {provider.revision}
              </p>
              <div className="flex gap-2">
                <Button
                  variant="outline"
                  onClick={() => {
                    setEditing(provider);
                    setOpen(true);
                  }}
                >
                  编辑 {provider.name}
                </Button>
                <Button
                  variant="outline"
                  disabled={provider.agentCount + provider.runtimeCount > 0 || pending}
                  onClick={() => setRemoving(provider)}
                >
                  删除 {provider.name}
                </Button>
              </div>
            </CardContent>
          </Card>
        ))}
      </div>
      <ProviderDialog open={open} initial={editing} onOpenChange={setOpen} onSaved={() => {}} />
      <ConfirmDialog
        open={removing !== null}
        onOpenChange={(next) => {
          if (!pending && !next) setRemoving(null);
        }}
        title="删除 Provider"
        description={`确认删除「${removing?.name ?? ''}」？存在 Agent 或未确认关闭运行引用时会拒绝删除。`}
        pending={pending}
        onConfirm={() => void remove()}
      />
    </div>
  );
}
