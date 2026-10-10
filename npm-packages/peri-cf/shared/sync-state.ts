import { readSessionView, type SessionView } from "@peri-code/sdk/view";
import type { Doc } from "yjs";
import { syncMetadataSchema, syncStateSchema, type SyncMetadata, type SyncState } from "./sync";

/** 文档对完整但版本不受支持：必须升级应用，不得当作空历史继续。 */
export class UnsupportedSchemaError extends Error {
  constructor(readonly schemaVersions: { chat: number | null; session: number | null }) {
    super('会话使用了不受支持的数据版本，请刷新页面或升级应用后再继续。');
    this.name = 'UnsupportedSchemaError';
  }
}

export function readSyncState(chat: Doc, session: Doc): SyncState {
  const metadata = syncMetadataSchema.parse(session.getMap("peri-cf").get("state"));
  const view = readSessionView(chat, session);
  if (view.readiness === "unsupported") throw new UnsupportedSchemaError(view.schemaVersions);
  return materializeSyncState(view, metadata);
}

/** 纯展示适配：只复制 SDK view 已确认的公开对象，不补造身份、时间或错误。 */
export function materializeSyncState(view: SessionView, metadata: SyncMetadata): SyncState {
  const known = view.readiness === "ready";
  return syncStateSchema.parse({
    ...metadata,
    readiness: view.readiness,
    entries: known ? view.entries : [],
    toolCalls: known ? view.toolCalls : [],
    turns: known ? view.turns : [],
    activeTurn: known ? view.activeTurn : { turnId: null, status: null },
    session: known ? view.session : {},
    tasks: known ? view.tasks : [],
    plans: { current: known ? view.plan : [], byTurn: known ? view.plansByTurn : {} },
    interactions: known ? view.interactions : [],
    pendingInteractions: known ? view.pendingInteractions : [],
    inputQueue: known ? view.inputQueue : null,
  });
}
