import { useEffect, useId, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from 'react';
import { CheckIcon, ChevronLeftIcon, ChevronRightIcon, Loader2Icon, MessageCircleQuestion, XIcon } from 'lucide-react';
import { cn } from '@/lib';
import { field, inkButton, paper } from '../../components/assistant-ui';
import {
  buildAskUserQuestionContent,
  type AskUserQuestion, type Interaction, type InteractionAnswerHandler,
  type AskUserQuestionDecision, type AskUserQuestionDraft, type AskUserQuestionDrafts,
} from '../runtime';

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
 * AskUserQuestion 卡片：多个问题时按题呈现，一次只显示一个 fieldset。
 *
 * 单选 radio、多选 checkbox、自由文本输入；进度点可跳转并标出已答题目。
 * 提交内容由 buildAskUserQuestionContent 按 wire 契约构造；结算状态以投影
 * 移除待答记录为准。
 */
export function AskUserQuestionCard({
  interaction, onAnswer,
}: { interaction: Interaction; onAnswer?: InteractionAnswerHandler }) {
  const questions = interaction.questions ?? [];
  const [drafts, setDrafts] = useState<AskUserQuestionDrafts>(() => initialDrafts(questions));
  const [step, setStep] = useState(0);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [settled, setSettled] = useState<Settled | null>(null);
  const [confirmingReject, setConfirmingReject] = useState(false);
  const inFlight = useRef(false);
  const mounted = useRef(true);
  const prefix = useId();
  const fieldsetRefs = useRef<Array<HTMLFieldSetElement | null>>([]);
  const previousStep = useRef(0);

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);

  // 同一会话里换了一条交互时，旧草稿不能带过去。
  useEffect(() => {
    setDrafts(initialDrafts(questions));
    setStep(0);
    setSettled(null);
    setError('');
    setConfirmingReject(false);
    previousStep.current = 0;
    // questions 随 interaction.id 冻结；不把数组放进依赖，避免父层重渲染时清空草稿。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [interaction.id]);

  const lastStep = Math.max(0, questions.length - 1);
  const activeIndex = Math.min(step, lastStep);
  const activeQuestion = questions[activeIndex];
  const multiple = questions.length > 1;
  const pending = interaction.status === 'pending';
  const interactive = pending && settled === null && Boolean(onAnswer) && !busy;
  const built = buildAskUserQuestionContent(questions, drafts);
  const title = interaction.title?.trim() || '需要你的回答';

  const answered = (question: AskUserQuestion) =>
    buildAskUserQuestionContent([question], drafts).ok;
  const answeredCount = questions.filter(answered).length;
  const hasDraft = questions.some((question) => {
    const draft = drafts[question.id];
    return Boolean(draft?.custom.trim() || draft?.selected.length);
  });

  const goTo = (index: number) => {
    setStep(Math.max(0, Math.min(index, lastStep)));
  };

  // 逐题跳转后把焦点交给当前问题，读屏会先读 legend 再读选项。
  useEffect(() => {
    if (previousStep.current === activeIndex) return;
    previousStep.current = activeIndex;
    fieldsetRefs.current[activeIndex]?.focus();
  }, [activeIndex]);

  const update = (questionId: string, draft: AskUserQuestionDraft) => {
    setError('');
    setConfirmingReject(false);
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
      const index = Math.max(0, questions.findIndex((question) => !answered(question)));
      setError(`第 ${index + 1} 题还没有回答。`);
      if (index === activeIndex) fieldsetRefs.current[index]?.focus();
      else goTo(index);
      return;
    }
    void submit({ action: 'accept', content: built.content });
  };

  const advance = () => {
    if (!answered(activeQuestion)) {
      setError('请先回答本题。');
      fieldsetRefs.current[activeIndex]?.focus();
      return;
    }
    setError('');
    if (activeIndex < lastStep) goTo(activeIndex + 1);
    else accept();
  };

  const reject = () => {
    if (!hasDraft) {
      void submit({ action: 'decline' });
      return;
    }
    setConfirmingReject(true);
  };

  const confirmReject = () => {
    setConfirmingReject(false);
    void submit({ action: 'decline' });
  };

  return <div
    data-slot="ask-user-question"
    aria-busy={busy || undefined}
    className={cn(paper, 'flex w-full max-w-xl flex-col gap-3.5 rounded-[20px] p-4')}
  >
    <div className="flex items-center gap-2.5">
      <span aria-hidden className="bg-foreground/[0.05] text-muted-foreground flex size-7 shrink-0 items-center justify-center rounded-lg">
        <MessageCircleQuestion className="size-3.5" />
      </span>
      <div className="flex min-w-0 flex-1 flex-col">
        <span className="truncate text-sm font-medium">{title}</span>
        <span className="text-muted-foreground text-xs">
          {multiple
            ? `第 ${activeIndex + 1} / ${questions.length} 题 · 已答 ${answeredCount} / ${questions.length}`
            : `${questions.length} 个问题`}
        </span>
      </div>
      {pending && !settled && <span className="text-muted-foreground flex shrink-0 items-center gap-1.5 text-xs">
        <span aria-hidden className="size-1.5 rounded-full bg-amber-500" />
        等待回答
      </span>}
    </div>

    {multiple && (
      <div className="flex items-center gap-1" role="group" aria-label="问题进度">
        {questions.map((question, index) => {
          const done = answered(question);
          const current = index === activeIndex;
          return <button
            key={question.id}
            type="button"
            disabled={!interactive}
            aria-label={`第 ${index + 1} 题${done ? '，已回答' : '，未回答'}`}
            aria-current={current ? 'step' : undefined}
            onClick={() => goTo(index)}
            className={cn(
              'flex h-6 min-w-6 items-center justify-center rounded-full px-1.5 text-[11px] font-medium transition-colors focus-visible:ring-2 focus-visible:ring-ring/40',
              current
                ? 'bg-foreground text-background'
                : done
                  ? 'bg-emerald-500/15 text-emerald-700 dark:text-emerald-400'
                  : 'bg-foreground/[0.06] text-muted-foreground',
              interactive ? 'cursor-pointer' : 'cursor-default',
            )}
          >
            {done && !current ? <CheckIcon className="size-3" /> : index + 1}
          </button>;
        })}
      </div>
    )}

    {activeQuestion && (() => {
      const draft = drafts[activeQuestion.id] ?? { selected: [], custom: '' };
      const groupName = `${prefix}-${activeIndex}`;
      const selected = new Set(draft.selected);
      const label = activeQuestion.header.trim() || `问题 ${activeIndex + 1}`;
      const questionId = `${prefix}-question-${activeIndex}`;
      const hasOptions = activeQuestion.options.length > 0;
      const inputLabel = `第 ${activeIndex + 1} 题：${hasOptions ? '其他答案' : '回答'}`;
      const changeCustom = (custom: string) => {
        if (activeQuestion.multiSelect) update(activeQuestion.id, { ...draft, custom });
        else update(activeQuestion.id, { custom, selected: custom.trim() ? [] : draft.selected });
      };
      const customKeyDown = (event: ReactKeyboardEvent<HTMLInputElement | HTMLTextAreaElement>) => {
        const isTextarea = event.currentTarget.tagName === 'TEXTAREA';
        const submitKey = isTextarea ? (event.metaKey || event.ctrlKey) && event.key === 'Enter' : event.key === 'Enter';
        if (!submitKey || event.nativeEvent.isComposing) return;
        event.preventDefault();
        advance();
      };
      return <fieldset
        key={activeQuestion.id}
        ref={(node) => { fieldsetRefs.current[activeIndex] = node; }}
        tabIndex={-1}
        data-question-id={activeQuestion.id}
        disabled={!interactive}
        aria-describedby={questionId}
        className={cn(
          field,
          'flex min-w-0 flex-col gap-2.5 rounded-xl px-3.5 py-3 outline-none focus-visible:ring-2 focus-visible:ring-ring/40',
        )}
      >
        <legend className="text-foreground/60 px-1 text-xs font-medium">{label}</legend>
        <p id={questionId} className="text-sm leading-relaxed break-words [overflow-wrap:anywhere]">{activeQuestion.question}</p>
        <p className="text-muted-foreground text-xs">
          {activeQuestion.multiSelect ? '可多选' : hasOptions ? '单选' : '自由输入'}
        </p>
        {hasOptions && <div className="flex max-h-[40vh] flex-col gap-1 overflow-y-auto pr-0.5">
          {activeQuestion.options.map((option, optionIndex) => {
            const checked = selected.has(option.value);
            return <label
              key={`${option.value}-${optionIndex}`}
              className={cn(
                'flex cursor-pointer items-start gap-2.5 rounded-lg px-2.5 py-2 text-[12.5px] transition-colors',
                checked ? 'bg-foreground/[0.07]' : 'hover:bg-foreground/[0.04]',
                !interactive && 'cursor-default',
              )}
            >
              <input
                type={activeQuestion.multiSelect ? 'checkbox' : 'radio'}
                name={groupName}
                value={option.value}
                checked={checked}
                disabled={!interactive}
                onChange={(event) => {
                  if (activeQuestion.multiSelect) {
                    const next = event.currentTarget.checked
                      ? [...draft.selected, option.value]
                      : draft.selected.filter((value) => value !== option.value);
                    update(activeQuestion.id, { ...draft, selected: next });
                  } else if (event.currentTarget.checked) {
                    // 单选下选择预设项即放弃自定义文本，避免两个答案同时生效。
                    update(activeQuestion.id, { selected: [option.value], custom: '' });
                  }
                }}
                className="accent-foreground mt-0.5 size-3 shrink-0"
              />
              <span className="flex min-w-0 flex-col gap-0.5">
                <span className="text-foreground/85 break-words">{option.label}</span>
                {option.description && <span className="text-muted-foreground text-[11.5px] break-words">{option.description}</span>}
              </span>
            </label>;
          })}
        </div>}
        <label className="flex flex-col gap-1.5">
          <span className="text-muted-foreground text-xs">
            {hasOptions ? '其他答案（可选，替代所选）' : '回答（必填）'}
          </span>
          {hasOptions ? <input
            type="text"
            value={draft.custom}
            disabled={!interactive}
            aria-label={inputLabel}
            placeholder="输入其它答案"
            onChange={(event) => changeCustom(event.currentTarget.value)}
            onKeyDown={customKeyDown}
            className={cn(
              field,
              'text-foreground/90 focus-visible:ring-foreground/20 h-11 rounded-lg border border-foreground/15 px-3 text-base outline-none placeholder:text-muted-foreground/70 focus-visible:ring-2 sm:h-9 sm:text-sm',
            )}
          /> : <textarea
            rows={3}
            value={draft.custom}
            disabled={!interactive}
            aria-label={inputLabel}
            placeholder="输入你的回答"
            onChange={(event) => changeCustom(event.currentTarget.value)}
            onKeyDown={customKeyDown}
            className={cn(
              field,
              'text-foreground/90 focus-visible:ring-foreground/20 min-h-20 resize-none rounded-lg border border-foreground/15 px-3 py-2 text-base outline-none placeholder:text-muted-foreground/70 focus-visible:ring-2 sm:text-sm',
            )}
          />}
        </label>
      </fieldset>;
    })()}

    {pending ? <div className="flex flex-col gap-2">
      {error && <p className="text-destructive text-xs" role="alert">{error}</p>}
      {settled ? <div className="flex items-center justify-end">
        <span className="text-muted-foreground flex items-center gap-1.5 text-xs">
          {settled === 'accepted'
            ? <><CheckIcon className="size-3.5 text-emerald-500" />已提交，等待服务端结算。</>
            : <><XIcon className="size-3.5" />已拒绝，等待服务端结算。</>}
        </span>
      </div> : confirmingReject ? (
        <div className="border-destructive/20 bg-destructive/5 flex flex-col gap-2 rounded-lg border p-2.5">
          <p className="text-destructive text-xs" role="alert">拒绝后已填写的内容不会提交。</p>
          <div className="flex gap-2">
            <button
              type="button"
              onClick={() => setConfirmingReject(false)}
              disabled={busy}
              className="text-muted-foreground hover:bg-foreground/[0.06] hover:text-foreground/90 h-11 flex-1 rounded-full text-xs font-medium transition-colors disabled:pointer-events-none disabled:opacity-40 sm:h-8"
            >
              继续填写
            </button>
            <button
              type="button"
              onClick={confirmReject}
              disabled={busy}
              className="border-destructive/30 text-destructive hover:bg-destructive/10 h-11 flex-1 rounded-full border text-xs font-medium transition-colors disabled:pointer-events-none disabled:opacity-40 sm:h-8"
            >
              确认拒绝
            </button>
          </div>
        </div>
      ) : (
        <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
          <button
            type="button"
            // 用户拒绝必须是明确拒绝：Rust 只把 decline 映射为 ToolRejected（用户拒绝回答）；
            // 无 _meta 的 cancel 会被当作生命周期取消并回落空答案。
            onClick={reject}
            disabled={!interactive}
            className="text-muted-foreground hover:bg-foreground/[0.06] hover:text-foreground/90 h-11 self-start rounded-full px-3.5 text-xs font-medium transition-[background-color,color,scale] duration-150 active:scale-[0.96] disabled:pointer-events-none disabled:opacity-40 sm:h-8"
          >
            {busy ? '提交中…' : '拒绝回答'}
          </button>
          <div className="flex flex-1 items-center gap-2 sm:flex-none">
            {multiple && activeIndex > 0 && <button
              type="button"
              onClick={() => goTo(activeIndex - 1)}
              disabled={!interactive}
              className="text-muted-foreground hover:bg-foreground/[0.06] hover:text-foreground/90 flex h-11 items-center gap-1 rounded-full px-3 text-xs font-medium transition-colors disabled:pointer-events-none disabled:opacity-40 sm:h-8"
            >
              <ChevronLeftIcon className="size-3.5" />
              上一题
            </button>}
            {multiple && activeIndex < lastStep ? <button
              type="button"
              onClick={advance}
              disabled={!interactive}
              className={cn(
                inkButton,
                'flex h-11 flex-1 items-center justify-center gap-1 rounded-full px-3.5 text-xs font-medium disabled:pointer-events-none disabled:opacity-40 sm:h-8 sm:flex-none',
              )}
            >
              下一题
              <ChevronRightIcon className="size-3.5" />
            </button> : <button
              type="button"
              onClick={accept}
              disabled={!interactive}
              className={cn(
                inkButton,
                'flex h-11 flex-1 items-center justify-center gap-1.5 rounded-full px-3.5 text-xs font-medium disabled:pointer-events-none disabled:opacity-40 sm:h-8 sm:flex-none',
              )}
            >
              {busy && <Loader2Icon className="size-3.5 animate-spin" />}
              {busy ? '提交中…' : '提交回答'}
            </button>}
          </div>
        </div>
      )}
    </div> : <p className="text-muted-foreground text-xs">
      {interaction.status === 'resolved' ? `已处理：${interaction.outcome ?? '已结算'}` : '已失效'}
    </p>}
    {pending && !onAnswer && <p className="text-muted-foreground text-xs" role="status">
      连接就绪后可以回答这条交互。
    </p>}
  </div>;
}
