import {
  useCallback, useEffect, useId, useLayoutEffect, useMemo, useRef, useState,
  type ChangeEvent,
  type KeyboardEvent,
  type ReactNode,
} from 'react';
import { ArrowUp, Square } from 'lucide-react';
import type { ChatSendOutcome } from '../runtime';
import { ComposerShell } from './ComposerShell';
import { ComposerSlashMenu } from './ComposerSlashMenu';
import { ComposerStatusBar } from './ComposerStatusBar';
import { composerLimitMessage, composerLimitState } from './composerLimits';
import type { ComposerMeta } from './composerMeta';
import type { ContextUsage } from './composerUsage';
import { historyRecallDirection, stepHistory } from './composerHistory';
import {
  applyComposerCommand, filterComposerCommands, findSlashTrigger,
  type ComposerCommand,
} from './composerSlash';

export interface ChatComposerProps {
  draft: string;
  onDraftChange: (value: string) => void;
  canSend: boolean;
  canStop: boolean;
  stopping: boolean;
  /** 待回答交互由外层渲染并注入，composer 不依赖交互视图模块。 */
  pendingContent?: ReactNode;
  /** 不可发送的真实原因（阻断/同步中）；缺席时回退到同步文案。 */
  disabledReason?: string | undefined;
  /** 全局错误横幅文案；已覆盖同一发送失败时，行内只保留重试入口，避免重复播报。 */
  errorBanner?: string | undefined;
  /** 返回发送结果：只有 `delivered` 为真才清空草稿，撤回/失败保留输入。 */
  onSend: (content: string) => Promise<ChatSendOutcome>;
  onStop: () => Promise<boolean>;
  onHeightChange?: (height: number) => void;
  focusSignal: number;
  /** 状态栏只读/可写元信息。 */
  meta?: ComposerMeta;
  /** 上下文用量；null 时隐藏 meter。 */
  usage?: ContextUsage | null;
  /** 本会话历史 prompt（旧→新）。 */
  historyPrompts?: readonly string[];
  /** ACP 提供的可用斜杠命令；空数组不显示菜单。 */
  commands?: readonly ComposerCommand[];
  /** 配置写入：存在且选项充足时才把只读标签升级为选择器。 */
  onConfigOption?: (configId: string, value: string) => Promise<void>;
}

/** Enter 发送、Shift+Enter 换行；IME 组合输入期间的 Enter 不发送。 */
export function isSubmitKey(event: {
  key: string; shiftKey: boolean; isComposing: boolean; keyCode: number;
}): boolean {
  return !event.isComposing && event.keyCode !== 229 && event.key === 'Enter' && !event.shiftKey;
}

/** 发送成功后仅当草稿仍是刚发送的内容时清空；失败或已被改写时保留。 */
export function nextDraftAfterSend(current: string, submitted: string, sent: boolean): string | null {
  return sent && current.trim() === submitted ? '' : null;
}

interface SendFailure {
  content: string;
  message: string;
  disposition?: 'rejected' | 'unknown' | 'not_sent';
}

const DEFAULT_SEND_ERROR = '消息未发送，请检查连接后重试。';

/**
 * 会话输入区：受控 textarea，Enter 发送 / Shift+Enter 换行，IME 组合输入不发送。
 * 运行中主按钮变为停止；状态栏承载配置、上下文用量、字数与斜杠/历史交互。
 */
