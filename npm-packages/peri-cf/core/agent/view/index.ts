/** 投影与交互视图：把同步事件折叠为展示状态，并处理征询/问答。 */
export { ChatProjection } from "./projection";
export { ChatInteractionQueue } from "./interactions";
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
