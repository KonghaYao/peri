/** chat 的运行期适配：投影到 assistant-ui 的单向转换、查询缓存与命令转发。 */
export {
  useChat, errorBanners, syncRetryAction,
  type ChatRetryAction, type ChatErrorBanner, type ChatErrorBanners,
} from "./useChat";
export {
  usePeriAssistantRuntime, usePeriRuntime, mergeAssistantTurns, UNKNOWN_BLOCK_PART_NAME,
  type PeriRuntimeSource,
} from "./adapter";
export {
  createChatQueries, summaryFacts, listRefetchInterval, mergeChatSummary,
  LIST_POLL_INTERVAL_MS, LIST_POLL_IDLE_INTERVAL_MS, LIST_POLL_ACTIVITY_MS,
  type ChatLiveFacts, type ChatReadApi, type ListPollingOptions,
} from "./queries";
export {
  commandPayloadReader, loadPayloadText, loadPayloadValue,
  type PayloadCommandSource, type PayloadTextResult, type PayloadValueResult,
} from "./payloadReader";
export {
  buildElicitationContent, initialElicitationValues, SUPPORTED_PERMISSION_OPTIONS,
  type ElicitationValues, type InteractionAnswerHandler,
} from "./interactionForm";
export type {
  AskUserQuestion, Chat, ChatSummary, Entry, EntryBlock, Interaction, InteractionAnswer, InteractionFieldView,
  PlanEntry, PayloadReadError, Phase, SessionInfo, SyncReadiness, SyncState, Task, ToolCall,
  ToolPayloadReadRef, ToolPayloadReader, ToolPayloadResponse, Turn,
} from "./types";
export type { ChatLoadState } from '../../remote';
