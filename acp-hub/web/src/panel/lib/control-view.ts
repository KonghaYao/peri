import * as Y from 'yjs';
import { asArray, asMap, getNum, getStr } from './yjs-values';

export interface ChatHeadInfo {
  chatId: string;
  title: string | null;
  status: string | null;
  activeTurnId: string | null;
  createdAt: string | null;
  updatedAt: string | null;
}
export interface AgentInfo {
  instanceId: string | null;
  sessionId: string | null;
  status: string | null;
  lastActivityAt: string | null;
  capabilities: unknown[];
  model: string | null;
  effort: string | null;
  contextWindow: number | null;
  contextUsed: number | null;
}
export interface ActiveTurnInfo { turnId: string | null; turnStatus: string | null; updatedAt: string | null }
export interface PendingPermission {
  permissionId: string | null;
  turnId: string | null;
  toolCallId: string | null;
  title: string | null;
  description: string | null;
  status: string | null;
  expiresAt: string | null;
  decision: string | null;
}
export interface ControlView {
  chat: ChatHeadInfo | null;
  agent: AgentInfo | null;
  activeTurn: ActiveTurnInfo | null;
  pendingPermissions: PendingPermission[];
}

/** Read-only browser projection of one session:{id} control document. */
export function renderControl(doc: Y.Doc): ControlView {
  const root = doc.getMap<unknown>('root');
  const result: ControlView = { chat: null, agent: null, activeTurn: null, pendingPermissions: [] };
  const session = asMap(root.get('session'));
  if (session) {
    result.chat = {
      chatId: getStr(session, 'session_id') || '',
      title: getStr(session, 'title'),
      status: getStr(session, 'status'),
      activeTurnId: getStr(session, 'active_turn_id'),
      createdAt: getStr(session, 'created_at'),
      updatedAt: getStr(session, 'updated_at'),
    };
    const turnId = getStr(session, 'active_turn_id');
    const turnStatus = getStr(session, 'active_turn_status');
    if (turnId || turnStatus) result.activeTurn = {
      turnId,
      turnStatus,
      updatedAt: getStr(session, 'active_turn_updated_at'),
    };
  }

  const agent = asMap(root.get('agent'));
  if (agent) result.agent = {
    instanceId: getStr(agent, 'instance_id'),
    sessionId: getStr(agent, 'acp_session_id') ?? getStr(agent, 'session_id'),
    status: getStr(agent, 'status'),
    lastActivityAt: getStr(agent, 'last_activity_at'),
    capabilities: asArray(agent.get('capabilities'))?.toArray() ?? [],
    model: getStr(agent, 'model'),
    effort: getStr(agent, 'effort'),
    contextWindow: getNum(agent, 'context_window'),
    contextUsed: getNum(agent, 'context_used'),
  };

  asMap(root.get('pending_permissions'))?.forEach((value) => {
    const permission = asMap(value);
    if (!permission || getStr(permission, 'status') !== 'pending') return;
    result.pendingPermissions.push({
      permissionId: getStr(permission, 'permission_id'),
      turnId: getStr(permission, 'turn_id'),
      toolCallId: getStr(permission, 'tool_call_id'),
      title: getStr(permission, 'title'),
      description: getStr(permission, 'description'),
      status: getStr(permission, 'status'),
      expiresAt: getStr(permission, 'expires_at'),
      decision: getStr(permission, 'decision'),
    });
  });
  result.pendingPermissions.sort((left, right) => {
    const leftExpiry = Date.parse(left.expiresAt || '');
    const rightExpiry = Date.parse(right.expiresAt || '');
    const expiryOrder = (Number.isFinite(leftExpiry) ? leftExpiry : Number.POSITIVE_INFINITY)
      - (Number.isFinite(rightExpiry) ? rightExpiry : Number.POSITIVE_INFINITY);
    if (expiryOrder) return expiryOrder;
    return (left.permissionId || '').localeCompare(right.permissionId || '');
  });
  return result;
}
