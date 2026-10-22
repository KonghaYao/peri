import { useEffect, useId, useRef, useState } from 'react';
import { CheckIcon, MessageCircleQuestion, XIcon } from 'lucide-react';
import { cn } from '@/lib';
import { field, inkButton, mono, paper } from '../../components/assistant-ui';
import {
  buildAskUserQuestionContent,
  type AskUserQuestionDecision,
  type AskUserQuestionDraft,
  type AskUserQuestionDrafts,
} from '@core/agent';
import type { InteractionAnswerHandler } from '../runtime';
import type { AskUserQuestion, Interaction } from '../runtime';

type Settled = 'accepted' | 'declined';

function initialDrafts(questions: readonly AskUserQuestion[]): AskUserQuestionDrafts {
  return Object.fromEntries(questions.map((question) => [
    question.id, { selected: [], custom: '' } satisfies AskUserQuestionDraft,
  ]));
}

function failureText(failure: unknown): string {
  return failure instanceof Error ? failure.message : '回答未能提交，请稍后重试。';
}

/**
 * AskUserQuestion 卡片：每个问题一个分组，单选 radio、多选 checkbox、自由文本输入。
 * 提交内容由 buildAskUserQuestionContent 按 wire 契约构造；结算状态以投影移除待答记录为准。
 */
