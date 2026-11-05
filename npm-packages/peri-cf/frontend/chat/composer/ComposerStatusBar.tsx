import { useEffect, useId, useRef, useState, type KeyboardEvent, type ReactNode } from 'react';
import type { ComposerLimitState } from './composerLimits';
import { composerLimitAnnouncement, composerLimitMessage } from './composerLimits';
import type { ComposerMeta, ComposerOption, ComposerSelector } from './composerMeta';
import type { ContextUsage } from './composerUsage';
import { ContextUsageMeter } from './ContextUsageMeter';

interface ComposerStatusBarProps {
  meta?: ComposerMeta;
  usage?: ContextUsage | null;
  limit?: ComposerLimitState;
  /** 桌面端快捷键提示；窄屏由 textarea 的 aria-describedby 承担。 */
  hint?: string;
  actions?: ReactNode;
  /** 有该回调且配置项提供了 select options 时，才渲染为可交互选择器。 */
  onSelectOption?: (configId: string, value: string) => Promise<void>;
}

interface PendingSelection {
  configId: string;
  value: string;
  label: string;
}

function MetaLabel({ label, value, slot }: { label: string; value: string; slot: string }) {
  return (
    <span className="inline-flex min-w-0 items-center gap-1" data-slot={slot}>
      <span className="text-muted-foreground/60 hidden shrink-0 sm:inline">{label}</span>
      <span className="text-foreground/80 min-w-0 truncate">{value}</span>
    </span>
  );
}

function optionMessage(error: unknown): string {
  return error instanceof Error && error.message ? error.message : '配置切换失败，请稍后重试。';
}

/**
 * Composer 状态栏：左���只读/可写配置、上下文用量与字数；右侧发送/停止。
 *
 * 可写项在 pending 时乐观显示目标值，但失败必须回滚；实际值仍以投影为准。
 */
