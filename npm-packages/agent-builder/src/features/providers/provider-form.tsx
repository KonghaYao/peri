import { type Provider, ProviderCreate, type ProviderType, providerOrigin } from '@shared/provider';
import { useQueryClient } from '@tanstack/react-query';
import { useEffect, useRef, useState } from 'react';
import { toast } from 'sonner';
import { ConfirmDialog } from '@/components/confirm-dialog';
import { Button } from '@/components/ui/button';
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { ApiClientError, describeApiError } from '@/lib/api-client';
import { createProvider, getProvider, invalidateProviders, updateProvider } from './api';

interface Props {
  readonly initial?: Provider | undefined;
  readonly onSaved: (provider: Provider) => void;
  readonly onCancel: () => void;
  readonly onPending: (pending: boolean) => void;
}

export function ProviderForm({ initial, onSaved, onCancel, onPending }: Props) {
  const client = useQueryClient();
  const [baseline, setBaseline] = useState(initial);
  const [name, setName] = useState(initial?.name ?? '');
  const [type, setType] = useState<ProviderType>(initial?.type ?? 'anthropic');
  const [baseUrl, setBaseUrl] = useState(initial?.baseUrl ?? '');
  const [key, setKey] = useState('');
  const [clear, setClear] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState('');
  const [conflict, setConflict] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const [reload, setReload] = useState(false);
  const fields = useRef<HTMLFieldSetElement>(null);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const save = async (confirmed = false) => {
    const parsed = ProviderCreate.safeParse({
      name,
      type,
      baseUrl: baseUrl === '' ? null : baseUrl,
      credentialSource: 'local',
      ...(clear
        ? { credential: { action: 'clear' } }
        : key === ''
          ? {}
          : { credential: { action: 'replace', value: key } }),
    });
    if (!parsed.success) {
      setKey('');
      setError(
        '请填写名称和协议；URL 只允许无凭据/query/fragment 的 HTTPS（HTTP 仅 loopback），key 不能全为空格。',
      );
      const field = parsed.error.issues[0]?.path[0];
      const id =
        field === 'name'
          ? 'shared-provider-name'
          : field === 'baseUrl'
            ? 'shared-provider-url'
            : field === 'credential'
              ? 'shared-provider-key'
              : 'shared-provider-type';
      fields.current?.querySelector<HTMLElement>(`#${id}`)?.focus();
      return;
    }
    const reuses = !clear && key === '' && baseline?.hasCredential === true;
    if (
      !confirmed &&
      baseline &&
      reuses &&
      providerOrigin(type, baseline.baseUrl) !== providerOrigin(type, parsed.data.baseUrl)
    ) {
      setConfirm(true);
      return;
    }
    setConfirm(false);
    setPending(true);
    onPending(true);
    setError('');
    try {
      const { type: _type, ...update } = parsed.data;
      const saved = baseline
        ? await updateProvider(
            baseline.id,
            { ...update, confirmCredentialOriginChange: confirmed },
            baseline.revision,
          )
        : await createProvider(parsed.data);
      if (!alive.current) return;
      setKey('');
      setClear(false);
      setBaseline(saved);
      await invalidateProviders(client);
      toast.success('Provider 已保存；仅影响后续新运行');
      onSaved(saved);
    } catch (failure) {
      if (!alive.current) return;
      setKey('');
      setError(describeApiError(failure));
      if (failure instanceof ApiClientError && failure.code === 'revision_conflict') setConflict(true);
      if (failure instanceof ApiClientError && failure.code === 'credential_origin_confirmation_required')
        setConfirm(true);
    } finally {
      if (alive.current) {
        setPending(false);
        onPending(false);
      }
    }
  };

  const reloadBaseline = async () => {
    if (!baseline) return;
    setKey('');
    setReload(false);
    setPending(true);
    onPending(true);
    try {
      const fresh = await getProvider(baseline.id);
      setBaseline(fresh);
      setName(fresh.name);
      setType(fresh.type);
      setBaseUrl(fresh.baseUrl ?? '');
      setKey('');
      setClear(false);
      setConflict(false);
      setError('');
    } catch (failure) {
      setError(describeApiError(failure));
    } finally {
      setPending(false);
      onPending(false);
    }
  };

  return (
    <div className="space-y-4">
      <fieldset ref={fields} disabled={pending} className="space-y-4">
        <div className="space-y-1.5">
          <Label htmlFor="shared-provider-name">Provider 名称</Label>
          <Input
            id="shared-provider-name"
            value={name}
            maxLength={80}
            onChange={(e) => setName(e.target.value)}
          />
        </div>
        <div className="space-y-1.5">
          <Label htmlFor="shared-provider-type">协议</Label>
          <Select
            value={type}
            disabled={baseline !== undefined}
            onValueChange={(value) => setType(value as ProviderType)}
          >
            <SelectTrigger id="shared-provider-type">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="anthropic">anthropic</SelectItem>
              <SelectItem value="openai">openai</SelectItem>
            </SelectContent>
          </Select>
        </div>
        <div className="space-y-1.5">
          <Label htmlFor="shared-provider-url">Base URL（可选）</Label>
          <Input
            id="shared-provider-url"
            value={baseUrl}
            onChange={(e) => setBaseUrl(e.target.value)}
            placeholder="留空使用协议默认 endpoint"
          />
          <p className="text-xs text-muted-foreground">
            HTTPS；HTTP 仅 loopback。禁止 userinfo、query、fragment 和首尾空格。
          </p>
        </div>
        <p className="text-xs text-muted-foreground">
          {baseline?.hasCredential
            ? '凭据已存在（不代表已认证）；输入框不会回填。'
            : '待填写 SQLite API key；未配置也可保存，但不能启动新运行。'}{' '}
          每条 Provider 独立保存在 SQLite；数据库、WAL/SHM 与备份均应作为秘密保护。
        </p>
        <div className="space-y-2">
          <Label htmlFor="shared-provider-key">API key（留空保持，永不读回）</Label>
          <Input
            id="shared-provider-key"
            type="password"
            autoComplete="new-password"
            value={key}
            disabled={clear}
            onChange={(e) => setKey(e.target.value)}
          />
          <Label className="flex items-center gap-2">
            <input
              type="checkbox"
              checked={clear}
              onChange={(e) => {
                setClear(e.target.checked);
                setKey('');
              }}
            />
            清除已存 key
          </Label>
        </div>
        {error ? (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        ) : null}
        {conflict ? (
          <Button type="button" variant="outline" onClick={() => setReload(true)}>
            重新加载服务端版本
          </Button>
        ) : null}
        <div className="flex justify-end gap-2">
          <Button
            type="button"
            variant="outline"
            onClick={() => {
              setKey('');
              onCancel();
            }}
          >
            取消
          </Button>
          <Button type="button" onClick={() => void save()}>
            {pending ? '保存中…' : '保存 Provider'}
          </Button>
        </div>
      </fieldset>
      <ConfirmDialog
        open={confirm}
        onOpenChange={setConfirm}
        title="确认凭据发送范围"
        description="Base URL 的 origin 已改变。继续会把现有 SQLite 凭据发送到新的目标；取消不会提交。"
        confirmLabel="确认并保存"
        onConfirm={() => void save(true)}
      />
      <ConfirmDialog
        open={reload}
        onOpenChange={setReload}
        title="重新加载 Provider"
        description="将丢弃当前未保存的表单和 key 输入，加载最新版本。"
        confirmLabel="重新加载"
        onConfirm={() => void reloadBaseline()}
      />
    </div>
  );
}

export function ProviderDialog({
  open,
  initial,
  onOpenChange,
  onSaved,
}: {
  readonly open: boolean;
  readonly initial?: Provider | undefined;
  readonly onOpenChange: (open: boolean) => void;
  readonly onSaved: (provider: Provider) => void;
}) {
  const [pending, setPending] = useState(false);
  useEffect(() => {
    if (!open) setPending(false);
  }, [open]);
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!pending) onOpenChange(next);
      }}
    >
      <DialogContent className="max-h-[90vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>{initial ? '编辑 Provider' : '添加 Provider'}</DialogTitle>
          <DialogDescription>共享连接与凭据；模型、指令和工具由每个 Agent 独立持有。</DialogDescription>
        </DialogHeader>
        {open ? (
          <ProviderForm
            key={initial?.id ?? 'new'}
            initial={initial}
            onPending={setPending}
            onCancel={() => onOpenChange(false)}
            onSaved={(provider) => {
              onSaved(provider);
              onOpenChange(false);
            }}
          />
        ) : null}
      </DialogContent>
    </Dialog>
  );
}
