/**
 * Compatibility barrel for projection consumers.
 *
 * Each server document kind has an independent reader; this file intentionally
 * contains no projection logic so Registry, chat and control schema changes
 * cannot silently couple through one implementation module.
 */
export { DocStore } from './doc-store';
export { renderRegistry } from './registry-view';
export type {
  ChatInfo,
  InstanceInfo,
  ProjectInfo,
  ProjectSessionInfo,
  RegistryView,
  SessionSummaryInfo,
  WorkspaceInfo,
} from './registry-view';
export { renderChat } from './chat-view';
export type {
  ChatEntry,
  ChatView,
  ReasoningBlock,
  ResourceInfo,
  ToolCallInfo,
} from './chat-view';
export { renderControl } from './control-view';
export type {
  ActiveTurnInfo,
  AgentInfo,
  ChatHeadInfo,
  ControlView,
  PendingPermission,
} from './control-view';
