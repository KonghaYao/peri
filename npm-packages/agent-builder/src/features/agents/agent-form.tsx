import { zodResolver } from '@hookform/resolvers/zod';
import { type AgentCapabilities, AgentDraft, type AgentDraftInput } from '@shared/agent';
import { Save } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import { Controller, useForm } from 'react-hook-form';
import { useNavigate } from 'react-router';
import { toast } from 'sonner';
import { ConfirmDialog } from '@/components/confirm-dialog';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Form, FormControl, FormField, FormItem, FormLabel, FormMessage } from '@/components/ui/form';
import { Input } from '@/components/ui/input';
import { Textarea } from '@/components/ui/textarea';
import { CapabilitySwitches } from '@/features/mcp/capability-switches';
import { McpServersField } from '@/features/mcp/mcp-servers-field';
import { ApiClientError, describeApiError } from '@/lib/api-client';
import { officialDefaultDraft } from './draft';
import { DraftNavigationGuard } from './draft-navigation-guard';
import { ProviderFields } from './provider-fields';

export interface AgentFormProps {
  readonly defaultValues: AgentDraft;
  readonly submitLabel: string;
  readonly capabilities: AgentCapabilities;
  readonly onSubmit: (draft: AgentDraft) => Promise<AgentDraft> | Promise<void>;
  readonly onSaved?: () => void;
  readonly onReload?: () => Promise<AgentDraft>;
  readonly successMessage?: string;
  readonly resetOnSuccess?: boolean;
  readonly showTemplate?: boolean;
}

