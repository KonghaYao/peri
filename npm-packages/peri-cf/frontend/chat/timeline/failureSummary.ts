import {
  appendSections, exitCodeOf, jsonExitCodeOf, readableText, record, sectionText, sectionsOf,
  type ToolSection as Section, type ToolStream as Stream,
} from './toolText';

export interface ToolFailureSummary {
  /** 实际错误/输出的首行；没有可读文本时为 null。 */
  detail: string | null;
  /** 退出码：rawOutput.exitCode 优先，其次 JSON 字段与文本标记。 */
  exitCode: string | null;
  /** 去掉 [stdout]/[stderr]/[Exit code: N] 标记后的完整文本。 */
  fullText: string;
}

const JSON_FIELDS = ['stderr', 'stdout', 'message', 'error'] as const;

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

/** JSON 字段按 stderr → stdout → message → error 的顺序取文本；字段名决定流。 */
function fieldSections(key: string, value: unknown): { sections: Section[]; exitCodes: string[] } {
  const raw = readableText(value);
  if (!raw) return { sections: [], exitCodes: [] };
  const extracted = sectionsOf(raw);
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

/**
 * 工具失败摘要：从真实 contentText/rawOutput 形状提取可读首行、退出码与全文。
 * `[stderr]` 等标记只用于分流，不进入 detail/fullText；JSON 对象优先 stderr。
 * 文本/退出码/分段规则与折叠态共用 `toolText`。
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
        structuredExitCode = exitCodeOf(fields.rawOutput);
        const rawFields = record(fields.rawOutput);
        if (rawFields) {
          for (const key of ['stderr', 'stdout', 'output', 'text', 'message', 'error']) {
            const extracted = fieldSections(key, rawFields[key]);
            appendSections(sections, extracted.sections);
            textExitCodes.push(...extracted.exitCodes);
          }
        } else {
          const extracted = sectionsOf(readableText(fields.rawOutput));
          appendSections(sections, extracted.sections);
          textExitCodes.push(...extracted.exitCodes);
        }
      }
    } else if (result !== undefined && result !== null) {
      contentText = readableText(result);
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
      const extracted = sectionsOf(contentText);
      appendSections(sections, extracted.sections);
      textExitCodes.push(...extracted.exitCodes);
    }
    detail = hasKnownField ? detailOf(sections) : null;
  } else {
    const extracted = sectionsOf(contentText);
    appendSections(sections, extracted.sections);
    textExitCodes.push(...extracted.exitCodes);
    detail = detailOf(sections);
  }

  if (!parsed && !sections.length && preview) {
    const extracted = sectionsOf(preview);
    appendSections(sections, extracted.sections);
    textExitCodes.push(...extracted.exitCodes);
    detail = detailOf(sections);
  }

  const fullText = sections.map(sectionText).filter(Boolean).join('\n').trim();
  return {
    detail,
    exitCode: structuredExitCode ?? jsonExitCodeOf(parsed ?? {}) ?? textExitCodes.find(Boolean) ?? exitCodeOf(fullText),
    fullText,
  };
}
