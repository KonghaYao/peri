import type { ChatEntry, ToolCallInfo } from '../panel/lib/chat-view';
import type { ControlView, PendingPermission } from '../panel/lib/control-view';
import type { ProjectInfo, ProjectSessionInfo, SessionSummaryInfo } from '../panel/lib/registry-view';
import {
  resetAuthenticatedSession,
  setChatEntries,
  setChatHead,
  setChatStatusSignal,
  setConnectionProblem,
  setConnState,
  setImportableSessions,
  setPermissions,
  setPersistentErrors,
  setProjects,
  setProjectSessions,
  setPromptRecovery,
  setPromptDeliveryReady,
  setRuntimeDocsState,
  setSelectedCid,
  setSelectedSessionId,
} from '../panel/store';
import { installPrincipalRole } from '../panel/lib/auth-state';
import { markMessageDeliveryUncertain, startMessageDelivery } from '../panel/lib/message-delivery';
import { acquireFixtureClock } from './fixture-clock';

export const VISUAL_NOW = Date.parse('2026-08-14T08:00:00+08:00');
export const DEFAULT_VISUAL_SCENARIO = 'conversation';
export const VISUAL_SCENARIO_IDS = ['catalog', 'conversation', 'permission-streaming', 'recovery-errors', 'terminal-readonly'] as const;
export type VisualScenarioId = typeof VISUAL_SCENARIO_IDS[number];
export type FixtureControlMode = 'display-only' | 'locally-interactive' | 'production-gated';

export interface VisualScenarioDefinition {
  id: VisualScenarioId;
  label: string;
  description: string;
  controls: FixtureControlMode;
}

export const visualScenarios: readonly VisualScenarioDefinition[] = [
  { id: 'catalog', label: '目录与快速开始', description: '多项目、空项目和未选会话。', controls: 'locally-interactive' },
  { id: 'conversation', label: '完整对话', description: 'Markdown、工具、资源与长内容。', controls: 'locally-interactive' },
  { id: 'permission-streaming', label: '许可与流式', description: '运行中回合、权限队列与停止入口。', controls: 'production-gated' },
  { id: 'recovery-errors', label: '恢复与错误', description: '历史证据、连接问题和不确定投递。', controls: 'display-only' },
  { id: 'terminal-readonly', label: '终态只读', description: '崩溃运行实例、归档目录与只读权限。', controls: 'locally-interactive' },
] as const;

const projects: ProjectInfo[] = [
  { id: 'project-perihelion', name: 'Perihelion', cwd: '/workspace/perihelion', instanceId: 'local', createdAt: '2026-08-01T02:00:00Z', updatedAt: '2026-08-14T00:00:00Z', archivedAt: null },
  { id: 'project-protocol-lab', name: 'ACP Protocol Lab', cwd: '/workspace/protocol-lab', instanceId: 'local', createdAt: '2026-07-20T02:00:00Z', updatedAt: '2026-08-13T02:00:00Z', archivedAt: null },
  { id: 'project-archive', name: '2025 experiments', cwd: '/workspace/archive', instanceId: 'local', createdAt: '2025-12-01T02:00:00Z', updatedAt: '2026-06-01T02:00:00Z', archivedAt: '2026-07-01T02:00:00Z' },
];

const sessions: ProjectSessionInfo[] = [
  { id: 'session-current', projectId: 'project-perihelion', acpSessionId: 'acp-thread-01J5WORLDCLASSCURRENT', title: '重构 ACP 会话恢复与投影边界', lifecycle: 'ready', updatedAt: '2026-08-14T00:00:00Z', lastOpenedAt: '2026-08-14T00:00:00Z', activeChatId: 'chat-current', archivedAt: null },
  { id: 'session-idle', projectId: 'project-perihelion', acpSessionId: 'acp-thread-01J5IDLESESSION', title: '审计工具调用可读性', lifecycle: 'ready', updatedAt: '2026-08-13T03:00:00Z', lastOpenedAt: '2026-08-13T03:00:00Z', activeChatId: null, archivedAt: null },
  { id: 'session-reconcile', projectId: 'project-perihelion', acpSessionId: null, title: '创建结果需要人工对账', lifecycle: 'reconciliation_required', updatedAt: '2026-08-12T04:00:00Z', lastOpenedAt: null, activeChatId: null, archivedAt: null },
  { id: 'session-protocol', projectId: 'project-protocol-lab', acpSessionId: 'acp-thread-01J5PROTOCOL', title: 'Wire contract compatibility', lifecycle: 'ready', updatedAt: '2026-08-10T05:00:00Z', lastOpenedAt: '2026-08-10T05:00:00Z', activeChatId: null, archivedAt: null },
  { id: 'session-archived', projectId: 'project-perihelion', acpSessionId: 'acp-thread-01J5ARCHIVED', title: '旧版 UI 对照记录', lifecycle: 'ready', updatedAt: '2026-07-04T05:00:00Z', lastOpenedAt: '2026-07-04T05:00:00Z', activeChatId: null, archivedAt: '2026-08-01T05:00:00Z' },
];