export function ComposerStatusBar({
  meta, usage, limit, hint, actions, onSelectOption,
}: ComposerStatusBarProps) {
  const root = useRef<HTMLDivElement>(null);
  const [openId, setOpenId] = useState<string | null>(null);
  const [highlight, setHighlight] = useState(0);
  const [pending, setPending] = useState<PendingSelection | null>(null);
  const [error, setError] = useState('');
  const generatedId = useId();
  const limitMessage = limit ? composerLimitMessage(limit) : null;
  const limitAnnouncement = limit ? composerLimitAnnouncement(limit) : '';

  const selectors = [meta?.model, meta?.mode, meta?.thinking].filter(
    (selector): selector is ComposerSelector => selector !== undefined,
  );

  useEffect(() => {
    if (!openId) return;
    const onPointerDown = (event: PointerEvent) => {
      if (root.current?.contains(event.target as Node)) return;
      setOpenId(null);
    };
    document.addEventListener('pointerdown', onPointerDown);
    return () => document.removeEventListener('pointerdown', onPointerDown);
  }, [openId]);

  // 投影追上目标值后结束 pending；配置项消失也结束，避免永久 busy。
  useEffect(() => {
    if (!pending) return;
    const selector = selectors.find((item) => item.id === pending.configId);
    if (!selector || selector.currentValue === pending.value) setPending(null);
  });

  const displayOf = (selector: ComposerSelector): ComposerSelector => {
    if (!pending || pending.configId !== selector.id) return selector;
    return { ...selector, currentValue: pending.value, currentLabel: pending.label };
  };

  const choose = async (selector: ComposerSelector, option: ComposerOption) => {
    if (!onSelectOption || pending) return;
    setOpenId(null);
    setError('');
    setPending({ configId: selector.id, value: option.value, label: option.name });
    try {
      await onSelectOption(selector.id, option.value);
    } catch (cause) {
      setPending(null);
      setError(optionMessage(cause));
    }
  };

  const openMenu = (selector: ComposerSelector) => {
    if (!onSelectOption || pending || selector.options.length === 0) return;
    const current = selector.options.findIndex((option) => option.value === selector.currentValue);
    setOpenId(selector.id);
    setHighlight(current >= 0 ? current : 0);
  };

  const onTriggerKeyDown = (
    event: KeyboardEvent<HTMLButtonElement>,
    selector: ComposerSelector,
  ) => {
    const displayed = displayOf(selector);
    const open = openId === selector.id;
    if (event.key === 'Escape') {
      if (!open) return;
      event.preventDefault();
      event.stopPropagation();
      setOpenId(null);
      return;
    }
    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault();
      if (!open) {
        openMenu(displayed);
        return;
      }
      const delta = event.key === 'ArrowDown' ? 1 : -1;
      setHighlight((index) => (
        (index + delta + displayed.options.length) % displayed.options.length
      ));
      return;
    }
    if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      if (!open) {
        openMenu(displayed);
        return;
      }
      const option = displayed.options[highlight];
      if (option) void choose(displayed, option);
      return;
    }
    if (event.key === 'Tab' && open) setOpenId(null);
  };

  const renderSelector = (selector: ComposerSelector, slot: string) => {
    const displayed = displayOf(selector);
    const interactive = onSelectOption !== undefined && selector.options.length > 1;
    if (!interactive) return <MetaLabel key={selector.id} label={selector.label} value={displayed.currentLabel} slot={slot} />;

    const open = openId === selector.id;
    const menuId = `${generatedId}-${selector.id}`;
    const busy = pending?.configId === selector.id;
    return (
      <span key={selector.id} className="relative inline-flex min-w-0" data-slot={slot}>
        <button
          type="button"
          className="hover:bg-muted focus-visible:ring-ring inline-flex min-w-0 items-center gap-1 rounded px-1 py-0.5 transition-colors focus-visible:ring-2 focus-visible:outline-none disabled:opacity-60"
          aria-label={`${selector.label}：${displayed.currentLabel}，点击切换`}
          aria-haspopup="listbox"
          aria-expanded={open}
          aria-controls={open ? menuId : undefined}
          aria-busy={busy}
          aria-disabled={pending !== null}
          onClick={() => {
            if (open) setOpenId(null);
            else openMenu(displayed);
          }}
          onKeyDown={(event) => onTriggerKeyDown(event, selector)}
        >
          <span className="text-muted-foreground/60 hidden shrink-0 sm:inline">{selector.label}</span>
          <span className="text-foreground/80 min-w-0 truncate">{displayed.currentLabel}</span>
          {busy && <span className="sr-only">切换中</span>}
        </button>
        {open && (
          <div
            id={menuId}
            role="listbox"
            aria-label={`${selector.label}选项`}
            className="bg-popover text-popover-foreground absolute bottom-full left-0 z-40 mb-2 max-h-64 w-56 overflow-y-auto rounded-lg border p-1 text-xs shadow-lg"
          >
            {displayed.options.map((option, index) => (
              <div
                key={option.value}
                role="option"
                aria-selected={option.value === displayed.currentValue}
                className={`cursor-pointer rounded-md px-2 py-1.5 ${
                  index === highlight ? 'bg-accent text-accent-foreground' : 'hover:bg-muted'
                }`}
                onMouseDown={(event) => event.preventDefault()}
                onClick={() => void choose(selector, option)}
              >
                <div className="truncate">{option.name}</div>
                {option.description && (
                  <div className="text-muted-foreground truncate">{option.description}</div>
                )}
              </div>
            ))}
          </div>
        )}
      </span>
    );
  };

  return (    <div ref={root} className="px-3 pb-2" data-slot="peri-composer-status">
      <div className="text-muted-foreground flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 text-[11px]">
        <div className="flex min-w-0 flex-1 flex-wrap items-center gap-x-2 gap-y-1">
          {meta?.model && renderSelector(meta.model, 'peri-composer-model')}
          {!meta?.model && meta?.modelLabel && (
            <MetaLabel label="模型" value={meta.modelLabel} slot="peri-composer-model" />
          )}
          {meta?.mode && renderSelector(meta.mode, 'peri-composer-mode')}
          {!meta?.mode && meta?.modeLabel && (
            <MetaLabel label="模式" value={meta.modeLabel} slot="peri-composer-mode" />
          )}
          {meta?.thinking && renderSelector(meta.thinking, 'peri-composer-thinking')}
          {usage && <ContextUsageMeter usage={usage} />}
          {limit?.showCount && limitMessage && (
            <span
              className={limit.overLimit ? 'text-destructive' : 'text-muted-foreground'}
              data-slot="peri-composer-count"
              aria-hidden="true"
            >
              {limit.length} / {limit.max}
            </span>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-2">
          {hint && <span className="text-muted-foreground/60 hidden sm:inline">{hint}</span>}
          {actions}
        </div>
      </div>
      <span className="sr-only" role="status" aria-live="polite" aria-atomic="true">
        {limitAnnouncement}
      </span>
      {error && (
        <p className="text-destructive mt-1 text-[11px]" role="alert" data-slot="peri-composer-config-error">
          {error}
        </p>
      )}
    </div>
  );
}
