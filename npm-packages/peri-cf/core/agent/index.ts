/**
 * Agent 展示模块的公共出口。平台无关：只依赖 SDK `/view` 与 shared DTO；
 * 远程连接、Action 与页面选择由 `frontend/remote` 提供。
 */
export {
  ChatProjection,
  MAX_ASK_USER_QUESTIONS,
  buildAskUserQuestionContent,
  normalizeAskUserQuestionDecision,
  parseAskUserQuestionElicitation,
  type AskUserQuestionContent,
  type AskUserQuestionDecision,
  type AskUserQuestionDraft,
  type AskUserQuestionDrafts,
  type AskUserQuestionElicitation,
  type AskUserQuestionParse,
  type ProjectionSummary,
} from "./view";
export { errorText, logError, publicError } from "./logging";
export type { Chat, ChatSummary, ChatDisplayState } from "../shared";
