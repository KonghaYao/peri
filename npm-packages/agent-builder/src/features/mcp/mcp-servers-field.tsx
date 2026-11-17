import type { McpServerDraft } from '@shared/agent';
import { Pencil, Plus, Trash2 } from 'lucide-react';
import { useState } from 'react';
import { ConfirmDialog } from '@/components/confirm-dialog';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { McpServerDialog } from './mcp-server-dialog';

export interface McpServersFieldProps {
  readonly value: Record<string, McpServerDraft>;
  readonly onChange: (next: Record<string, McpServerDraft>) => void;
  readonly disabled?: boolean;
}

interface EditingServer {
  readonly name: string;
  readonly draft: McpServerDraft;
}

function summarize(draft: McpServerDraft): string {
  if (draft.type === 'http') return draft.url;
  return [draft.command, ...(draft.args ?? [])].join(' ');
}

export function McpServersField({ value, onChange, disabled = false }: McpServersFieldProps) {
  const [dialogOpen, setDialogOpen] = useState(false);
  const [editing, setEditing] = useState<EditingServer | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<string | null>(null);

  const entries = Object.entries(value).sort(([left], [right]) => left.localeCompare(right));
  const existingNames = Object.keys(value).filter((name) => name !== editing?.name);

  const openCreate = () => {
    setEditing(null);
    setDialogOpen(true);
  };

  const openEdit = (name: string, draft: McpServerDraft) => {
    setEditing({ name, draft });
    setDialogOpen(true);
  };

  const save = (name: string, draft: McpServerDraft) => {
    const next = { ...value };
    if (editing && editing.name !== name) delete next[editing.name];
    next[name] = draft;
    onChange(next);
    setEditing(null);
  };

  return (
    <div className="space-y-3">
      {entries.length === 0 ? (
        <p className="rounded-lg border border-dashed px-4 py-6 text-center text-sm text-muted-foreground">
          尚未添加 MCP 服务器。
        </p>
      ) : (
        <ul className="divide-y rounded-lg border">
          {entries.map(([name, draft]) => (
            <li key={name} className="flex min-w-0 items-center gap-3 px-3 py-2">
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-2">
                  <span className="truncate font-mono text-sm">{name}</span>
                  <Badge variant="secondary">{draft.type}</Badge>
                </div>
                <p className="mt-0.5 truncate font-mono text-xs text-muted-foreground">{summarize(draft)}</p>
              </div>
              <Button
                type="button"
                variant="ghost"
                size="icon"
                aria-label={`编辑 ${name}`}
                disabled={disabled}
                onClick={() => openEdit(name, draft)}
              >
                <Pencil />
              </Button>
              <Button
                type="button"
                variant="ghost"
                size="icon"
                aria-label={`删除 ${name}`}
                className="text-destructive hover:text-destructive"
                disabled={disabled}
                onClick={() => setDeleteTarget(name)}
              >
                <Trash2 />
              </Button>
            </li>
          ))}
        </ul>
      )}

      <Button type="button" variant="outline" size="sm" onClick={openCreate} disabled={disabled}>
        <Plus />
        添加 MCP 服务器
      </Button>

      <McpServerDialog
        open={dialogOpen}
        onOpenChange={(open) => {
          setDialogOpen(open);
          if (!open) setEditing(null);
        }}
        {...(editing === null ? {} : { initial: editing })}
        existingNames={existingNames}
        onSave={save}
      />

      <ConfirmDialog
        open={deleteTarget !== null}
        onOpenChange={(open) => {
          if (!open) setDeleteTarget(null);
        }}
        title="删除 MCP 服务器"
        description={`删除「${deleteTarget ?? ''}」后，保存 Agent 时才会写入数据库。`}
        onConfirm={() => {
          if (!deleteTarget) return;
          const next = { ...value };
          delete next[deleteTarget];
          onChange(next);
          setDeleteTarget(null);
        }}
      />
    </div>
  );
}