export function AgentForm({
  defaultValues,
  submitLabel,
  capabilities,
  onSubmit,
  onSaved,
  onReload,
  successMessage = '已保存',
  resetOnSuccess = true,
  showTemplate = false,
}: AgentFormProps) {
  const navigate = useNavigate();
  const form = useForm<AgentDraftInput, unknown, AgentDraft>({
    resolver: zodResolver(AgentDraft),
    defaultValues: structuredClone(defaultValues),
    mode: 'onSubmit',
  });
  const [advanced, setAdvanced] = useState(false);
  const [confirm, setConfirm] = useState<'template' | 'blank' | 'reload' | null>(null);
  const [undo, setUndo] = useState<AgentDraft | null>(null);
  const [conflict, setConflict] = useState(false);
  const [failure, setFailure] = useState('');
  const [saved, setSaved] = useState(false);
  const [reloading, setReloading] = useState(false);
  const applying = useRef(false);
  const focusAdvanced = useRef<HTMLDivElement>(null);
  const dirty = form.formState.isDirty;
  const pending = form.formState.isSubmitting || reloading;
  useEffect(() => {
    const watch = form.watch(() => {
      if (!applying.current) setUndo(null);
      setSaved(false);
    });
    return () => watch.unsubscribe();
  }, [form]);
  useEffect(() => {
    if (!dirty || saved) return;
    const warn = (event: BeforeUnloadEvent) => event.preventDefault();
    window.addEventListener('beforeunload', warn);
    return () => window.removeEventListener('beforeunload', warn);
  }, [dirty, saved]);

  const apply = (draft: AgentDraft) => {
    applying.current = true;
    form.setValue('name', draft.name, { shouldDirty: true });
    form.setValue('description', draft.description, { shouldDirty: true });
    form.setValue('config', structuredClone(draft.config), { shouldDirty: true });
    applying.current = false;
  };
  const applyTemplate = () => {
    const before = structuredClone(form.getValues()) as AgentDraft;
    apply(officialDefaultDraft(before));
    setUndo(before);
    setConfirm(null);
  };
  const blank = () => {
    const current = structuredClone(form.getValues()) as AgentDraft;
    apply({ ...current, config: { ...current.config, instructions: '', mcpServers: {}, metaHarness: {} } });
    setUndo(null);
    setConfirm(null);
  };
  const submit = form.handleSubmit(
    async (values) => {
      setFailure('');
      try {
        const accepted = await onSubmit(values);
        if (resetOnSuccess) form.reset(accepted ?? values);
        setUndo(null);
        setConflict(false);
        setSaved(true);
        if (successMessage) toast.success(successMessage);
      } catch (error) {
        const message = describeApiError(error);
        setFailure(message);
        toast.error(message);
        if (error instanceof ApiClientError && error.code === 'revision_conflict') setConflict(true);
      }
    },
    (errors) => {
      if (
        errors.description ||
        errors.config?.instructions ||
        errors.config?.mcpServers ||
        errors.config?.metaHarness
      ) {
        setAdvanced(true);
        requestAnimationFrame(() => {
          const target =
            focusAdvanced.current?.querySelector<HTMLElement>('[aria-invalid="true"]') ??
            focusAdvanced.current;
          target?.focus();
        });
      } else if (errors.config?.provider) {
        document.getElementById('provider-model')?.focus();
      }
    },
  );
  useEffect(() => {
    if (saved) onSaved?.();
  }, [saved, onSaved]);
  const values = form.watch();
  const count =
    Object.keys(values.config?.mcpServers ?? {}).length +
    Object.keys(values.config?.metaHarness ?? {}).length +
    (values.config?.instructions ? 1 : 0) +
    (values.description ? 1 : 0);

  return (
    <Form {...form}>
      <DraftNavigationGuard dirty={dirty && !saved} />
      <form className="space-y-5" onSubmit={submit} noValidate>
        <fieldset disabled={pending} className="space-y-5">
          {showTemplate ? (
            <Card>
              <CardHeader>
                <CardTitle>Peri 官方默认模板</CardTitle>
                <CardDescription>
                  应用内置预设；沿用 Peri 默认能力，Provider/模型由你选择。应用模板不会保存或启动对话。
                </CardDescription>
              </CardHeader>
              <CardContent className="flex flex-wrap gap-2">
                <Button
                  type="button"
                  onClick={() => (dirty || count > 0 ? setConfirm('template') : applyTemplate())}
                >
                  应用模板
                </Button>
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => (dirty ? setConfirm('blank') : blank())}
                >
                  从空白开始
                </Button>
                {undo ? (
                  <Button
                    type="button"
                    variant="outline"
                    onClick={() => {
                      apply(undo);
                      setUndo(null);
                    }}
                  >
                    撤销本次应用
                  </Button>
                ) : null}
              </CardContent>
            </Card>
          ) : null}
          <Card>
            <CardHeader>
              <CardTitle>基本配置</CardTitle>
            </CardHeader>
            <CardContent className="space-y-4">
              <FormField
                control={form.control}
                name="name"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>名称</FormLabel>
                    <FormControl>
                      <Input placeholder="例如 Reviewer" maxLength={80} {...field} />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <Controller
                control={form.control}
                name="config.provider"
                render={({ field }) => (
                  <ProviderFields
                    value={field.value ?? null}
                    onChange={field.onChange}
                    disabled={pending}
                    autoSelect={showTemplate}
                  />
                )}
              />
              {form.formState.errors.config?.provider ? (
                <p role="alert" className="text-sm text-destructive">
                  请检查 Provider 和模型 ID（1–200 字符）。
                </p>
              ) : null}
            </CardContent>
          </Card>
          <details
            open={advanced}
            onToggle={(event) => setAdvanced(event.currentTarget.open)}
            className="rounded-xl border bg-card p-4"
          >
            <summary className="cursor-pointer font-medium">
              高级设置 · {count === 0 ? '沿用默认' : `${count} 项非默认配置`}
            </summary>
            <div ref={focusAdvanced} className="space-y-5 pt-5" tabIndex={-1}>
              <FormField
                control={form.control}
                name="description"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>描述</FormLabel>
                    <FormControl>
                      <Input maxLength={400} {...field} />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name="config.instructions"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Instructions</FormLabel>
                    <FormControl>
                      <Textarea className="min-h-32 font-mono text-xs" maxLength={32_768} {...field} />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <div>
                <h3 className="mb-3 font-medium">MCP 服务器</h3>
                <Controller
                  control={form.control}
                  name="config.mcpServers"
                  render={({ field }) => (
                    <McpServersField value={field.value ?? {}} onChange={field.onChange} disabled={pending} />
                  )}
                />
              </div>
              <div>
                <h3 className="mb-3 font-medium">能力开关</h3>
                <Controller
                  control={form.control}
                  name="config.metaHarness"
                  render={({ field }) => (
                    <CapabilitySwitches
                      keys={capabilities.metaHarnessKeys}
                      value={field.value ?? {}}
                      onChange={field.onChange}
                      disabled={pending}
                    />
                  )}
                />
              </div>
              {form.formState.errors.config?.mcpServers || form.formState.errors.config?.metaHarness ? (
                <p role="alert">请检查高级配置中的 MCP 或能力字段。</p>
              ) : null}
            </div>
          </details>
          {failure ? (
            <p role="alert" className="text-sm text-destructive">
              {failure}
            </p>
          ) : null}
          {conflict && onReload ? (
            <Button type="button" variant="outline" onClick={() => setConfirm('reload')}>
              重新加载服务端版本
            </Button>
          ) : null}
          <div className="flex justify-end gap-2">
            <Button type="button" variant="outline" onClick={() => navigate('/agents')}>
              取消
            </Button>
            <Button type="submit" disabled={pending}>
              <Save />
              {pending ? '保存中…' : submitLabel}
            </Button>
          </div>
        </fieldset>
      </form>
      <ConfirmDialog
        open={confirm !== null}
        onOpenChange={(open) => {
          if (!open) setConfirm(null);
        }}
        title={confirm === 'reload' ? '重新加载 Agent' : '替换高级配置'}
        description={
          confirm === 'reload'
            ? '将丢弃当前未保存的草稿，加载服务端最新版本。'
            : '将重置系统指令、MCP 和能力覆盖；保留名称、描述及 Provider/模型。取消不会修改草稿。'
        }
        confirmLabel={confirm === 'reload' ? '重新加载' : '确认应用'}
        onConfirm={() => {
          if (confirm === 'template') applyTemplate();
          else if (confirm === 'blank') blank();
          else if (onReload) {
            setConfirm(null);
            setReloading(true);
            void onReload()
              .then((draft) => {
                form.reset(draft);
                setConflict(false);
                setFailure('');
                setUndo(null);
              })
              .catch((error) => setFailure(describeApiError(error)))
              .finally(() => setReloading(false));
          }
        }}
      />
    </Form>
  );
}