const tool = (overrides: Partial<ToolCallInfo>): ToolCallInfo => ({
  toolCallId: 'tool-01J5READ', name: 'Read', status: 'completed', arguments: { path: 'acp-hub/server/src/control/hub.rs' }, result: { lines: 184, note: 'startup restores metadata before accepting connections' }, resultOmitted: false, resultBytes: 118, publicError: null, startedAt: '2026-08-14T00:02:00Z', completedAt: '2026-08-14T00:02:01.480Z', ...overrides,
});

const entries: ChatEntry[] = [
  { id: 'entry-user-1', turnId: 'turn-1', kind: 'message', role: 'user', status: 'completed', authorUserId: 'local-user', sourceCommandId: 'command-user-1', createdAt: '2026-08-14T00:01:00Z', completedAt: '2026-08-14T00:01:00Z', text: '检查 session 恢复链路，确保重启后不会把旧 runtime 当成仍然存活。', reasoning: [], toolCalls: [], resources: [], error: null },
  { id: 'entry-assistant-1', turnId: 'turn-1', kind: 'message', role: 'assistant', status: 'completed', authorUserId: null, sourceCommandId: null, createdAt: '2026-08-14T00:01:02Z', completedAt: '2026-08-14T00:03:30Z', text: '## 结论\n\n恢复模型需要明确区分：\n\n- `project session`：持久入口\n- `ACP session`：可加载的 thread\n- `runtime chat`：一次进程激活\n\n```rust\nif binding.is_stale() {\n    activate_with_session_load(acp_session_id).await?;\n}\n```\n\n完整约束记录在 [architecture.md](https://example.test/architecture)。', reasoning: [{ text: '先核对 metadata authority，再检查 Registry 只读投影与 session/load 时序。', visibility: 'user' }], toolCalls: [tool({}), tool({ toolCallId: 'tool-01J5LARGE', name: 'cargo test', result: null, resultOmitted: true, resultBytes: 2_451_880, arguments: { package: 'acp-hub-server', test: 'restart_restores_project_session' } })], resources: [{ resourceId: 'resource://architecture-contract', mediaType: 'text/markdown', name: '会话恢复架构契约' }], error: null },
  { id: 'entry-user-2', turnId: 'turn-2', kind: 'message', role: 'user', status: 'completed', authorUserId: 'local-user', sourceCommandId: 'command-user-2', createdAt: '2026-08-14T00:05:00Z', completedAt: '2026-08-14T00:05:00Z', text: '继续验证失败路径与公开错误。', reasoning: [], toolCalls: [], resources: [], error: null },
  { id: 'entry-assistant-2', turnId: 'turn-2', kind: 'message', role: 'assistant', status: 'failed', authorUserId: null, sourceCommandId: null, createdAt: '2026-08-14T00:05:02Z', completedAt: '2026-08-14T00:05:20Z', text: '数据库写入失败时不会发送 committed Ack。', reasoning: [], toolCalls: [tool({ toolCallId: 'tool-01J5FAIL', name: 'Finalize metadata', status: 'failed', result: null, resultOmitted: false, publicError: { code: 'METADATA_UNAVAILABLE', message: 'metadata transaction could not be committed' } })], resources: [], error: { code: 'METADATA_UNAVAILABLE', message: '会话元数据暂时不可写，已保留对账状态。' } },
];

const permissions: PendingPermission[] = [
  { permissionId: 'permission-write', turnId: 'turn-stream', toolCallId: 'tool-write', title: '修改工作区文件', description: 'Agent 请求更新 acp-hub Web 的视觉回归场景。', status: 'pending', expiresAt: '2026-08-14T00:30:00Z', decision: null },
  { permissionId: 'permission-command', turnId: 'turn-stream', toolCallId: 'tool-command', title: '运行测试命令', description: '执行 bun run test，不会访问工作区之外的数据。', status: 'pending', expiresAt: '2026-08-14T00:31:00Z', decision: null },
];

const importable: SessionSummaryInfo[] = [
  { sessionId: 'acp-thread-01J5IMPORTA', title: 'Investigate registry replay', status: 'ready', updatedAt: '2026-08-13T04:00:00Z', cwd: '/workspace/perihelion' },
  { sessionId: 'acp-thread-01J5IMPORTB', title: 'Review SQLite migration invariants', status: 'ready', updatedAt: '2026-08-12T04:00:00Z', cwd: '/workspace/perihelion' },
];

