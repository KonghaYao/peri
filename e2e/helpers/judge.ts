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
4. detail 字段用中文简述判断依据，失败时必须说明原因

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

/**
 * 解析并严格校验 Judge 响应。
 *
 * Judge 必须逐项返回与输入 criteria 一一对应的布尔结论；缺项、空数组、
 * 错误顺序、非布尔 pass 或无效 JSON 都视为失败，不能让 E2E 静默通过。
 */
export function parseJudgeChecks(
  rawResponse: string,
  criteria: string[],
): JudgeCheck[] {
  try {
    // 清理 BOM 和不可见字符；部分模型（mimo 等）在 JSON 前可能输出控制字符。
    const cleaned = rawResponse.trim().replace(/^\uFEFF/, "");
    const parsed: unknown = JSON.parse(cleaned);

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
  } catch (error) {
    const reason = error instanceof Error ? error.message : "未知错误";
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

  const durationMs = Date.now() - startTime;
  const rawResponse = response.choices[0]?.message?.content || "";
  const usage = response.usage
    ? {
        prompt_tokens: response.usage.prompt_tokens,
        completion_tokens: response.usage.completion_tokens,
      }
    : { prompt_tokens: 0, completion_tokens: 0 };

  const checks = parseJudgeChecks(rawResponse, input.criteria);

  return {
    pass:
      input.criteria.length > 0 &&
      checks.length === input.criteria.length &&
      checks.every((check) => check.pass),
    checks,
    model: JUDGE_MODEL,
    usage,
    duration_ms: durationMs,
  };
}
