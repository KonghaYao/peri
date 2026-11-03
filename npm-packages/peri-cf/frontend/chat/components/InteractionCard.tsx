import { useState } from 'react';
import type { InteractionAnswer, InteractionFieldView } from '@peri-code/sdk/view';
import { ApprovalCard, type ApprovalState } from '../../components/assistant-ui';
import {
  ElicitationForm, type ElicitationField, type ElicitationState,
} from '../../components/assistant-ui';
import { AskUserQuestionCard } from './AskUserQuestionCard';
import {
  buildElicitationContent, initialElicitationValues, SUPPORTED_PERMISSION_OPTIONS,
  type ElicitationValues, type InteractionAnswerHandler,
} from '../runtime';
import type { Interaction } from '../runtime';

/** 本地结算态只记录本页已提交的方向；服务端是否结算仍以投影状态为准。 */
type Settled = 'accepted' | 'declined';

/** 提交与本地反馈：服务端结果由下一次投影接管，这里只承担提交中、失败与已提交标记。 */
function useSubmission(interactionId: string, onAnswer?: InteractionAnswerHandler) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [settled, setSettled] = useState<Settled | null>(null);
  const submit = async (answer: InteractionAnswer, outcome: Settled) => {
    if (!onAnswer || busy) return;
    setBusy(true);
    setError('');
    try {
      await onAnswer(interactionId, answer);
      setSettled(outcome);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : '回答未能提交，请稍后重试。');
    } finally {
      setBusy(false);
    }
  };
  return { busy, error, settled, setError, submit };
}

/**
 * permission：允许一次 / 拒绝。SDK 契约之外的 optionId 不渲染按钮，只说明不可用。
 */
function PermissionCard({ interaction, onAnswer }: { interaction: Interaction; onAnswer?: InteractionAnswerHandler }) {
  const { busy, error, settled, submit } = useSubmission(interaction.id, onAnswer);
  const pending = interaction.status === 'pending';
  const choices = interaction.options ?? [];
  const allowOnce = choices.find((option) => option.optionId === 'allow_once');
  const rejectOnce = choices.find((option) => option.optionId === 'reject_once');
  const unsupported = choices.filter((option) => !SUPPORTED_PERMISSION_OPTIONS.has(option.optionId));
  const active = pending && settled === null;
  const answerable = active && Boolean(onAnswer) && Boolean(allowOnce || rejectOnce);

  const state: ApprovalState = active ? 'request'
    : pending ? 'running'
      : interaction.status === 'resolved' ? 'done' : 'denied';
  const statusLabel = state === 'running' ? (settled === 'declined' ? '已拒绝，等待服务端结算。' : '已提交，等待服务端结算。')
    : state === 'done' ? `已处理：${interaction.outcome ?? '已结算'}`
      : state === 'denied' ? '已失效' : undefined;

  return <div className="flex flex-col items-start gap-1.5">
    <ApprovalCard
      state={state}
      title={interaction.title ?? '权限请求'}
      subtitle={interaction.message ?? '需要你的授权'}
      statusLabel={statusLabel}
      className={busy ? 'pointer-events-none opacity-60' : undefined}
      allowOnceLabel={allowOnce?.name}
      denyLabel={rejectOnce?.name}
      onAllowOnce={answerable && allowOnce ? () => void submit({ kind: 'permission', decision: 'allow_once' }, 'accepted') : undefined}
      onDeny={answerable && rejectOnce ? () => void submit({ kind: 'permission', decision: 'reject_once' }, 'declined') : undefined}
    />
    {unsupported.length > 0 && <p className="text-muted-foreground text-xs">
      以下选项暂不支持：{unsupported.map((option) => option.name).join('、')}
    </p>}
    {active && !answerable && <p className="text-muted-foreground text-xs" role="status">
      {onAnswer ? '这条交互当前不可回答（没有可提交的选项）。' : '连接就绪后可以回答这条交互。'}
    </p>}
    {error && <p className="text-destructive text-xs" role="alert">{error}</p>}
  </div>;
}

