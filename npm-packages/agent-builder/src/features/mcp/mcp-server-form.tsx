import type { McpServerDraft } from '@shared/agent';
import { useState } from 'react';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Textarea } from '@/components/ui/textarea';
import { createKeyValueRow, KeyValueEditor, type KeyValueRow } from './key-value-editor';

const NAME_PATTERN = /^[A-Za-z0-9_-]{1,64}$/;

export interface McpServerFormProps {
  readonly initial?: { readonly name: string; readonly draft: McpServerDraft };
  /** Names used by other servers; the edited server's own name must not be passed. */
  readonly existingNames: readonly string[];
  readonly onSave: (name: string, draft: McpServerDraft) => void;
  readonly onCancel: () => void;
}

function rowsFromRecord(record: Record<string, string> | undefined): KeyValueRow[] {
  return Object.entries(record ?? {}).map(([key, value]) => createKeyValueRow(key, value));
}

function recordFromRows(rows: readonly KeyValueRow[]): Record<string, string> {
  return Object.fromEntries(rows.map((row) => [row.key.trim(), row.value]));
}

function rowError(rows: readonly KeyValueRow[]): string | undefined {
  const seen = new Set<string>();
  for (const row of rows) {
    if (row.key.trim().length === 0) return '键不能为空';
    if (seen.has(row.key.trim())) return '存在重复的键';
    seen.add(row.key.trim());
  }
  return undefined;
}

function urlError(value: string): string | undefined {
  if (value.trim().length === 0) return '请输入 URL';
  try {
    const parsed = new URL(value.trim());
    return parsed.protocol === 'http:' || parsed.protocol === 'https:' ? undefined : '仅支持 http(s) URL';
  } catch {
    return '请输入合法的 http(s) URL';
  }
}

function nameError(value: string, existingNames: readonly string[]): string | undefined {
  const trimmed = value.trim();
  if (trimmed.length === 0) return '请输入服务器名称';
  if (!NAME_PATTERN.test(trimmed)) return '仅允许字母、数字、下划线和连字符（1–64 个字符）';
  if (existingNames.includes(trimmed)) return '名称已被使用';
  return undefined;
}

