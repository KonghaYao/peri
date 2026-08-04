/**
 * LLM-as-Judge 模块
 *
 * 将终端 snapshot（ANSI 原始文本）和检查清单发给 OpenAI，
 * 返回结构化判断结果。
 *
 * 配置：通过环境变量 OPENAI_API_KEY / OPENAI_BASE_URL / JUDGE_MODEL
 */
import OpenAI from "openai";

// ---- 类型 ----

export interface JudgeInput {
  /** ANSI 原始文本 */
  ansiRaw: string;
  /** 中文检查清单 */
  criteria: string[];
}

export interface JudgeCheck {
  criterion: string;
  pass: boolean;
  detail: string;
}

export interface JudgeResult {
  pass: boolean;
  checks: JudgeCheck[];
  model: string;
  usage: { prompt_tokens: number; completion_tokens: number };
  duration_ms: number;
}

// ---- OpenAI 客户端 ----

function getClient(): OpenAI {
  const apiKey = process.env.OPENAI_API_KEY;
  if (!apiKey) {
    throw new Error("缺少 OPENAI_API_KEY 环境变量，无法初始化 LLM judge");
  }
  return new OpenAI({
    apiKey,
    baseURL: process.env.OPENAI_BASE_URL || undefined,
  });
}

const JUDGE_MODEL = process.env.JUDGE_MODEL || "gpt-4.1-mini";

// ---- System Prompt ----

const SYSTEM_PROMPT = `你是一个终端 UI 测试的自动化评判者。你会收到一段终端屏幕的原始内容（包含 ANSI 转义序列），
以及一份中文检查清单。请逐条判断屏幕是否满足每条要求。

## ANSI 转义序列速查
- \\x1b[<n>m: SGR 样式（如 \\x1b[32m=绿色文字，\\x1b[1m=粗体，\\x1b[0m=复位）
- \\x1b[H / \\x1b[2J: 光标复位/清屏
- \\x1b[<row>;<col>H: 光标定位
- 其他 CSI 序列: \\x1b[<params><letter>

## 评判原则
1. 忽略 ANSI 转义序列本身，关注它们表达的样式信息
2. 如果检查项要求"包含某文本"，在屏幕文本中模糊搜索即可（不区分 ANSI 干扰）
3. 如果检查项要求"某个区域显示"，根据布局常识判断位置（顶部=前几行、底部=末几行）
4. 负向断言（"不应出现 X"、"已关闭"、"不再显示 X"）：只要屏幕中未发现 X 即判 pass=true，不需要额外证据。例如"面板已关闭"的判据就是屏幕上找不到该面板的任何元素（Tab 标签、列表、面板布局），此时界面回到欢迎页/主聊天界面即为通过。
5. 可接受状态集合：检查项中若列出多个可接受状态（如"空白或仅显示欢迎页/logo"、"如 A 或 B"），屏幕内容属于其中任一状态即判 pass=true。
6. 结论必须与 detail 一致：若 detail 描述的事实表明屏幕满足检查项（如"未找到面板元素"、"屏幕已是欢迎页"），则 pass 必须是 true；若判断失败，detail 必须指出屏幕实际内容与要求的明确差异。发现矛盾时以事实为准重新判断。
7. detail 字段用中文简述判断依据，失败时必须说明原因

## 输出格式
严格返回 JSON（不要 markdown 代码块包裹）。checks 的数量、顺序和 id 必须与输入检查清单完全一致；每项仅按对应 id 判断，不要回显 criterion:
{
  "checks": [
    { "id": 1, "pass": true, "detail": "判断依据" }
  ]
}`;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function failedChecks(criteria: string[], detail: string): JudgeCheck[] {
  return criteria.map((criterion) => ({
    criterion,
    pass: false,
    detail,
  }));
}

const VALID_JSON_ESCAPES = new Set(['"', "\\", "/", "b", "f", "n", "r", "t"]);

function isHexDigit(ch: string | undefined): boolean {
  return ch !== undefined && /[0-9a-fA-F]/.test(ch);
}

/**
 * 修复 Judge 响应 JSON 中常见的非法转义序列。
 *
 * 模型可能把字面反斜杠直接写进字符串（如 `C:\Users`、`\x1b[32m`），
 * JSON.parse 因此抛出 "Bad escaped character in JSON"。
 * 这里把非法的 `\X`（X 不是合法转义字符，或 `\u` 后不是 4 位十六进制）
 * 转义为 `\\X`。合法 JSON 中反斜杠只出现在字符串内且必须紧跟合法转义字符，
 * 因此该变换对合法 JSON 是幂等的，可安全用于任意输入。
 */
function repairJsonEscapes(raw: string): string {
  let result = "";
  let i = 0;
  while (i < raw.length) {
    const ch = raw[i];
    if (ch !== "\\") {
      result += ch;
      i += 1;
      continue;
    }

    const next = raw[i + 1];
    if (
      next === "u" &&
      [raw[i + 2], raw[i + 3], raw[i + 4], raw[i + 5]].every(isHexDigit)
    ) {
      result += raw.slice(i, i + 6);
      i += 6;
      continue;
    }
    if (next !== undefined && VALID_JSON_ESCAPES.has(next)) {
      result += ch + next;
      i += 2;
      continue;
    }

    // 孤立反斜杠：`\X` -> `\\X`（字符串末尾的 `\` 也双写）
    result += "\\\\";
    if (next !== undefined) {
      result += next;
      i += 2;
    } else {
      i += 1;
    }
  }
  return result;
}

