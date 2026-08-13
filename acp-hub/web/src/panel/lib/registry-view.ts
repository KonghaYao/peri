import * as Y from 'yjs';
import { asMap, getStr } from './yjs-values';

export interface InstanceInfo {
  id: string;
  hostname: string | null;
  status: string | null;
  tokenId: string | null;
  registeredAt: string | null;
  lastHeartbeat: string | null;
  chatCount: unknown;
}

export interface ChatInfo {
  id: string;
  instanceId: string | null;
  title: string | null;
  status: string | null;
  gap: unknown;
  updatedAt: string | null;
  cwd: string | null;
  workspaceId: string | null;
}

export interface WorkspaceInfo {
  id: string;
  name: string;
  cwd: string;
  createdAt: string | null;
  updatedAt: string | null;
}

export interface ProjectInfo {
  id: string;
  name: string;
  cwd: string;
  instanceId: string;
  createdAt: string | null;
  updatedAt: string | null;
  archivedAt: string | null;
}

export interface ProjectSessionInfo {
  id: string;
  projectId: string;
  acpSessionId: string | null;
  title: string;
  lifecycle: string;
  updatedAt: string | null;
  lastOpenedAt: string | null;
  activeChatId: string | null;
  archivedAt?: string | null;
}

export interface SessionSummaryInfo {
  sessionId: string;
  title: string | null;
  status: string | null;
  updatedAt: string | null;
  cwd: string | null;
  boundChatId?: string | null;
}

export interface RegistryView {
  instances: InstanceInfo[];
  chats: ChatInfo[];
  sessions: SessionSummaryInfo[];
  workspaces: WorkspaceInfo[];
  projects: ProjectInfo[];
  projectSessions: ProjectSessionInfo[];
  globalStatus: string;
  schemaVersion: unknown;
  projectionVersion: unknown;
}

/** Read-only browser projection of the hub:registry document. */
export function renderRegistry(doc: Y.Doc): RegistryView {
  const root = doc.getMap<unknown>('root');
  const instances: InstanceInfo[] = [];
  const chats: ChatInfo[] = [];
  const sessions: SessionSummaryInfo[] = [];
  const workspaces: WorkspaceInfo[] = [];
  const projects: ProjectInfo[] = [];
  const projectSessions: ProjectSessionInfo[] = [];

  asMap(root.get('instances'))?.forEach((value, id) => {
    const map = asMap(value);
    instances.push({
      id,
      hostname: getStr(map, 'hostname'),
      status: getStr(map, 'status'),
      tokenId: getStr(map, 'token_id'),
      registeredAt: getStr(map, 'registered_at'),
      lastHeartbeat: getStr(map, 'last_heartbeat'),
      chatCount: map?.get('chat_count') ?? null,
    });
  });

  asMap(root.get('chats'))?.forEach((value, id) => {
    const map = asMap(value);
    chats.push({
      id,
      instanceId: getStr(map, 'instance_id'),
      title: getStr(map, 'title'),
      status: getStr(map, 'status'),
      gap: map?.get('gap') ?? null,
      updatedAt: getStr(map, 'updated_at'),
      cwd: getStr(map, 'cwd'),
      workspaceId: getStr(map, 'workspace_id'),
    });
  });

  asMap(root.get('workspaces'))?.forEach((value, id) => {
    const map = asMap(value);
    workspaces.push({
      id,
      name: getStr(map, 'name') || '',
      cwd: getStr(map, 'cwd') || '',
      createdAt: getStr(map, 'created_at'),
      updatedAt: getStr(map, 'updated_at'),
    });
  });
  workspaces.sort((left, right) => String(right.createdAt || '').localeCompare(String(left.createdAt || '')));

  asMap(root.get('projects'))?.forEach((value, id) => {
    const map = asMap(value);
    if (!map) return;
    projects.push({
      id,
      name: getStr(map, 'name') || id,
      cwd: getStr(map, 'cwd') || '',
      instanceId: getStr(map, 'instance_id') || '',
      createdAt: getStr(map, 'created_at'),
      updatedAt: getStr(map, 'updated_at'),
      archivedAt: getStr(map, 'archived_at'),
    });
  });
  projects.sort((left, right) => String(right.updatedAt || '').localeCompare(String(left.updatedAt || '')));

  asMap(root.get('project_sessions'))?.forEach((value, id) => {
    const map = asMap(value);
    if (!map) return;
    projectSessions.push({
      id,
      projectId: getStr(map, 'project_id') || '',
      acpSessionId: getStr(map, 'acp_session_id'),
      title: getStr(map, 'title') || '新对话',
      lifecycle: getStr(map, 'lifecycle') || 'pending',
      updatedAt: getStr(map, 'updated_at'),
      lastOpenedAt: getStr(map, 'last_opened_at'),
      activeChatId: getStr(map, 'active_chat_id'),
      archivedAt: getStr(map, 'archived_at'),
    });
  });
  projectSessions.sort((left, right) => String(right.lastOpenedAt || right.updatedAt || '').localeCompare(String(left.lastOpenedAt || left.updatedAt || '')));

  const seen = new Set<string>();
  asMap(root.get('sessions'))?.forEach((value) => {
    const map = asMap(value);
    const sessionId = getStr(map, 'session_id') || '';
    if (!map || !sessionId || seen.has(sessionId)) return;
    seen.add(sessionId);
    sessions.push({
      sessionId,
      title: getStr(map, 'title'),
      status: getStr(map, 'status'),
      updatedAt: getStr(map, 'updated_at'),
      cwd: getStr(map, 'cwd'),
    });
  });
  sessions.sort((left, right) => String(right.updatedAt || '').localeCompare(String(left.updatedAt || '')));

  return {
    instances,
    chats,
    sessions,
    workspaces,
    projects,
    projectSessions,
    globalStatus: getStr(asMap(root.get('global')), 'status') || 'unknown',
    schemaVersion: root.get('schema_version'),
    projectionVersion: root.get('projection_version'),
  };
}
