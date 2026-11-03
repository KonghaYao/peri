/**
 * 工具结果文本提取：折叠态（`toolPresentation`）与失败态（`failureSummary`）
 * 共用同一套「读文本 / 读退出码 / 拆流分段」规则。
 *
 * 两种文本策略刻意不同，不能合并：
 * - `rawOutputText` 拼接全部可读字段，用于展示完整输出；
 * - `readableText` 只取首个可读字段，用于失败摘要选择错误文本。
 * 退出码与分段解析只有一份实现，保证两态对同一 rawOutput 判定一致。
 */

export type ToolStream = "stdout" | "stderr" | "plain";

export interface ToolSection {
  stream: ToolStream;
  lines: string[];
}

export interface ToolSections {
  sections: ToolSection[];
  exitCodes: string[];
}

export function record(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

/** 参数目标：字符串去空白后为空时视为缺失；数字/布尔按展示文本处理。 */
export function argumentText(value: unknown): string | undefined {
  if (typeof value === "string") return value.trim() || undefined;
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  return undefined;
}

/** 只摊平可读文本；未知对象不倾倒 JSON。取首个非空字段（message → error → text → ...）。 */
export function readableText(value: unknown): string {
  if (typeof value === "string") return value.trim();
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  if (Array.isArray(value)) return value.map(readableText).filter(Boolean).join("\n");
  const fields = record(value);
  if (!fields) return "";
  for (const key of ["message", "error", "text", "output", "stderr", "stdout"]) {
    const found = readableText(fields[key]);
    if (found) return found;
  }
  return "";
}

/** rawOutput 常见形态（字符串、内容块或 {stdout,stderr}）转纯文本；未知对象不倾倒 JSON。 */
export function rawOutputText(value: unknown): string {
  if (typeof value === "string") return value;
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  if (Array.isArray(value)) return value.map(rawOutputText).filter(Boolean).join("\n");
  const fields = record(value);
  if (!fields) return "";
  return ["stdout", "stderr", "output", "text", "message", "error"]
    .map((key) => (fields[key] === undefined ? "" : rawOutputText(fields[key])))
    .filter(Boolean)
    .join("\n");
}

const EXIT_CODE_PATTERN = /\bexit(?:\s*code)?["']?\s*[:=]?\s*(\d+)\b/i;

/** 提取退出码：文本优先，其次 rawOutput 的常见结构化字段。 */
export function exitCodeOf(value: unknown): string | null {
  if (typeof value === "number") return Number.isInteger(value) ? String(value) : null;
  if (typeof value === "string") return value.match(EXIT_CODE_PATTERN)?.[1] ?? null;
  if (Array.isArray(value)) {
    for (const item of value) {
      const found = exitCodeOf(item);
      if (found !== null) return found;
    }
    return null;
  }
  const fields = record(value);
  if (!fields) return null;
  for (const key of ["exitCode", "exit_code", "code"]) {
    const found = exitCodeOf(fields[key]);
    if (found !== null) return found;
  }
  for (const key of ["stdout", "stderr", "output", "text", "message", "error"]) {
    const found = exitCodeOf(fields[key]);
    if (found !== null) return found;
  }
  return null;
}

/** JSON 结构化退出码：只接受整数或纯整数文本，不吃文本里的 `exit` 标记。 */
export function jsonExitCodeOf(fields: Record<string, unknown>): string | null {
  for (const key of ["exitCode", "exit_code", "code"]) {
    const value = fields[key];
    if (typeof value === "number" && Number.isInteger(value)) return String(value);
    if (typeof value === "string" && /^-?\d+$/.test(value.trim())) return value.trim();
  }
  return null;
}

const EXIT_MARKER = /\[exit\s*code\s*:\s*(-?\d+)\]/gi;
const STREAM_LINE = /^\[(stdout|stderr)\]\s*(.*)$/i;
const STREAM_TOKEN = /\[(?:stdout|stderr)\]/gi;

function pushLine(sections: ToolSection[], stream: ToolStream, line: string): void {
  const last = sections[sections.length - 1];
  if (last?.stream === stream) last.lines.push(line);
  else sections.push({ stream, lines: [line] });
}

/** 标记行只承载流与退出码信息，不进入错误文本。 */
export function sectionsOf(raw: string): ToolSections {
  const sections: ToolSection[] = [];
  const exitCodes: string[] = [];
  let stream: ToolStream = "plain";
  for (const rawLine of raw.split(/\r?\n/)) {
    const line = rawLine.trim();
    if (!line) continue;
    for (const match of line.matchAll(EXIT_MARKER)) exitCodes.push(match[1]!);
    const streamLine = STREAM_LINE.exec(line);
    if (streamLine) {
      stream = streamLine[1]!.toLowerCase() as ToolStream;
      const rest = streamLine[2]!.replace(EXIT_MARKER, "").trim();
      if (rest) pushLine(sections, stream, rest);
      continue;
    }
    const cleaned = line.replace(EXIT_MARKER, "").replace(STREAM_TOKEN, "").trim();
    if (cleaned) pushLine(sections, stream, cleaned);
  }
  return { sections, exitCodes };
}

export function sectionText(section: ToolSection): string {
  return section.lines.join("\n");
}

/** 合并分段，跳过与已有分段完全相同的文本，避免 contentText/rawOutput 重复展示。 */
export function appendSections(target: ToolSection[], source: ToolSection[]): void {
  for (const section of source) {
    const value = sectionText(section);
    if (!value) continue;
    if (target.some((existing) => sectionText(existing) === value)) continue;
    const last = target[target.length - 1];
    if (last?.stream === section.stream) last.lines.push(...section.lines);
    else target.push({ stream: section.stream, lines: [...section.lines] });
  }
}