function control(active = false): ControlView {
  return {
    chat: { chatId: 'chat-current', title: '重构 ACP 会话恢复与投影边界', status: 'active', activeTurnId: active ? 'turn-stream' : null, createdAt: '2026-08-14T00:00:00Z', updatedAt: '2026-08-14T00:10:00Z' },
    agent: { instanceId: 'local', sessionId: 'acp-thread-01J5WORLDCLASSCURRENT', status: active ? 'running' : 'ready', lastActivityAt: '2026-08-14T00:10:00Z', capabilities: ['loadSession', 'prompt', 'cancel'], model: 'gpt-5.6', effort: 'high', contextWindow: 200_000, contextUsed: 34_500 },
    activeTurn: active ? { turnId: 'turn-stream', turnStatus: 'awaitingPermission', updatedAt: '2026-08-14T00:10:00Z' } : null,
    pendingPermissions: active ? permissions : [],
  };
}

function seedCatalog(): void {
  setProjects(projects);
  setProjectSessions(sessions);
  setImportableSessions(importable);
  setConnState({ text: '本机 server 已连接', kind: 'ok' });
  setPromptDeliveryReady(true);
  setChatStatusSignal({ 'chat-current': 'active' });
}

function selectConversation(currentEntries = entries, head = control(false)): void {
  setSelectedSessionId('session-current');
  setSelectedCid('chat-current');
  setRuntimeDocsState({ chat: true, control: true });
  setChatEntries(currentEntries);
  setChatHead(head);
  setPermissions(head.pendingPermissions);
}

export function resolveVisualScenario(value: string | null | undefined): VisualScenarioId {
  return (VISUAL_SCENARIO_IDS as readonly string[]).includes(value || '') ? value as VisualScenarioId : DEFAULT_VISUAL_SCENARIO;
}

/** Installs static render facts only. Transport actions remain production-gated:
 * this fixture never changes store-private `ready` or `currentCid`. */
export function installVisualScenario(value: string | null | undefined): { scenario: VisualScenarioDefinition; dispose: () => void } {
  const id = resolveVisualScenario(value);
  const releaseClock = acquireFixtureClock(VISUAL_NOW);
  try {
    resetAuthenticatedSession();
    installPrincipalRole(id === 'terminal-readonly' ? 'read-only' : 'full');
    seedCatalog();

  if (id === 'conversation') selectConversation();
  if (id === 'permission-streaming') {
    const streaming = [...entries, { ...entries[1], id: 'entry-stream', turnId: 'turn-stream', status: 'streaming', text: '正在核对权限边界与工具调用顺序…', completedAt: null, reasoning: [], toolCalls: [tool({ toolCallId: 'tool-stream', name: 'Apply patch', status: 'awaitingPermission', result: null, resultOmitted: null })], resources: [], error: null }];
    selectConversation(streaming, control(true));
  }
  if (id === 'recovery-errors') {
    selectConversation(entries.slice(0, 2));
    setConnectionProblem({ code: 4501, title: '与服务器的连接已超时', detail: '页面休眠后心跳中断。持久会话仍保存在 server 中。', action: 'login' });
    setPersistentErrors([{ id: 1, title: '结果尚未确认', detail: '重命名操作已发送，但连接在终态回复前中断。请使用原请求重新确认。', commandId: 'command-uncertain-metadata', retryable: true, retrying: false }]);
    setPromptRecovery({ sessionId: 'session-current', loading: false, truncated: true, evidenceIncomplete: true, error: null, prompts: [
      { commandId: 'command-history-projected', turnId: 'turn-old-1', status: 'projected', createdAt: '2026-08-13T00:00:00Z', updatedAt: '2026-08-13T00:01:00Z' },
      { commandId: 'command-history-unknown', turnId: 'turn-old-2', status: 'delivery_unknown', createdAt: '2026-08-12T00:00:00Z', updatedAt: '2026-08-12T00:01:00Z' },
    ] });
    startMessageDelivery('command-message-uncertain', '请继续完成恢复审计。', 'session-current', 'chat-current');
    markMessageDeliveryUncertain('command-message-uncertain');
  }
  if (id === 'terminal-readonly') {
    selectConversation(entries.slice(0, 2), { ...control(false), chat: { ...control(false).chat!, status: 'crashed' }, agent: { ...control(false).agent!, status: 'offline' } });
    setChatStatusSignal({ 'chat-current': 'crashed' });
  }

    const scenario = visualScenarios.find((item) => item.id === id)!;
    let disposed = false;
    return { scenario, dispose: () => {
      if (disposed) return;
      disposed = true;
      try {
        resetAuthenticatedSession();
      } finally {
        releaseClock();
      }
    } };
  } catch (error) {
    releaseClock();
    throw error;
  }
}
