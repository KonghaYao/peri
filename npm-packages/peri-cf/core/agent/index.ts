/**
 * Agent 展示模块的公共出口。平台无关：只依赖 SDK `/view` 与 shared DTO；
 * 远程连接、Action 与页面选择由 `frontend/remote` 提供。
 */
export {
  ChatProjection,
  buildAskUserQuestionContent,
  type AskUserQuestionDecision,
  type AskUserQuestionDraft,
  type AskUserQuestionDrafts,
} from "./view";
export { errorText, logError } from "./logging";
