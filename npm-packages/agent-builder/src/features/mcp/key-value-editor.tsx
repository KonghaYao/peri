import { Plus, Trash2 } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';

export interface KeyValueRow {
  readonly id: string;
  readonly key: string;
  readonly value: string;
}

export function createKeyValueRow(key = '', value = ''): KeyValueRow {
  return { id: crypto.randomUUID(), key, value };
}

export interface KeyValueEditorProps {
  readonly rows: readonly KeyValueRow[];
  readonly onChange: (rows: KeyValueRow[]) => void;
  readonly keyPlaceholder?: string;
  readonly valuePlaceholder?: string;
  readonly addLabel?: string;
}

export function KeyValueEditor({
  rows,
  onChange,
  keyPlaceholder = '名称',
  valuePlaceholder = '值',
  addLabel = '添加一行',
}: KeyValueEditorProps) {
  const update = (index: number, patch: Partial<KeyValueRow>) => {
    onChange(rows.map((row, current) => (current === index ? { ...row, ...patch } : row)));
  };

  return (
    <div className="space-y-2">
      {rows.map((row, index) => (
        <div key={row.id} className="flex items-center gap-2">
          <Input
            aria-label={`${keyPlaceholder} ${index + 1}`}
            placeholder={keyPlaceholder}
            value={row.key}
            onChange={(event) => update(index, { key: event.target.value })}
            className="min-w-0 flex-1 font-mono text-xs"
          />
          <Input
            aria-label={`${valuePlaceholder} ${index + 1}`}
            placeholder={valuePlaceholder}
            value={row.value}
            onChange={(event) => update(index, { value: event.target.value })}
            className="min-w-0 flex-1 font-mono text-xs"
          />
          <Button
            type="button"
            variant="ghost"
            size="icon"
            aria-label={`删除第 ${index + 1} 行`}
            onClick={() => onChange(rows.filter((_, current) => current !== index))}
          >
            <Trash2 />
          </Button>
        </div>
      ))}
      <Button
        type="button"
        variant="outline"
        size="sm"
        onClick={() => onChange([...rows, createKeyValueRow()])}
      >
        <Plus />
        {addLabel}
      </Button>
    </div>
  );
}