/**
 * 解析 Judge 响应 JSON：先按原样解析，失败时修复常见转义问题后重试。
 *
 * @returns parsed 为 null 时表示修复后仍无法解析，error 为首次解析的错误信息。
 */
function parseJudgeJson(
  rawResponse: string,
): { parsed: unknown | null; error: string } {
  // 清理 BOM 和不可见字符；部分模型（mimo 等）在 JSON 前可能输出控制字符。
  const cleaned = rawResponse.trim().replace(/^\uFEFF/, "");
  try {
    return { parsed: JSON.parse(cleaned), error: "" };
  } catch (error) {
    const reason = error instanceof Error ? error.message : "未知错误";
    try {
      return { parsed: JSON.parse(repairJsonEscapes(cleaned)), error: "" };
    } catch {
      return { parsed: null, error: reason };
    }
  }
}

/**
 * 严格校验已解析的 Judge 响应；任一校验不通过即抛出错误。
 */
function validateJudgeChecks(parsed: unknown, criteria: string[]): JudgeCheck[] {
  if (!isRecord(parsed) || !Array.isArray(parsed.checks)) {
    throw new Error("缺少 checks 数组");
  }

  if (parsed.checks.length !== criteria.length) {
    throw new Error(
      `checks 数量不匹配：期望 ${criteria.length}，收到 ${parsed.checks.length}`,
    );
  }

  return parsed.checks.map((candidate, index) => {
    if (!isRecord(candidate)) {
      throw new Error(`第 ${index + 1} 个 check 不是对象`);
    }

    const id = candidate.id;
    const pass = candidate.pass;
    const detail = candidate.detail;

    if (id !== index + 1) {
      throw new Error(`第 ${index + 1} 个 check 的 id 不匹配`);
    }

    if (typeof pass !== "boolean") {
      throw new Error(`第 ${index + 1} 个 check 的 pass 不是布尔值`);
    }

    if (typeof detail !== "string" || detail.trim().length === 0) {
      throw new Error(`第 ${index + 1} 个 check 的 detail 不是非空字符串`);
    }

    return {
      criterion: criteria[index],
      pass,
      detail,
    };
  });
}

/**
 * 解析并严格校验 Judge 响应。
 *
 * Judge 必须逐项返回与输入 criteria 一一对应的布尔结论；缺项、空数组、
 * 错误顺序、非布尔 pass 或无效 JSON 都视为失败，不能让 E2E 静默通过。
 * 解析前会先修复常见转义问题（如普通字符前的孤立反斜杠），修复后仍无效才判失败。
 */
export function parseJudgeChecks(
  rawResponse: string,
  criteria: string[],
): JudgeCheck[] {
  const { parsed, error } = parseJudgeJson(rawResponse);
  if (parsed === null) {
    return failedChecks(criteria, `Judge 返回无效 JSON 响应（${error}）`);
  }

  try {
    return validateJudgeChecks(parsed, criteria);
  } catch (validationError) {
    const reason =
      validationError instanceof Error ? validationError.message : "未知错误";
    return failedChecks(criteria, `Judge 返回无效 JSON 响应（${reason}）`);
  }
}

// ---- 主函数 ----

/**
 * LLM judge：传入 snapshot ANSI 和检查清单，返回结构化判断
 */
export async function judge(input: JudgeInput): Promise<JudgeResult> {
  const client = getClient();
  const startTime = Date.now();

  const criteriaText = input.criteria
    .map((criterion, index) => `[id: ${index + 1}] ${criterion}`)
    .join("\n");

  const userMessage = `## 检查清单
${criteriaText}

## 终端屏幕内容（ANSI 原始）
\`\`\`
${input.ansiRaw}
\`\`\``;

  const MAX_JUDGE_ATTEMPTS = 2;

  let lastUsage: JudgeResult["usage"] = { prompt_tokens: 0, completion_tokens: 0 };
  let checks: JudgeCheck[] | null = null;

  for (let attempt = 1; attempt <= MAX_JUDGE_ATTEMPTS; attempt++) {
    const response = await client.chat.completions.create({
      model: JUDGE_MODEL,
      messages: [
        { role: "system", content: SYSTEM_PROMPT },
        { role: "user", content: userMessage },
      ],
      response_format: { type: "json_object" },
      temperature: 0,
      max_tokens: 2000,
    });

    const rawResponse = response.choices[0]?.message?.content || "";
    if (response.usage) {
      lastUsage = {
        prompt_tokens: response.usage.prompt_tokens,
        completion_tokens: response.usage.completion_tokens,
      };
    }

    const { parsed, error } = parseJudgeJson(rawResponse);
    if (parsed !== null) {
      checks = parseJudgeChecks(rawResponse, input.criteria);
      break;
    }

    // JSON 语法无效（修复后仍无法解析）：最多重试一次，共 2 次调用。
    if (attempt === MAX_JUDGE_ATTEMPTS) {
      checks = failedChecks(
        input.criteria,
        `Judge 返回无效 JSON 响应（${error}）`,
      );
    }
  }

  // 循环保证至少执行一次后 checks 一定非空，此处仅为类型收窄兜底。
  if (checks === null) {
    checks = failedChecks(input.criteria, "Judge 返回无效 JSON 响应（未知错误）");
  }

  const durationMs = Date.now() - startTime;

  return {
    pass:
      input.criteria.length > 0 &&
      checks.length === input.criteria.length &&
      checks.every((check) => check.pass),
    checks,
    model: JUDGE_MODEL,
    usage: lastUsage,
    duration_ms: durationMs,
  };
}
