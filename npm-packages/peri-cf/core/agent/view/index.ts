/** 投影与交互视图：只读副本投影与 AskUserQuestion 展示适配。 */
export { ChatProjection, type ProjectionSummary } from "./projection";
export {
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
} from "./askUserQuestion";
