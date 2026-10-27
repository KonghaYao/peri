import type { ReactNode } from 'react';
import { ListChecks, MessageCircleQuestion, Timer } from 'lucide-react';
import type { Interaction, ChatDisplayState } from '../runtime';

const TERMINAL_TASK = new Set(['completed', 'failed', 'cancelled']);

function interactionStatus(interaction: Interaction): string {
  if (interaction.status === 'pending') return '等待回答';
  if (interaction.status === 'resolved') return `已处理：${interaction.outcome ?? '已结算'}`;
  return '已失效';
}

function interactionName(interaction: Interaction): string {
  return interaction.title ?? interaction.message ?? (interaction.kind === 'permission' ? '权限请求' : '表单请求');
}

function Section({ icon, title, children }: { icon?: ReactNode; title: string; children: ReactNode }) {
  return <section className="flex flex-col gap-1.5">
    <h3 className="text-muted-foreground flex items-center gap-1.5 text-[11px] font-medium tracking-wide uppercase">
      {icon}{title}
    </h3>
    {children}
  </section>;
}

const listClass = 'text-foreground/80 flex flex-col gap-1 text-xs';
const emptyClass = 'text-muted-foreground text-xs';

export type DetailsPanelMode = 'all' | 'details' | 'plan' | 'tasks';

/**
 * 右面板内容：只读复用现有会话、任务、计划、队列与交互记录。
 * `mode` 让右面板的详情/计划/任务 tab 各自只展示一组事实；数据全部来自 `ChatDisplayState`。
 */
export function DetailsPanel({ state, mode = 'all' }: { state: ChatDisplayState | null; mode?: DetailsPanelMode }) {
  if (!state) return <p className={emptyClass} data-slot="peri-details-panel">会话尚未加载。</p>;
  if (state.readiness !== 'ready')
    return <p className={emptyClass} data-slot="peri-details-panel">会话数据尚未就绪。</p>;

  const turnId = state.activeTurn.turnId;
  const plans = turnId ? state.plans.byTurn[turnId] ?? state.plans.current : state.plans.current;
  const openTasks = state.tasks.filter((task) => !TERMINAL_TASK.has(task.status));
  const queueItems = state.inputQueue?.items ?? [];
  const showDetails = mode === 'all' || mode === 'details';
  const showTasks = mode === 'all' || mode === 'tasks';
  const showPlan = mode === 'all' || mode === 'plan';

  return <div className="flex flex-col gap-5 p-4" data-slot="peri-details-panel" data-mode={mode}>
    {showDetails && (
      <Section title="会话">
        <ul className={listClass}>
          <li>{state.session.title ?? state.chat.title}</li>
          {state.session.model && <li>模型：{state.session.model}</li>}
          {state.session.mode && <li>模式：{state.session.mode}</li>}
          <li>运行状态：{state.activeTurn.status ?? '空闲'}{state.running ? '（执行中）' : ''}</li>
          {state.session.activeTurnInteractionStatus && <li>等待交互：{state.session.activeTurnInteractionStatus}</li>}
          {state.session.legacyIncomplete && <li className="text-muted-foreground">历史由旧记录导入，可能缺少推理与工具细节。</li>}
          {state.executionBlocked && <li className="text-muted-foreground">宿主执行被阻断，请先确认停止状态。</li>}
        </ul>
      </Section>
    )}

    {showTasks && (
      <Section icon={<Timer className="size-3.5" />} title={`任务 ${openTasks.length}/${state.tasks.length}`}>
        {state.tasks.length === 0 ? <p className={emptyClass}>没有任务记录。</p>
          : <ul className={listClass}>
            {state.tasks.map((task) => <li key={task.taskId}>
              <span>{task.title}</span>
              <span className="text-muted-foreground"> {task.status}{task.summary ? ` · ${task.summary}` : ''}</span>
              {task.status.startsWith('unobserved:') && <span className="text-muted-foreground"> （重启后不可观测，未确认仍在运行）</span>}
            </li>)}
          </ul>}
      </Section>
    )}

    {showPlan && (
      <Section icon={<ListChecks className="size-3.5" />} title="计划">
        {plans.length === 0 ? <p className={emptyClass}>当前没有计划。</p>
          : <ul className={listClass}>
            {plans.map((entry, index) => <li key={`${entry.content}:${index}`}>
              <span>{entry.content}</span>
              <span className="text-muted-foreground"> {entry.status}{entry.activeForm ? ` · ${entry.activeForm}` : ''}</span>
            </li>)}
          </ul>}
      </Section>
    )}

    {showDetails && (
      <Section title="输入队列">
        {queueItems.length === 0 ? <p className={emptyClass}>队列为空。</p>
          : <ul className={listClass}>
            {queueItems.map((item) => <li key={item.inputId}>{item.state}</li>)}
          </ul>}
      </Section>
    )}

    {showDetails && (
      <Section icon={<MessageCircleQuestion className="size-3.5" />} title="交互记录">
        {state.interactions.length === 0 ? <p className={emptyClass}>没有交互记录。</p>
          : <ul className={listClass}>
            {state.interactions.map((interaction) => <li key={interaction.id}>
              <span>{interactionName(interaction)}</span>
              <span className="text-muted-foreground"> {interactionStatus(interaction)}</span>
            </li>)}
          </ul>}
      </Section>
    )}
  </div>;
}