/** Inline validation so duplicate names, bad URLs and empty commands never reach the API. */
export function McpServerForm({ initial, existingNames, onSave, onCancel }: McpServerFormProps) {
  const initialDraft = initial?.draft;
  const [name, setName] = useState(initial?.name ?? '');
  const [type, setType] = useState<'http' | 'stdio'>(initialDraft?.type ?? 'http');
  const [url, setUrl] = useState(initialDraft?.type === 'http' ? initialDraft.url : '');
  const [command, setCommand] = useState(initialDraft?.type === 'stdio' ? initialDraft.command : '');
  const [argsText, setArgsText] = useState(
    initialDraft?.type === 'stdio' ? (initialDraft.args ?? []).join('\n') : '',
  );
  const [headers, setHeaders] = useState<KeyValueRow[]>(
    initialDraft?.type === 'http' ? rowsFromRecord(initialDraft.headers) : [],
  );
  const [env, setEnv] = useState<KeyValueRow[]>(
    initialDraft?.type === 'stdio' ? rowsFromRecord(initialDraft.env) : [],
  );
  const [submitted, setSubmitted] = useState(false);

  const currentNameError = nameError(name, existingNames);
  const currentUrlError = type === 'http' ? urlError(url) : undefined;
  const currentCommandError = type === 'stdio' && command.trim().length === 0 ? '请输入启动命令' : undefined;
  const currentRowError = rowError(type === 'http' ? headers : env);
  const canSubmit =
    currentNameError === undefined &&
    currentUrlError === undefined &&
    currentCommandError === undefined &&
    currentRowError === undefined;

  const showNameError = currentNameError !== undefined && (name.length > 0 || submitted);
  const showUrlError = currentUrlError !== undefined && (url.length > 0 || submitted);
  const showCommandError = currentCommandError !== undefined && (command.length > 0 || submitted);

  const submit = () => {
    setSubmitted(true);
    if (!canSubmit) return;

    const trimmedName = name.trim();
    if (type === 'http') {
      const headerRecord = recordFromRows(headers);
      onSave(trimmedName, {
        type: 'http',
        url: url.trim(),
        ...(Object.keys(headerRecord).length === 0 ? {} : { headers: headerRecord }),
      });
      return;
    }

    const args = argsText
      .split('\n')
      .map((line) => line.trim())
      .filter((line) => line.length > 0);
    const envRecord = recordFromRows(env);
    onSave(trimmedName, {
      type: 'stdio',
      command: command.trim(),
      ...(args.length === 0 ? {} : { args }),
      ...(Object.keys(envRecord).length === 0 ? {} : { env: envRecord }),
    });
  };

  return (
    <div className="space-y-4">
      <div className="space-y-2">
        <Label htmlFor="mcp-name">服务器名称</Label>
        <Input
          id="mcp-name"
          value={name}
          placeholder="例如 docs"
          className="font-mono"
          aria-invalid={showNameError}
          onChange={(event) => setName(event.target.value)}
        />
        {showNameError ? <p className="text-xs text-destructive">{currentNameError}</p> : null}
      </div>

      <div className="space-y-2">
        <Label>传输方式</Label>
        <div className="flex gap-2">
          <Button
            type="button"
            size="sm"
            variant={type === 'http' ? 'default' : 'outline'}
            onClick={() => setType('http')}
          >
            http
          </Button>
          <Button
            type="button"
            size="sm"
            variant={type === 'stdio' ? 'default' : 'outline'}
            onClick={() => setType('stdio')}
          >
            stdio
          </Button>
        </div>
      </div>

      {type === 'http' ? (
        <>
          <div className="space-y-2">
            <Label htmlFor="mcp-url">URL</Label>
            <Input
              id="mcp-url"
              value={url}
              placeholder="https://example.com/mcp"
              className="font-mono text-xs"
              aria-invalid={showUrlError}
              onChange={(event) => setUrl(event.target.value)}
            />
            {showUrlError ? <p className="text-xs text-destructive">{currentUrlError}</p> : null}
          </div>
          <div className="space-y-2">
            <Label>请求头（headers）</Label>
            <KeyValueEditor
              rows={headers}
              onChange={setHeaders}
              keyPlaceholder="Header"
              valuePlaceholder="值（保存到本地库）"
              addLabel="添加请求头"
            />
          </div>
        </>
      ) : (
        <>
          <div className="space-y-2">
            <Label htmlFor="mcp-command">启动命令</Label>
            <Input
              id="mcp-command"
              value={command}
              placeholder="npx"
              className="font-mono text-xs"
              aria-invalid={showCommandError}
              onChange={(event) => setCommand(event.target.value)}
            />
            {showCommandError ? <p className="text-xs text-destructive">{currentCommandError}</p> : null}
          </div>
          <div className="space-y-2">
            <Label htmlFor="mcp-args">参数（每行一个）</Label>
            <Textarea
              id="mcp-args"
              value={argsText}
              placeholder={'-y\n@modelcontextprotocol/server-everything'}
              className="min-h-20 font-mono text-xs"
              onChange={(event) => setArgsText(event.target.value)}
            />
          </div>
          <div className="space-y-2">
            <Label>环境变量（env）</Label>
            <KeyValueEditor
              rows={env}
              onChange={setEnv}
              keyPlaceholder="ENV_NAME"
              valuePlaceholder="值（保存到本地库）"
              addLabel="添加环境变量"
            />
          </div>
        </>
      )}

      {currentRowError ? <p className="text-xs text-destructive">{currentRowError}</p> : null}

      <div className="flex justify-end gap-2 pt-2">
        <Button type="button" variant="outline" onClick={onCancel}>
          取消
        </Button>
        <Button type="button" onClick={submit}>
          {initial ? '保存修改' : '添加服务器'}
        </Button>
      </div>
    </div>
  );
}
