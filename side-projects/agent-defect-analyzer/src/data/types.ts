/** Stable, privacy-preserving data contracts for local analysis. */

export type StoredRole = "user" | "assistant" | "system" | "system_reminder" | "tool" | string;
export type MessageOrigin = "own" | "inherited";

export interface ThreadRow {
  id: string;
  title: string | null;
  cwd: string;
  created_at: string;
  updated_at: string;
  message_count: number;
  parent_thread_id: string | null;
  snapshot_at_message_id: string | null;
  hidden: number;
  cancel_policy: string | null;
  config: string | null;
  cached_context: string | null;
  frozen_context: string | null;
  inherited_context: string | null;
  agent_status: string | null;
  context_cache_epoch: number | null;
}

/** Thread metadata used by list/reporting paths; excludes potentially large context blobs. */
export type ThreadSummary = Omit<ThreadRow, "config" | "cached_context" | "frozen_context" | "inherited_context">;

export interface MessageRow {
  message_id: string;
  thread_id: string;
  role: StoredRole;
  content: string;
  truncated: number;
  excluded: number;
  projection: string | null;
  sequence: number;
}

export interface ToolCall {
  id: string;
  name: string;
  arguments: unknown;
  source: "content" | "tool_calls";
  sources: Array<"content" | "tool_calls">;
}

export interface ToolResult {
  id: string;
  content: string;
  /** null means the persisted payload did not state success or failure. */
  isError: boolean | null;
  source: "content" | "message";
  sources: Array<"content" | "message">;
}

export interface NormalizedMessage {
  messageId: string;
  threadId: string;
  sequence: number;
  origin: MessageOrigin;
  role: StoredRole;
  text: string;
  calls: ToolCall[];
  results: ToolResult[];
  excludedFromContext: boolean;
  truncated: boolean;
  isSummary: boolean;
  parseIssues: string[];
}

export interface SchemaCapabilities {
  userVersion: number;
  tables: string[];
  columns: Record<string, string[]>;
  optionalMissing: Record<string, string[]>;
}

export interface ReadOnlySnapshot<T> {
  value: T;
  /** Number of statements evaluated while the snapshot was open. */
  statements: number;
}
