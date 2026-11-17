import { type AgentProvider, isReferenceProvider } from '@shared/agent';
import { MODEL_SUGGESTIONS, type Provider, type ProviderType } from '@shared/provider';
import { useEffect, useRef, useState } from 'react';
import { ConfirmDialog } from '@/components/confirm-dialog';
import { ErrorState } from '@/components/error-state';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { useProviders } from '@/features/providers/api';
import { ProviderDialog } from '@/features/providers/provider-form';

export interface ProviderFieldsProps {
  readonly value: AgentProvider | null;
  readonly onChange: (value: AgentProvider | null) => void;
  readonly disabled?: boolean;
  readonly autoSelect?: boolean;
}

export function ProviderFields({
  value,
  onChange,
  disabled = false,
  autoSelect = false,
}: ProviderFieldsProps) {
  const query = useProviders();
  const [dialog, setDialog] = useState(false);
  const [switchTo, setSwitchTo] = useState<Provider | null>(null);
  const [created, setCreated] = useState<Provider | null>(null);
  const touched = useRef(false);
  const initialized = useRef(false);
  const currentValue = useRef(value);
  currentValue.current = value;
  const list =
    created && !query.data?.some((item) => item.id === created.id)
      ? [...(query.data ?? []), created]
      : (query.data ?? []);
  const selected =
    value && isReferenceProvider(value) ? list.find((item) => item.id === value.providerId) : undefined;
  const type: ProviderType | undefined =
    selected?.type ?? (value && !isReferenceProvider(value) ? value.type : undefined);
  const change = (next: AgentProvider | null) => {
    touched.current = true;
    onChange(next);
  };
  useEffect(() => {
    if (!autoSelect || initialized.current || !query.data) return;
    initialized.current = true;
    const usable = query.data.filter((item) => item.hasCredential);
    if (!touched.current && currentValue.current === null && usable.length === 1 && usable[0]) {
      const only = usable[0];
      onChange({ kind: 'reference', providerId: only.id, model: MODEL_SUGGESTIONS[only.type] });
    }
  }, [autoSelect, query.data, onChange]);

  const select = (provider: Provider) => {
    const model = value?.model ?? '';
    if (
      value &&
      type !== undefined &&
      type !== provider.type &&
      model !== '' &&
      model !== MODEL_SUGGESTIONS[type]
    ) {
      setSwitchTo(provider);
      return;
    }
    change({
      kind: 'reference',
      providerId: provider.id,
      model:
        model === '' || (type && model === MODEL_SUGGESTIONS[type])
          ? MODEL_SUGGESTIONS[provider.type]
          : model,
    });
  };
  const configured = selected?.hasCredential;

  return (
    <div className="space-y-3">
      {query.isPending ? (
        <p role="status" className="text-sm">
          加载 Providers…
        </p>
      ) : null}
      {query.isError ? (
        <ErrorState title="无法加载 Providers" error={query.error} onRetry={() => void query.refetch()} />
      ) : null}
      {query.isSuccess && list.length === 0 ? (
        <p className="text-sm text-muted-foreground">还没有共享 Provider，可在此添加并返回当前草稿。</p>
      ) : null}
      <div className="space-y-1.5">
        <Label htmlFor="agent-provider">Provider</Label>
        <Select
          disabled={disabled}
          value={value ? (isReferenceProvider(value) ? value.providerId : 'legacy') : 'none'}
          onValueChange={(id) => {
            if (id === 'none') change(null);
            else {
              const provider = list.find((item) => item.id === id);
              if (provider) select(provider);
            }
          }}
        >
          <SelectTrigger id="agent-provider">
            <SelectValue placeholder="请选择 Provider" />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="none">未选择 Provider</SelectItem>
            {value && !isReferenceProvider(value) ? (
              <SelectItem value="legacy">旧内嵌配置 · {value.id}</SelectItem>
            ) : null}
            {list.map((item) => (
              <SelectItem key={item.id} value={item.id}>
                {item.name} · {item.type} · {item.hasCredential ? '凭据存在' : '未配置'}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
      {value && isReferenceProvider(value) && query.isSuccess && !selected ? (
        <p role="alert">引用的 Provider 不存在，请重新选择。</p>
      ) : null}
      {value && !isReferenceProvider(value) ? (
        <p className="text-xs text-amber-600">
          旧内嵌配置仅可编辑；请明确选择共享 Provider，并在 Providers 管理页填写 SQLite API key 后启动新运行。
        </p>
      ) : configured === false ? (
        <p className="text-xs text-amber-600">
          此 Provider 待填写 SQLite API key；可以保存，但不能启动新运行。
        </p>
      ) : null}
      <div className="space-y-1.5">
        <Label htmlFor="provider-model">模型 ID</Label>
        <Input
          id="provider-model"
          disabled={disabled || value === null}
          value={value?.model ?? ''}
          placeholder={type ? MODEL_SUGGESTIONS[type] : '先选择 Provider'}
          onChange={(e) => {
            if (value) change({ ...value, model: e.target.value });
          }}
        />
        <p className="text-xs text-muted-foreground">
          建议值可编辑，不保证供应商当前支持；凭据存在不是连接成功。
        </p>
      </div>
      {value && !isReferenceProvider(value) ? (
        <details className="rounded border p-3">
          <summary>旧内嵌 Provider（兼容编辑）</summary>
          <div className="space-y-2 pt-3">
            <Label htmlFor="legacy-type">协议（旧内嵌配置）</Label>
            <Select
              value={value.type}
              disabled={disabled}
              onValueChange={(next) => {
                const type = next === 'openai' ? 'openai' : 'anthropic';
                if (type === value.type) return;
                if (
                  value.model !== MODEL_SUGGESTIONS[value.type] &&
                  !window.confirm('切换协议将更新手填模型为新协议建议，是否继续？')
                )
                  return;
                change({ ...value, type, model: MODEL_SUGGESTIONS[type] });
              }}
            >
              <SelectTrigger id="legacy-type">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="anthropic">anthropic</SelectItem>
                <SelectItem value="openai">openai</SelectItem>
              </SelectContent>
            </Select>
            <Label htmlFor="legacy-id">Provider ID</Label>
            <Input
              id="legacy-id"
              value={value.id}
              onChange={(e) => change({ ...value, id: e.target.value })}
            />
            <Label htmlFor="legacy-url">Base URL（旧配置）</Label>
            <Input
              id="legacy-url"
              value={value.baseUrl ?? ''}
              onChange={(e) => {
                const next = { ...value };
                if (e.target.value === '') delete next.baseUrl;
                else next.baseUrl = e.target.value;
                change(next);
              }}
            />
          </div>
        </details>
      ) : null}
      <Button type="button" variant="outline" disabled={disabled} onClick={() => setDialog(true)}>
        添加 Provider
      </Button>
      <ProviderDialog
        open={dialog}
        onOpenChange={setDialog}
        onSaved={(provider) => {
          setCreated(provider);
          select(provider);
        }}
      />
      <ConfirmDialog
        open={switchTo !== null}
        onOpenChange={(open) => {
          if (!open) setSwitchTo(null);
        }}
        title="切换 Provider 协议"
        description="当前模型是手填值。切换协议后将改用新协议的模型建议，其他草稿值保持不变。"
        confirmLabel="切换并更新模型"
        onConfirm={() => {
          if (switchTo)
            change({ kind: 'reference', providerId: switchTo.id, model: MODEL_SUGGESTIONS[switchTo.type] });
          setSwitchTo(null);
        }}
      />
    </div>
  );
}
