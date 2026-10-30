import type { AskUserQuestionDTO as AskUserQuestion } from "../../remote";

/**
 * AskUserQuestion 的回答内容契约：答案按问题 ID 回传，单选题为 string，
 * 多选题为 string[]；展示层由 `buildAskUserQuestionContent` 构造该形状。
 */
export type AskUserQuestionDraft = { selected: readonly string[]; custom: string };
export type AskUserQuestionDrafts = Record<string, AskUserQuestionDraft>;

type AskUserQuestionContent = Record<string, string | string[]>;

export type AskUserQuestionDecision =
  | { action: "accept"; content: AskUserQuestionContent }
  | { action: "decline" | "cancel" };

/**
 * 草稿 → accept content：单选优先自定义文本，多选拼接选中项与非空自定义文本，
 * 无选项只接受自定义文本；任一题未回答则返回 missing。
 */
export function buildAskUserQuestionContent(
  questions: readonly AskUserQuestion[],
  drafts: AskUserQuestionDrafts,
): { ok: true; content: AskUserQuestionContent } | { ok: false; missing: string[] } {
  const content: AskUserQuestionContent = {};
  const missing: string[] = [];
  for (const question of questions) {
    const draft = drafts[question.id] ?? { selected: [], custom: "" };
    const allowed = new Set(question.options.map((option) => option.value));
    const selected = draft.selected.filter((value) => allowed.has(value));
    const custom = draft.custom.trim();
    if (question.multiSelect) {
      const values = [...selected];
      if (custom.length > 0 && !values.includes(custom)) values.push(custom);
      if (values.length === 0) missing.push(question.id);
      else content[question.id] = values;
    } else if (custom.length > 0) {
      content[question.id] = custom;
    } else if (selected.length > 0) {
      content[question.id] = selected[0];
    } else {
      missing.push(question.id);
    }
  }
  return missing.length > 0 ? { ok: false, missing } : { ok: true, content };
}