export function AskUserQuestionCard({
  interaction, onAnswer,
}: { interaction: Interaction; onAnswer?: InteractionAnswerHandler }) {
  const questions = interaction.questions ?? [];
  const [drafts, setDrafts] = useState<AskUserQuestionDrafts>(() => initialDrafts(questions));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [settled, setSettled] = useState<Settled | null>(null);
  const inFlight = useRef(false);
  const mounted = useRef(true);
  const prefix = useId();

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);

  const pending = interaction.status === 'pending';
  const interactive = pending && settled === null && Boolean(onAnswer) && !busy;
  const built = buildAskUserQuestionContent(questions, drafts);
  const missing = built.ok ? 0 : built.missing.length;

  const update = (questionId: string, draft: AskUserQuestionDraft) => {
    setDrafts((current) => ({ ...current, [questionId]: draft }));
  };

  const submit = async (decision: AskUserQuestionDecision) => {
    if (!onAnswer || inFlight.current) return;
    inFlight.current = true;
    setBusy(true);
    setError('');
    try {
      await onAnswer(interaction.id, { kind: 'elicitation', decision });
      if (mounted.current) setSettled(decision.action === 'accept' ? 'accepted' : 'declined');
    } catch (failure) {
      if (mounted.current) setError(failureText(failure));
    } finally {
      inFlight.current = false;
      if (mounted.current) setBusy(false);
    }
  };

  const accept = () => {
    if (!built.ok) {
      setError(`请先回答所有问题（还有 ${built.missing.length} 个未完成）。`);
      return;
    }
    void submit({ action: 'accept', content: built.content });
  };

  return <div
    data-slot="ask-user-question"
    aria-busy={busy || undefined}
    className={cn(paper, 'flex w-full max-w-xl flex-col gap-3.5 rounded-[20px] p-4', busy && 'opacity-60')}
  >
    <div className="flex items-center gap-2.5">
      <span aria-hidden className="bg-foreground/[0.05] text-muted-foreground flex size-7 shrink-0 items-center justify-center rounded-lg">
        <MessageCircleQuestion className="size-3.5" />
      </span>
      <div className="flex min-w-0 flex-1 flex-col">
        <span className="truncate text-[13.5px] font-medium">需要你的回答</span>
        <span className="text-muted-foreground text-xs">{questions.length} 个问题</span>
      </div>
      {pending && <span className={cn(mono, 'text-muted-foreground shrink-0')}>等待回答</span>}
    </div>

    <div className="flex flex-col gap-2.5">
      {questions.map((question, index) => {
        const draft = drafts[question.id] ?? { selected: [], custom: '' };
        const groupName = `${prefix}-${index}`;
        const selected = new Set(draft.selected);
        const label = question.header.trim() || `问题 ${index + 1}`;
        const inputLabel = `自定义回答：${label}`;
        return <fieldset
          key={question.id}
          data-question-id={question.id}
          disabled={!interactive}
          className={cn(field, 'flex min-w-0 flex-col gap-2 rounded-xl px-3 py-2.5')}
        >
          <legend className={cn(mono, 'text-muted-foreground px-1')}>{label}</legend>
          <p className="text-[13px] leading-relaxed">{question.question}</p>
          <p className={cn(mono, 'text-muted-foreground')}>
            {question.multiSelect ? '可多选' : question.options.length > 0 ? '单选' : '自由输入'}
          </p>
          {question.options.length > 0 && <div className="flex flex-col gap-1">
            {question.options.map((option, optionIndex) => {
              const checked = selected.has(option.value);
              return <label
                key={`${option.value}-${optionIndex}`}
                className={cn(
                  'flex cursor-pointer items-start gap-2 rounded-lg px-2 py-1.5 text-xs transition-colors',
                  checked ? 'bg-foreground/[0.07]' : 'hover:bg-foreground/[0.04]',
                  !interactive && 'cursor-default',
                )}
              >
                <input
                  type={question.multiSelect ? 'checkbox' : 'radio'}
                  name={groupName}
                  value={option.value}
                  checked={checked}
                  disabled={!interactive}
                  onChange={(event) => {
                    if (question.multiSelect) {
                      const next = event.currentTarget.checked
                        ? [...draft.selected, option.value]
                        : draft.selected.filter((value) => value !== option.value);
                      update(question.id, { ...draft, selected: next });
                    } else if (event.currentTarget.checked) {
                      // 单选下选择预设项即放弃自定义文本，避免两个答案同时生效。
                      update(question.id, { selected: [option.value], custom: '' });
                    }
                  }}
                  className="accent-foreground mt-0.5 size-3 shrink-0"
                />
                <span className="flex min-w-0 flex-col gap-0.5">
                  <span className="text-foreground/85 break-words">{option.label}</span>
                  {option.description && <span className="text-muted-foreground break-words">{option.description}</span>}
                </span>
              </label>;
            })}
          </div>}
          <label className="flex flex-col gap-1">
            <span className={cn(mono, 'text-muted-foreground')}>{question.options.length > 0 ? '自定义输入（可选）' : '回答'}</span>
            <input
              type="text"
              value={draft.custom}
              disabled={!interactive}
              aria-label={inputLabel}
              placeholder={question.options.length > 0 ? '输入其它答案' : '输入你的回答'}
              onChange={(event) => {
                const custom = event.currentTarget.value;
                if (question.multiSelect) update(question.id, { ...draft, custom });
                else update(question.id, { custom, selected: custom.trim() ? [] : draft.selected });
              }}
              className={cn(
                field,
                'text-foreground/80 focus-visible:ring-foreground/20 rounded-lg px-2.5 py-1.5 text-xs outline-none focus-visible:ring-1',
              )}
            />
          </label>
        </fieldset>;
      })}
    </div>

    {error && <p className="text-destructive text-xs" role="alert">{error}</p>}
    {interactive && missing > 0 && <p className="text-muted-foreground text-xs" role="status">
      还有 {missing} 个问题未回答。
    </p>}
    {pending ? <div className="flex min-h-8 flex-wrap items-center justify-end gap-2">
      {settled ? <span className="text-muted-foreground flex items-center gap-1.5 text-xs">
        {settled === 'accepted'
          ? <><CheckIcon className="size-3.5 text-emerald-500" />已提交，等待服务端结算。</>
          : <><XIcon className="size-3.5" />已拒绝，等待服务端结算。</>}
      </span> : <>
        <button
          type="button"
          // 用户取消必须是明确拒绝：Rust 只把 decline 映射为 ToolRejected（用户拒绝回答）；
          // 无 _meta 的 cancel 会被当作生命周期取消并回落空答案。
          onClick={() => void submit({ action: 'decline' })}
          disabled={!interactive}
          className="text-muted-foreground hover:bg-foreground/[0.06] hover:text-foreground/90 h-8 rounded-full px-3.5 text-xs font-medium transition-[background-color,color,scale] duration-150 active:scale-[0.96] disabled:pointer-events-none disabled:opacity-40"
        >
          取消
        </button>
        <button
          type="button"
          onClick={accept}
          disabled={!interactive || !built.ok}
          className={cn(
            inkButton,
            'flex h-8 items-center rounded-full px-3.5 text-xs font-medium disabled:pointer-events-none disabled:opacity-40',
          )}
        >
          提交回答
        </button>
      </>}
    </div> : <p className="text-muted-foreground text-xs">
      {interaction.status === 'resolved' ? `已处理：${interaction.outcome ?? '已结算'}` : '已失效'}
    </p>}
    {pending && !onAnswer && <p className="text-muted-foreground text-xs" role="status">
      连接就绪后可以回答这条交互。
    </p>}
  </div>;
}
