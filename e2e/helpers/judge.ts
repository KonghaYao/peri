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
  detail?: string;
}

export interface JudgeResult {
  pass: boolean;
  checks: JudgeCheck[];
  model: string;
  usage: { prompt_tokens: number; completion_tokens: number };
  raw_response?: string;
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
严格返回 JSON（不要 markdown 代码块包裹）:
{
  "checks": [
    { "criterion": "原始检查项文本", "pass": true, "detail": "判断依据" }
  ]
}`;

// ---- 主函数 ----

/**
 * LLM judge：传入 snapshot ANSI 和检查清单，返回结构化判断
 */
export async function judge(input: JudgeInput): Promise<JudgeResult> {
  const client = getClient();
  const startTime = Date.now();

  const criteriaText = input.criteria
    .map((c, i) => `${i + 1}. ${c}`)
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

  // 解析 JSON
  let checks: JudgeCheck[] = [];
  try {
    const parsed = JSON.parse(rawResponse);
    checks = (parsed.checks || []).map((c: any) => ({
      criterion: c.criterion || "",
      pass: Boolean(c.pass),
      detail: c.detail || undefined,
    }));
  } catch {
    // JSON 解析失败，强制全部 fail
    checks = input.criteria.map((c) => ({
      criterion: c,
      pass: false,
      detail: `Judge 返回的 JSON 解析失败。原始响应: ${rawResponse.slice(0, 300)}`,
    }));
  }

  return {
    pass: checks.every((c) => c.pass),
    checks,
    model: JUDGE_MODEL,
    usage,
    raw_response: rawResponse,
    duration_ms: durationMs,
  };
}