/** SDK 字段视图 → 官方表单字段：boolean→toggle、enum→choice、其余→text；值统一为字符串。 */
function toElicitationField(field: InteractionFieldView, value: string | boolean | number | undefined): ElicitationField {
  const shared = {
    name: field.key, label: field.title, required: field.required,
    description: field.description || undefined,
  };
  if (field.type === 'boolean') return { ...shared, value: value === true ? 'true' : 'false', kind: 'toggle' };
  if (field.enum?.length) {
    return { ...shared, value: String(value ?? ''), kind: 'choice', options: field.enum.map((option) => String(option)) };
  }
  return { ...shared, value: String(value ?? ''), kind: 'text' };
}

/**
 * elicitation：官方表单承载字段与回执，提交内容仍由 buildElicitationContent 构建。
 */
function ElicitationCard({ interaction, onAnswer }: { interaction: Interaction; onAnswer?: InteractionAnswerHandler }) {
  const fields = interaction.fields ?? [];
  const [values, setValues] = useState<ElicitationValues>(() => initialElicitationValues(fields));
  const { busy, error, settled, setError, submit } = useSubmission(interaction.id, onAnswer);
  const pending = interaction.status === 'pending';
  const active = pending && settled === null;
  const answerable = active && Boolean(onAnswer) && !interaction.formUnsupported && fields.length > 0;
  const state: ElicitationState = settled ?? (pending ? 'request' : 'accepted');
  const locked = busy || (active && !answerable);

  const change = (name: string, value: string) => {
    const field = fields.find((item) => item.key === name);
    // boolean 字段在 SDK 值域里是布尔，表单控件给的是 "true"/"false" 字符串，这里转回。
    setValues((current) => ({ ...current, [name]: field?.type === 'boolean' ? value === 'true' : value }));
  };
  const accept = async () => {
    const built = buildElicitationContent(fields, values);
    if (!built.ok) { setError('请先填写必填项，并检查数值格式。'); return; }
    await submit({ kind: 'elicitation', decision: { action: 'accept', content: built.content } }, 'accepted');
  };

  return <div className="flex flex-col items-start gap-1.5">
    <ElicitationForm
      server={interaction.title ?? '工具请求'}
      message={interaction.message ?? '需要你的回答'}
      fields={fields.map((field) => toElicitationField(field, values[field.key]))}
      state={state}
      className={locked ? 'pointer-events-none opacity-60' : undefined}
      onFieldChange={answerable ? change : undefined}
      onAccept={answerable ? () => void accept() : undefined}
      onDecline={answerable ? () => void submit({ kind: 'elicitation', decision: { action: 'decline' } }, 'declined') : undefined}
    />
    {!pending && <p className="text-muted-foreground text-xs">
      {interaction.status === 'resolved' ? `已处理：${interaction.outcome ?? '已结算'}` : '已失效'}
    </p>}
    {interaction.formUnsupported && <p className="text-muted-foreground text-xs">该表单类型暂不支持回答（{interaction.formUnsupported}）。</p>}
    {active && !answerable && !interaction.formUnsupported && <p className="text-muted-foreground text-xs" role="status">
      {onAnswer ? '这条交互当前不可回答（没有可提交的选项）。' : '连接就绪后可以回答这条交互。'}
    </p>}
    {error && <p className="text-destructive text-xs" role="alert">{error}</p>}
  </div>;
}

/** 单条交互卡：待回答时可操作，结算后只读。回答通道由 onAnswer 注入，本组件不持有连接。 */
export function InteractionCard({ interaction, onAnswer }: { interaction: Interaction; onAnswer?: InteractionAnswerHandler }) {
  if (interaction.questions?.length) return <AskUserQuestionCard interaction={interaction} onAnswer={onAnswer} />;
  return interaction.kind === 'permission'
    ? <PermissionCard interaction={interaction} onAnswer={onAnswer} />
    : <ElicitationCard interaction={interaction} onAnswer={onAnswer} />;
}
