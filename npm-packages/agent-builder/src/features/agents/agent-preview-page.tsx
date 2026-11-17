import { Copy, RefreshCw, TriangleAlert } from 'lucide-react';
import { Link, useParams } from 'react-router';
import { toast } from 'sonner';
import { ErrorState } from '@/components/error-state';
import { Badge } from '@/components/ui/badge';
import { Button, buttonVariants } from '@/components/ui/button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Skeleton } from '@/components/ui/skeleton';
import { useAgent, useAgentPreview } from './api';

export function AgentPreviewPage() {
  const { agentId } = useParams();
  const agentQuery = useAgent(agentId);
  const previewQuery = useAgentPreview(agentQuery.data);

  // A disabled query stays `isPending` forever, so errors must win before the skeleton.
  if (agentQuery.isError || previewQuery.isError) {
    return (
      <div className="space-y-4">
        <ErrorState
          title="无法生成预览"
          error={agentQuery.error ?? previewQuery.error ?? new Error('Preview unavailable')}
          onRetry={() => {
            void agentQuery.refetch();
            void previewQuery.refetch();
          }}
        />
        <Link className={buttonVariants({ variant: 'outline', size: 'sm' })} to="/agents">
          返回列表
        </Link>
      </div>
    );
  }

  if (agentQuery.isPending || previewQuery.isPending) {
    return (
      <div className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_320px]">
        <Skeleton className="h-96 w-full" />
        <div className="space-y-4">
          <Skeleton className="h-32 w-full" />
          <Skeleton className="h-48 w-full" />
        </div>
      </div>
    );
  }

  if (!agentQuery.data || !previewQuery.data) {
    return <ErrorState title="无法生成预览" error={new Error('Preview unavailable')} />;
  }

  const preview = previewQuery.data;
  const json = JSON.stringify(preview.target, null, 2);
  const overrides = Object.entries(agentQuery.data.config.metaHarness).sort(([left], [right]) =>
    left.localeCompare(right),
  );

  const copyJson = async () => {
    try {
      await navigator.clipboard.writeText(json);
      toast.success('已复制目标 JSON');
    } catch {
      toast.error('复制失败，请手动选择 JSON');
    }
  };

  return (
    <div className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_320px]">
      <Card className="min-w-0">
        <CardHeader className="flex-row items-start justify-between gap-3 space-y-0">
          <div>
            <CardTitle>运行目标</CardTitle>
            <CardDescription>
              apiKey、authToken 与 MCP headers/env 值已替换为 [redacted]；此请求不写入数据库。
            </CardDescription>
          </div>
          <div className="flex shrink-0 gap-2">
            <Button variant="outline" size="sm" onClick={() => void copyJson()}>
              <Copy />
              复制 JSON
            </Button>
            <Button
              variant="ghost"
              size="icon"
              aria-label="重新生成预览"
              onClick={() => void previewQuery.refetch()}
            >
              <RefreshCw />
            </Button>
          </div>
        </CardHeader>
        <CardContent>
          <pre className="max-h-[560px] overflow-auto rounded-lg bg-muted p-3 font-mono text-xs leading-relaxed">
            {json}
          </pre>
        </CardContent>
      </Card>

      <div className="space-y-4">
        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2 text-base">
              <TriangleAlert className="size-4" />
              警告
            </CardTitle>
            <CardDescription>这些警告来自当前运行时的能力边界。</CardDescription>
          </CardHeader>
          <CardContent>
            {preview.warnings.length === 0 ? (
              <p className="text-sm text-muted-foreground">没有警告。</p>
            ) : (
              <ul className="space-y-3">
                {preview.warnings.map((warning) => (
                  <li key={warning.code} className="space-y-1">
                    <Badge variant="secondary" className="font-mono text-[10px]">
                      {warning.code}
                    </Badge>
                    <p className="text-sm text-muted-foreground">{warning.message}</p>
                  </li>
                ))}
              </ul>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="text-base">能力覆盖</CardTitle>
            <CardDescription>未列出的能力键使用运行时默认值。</CardDescription>
          </CardHeader>
          <CardContent>
            {overrides.length === 0 ? (
              <p className="text-sm text-muted-foreground">没有显式覆盖。</p>
            ) : (
              <ul className="divide-y rounded-lg border">
                {overrides.map(([key, enabled]) => (
                  <li key={key} className="flex items-center gap-2 px-3 py-2">
                    <span className="min-w-0 flex-1 truncate font-mono text-xs" title={key}>
                      {key}
                    </span>
                    <Badge variant={enabled ? 'default' : 'secondary'}>{enabled ? '开启' : '关闭'}</Badge>
                  </li>
                ))}
              </ul>
            )}
          </CardContent>
        </Card>
      </div>
    </div>
  );
}
