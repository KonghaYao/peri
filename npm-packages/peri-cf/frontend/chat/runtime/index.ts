/** chat 的运行期适配：副本投影到 assistant-ui、查询缓存与命令转发。 */
export {
  useChat, errorBanners, syncRetryAction,
  type ChatRetryAction, type ChatErrorBanner, type ChatErrorBanners,
} from "./useChat";
export {
  usePeriAssistantRuntime, usePeriRuntime, mergeAssistantTurns, UNKNOWN_BLOCK_PART_NAME,
  type PeriRuntimeSource,
} from "./adapter";
export {
  createChatQueries, listRefetchInterval, historyKey,
  LIST_POLL_INTERVAL_MS, LIST_POLL_IDLE_INTERVAL_MS, LIST_POLL_ACTIVITY_MS,
  type ChatReadApi, type ListPollingOptions,
} from "./queries";
export {
  chatPayloadReader, unavailablePayloadReader, loadPayloadText, loadPayloadValue,
  type PayloadSource, type PayloadTextResult, type PayloadValueResult,
} from "./payloadReader";
export {
  buildElicitationContent, initialElicitationValues, SUPPORTED_PERMISSION_OPTIONS,
  type ElicitationValues, type InteractionAnswerHandler,
} from "./interactionForm";
export type {
  AskUserQuestion, Chat, ChatSummary, Entry, EntryBlock, Interaction, InteractionAnswer, InteractionFieldView,
  PlanEntry, PayloadReadError, Phase, SessionInfo, ChatReadiness, ChatDisplayState, Task, ToolCall,
  ToolPayloadReadRef, ToolPayloadReader, ToolPayloadResponse, Turn, ChatLoadState,
} from "./types";
