import { useCallback, useLayoutEffect, useRef, useState, type ReactNode } from 'react';
import { ArrowUp, Square } from 'lucide-react';
import type { ChatSendOutcome } from '../runtime';
import { ComposerShell } from './ComposerShell';

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
  /** 返回发送结果：只有 `delivered` 为真才清空草稿，撤回/失败保留输入。 */
  onSend: (content: string) => Promise<ChatSendOutcome>;
  onStop: () => Promise<boolean>;
  onHeightChange?: (height: number) => void;
  focusSignal: number;
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

/**
 * 会话输入区：受控 textarea，Enter 发送 / Shift+Enter 换行，IME 组合输入不发送。
 * 运行中主按钮变为停止；本轮不提供模型、slash、附件与队列入口。
 */
export function ChatComposer({
  draft, onDraftChange, canSend, canStop, stopping, pendingContent, disabledReason,
  onSend, onStop, onHeightChange, focusSignal,
}: ChatComposerProps) {
  const textarea = useRef<HTMLTextAreaElement>(null);
  const draftRef = useRef(draft);
  draftRef.current = draft;
  const [submitting, setSubmitting] = useState(false);
  const [localError, setLocalError] = useState('');

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
  }, [focusSignal]);

  const submit = useCallback(async () => {
    const content = draftRef.current.trim();
    if (!content || !canSend || submitting) return;
    setLocalError('');
    setSubmitting(true);
    try {
      const outcome = await onSend(content);
      const next = nextDraftAfterSend(draftRef.current, content, outcome.delivered);
      if (next !== null) onDraftChange(next);
      else if (!outcome.delivered) setLocalError(outcome.message || '消息未发送，请检查连接后重试。');
    } finally {
      setSubmitting(false);
    }
  }, [canSend, onDraftChange, onSend, submitting]);

  const stop = useCallback(async () => {
    if (stopping) return;
    setLocalError('');
    await onStop();
  }, [onStop, stopping]);

  const busy = canStop;
  const actionDisabled = busy ? stopping : !canSend || submitting || draft.trim().length === 0;

  return (
    <ComposerShell onHeightChange={onHeightChange}>
      {pendingContent}
      {localError && (
        <p className="text-destructive px-4 pt-2 text-xs" role="alert" data-slot="peri-composer-error">
          {localError}
        </p>
      )}
      <div className="flex items-end gap-2 px-3 py-2.5">
        <textarea
          ref={textarea}
          value={draft}
          rows={1}
          placeholder={busy ? '任务运行中，可先起草消息…' : canSend
            ? '输入消息，Enter 发送，Shift+Enter 换行'
            : disabledReason ?? '等待会话同步…'}
          className="placeholder:text-muted-foreground/70 max-h-[200px] min-h-9 w-full resize-none bg-transparent px-1.5 py-1.5 text-sm leading-6 outline-none"
          aria-label="输入消息"
          onChange={(event) => onDraftChange(event.target.value)}
          onKeyDown={(event) => {
            const native = event.nativeEvent;
            if (!isSubmitKey({
              key: event.key, shiftKey: event.shiftKey,
              isComposing: native.isComposing, keyCode: event.keyCode,
            })) return;
            if (!canSend) return;
            event.preventDefault();
            void submit();
          }}
        />
        <button
          type="button"
          className="bg-primary text-primary-foreground hover:bg-primary/85 inline-flex size-8 shrink-0 items-center justify-center rounded-full transition-colors disabled:opacity-40"
          aria-label={busy ? '停止' : '发送'}
          disabled={actionDisabled}
          onClick={() => { if (busy) void stop(); else void submit(); }}
        >
          {busy ? <Square className="size-3.5 fill-current" /> : <ArrowUp className="size-4" />}
        </button>
      </div>
      <div className="text-muted-foreground/60 flex items-center justify-between px-4 pb-2 text-[10px]">
        <span>Enter 发送 · Shift+Enter 换行</span>
        {busy && <span>{stopping ? '正在停止…' : '运行中'}</span>}
      </div>
    </ComposerShell>
  );
}
