/** chat 的运行期适配：副本投影与命令转发。 */
export {
  useChat, errorBanners, syncRetryAction,
  type ChatRetryAction,
} from "./useChat";
export { loadPayloadValue } from "./payloadReader";
export {
  buildAskUserQuestionContent,
  type AskUserQuestionDecision, type AskUserQuestionDraft, type AskUserQuestionDrafts,
} from "./askUserQuestion";
export {
  buildElicitationContent, initialElicitationValues, SUPPORTED_PERMISSION_OPTIONS,
  type ElicitationValues, type InteractionAnswerHandler,
} from "./interactionForm";
export type {
  AskUserQuestion, ChatSummary, Entry, Interaction, ChatDisplayState,
  ToolCall, ToolPayloadReader, Turn, ChatLoadState, ChatSendOutcome,
} from "./types";
