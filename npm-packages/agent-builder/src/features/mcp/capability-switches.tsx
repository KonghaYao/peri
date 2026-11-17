import { Search } from 'lucide-react';
import { useState } from 'react';
import { Input } from '@/components/ui/input';
import { cn } from '@/lib/utils';

export type CapabilityState = 'default' | 'on' | 'off';

const OPTIONS: ReadonlyArray<{ value: CapabilityState; label: string }> = [
  { value: 'default', label: '默认' },
  { value: 'on', label: '开' },
  { value: 'off', label: '关' },
];

export interface CapabilitySwitchesProps {
  readonly keys: readonly string[];
  readonly value: Record<string, boolean>;
  readonly onChange: (next: Record<string, boolean>) => void;
  readonly disabled?: boolean;
}

function stateOf(value: Record<string, boolean>, key: string): CapabilityState {
  if (!(key in value)) return 'default';
  return value[key] ? 'on' : 'off';
}

function applyState(
  value: Record<string, boolean>,
  key: string,
  state: CapabilityState,
): Record<string, boolean> {
  const next = { ...value };
  if (state === 'default') {
    delete next[key];
  } else {
    next[key] = state === 'on';
  }
  return next;
}

/** Tri-state keeps "not set" distinct from an explicit false so runtime defaults still apply. */
export function CapabilitySwitches({ keys, value, onChange, disabled = false }: CapabilitySwitchesProps) {
  const [search, setSearch] = useState('');
  const needle = search.trim().toLowerCase();
  const visibleKeys = needle.length === 0 ? keys : keys.filter((key) => key.toLowerCase().includes(needle));
  const overridden = keys.filter((key) => key in value).length;

  return (
    <div className="space-y-3">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <p className="text-xs text-muted-foreground">
          已显式设置 {overridden} / {keys.length} 项；「默认」表示不覆盖运行时值。
        </p>
        <div className="relative sm:w-56">
          <Search className="pointer-events-none absolute left-2 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" />
          <Input
            aria-label="搜索能力键"
            placeholder="搜索能力键"
            value={search}
            className="h-8 pl-7 text-xs"
            onChange={(event) => setSearch(event.target.value)}
          />
        </div>
      </div>

      <div className="max-h-80 divide-y overflow-y-auto rounded-lg border">
        {visibleKeys.length === 0 ? (
          <p className="px-4 py-6 text-center text-sm text-muted-foreground">没有匹配的能力键。</p>
        ) : (
          visibleKeys.map((key) => {
            const state = stateOf(value, key);
            return (
              <div key={key} className="flex items-center gap-2 px-3 py-2">
                <span className="min-w-0 flex-1 truncate font-mono text-xs" title={key}>
                  {key}
                </span>
                <div className="flex shrink-0 gap-1">
                  {OPTIONS.map((option) => (
                    <button
                      key={option.value}
                      type="button"
                      aria-pressed={state === option.value}
                      disabled={disabled}
                      className={cn(
                        'rounded-md border px-2 py-1 text-xs transition-colors disabled:opacity-50',
                        state === option.value
                          ? 'border-primary bg-primary text-primary-foreground'
                          : 'border-input bg-background text-muted-foreground hover:bg-accent hover:text-accent-foreground',
                      )}
                      onClick={() => onChange(applyState(value, key, option.value))}
                    >
                      {option.label}
                    </button>
                  ))}
                </div>
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}
