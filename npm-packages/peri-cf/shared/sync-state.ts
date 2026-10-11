import type { SessionView } from "@peri-code/sdk/view";
import { syncStateSchema, type SyncMetadata, type SyncState } from "./sync";

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
