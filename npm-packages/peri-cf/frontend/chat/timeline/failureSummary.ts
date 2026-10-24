import { toolExitCode } from './toolPresentation';

export interface ToolFailureSummary {
  /** 实际错误/输出的首行；没有可读文本时为 null。 */
  detail: string | null;
  /** 退出码：rawOutput.exitCode 优先，其次 JSON 字段与文本标记。 */
  exitCode: string | null;
  /** 去掉 [stdout]/[stderr]/[Exit code: N] 标记后的完整文本。 */
  fullText: string;
}

type Stream = 'stdout' | 'stderr' | 'plain';

interface Section {
  stream: Stream;
  lines: string[];
}

interface Extracted {
  sections: Section[];
  exitCodes: string[];
}

const EXIT_MARKER = /\[exit\s*code\s*:\s*(-?\d+)\]/gi;
const STREAM_LINE = /^\[(stdout|stderr)\]\s*(.*)$/i;
const STREAM_TOKEN = /\[(?:stdout|stderr)\]/gi;
const JSON_FIELDS = ['stderr', 'stdout', 'message', 'error'] as const;

function record(value: unknown): Record<string, unknown> | null {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

/** 只摊平可读文本；未知对象不倾倒 JSON。 */
function textValue(value: unknown): string {
  if (typeof value === 'string') return value.trim();
  if (typeof value === 'number' || typeof value === 'boolean') return String(value);
  if (Array.isArray(value)) return value.map(textValue).filter(Boolean).join('\n');
  const fields = record(value);
  if (!fields) return '';
  for (const key of ['message', 'error', 'text', 'output', 'stderr', 'stdout']) {
    const found = textValue(fields[key]);
    if (found) return found;
  }
  return '';
}

function pushLine(sections: Section[], stream: Stream, line: string): void {
  const last = sections[sections.length - 1];
  if (last?.stream === stream) last.lines.push(line);
  else sections.push({ stream, lines: [line] });
}

/** 标记行只承载流与退出码信息，不进入错误文本。 */
function extract(raw: string): Extracted {
  const sections: Section[] = [];
  const exitCodes: string[] = [];
  let stream: Stream = 'plain';
  for (const rawLine of raw.split(/\r?\n/)) {
    const line = rawLine.trim();
    if (!line) continue;
    for (const match of line.matchAll(EXIT_MARKER)) exitCodes.push(match[1]!);
    const streamLine = STREAM_LINE.exec(line);
    if (streamLine) {
      stream = streamLine[1]!.toLowerCase() as Stream;
      const rest = streamLine[2]!.replace(EXIT_MARKER, '').trim();
      if (rest) pushLine(sections, stream, rest);
      continue;
    }
    const cleaned = line.replace(EXIT_MARKER, '').replace(STREAM_TOKEN, '').trim();
    if (cleaned) pushLine(sections, stream, cleaned);
  }
  return { sections, exitCodes };
}

function isJsonValue(line: string): boolean {
  const trimmed = line.trim();
  const bracketed = (trimmed.startsWith('{') && trimmed.endsWith('}'))
    || (trimmed.startsWith('[') && trimmed.endsWith(']'));
  if (!bracketed) return false;
  try {
    JSON.parse(trimmed);
    return true;
  } catch {
    return false;
  }
}

function firstLine(sections: Section[], stream: Stream): string | null {
  for (const section of sections) {
    if (section.stream !== stream) continue;
    const line = section.lines.find((value) => value.trim() && !isJsonValue(value));
    if (line) return line.trim();
  }
  return null;
}

/** stderr 优先于 stdout；无标记文本回退到首个可读行。 */
function detailOf(sections: Section[]): string | null {
  return firstLine(sections, 'stderr')
    ?? firstLine(sections, 'stdout')
    ?? firstLine(sections, 'plain');
}

function sectionText(section: Section): string {
  return section.lines.join('\n');
}

function appendSections(target: Section[], source: Section[]): void {
  for (const section of source) {
    const value = sectionText(section);
    if (!value) continue;
    if (target.some((existing) => sectionText(existing) === value)) continue;
    const last = target[target.length - 1];
    if (last?.stream === section.stream) last.lines.push(...section.lines);
    else target.push({ stream: section.stream, lines: [...section.lines] });
  }
}

/** JSON 字段按 stderr → stdout → message → error 的顺序取文本；字段名决定流。 */
function fieldSections(key: string, value: unknown): Extracted {
  const raw = textValue(value);
  if (!raw) return { sections: [], exitCodes: [] };
  const extracted = extract(raw);
  const fallback: Stream = key === 'stderr' ? 'stderr' : key === 'stdout' ? 'stdout' : 'plain';
  return {
    sections: extracted.sections.map((section) => (section.stream === 'plain'
      ? { stream: fallback, lines: section.lines }
      : section)),
    exitCodes: extracted.exitCodes,
  };
}

function parseObject(text: string): Record<string, unknown> | null {
  const trimmed = text.trim();
  if (!trimmed.startsWith('{')) return null;
  try {
    return record(JSON.parse(trimmed));
  } catch {
    return null;
  }
}

function jsonExitCode(fields: Record<string, unknown>): string | null {
  for (const key of ['exitCode', 'exit_code', 'code']) {
    const value = fields[key];
    if (typeof value === 'number' && Number.isInteger(value)) return String(value);
    if (typeof value === 'string' && /^-?\d+$/.test(value.trim())) return value.trim();
  }
  return null;
}

/**
 * 工具失败摘要：从真实 contentText/rawOutput 形状提取可读首行、退出码与全文。
 * `[stderr]` 等标记只用于分流，不进入 detail/fullText；JSON 对象优先 stderr。
 */
export function toolFailureSummary(result: unknown, preview = ''): ToolFailureSummary {
  const sections: Section[] = [];
  const textExitCodes: string[] = [];
  let structuredExitCode: string | null = null;
  let contentText = '';

  if (typeof result === 'string') {
    contentText = result;
  } else {
    const fields = record(result);
    if (fields) {
      if (typeof fields.contentText === 'string') contentText = fields.contentText;
      if (fields.rawOutput !== undefined) {
        structuredExitCode = toolExitCode(fields.rawOutput);
        const rawFields = record(fields.rawOutput);
        if (rawFields) {
          for (const key of ['stderr', 'stdout', 'output', 'text', 'message', 'error']) {
            const extracted = fieldSections(key, rawFields[key]);
            appendSections(sections, extracted.sections);
            textExitCodes.push(...extracted.exitCodes);
          }
        } else {
          const extracted = extract(textValue(fields.rawOutput));
          appendSections(sections, extracted.sections);
          textExitCodes.push(...extracted.exitCodes);
        }
      }
    } else if (result !== undefined && result !== null) {
      contentText = textValue(result);
    }
  }

  const parsed = parseObject(contentText);
  let detail: string | null;
  if (parsed) {
    for (const key of JSON_FIELDS) {
      const extracted = fieldSections(key, parsed[key]);
      appendSections(sections, extracted.sections);
      textExitCodes.push(...extracted.exitCodes);
    }
    // 已知字段为空时不把 JSON 原文当错误文本；未知对象才保留原文供展开。
    const hasKnownField = JSON_FIELDS.some((key) => parsed[key] !== undefined);
    if (!hasKnownField) {
      const extracted = extract(contentText);
      appendSections(sections, extracted.sections);
      textExitCodes.push(...extracted.exitCodes);
    }
    detail = hasKnownField ? detailOf(sections) : null;
  } else {
    const extracted = extract(contentText);
    appendSections(sections, extracted.sections);
    textExitCodes.push(...extracted.exitCodes);
    detail = detailOf(sections);
  }

  if (!parsed && !sections.length && preview) {
    const extracted = extract(preview);
    appendSections(sections, extracted.sections);
    textExitCodes.push(...extracted.exitCodes);
    detail = detailOf(sections);
  }

  const fullText = sections.map(sectionText).filter(Boolean).join('\n').trim();
  return {
    detail,
    exitCode: structuredExitCode ?? jsonExitCode(parsed ?? {}) ?? textExitCodes.find(Boolean) ?? toolExitCode(fullText),
    fullText,
  };
}
