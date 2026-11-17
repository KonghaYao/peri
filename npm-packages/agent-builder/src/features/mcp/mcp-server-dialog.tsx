import type { McpServerDraft } from '@shared/agent';
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { McpServerForm } from './mcp-server-form';

export interface McpServerDialogProps {
  readonly open: boolean;
  readonly onOpenChange: (open: boolean) => void;
  readonly initial?: { readonly name: string; readonly draft: McpServerDraft };
  readonly existingNames: readonly string[];
  readonly onSave: (name: string, draft: McpServerDraft) => void;
}

export function McpServerDialog({
  open,
  onOpenChange,
  initial,
  existingNames,
  onSave,
}: McpServerDialogProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[90vh] max-w-2xl overflow-y-auto">
        <DialogHeader>
          <DialogTitle>{initial ? '编辑 MCP 服务器' : '新增 MCP 服务器'}</DialogTitle>
          <DialogDescription>
            只支持 http 与 stdio；header/env 值会以明文保存在本机数据库（文件权限 0600）。
          </DialogDescription>
        </DialogHeader>
        <McpServerForm
          key={`${initial?.name ?? 'new'}-${open ? 'open' : 'closed'}`}
          {...(initial === undefined ? {} : { initial })}
          existingNames={existingNames}
          onCancel={() => onOpenChange(false)}
          onSave={(name, draft) => {
            onSave(name, draft);
            onOpenChange(false);
          }}
        />
      </DialogContent>
    </Dialog>
  );
}
