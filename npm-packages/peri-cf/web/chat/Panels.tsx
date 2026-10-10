import { useState } from 'react';
import { CircleAlert, ListChecks, MessageCircleQuestion, Timer } from 'lucide-react';
import type { InteractionAnswer, InteractionFieldView } from '@peri-code/sdk/view';
import { submitInteractionAnswer } from '../api/client';
import { buildElicitationContent, initialElicitationValues, type ElicitationValues } from './interactionForm';
import type { Interaction, SyncState } from './types';

const TERMINAL_TASK = new Set(['completed', 'failed', 'cancelled']);
/** SDK 契约支持的权限决定；其它 optionId 暂不支持（不误发）。 */
const SUPPORTED_PERMISSION_OPTIONS = new Set(['allow_once', 'reject_once']);

function fieldInput(field: InteractionFieldView, values: ElicitationValues, change: (key: string, value: string | boolean) => void) {
  const id = `interaction-field-${field.key}`;
  if (field.type === 'boolean') {
    return <label key={field.key} className="interaction-field" htmlFor={id}>
      <input id={id} type="checkbox" checked={values[field.key] === true}
        onChange={(event) => change(field.key, event.target.checked)} /> {field.title}
      {field.description && <span className="muted"> {field.description}</span>}
    </label>;
  }
  if (field.enum?.length) {
    return <label key={field.key} className="interaction-field" htmlFor={id}>
      <span>{field.title}{field.required ? '（必填）' : ''}</span>
      <select id={id} value={String(values[field.key] ?? '')} onChange={(event) => change(field.key, event.target.value)}>
        <option value="">请选择</option>
        {field.enum.map((option) => <option key={String(option)} value={String(option)}>{String(option)}</option>)}
      </select>
      {field.description && <span className="muted"> {field.description}</span>}
    </label>;
  }
  return <label key={field.key} className="interaction-field" htmlFor={id}>
    <span>{field.title}{field.required ? '（必填）' : ''}</span>
    <input id={id} type={field.type === 'string' ? 'text' : 'number'}
      step={field.type === 'integer' ? 1 : undefined}
      value={String(values[field.key] ?? '')} onChange={(event) => change(field.key, event.target.value)} />
    {field.description && <span className="muted"> {field.description}</span>}
  </label>;
}

function InteractionCard({ interaction, chatId, token }: { interaction: Interaction; chatId?: string; token?: string }) {
  const [values, setValues] = useState<ElicitationValues>(() => initialElicitationValues(interaction.fields ?? []));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [submitted, setSubmitted] = useState(false);
  const permission = interaction.kind === 'permission';
  const pending = interaction.status === 'pending';
  const canSubmit = Boolean(pending && chatId && token && !interaction.formUnsupported);
  const choices = interaction.options ?? [];
  const supportedChoices = permission ? choices.filter((option) => SUPPORTED_PERMISSION_OPTIONS.has(option.optionId)) : [];
  // 可回答 = 有支持的权限选项，或有可渲染的表单字段。
  const answerable = canSubmit && !submitted && (permission ? supportedChoices.length > 0 : (interaction.fields?.length ?? 0) > 0);

  const submit = async (answer: InteractionAnswer) => {
    if (!chatId || !token || busy) return;
    setBusy(true);
    setError('');
    try {
      await submitInteractionAnswer(token, chatId, interaction.id, answer);
      setSubmitted(true);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : '回答未能提交，请稍后重试。');
    } finally {
      setBusy(false);
    }
  };

  const submitForm = async () => {
    const built = buildElicitationContent(interaction.fields ?? [], values);
    if (!built.ok) { setError('请先填写必填项，并检查数值格式。'); return; }
    await submit({ kind: 'elicitation', decision: { action: 'accept', content: built.content } });
  };

  return <li className="panel-interaction">
    {permission ? <CircleAlert size={14} /> : <MessageCircleQuestion size={14} />}
    <div>
      <strong>{permission ? interaction.title ?? '权限请求' : interaction.message ?? '需要你的回答'}</strong>
      <p className="muted">{pending ? (submitted ? '已提交，等待服务端结算。' : '等待回答')
        : interaction.status === 'resolved' ? `已处理：${interaction.outcome ?? '已结算'}` : '已失效'}</p>
      {permission && interaction.options && <p className="muted">可选项：{interaction.options.map((option) => option.name).join(' / ')}</p>}
      {interaction.formUnsupported && <p className="muted">该表单类型暂不支持回答（{interaction.formUnsupported}）。</p>}

      {canSubmit && !submitted && permission && choices.length > 0 && <div className="interaction-actions">
        {choices.map((option) => SUPPORTED_PERMISSION_OPTIONS.has(option.optionId)
          ? <button key={option.optionId} type="button" className="text-button" disabled={busy}
              onClick={() => void submit({ kind: 'permission', decision: option.optionId as 'allow_once' | 'reject_once' })}>
              {option.name}
            </button>
          : <span key={option.optionId} className="muted">{option.name}（暂不支持）</span>)}
      </div>}

      {answerable && !permission && <div className="interaction-form">
        {interaction.fields!.map((field) => fieldInput(field, values, (key, value) => setValues((current) => ({ ...current, [key]: value }))))}
        <div className="interaction-actions">
          <button type="button" className="text-button" disabled={busy} onClick={() => void submitForm()}>提交</button>
          <button type="button" className="text-button" disabled={busy}
            onClick={() => void submit({ kind: 'elicitation', decision: { action: 'decline' } })}>拒绝回答</button>
        </div>
      </div>}

      {!pending && !interaction.formUnsupported && <p className="muted">回答闭环已在服务端结算，本页只读展示。</p>}
      {pending && !answerable && <p className="muted" role="status">{chatId && token
        ? '这条交互当前不可回答（没有可提交的选项）。' : '连接就绪后可以回答这条交互。'}</p>}
      {error && <p className="has-error" role="alert">{error}</p>}
    </div>
  </li>;
}

