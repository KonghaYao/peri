import type {
  AskUserQuestionDTO as AskUserQuestion, AskUserQuestionOptionDTO as AskUserQuestionOption,
} from "../../shared";

/**
 * AskUserQuestion 的 ACP wire 契约（`transport_broker.rs`）：一次 `elicitation/create`（mode=form），
 * 属性键 `ask_user_question_<index>`；title=header，description=question。
 * 单选/无选项为 string（后者无 oneOf/anyOf），多选为 array；答案按同一属性键回传。
 */
const QUESTION_KEY = /^ask_user_question_(\d+)$/;
export const MAX_ASK_USER_QUESTIONS = 4;

export type AskUserQuestionElicitation = {
  message: string;
  requestedSchema: Record<string, unknown>;
  questions: AskUserQuestion[];
};

export type AskUserQuestionParse =
  | { kind: "not-ask-user" }
  | { kind: "invalid"; reason: string }
  | { kind: "ask-user"; request: AskUserQuestionElicitation };

export type AskUserQuestionDraft = { selected: readonly string[]; custom: string };
export type AskUserQuestionDrafts = Record<string, AskUserQuestionDraft>;

export type AskUserQuestionContent = Record<string, string | string[]>;

export type AskUserQuestionDecision =
  | { action: "accept"; content: AskUserQuestionContent }
  | { action: "decline" | "cancel" };

function record(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function parseOptions(value: unknown): AskUserQuestionOption[] | null {
  if (!Array.isArray(value) || value.length === 0) return null;
  const options: AskUserQuestionOption[] = [];
  for (const raw of value) {
    const option = record(raw);
    if (!option) return null;
    const optionValue = option.const;
    const title = option.title;
    const description = option.description;
    if (typeof optionValue !== "string" || optionValue.length === 0) return null;
    if (typeof title !== "string") return null;
    if (description !== undefined && typeof description !== "string") return null;
    options.push({
      value: optionValue,
      label: title.length > 0 ? title : optionValue,
      ...(description ? { description } : {}),
    });
  }
  return options;
}

function parseQuestion(key: string, raw: unknown): AskUserQuestion | string {
  const property = record(raw);
  if (!property) return `${key}: property is not an object`;
  if (typeof property.title !== "string") return `${key}: missing title`;
  if (typeof property.description !== "string" || property.description.trim().length === 0)
    return `${key}: missing description`;
  const header = property.title;
  const question = property.description;
  if (property.type === "string") {
    if (property.enum !== undefined) return `${key}: unsupported enum`;
    if (property.oneOf === undefined) {
      return { id: key, header, question, multiSelect: false, options: [] };
    }
    const options = parseOptions(property.oneOf);
    if (!options) return `${key}: invalid oneOf`;
    return { id: key, header, question, multiSelect: false, options };
  }
  if (property.type === "array") {
    const items = record(property.items);
    if (!items || items.type !== "string") return `${key}: invalid array items`;
    const options = parseOptions(items.anyOf);
    if (!options) return `${key}: invalid items.anyOf`;
    return { id: key, header, question, multiSelect: true, options };
  }
  return `${key}: unsupported type`;
}

/**
 * 识别并解析 AskUserQuestion payload：仅当全部属性键匹配 `ask_user_question_<index>` 时认领，
 * 否则交回 SDK 的通用 elicitation；结构不合契约返回 `invalid`，调用方必须按拒绝结算。
 */
export function parseAskUserQuestionElicitation(params: unknown): AskUserQuestionParse {
  const request = record(params);
  if (!request || request.mode !== "form") return { kind: "not-ask-user" };
  const schema = record(request.requestedSchema);
  const properties = schema ? record(schema.properties) : null;
  if (!schema || !properties) return { kind: "not-ask-user" };
  const entries = Object.entries(properties);
  const matched = entries.filter(([key]) => QUESTION_KEY.test(key));
  if (matched.length === 0) return { kind: "not-ask-user" };
  if (matched.length !== entries.length) return { kind: "invalid", reason: "mixed property keys" };
  if (entries.length > MAX_ASK_USER_QUESTIONS) return { kind: "invalid", reason: "too many questions" };
  const questions: AskUserQuestion[] = [];
  const indexes = new Set<number>();
  for (const [key, raw] of entries.sort(([left], [right]) => {
    const leftIndex = Number(QUESTION_KEY.exec(left)?.[1] ?? 0);
    const rightIndex = Number(QUESTION_KEY.exec(right)?.[1] ?? 0);
    return leftIndex - rightIndex;
  })) {
    const index = Number(QUESTION_KEY.exec(key)?.[1]);
    if (indexes.has(index)) return { kind: "invalid", reason: `duplicate question index ${index}` };
    indexes.add(index);
    const parsed = parseQuestion(key, raw);
    if (typeof parsed === "string") return { kind: "invalid", reason: parsed };
    questions.push(parsed);
  }
  if (questions.length === 0 || questions.length > MAX_ASK_USER_QUESTIONS)
    return { kind: "invalid", reason: "invalid question count" };
  return {
    kind: "ask-user",
    request: {
      message: typeof request.message === "string" ? request.message : "",
      requestedSchema: schema,
      questions,
    },
  };
}

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
      if (custom.length > 0) values.push(custom);
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

/** 回答通道校验：键与类型必须精确匹配，否则回落 decline，不把畸形内容当成 accept。 */
export function normalizeAskUserQuestionDecision(
  value: unknown,
  questions: readonly AskUserQuestion[],
): AskUserQuestionDecision {
  const decision = record(value);
  if (!decision) return { action: "decline" };
  if (decision.action === "decline" || decision.action === "cancel") return { action: decision.action };
  if (decision.action !== "accept") return { action: "decline" };
  const content = record(decision.content);
  if (!content) return { action: "decline" };
  const declared = new Set(questions.map((question) => question.id));
  if (Object.keys(content).some((key) => !declared.has(key))) return { action: "decline" };
  const normalized: AskUserQuestionContent = {};
  for (const question of questions) {
    const item = content[question.id];
    if (question.multiSelect) {
      if (!Array.isArray(item) || item.some((entry) => typeof entry !== "string"))
        return { action: "decline" };
      normalized[question.id] = [...item] as string[];
    } else {
      if (typeof item !== "string") return { action: "decline" };
      normalized[question.id] = item;
    }
  }
  return { action: "accept", content: normalized };
}