export function ChatComposer({
  draft, onDraftChange, canSend, canStop, stopping, pendingContent, disabledReason,
  onSend, onStop, onHeightChange, focusSignal, meta, usage, historyPrompts = [],
  commands = [], onConfigOption, errorBanner,
}: ChatComposerProps) {
  const textarea = useRef<HTMLTextAreaElement>(null);
  const draftRef = useRef(draft);
  draftRef.current = draft;
  const submittingRef = useRef(false);
  const [submitting, setSubmitting] = useState(false);
  const [failure, setFailure] = useState<SendFailure | null>(null);
  const [recallIndex, setRecallIndex] = useState(-1);
  const [caret, setCaret] = useState(draft.length);
  const [caretRequest, setCaretRequest] = useState<{ at: number; token: number } | null>(null);
  const [dismissedTrigger, setDismissedTrigger] = useState<string | null>(null);
  const [highlight, setHighlight] = useState(0);
  const generatedId = useId();
  const limit = composerLimitState(draft);

  const trigger = findSlashTrigger(draft, caret);
  const triggerKey = trigger ? `${trigger.start}:${trigger.query}` : null;
  const slashOpen = trigger !== null && commands.length > 0 && dismissedTrigger !== triggerKey;
  const filteredCommands = useMemo(
    () => (trigger ? filterComposerCommands(commands, trigger.query) : []),
    [commands, trigger?.query],
  );
  const slashMenuId = `${generatedId}-slash`;
  const hintId = `${generatedId}-hint`;
  const limitId = `${generatedId}-limit`;
  const errorId = `${generatedId}-error`;
  const disabledId = `${generatedId}-disabled`;

  useEffect(() => {
    setHighlight(0);
  }, [triggerKey]);

  useEffect(() => {
    setHighlight((index) => (filteredCommands.length === 0
      ? 0
      : Math.min(index, filteredCommands.length - 1)));
  }, [filteredCommands.length]);

  useLayoutEffect(() => {
    const element = textarea.current;
    if (!element) return;
    element.style.height = 'auto';
    element.style.height = `${Math.min(element.scrollHeight, 200)}px`;
  }, [draft]);

  useLayoutEffect(() => {
    const element = textarea.current;
    if (!element) return;
    element.focus();
    element.setSelectionRange(element.value.length, element.value.length);
    setCaret(element.value.length);
  }, [focusSignal]);

  useLayoutEffect(() => {
    if (!caretRequest) return;
    const element = textarea.current;
    if (!element) return;
    element.focus();
    element.setSelectionRange(caretRequest.at, caretRequest.at);
    setCaret(caretRequest.at);
  }, [caretRequest]);

  const requestCaret = useCallback((at: number) => {
    setCaretRequest((previous) => ({ at, token: (previous?.token ?? 0) + 1 }));
  }, []);

  const submit = useCallback(async (contentOverride?: string) => {
    const content = (contentOverride ?? draftRef.current).trim();
    if (
      !content || !canSend || submittingRef.current
      || composerLimitState(draftRef.current).overLimit
    ) return;
    setFailure(null);
    submittingRef.current = true;
    setSubmitting(true);
    try {
      const outcome = await onSend(content);
      const next = nextDraftAfterSend(draftRef.current, content, outcome.delivered);
      if (next !== null) {
        onDraftChange(next);
        setRecallIndex(-1);
      } else if (!outcome.delivered) {
        setFailure({
          content,
          message: outcome.message || DEFAULT_SEND_ERROR,
          ...(outcome.disposition ? { disposition: outcome.disposition } : {}),
        });
      }
    } finally {
      submittingRef.current = false;
      setSubmitting(false);
    }
  }, [canSend, onDraftChange, onSend]);

  const stop = useCallback(async () => {
    if (stopping) return;
    setFailure(null);
    await onStop();
  }, [onStop, stopping]);

  const chooseCommand = useCallback((command: ComposerCommand) => {
    if (!trigger) return;
    const next = applyComposerCommand(draftRef.current, trigger, command.name);
    onDraftChange(next.text);
    setRecallIndex(-1);
    setDismissedTrigger(null);
    setHighlight(0);
    requestCaret(next.caret);
  }, [onDraftChange, requestCaret, trigger]);

  const handleChange = useCallback((event: ChangeEvent<HTMLTextAreaElement>) => {
    const value = event.target.value;
    setCaret(event.target.selectionStart ?? value.length);
    setRecallIndex(-1);
    if (failure && value.trim() !== failure.content) setFailure(null);
    onDraftChange(value);
  }, [failure, onDraftChange]);

  const handleKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    const native = event.nativeEvent;
    if (native.isComposing || event.keyCode === 229) return;

    if (slashOpen) {
      if (event.key === 'Escape') {
        event.preventDefault();
        event.stopPropagation();
        setDismissedTrigger(triggerKey);
        return;
      }
      if (filteredCommands.length > 0) {
        if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
          event.preventDefault();
          const delta = event.key === 'ArrowDown' ? 1 : -1;
          setHighlight((index) => (index + delta + filteredCommands.length) % filteredCommands.length);
          return;
        }
        if (event.key === 'Enter' || event.key === 'Tab') {
          event.preventDefault();
          const command = filteredCommands[highlight];
          if (command) chooseCommand(command);
          return;
        }
      } else if (event.key === 'Enter') {
        // 菜单打开且无匹配时不能把 Enter 当作发送，避免误发半截命令。
        event.preventDefault();
        return;
      }
    }

    const direction = historyRecallDirection({
      key: event.key,
      shiftKey: event.shiftKey,
      altKey: event.altKey,
      ctrlKey: event.ctrlKey,
      metaKey: event.metaKey,
      isComposing: native.isComposing,
      keyCode: event.keyCode,
    });
    if (direction) {
      const atStart = event.currentTarget.selectionStart === 0;
      const atEnd = event.currentTarget.selectionEnd === event.currentTarget.value.length;
      const activeRecall = recallIndex >= 0;
      const canStep = direction === 'previous'
        ? (activeRecall || (draft === '' && atStart))
        : activeRecall && atEnd;
      if (canStep) {
        const step = stepHistory(historyPrompts, recallIndex, direction);
        if (step) {
          event.preventDefault();
          setRecallIndex(step.index);
          onDraftChange(step.text);
          requestCaret(step.text.length);
          return;
        }
      }
    }

    if (!isSubmitKey({
      key: event.key, shiftKey: event.shiftKey,
      isComposing: native.isComposing, keyCode: event.keyCode,
    })) return;
    if (!canSend || limit.overLimit) return;
    event.preventDefault();
    void submit();
  };

  const updateCaret = (event: { currentTarget: HTMLTextAreaElement }) => {
    setCaret(event.currentTarget.selectionStart ?? event.currentTarget.value.length);
  };

  const busy = canStop;
  const actionDisabled = busy
    ? stopping
    : !canSend || submitting || draft.trim().length === 0 || limit.overLimit;
  const canRetry = failure?.disposition === 'rejected' && draft.trim() === failure.content;
  const bannerCoversFailure = failure !== null && errorBanner !== undefined
    && errorBanner.length > 0 && errorBanner.includes(failure.message);
  const showDisabledReason = !busy && !canSend && draft.length > 0 && disabledReason !== undefined;
  const describedBy = [
    hintId,
    limit.overLimit ? limitId : null,
    failure && !bannerCoversFailure ? errorId : null,
    showDisabledReason ? disabledId : null,
  ].filter((value): value is string => value !== null).join(' ');

  return (
    <ComposerShell onHeightChange={onHeightChange}>
      {pendingContent}
      <span id={hintId} className="sr-only">Enter 发送，Shift+Enter 换行</span>
      {failure && !bannerCoversFailure && (
        <div
          id={errorId}
          className="text-destructive flex items-start gap-2 px-4 pt-2 text-xs"
          role="alert"
          data-slot="peri-composer-error"
        >
          <span className="min-w-0 flex-1">{failure.message}</span>
          {canRetry && (
            <button
              type="button"
              className="hover:bg-destructive/10 shrink-0 rounded px-1.5 py-0.5 underline underline-offset-2"
              aria-label="重试发送"
              disabled={submitting}
              onClick={() => void submit(failure.content)}
            >
              重试
            </button>
          )}
        </div>
      )}
      {failure && bannerCoversFailure && canRetry && (
        <div className="flex justify-end px-4 pt-2" data-slot="peri-composer-retry">
          <button
            type="button"
            className="text-destructive hover:bg-destructive/10 shrink-0 rounded px-1.5 py-0.5 text-xs underline underline-offset-2"
            aria-label="重试发送"
            disabled={submitting}
            onClick={() => void submit(failure.content)}
          >
            重试
          </button>
        </div>
      )}
      {limit.overLimit && (
        <p
          id={limitId}
          className="text-destructive px-4 pt-2 text-xs"
          role="alert"
          data-slot="peri-composer-limit-error"
        >
          {composerLimitMessage(limit)}
        </p>
      )}
      {showDisabledReason && (
        <p
          id={disabledId}
          className="text-muted-foreground px-4 pt-2 text-xs"
          data-slot="peri-composer-disabled"
        >
          {disabledReason}
        </p>
      )}
      <div className="relative flex items-end gap-2 px-3 pt-2">
        {slashOpen && (
          <ComposerSlashMenu
            id={slashMenuId}
            commands={filteredCommands}
            activeIndex={highlight}
            onSelect={chooseCommand}
          />
        )}
        <textarea
          ref={textarea}
          value={draft}
          rows={1}
          placeholder={busy ? '任务运行中，可先起草消息…' : canSend
            ? '输入消息，Enter 发送，Shift+Enter 换行'
            : disabledReason ?? '等待会话同步…'}
          className="placeholder:text-muted-foreground/70 max-h-[200px] min-h-9 w-full resize-none bg-transparent px-1.5 py-1.5 text-sm leading-6 outline-none"
          aria-label="输入消息"
          aria-describedby={describedBy}
          aria-invalid={limit.overLimit || undefined}
          aria-autocomplete={commands.length > 0 ? 'list' : undefined}
          aria-controls={slashOpen ? slashMenuId : undefined}
          aria-expanded={slashOpen || undefined}
          aria-activedescendant={
            slashOpen && filteredCommands.length > 0
              ? `${slashMenuId}-option-${highlight}`
              : undefined
          }
          onChange={handleChange}
          onSelect={updateCaret}
          onClick={updateCaret}
          onKeyUp={updateCaret}
          onKeyDown={handleKeyDown}
        />
      </div>
      <span className="sr-only" role="status" aria-live="polite" aria-atomic="true">
        {slashOpen ? '命令菜单已打开' : ''}
      </span>
      <ComposerStatusBar
        meta={meta}
        usage={usage}
        limit={limit}
        hint={busy ? (stopping ? '正在停止…' : '运行中') : 'Enter 发送 · Shift+Enter 换行'}
        onSelectOption={onConfigOption}
        actions={(
          <button
            type="button"
            className="bg-primary text-primary-foreground hover:bg-primary/85 inline-flex size-8 shrink-0 items-center justify-center rounded-full transition-colors disabled:opacity-40"
            aria-label={busy ? '停止' : '发送'}
            disabled={actionDisabled}
            onClick={() => { if (busy) void stop(); else void submit(); }}
          >
            {busy ? <Square className="size-3.5 fill-current" /> : <ArrowUp className="size-4" />}
          </button>
        )}
      />
    </ComposerShell>
  );
}