/** 只读消费面 + 交互回答（鉴权命令，不写 Y.Doc）。 */
export function ChatPanels({ state, chatId, token }: { state: SyncState | null; chatId?: string; token?: string }) {
  if (!state || state.readiness !== 'ready') return null;
  const pending = state.pendingInteractions;
  const active = state.activeTurn.turnId;
  const plans = active ? state.plans.byTurn[active] ?? state.plans.current : state.plans.current;
  const openTasks = state.tasks.filter((task) => !TERMINAL_TASK.has(task.status));
  return <details className="chat-panels" open={pending.length > 0}>
    <summary>
      会话面板 · 任务 {openTasks.length}/{state.tasks.length} · 计划 {plans.length} · 队列 {state.inputQueue?.items.length ?? 0} · 交互 {pending.length}
    </summary>
    <div className="panels">
      <section className="panel">
        <h3>会话</h3>
        <ul className="panel-list">
          <li>{state.session.title ?? state.chat.title}</li>
          {state.session.model && <li>模型：{state.session.model}</li>}
          {state.session.mode && <li>模式：{state.session.mode}</li>}
          <li>运行状态：{state.activeTurn.status ?? '空闲'}{state.running ? '（执行中）' : ''}</li>
          {state.session.activeTurnInteractionStatus && <li>等待交互：{state.session.activeTurnInteractionStatus}</li>}
          {state.session.legacyIncomplete && <li>历史由旧记录导入，可能缺少推理与工具细节。</li>}
          {state.executionBlocked && <li>宿主执行被阻断，请先确认停止状态。</li>}
        </ul>
      </section>
      <section className="panel">
        <h3><Timer size={14} /> 任务</h3>
        {state.tasks.length === 0 ? <p className="muted">没有任务记录。</p> : <ul className="panel-list">
          {state.tasks.map((task) => <li key={task.taskId}>
            <strong>{task.title}</strong>
            <span className="muted"> {task.status}{task.summary ? ` · ${task.summary}` : ''}</span>
            {task.status.startsWith('unobserved:') && <span className="muted"> （重启后不可观测，未确认仍在运行）</span>}
          </li>)}
        </ul>}
      </section>
      <section className="panel">
        <h3><ListChecks size={14} /> 计划</h3>
        {plans.length === 0 ? <p className="muted">当前没有计划。</p> : <ul className="panel-list">
          {plans.map((entry, index) => <li key={`${entry.content}:${index}`}>
            <strong>{entry.content}</strong><span className="muted"> {entry.status}{entry.activeForm ? ` · ${entry.activeForm}` : ''}</span>
          </li>)}
        </ul>}
      </section>
      <section className="panel">
        <h3>输入队列</h3>
        {!state.inputQueue || state.inputQueue.items.length === 0 ? <p className="muted">队列为空。</p>
          : <ul className="panel-list">{state.inputQueue.items.map((item) => <li key={item.inputId}>{item.state}</li>)}</ul>}
      </section>
      <section className="panel">
        <h3>交互</h3>
        {state.interactions.length === 0 ? <p className="muted">没有交互记录。</p>
          : <ul className="panel-list">{state.interactions.map((interaction) =>
            <InteractionCard key={interaction.id} interaction={interaction} chatId={chatId} token={token} />)}</ul>}
      </section>
    </div>
  </details>;
}
