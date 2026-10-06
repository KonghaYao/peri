import { readSessionView } from "@peri-code/sdk/view";
import type { Doc } from "yjs";
import { syncMetadataSchema, syncStateSchema, type SyncState } from "./sync";

export function readSyncState(chat: Doc, session: Doc): SyncState {
  const metadata = syncMetadataSchema.parse(session.getMap("peri-cf").get("state"));
  const view = readSessionView(chat, session);
  const messages = view.entries.map((entry) => {
    const fields = metadata.entryMetadata[entry.entryId];
    const status = entry.role === "user" ? "completed" : fields?.error ? "error"
      : ["completed", "cancelled", "error"].includes(entry.status) ? entry.status : "running";
    return {
      id: fields?.id ?? entry.entryId, role: entry.role,
      content: entry.blocks.flatMap((block) => block.type === "text" ? [block.text] : []).join(""),
      createdAt: fields?.createdAt ?? metadata.chat.updatedAt, status,
      ...(fields?.error ? { error: fields.error } : {}),
    };
  });
  return syncStateSchema.parse({ ...metadata, messages });
}
