import type { ToolCall } from '../runtime';

export type ToolCategory =
  | 'command' | 'read' | 'write' | 'edit' | 'search' | 'web' | 'task' | 'ask' | 'plan' | 'other';

export interface ToolPresentation {
  category: ToolCategory;
  /** 动作标题，例如「运行命令」。 */
  action: string;
  /** 动作目标，例如命令、路径或搜索词。 */
  target?: string;
}

function record(value: unknown): Record<string, unknown> | null {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function text(value: unknown): string | undefined {
  if (typeof value === 'string') return value.trim() || undefined;
  if (typeof value === 'number' || typeof value === 'boolean') return String(value);
  return undefined;
}

/** MCP 工具名 `mcp__server__Tool` 取末段；其余保持原名。 */
export function effectiveToolName(name: string): string {
  const parts = name.split('__').filter(Boolean);
  return parts.length > 1 ? parts[parts.length - 1]! : name;
}

function categoryOf(name: string): { category: ToolCategory; action: string; keys: string[] } {
  switch (name.toLowerCase()) {
    case 'bash': case 'shell': case 'execute': case 'terminal': case 'run':
      return { category: 'command', action: '运行命令', keys: ['command', 'cmd'] };
    case 'read': case 'readfile': case 'read_file':
      return { category: 'read', action: '读取文件', keys: ['file_path', 'path'] };
    case 'write': case 'writefile': case 'write_file':
      return { category: 'write', action: '写入文件', keys: ['file_path', 'path'] };
    case 'edit': case 'multiedit': case 'multi_edit': case 'apply_patch': case 'patch':
      return { category: 'edit', action: '编辑文件', keys: ['file_path', 'path'] };
    case 'grep': case 'search':
      return { category: 'search', action: '搜索代码', keys: ['pattern', 'query'] };
    case 'glob': case 'find':
      return { category: 'search', action: '查找文件', keys: ['pattern', 'glob'] };
    case 'webfetch': case 'web_fetch': case 'fetch':
      return { category: 'web', action: '读取网页', keys: ['url'] };
    case 'websearch': case 'web_search':
      return { category: 'web', action: '搜索网页', keys: ['query'] };
    case 'task': case 'agent': case 'subagent':
      return { category: 'task', action: '运行子任务', keys: ['description', 'prompt'] };
    case 'askuserquestion': case 'ask_user_question':
      return { category: 'ask', action: '询问用户', keys: [] };
    case 'todowrite': case 'todo_write': case 'todo':
      return { category: 'plan', action: '更新计划', keys: [] };
    default:
      return { category: 'other', action: effectiveToolName(name), keys: [] };
  }
}

/** 从工具入参提取展示事实；缺失时不补造目标。 */
export function toolPresentation(tool: ToolCall | null): ToolPresentation {
  if (!tool) return { category: 'other', action: '工具调用不可用' };
  const mapping = categoryOf(effectiveToolName(tool.name));
  const args = record(tool.arguments);
  const target = mapping.keys
    .map((key) => text(args?.[key]))
    .find((value): value is string => value !== undefined);
  return { category: mapping.category, action: mapping.action, ...(target ? { target } : {}) };
}

export interface ToolResultPresentation {
  /** 可直接展示的纯文本；不把 contentText 包进 JSON 引号与转义。 */
  text: string;
  /** 从文本或结构化 rawOutput 提取的退出码。 */
  exitCode: string | null;
}

const EXIT_CODE_PATTERN = /\bexit(?:\s*code)?["']?\s*[:=]?\s*(\d+)\b/i;

/** 提取退出码：文本优先，其次 rawOutput 的常见结构化字段。 */
export function toolExitCode(value: unknown): string | null {
  if (typeof value === 'number') return Number.isInteger(value) ? String(value) : null;
  if (typeof value === 'string') return value.match(EXIT_CODE_PATTERN)?.[1] ?? null;
  if (Array.isArray(value)) {
    for (const item of value) {
      const found = toolExitCode(item);
      if (found !== null) return found;
    }
    return null;
  }
  const fields = record(value);
  if (!fields) return null;
  for (const key of ['exitCode', 'exit_code', 'code']) {
    const found = toolExitCode(fields[key]);
    if (found !== null) return found;
  }
  for (const key of ['stdout', 'stderr', 'output', 'text', 'message', 'error']) {
    const found = toolExitCode(fields[key]);
    if (found !== null) return found;
  }
  return null;
}

/** rawOutput 常见形态（字符串、内容块或 {stdout,stderr}）转纯文本；未知对象不倾倒 JSON。 */
function rawOutputText(value: unknown): string {
  if (typeof value === 'string') return value;
  if (typeof value === 'number' || typeof value === 'boolean') return String(value);
  if (Array.isArray(value)) return value.map(rawOutputText).filter(Boolean).join('\n');
  const fields = record(value);
  if (!fields) return '';
  return ['stdout', 'stderr', 'output', 'text', 'message', 'error']
    .map((key) => (fields[key] === undefined ? '' : rawOutputText(fields[key])))
    .filter(Boolean)
    .join('\n');
}

/**
 * 工具结果展示：优先标准 `contentText`，其次 stdout/stderr 等纯文本；
 * 只有完全没有可读文本时才退回 JSON，避免真实错误被引号与转义包住。
 */
export function toolResultPresentation(result: unknown): ToolResultPresentation {
  if (result === undefined || result === null) return { text: '', exitCode: null };
  if (typeof result === 'string') return { text: result, exitCode: toolExitCode(result) };
  if (typeof result !== 'object') {
    const value = String(result);
    return { text: value, exitCode: toolExitCode(value) };
  }
  const fields = result as Record<string, unknown>;
  const content = typeof fields.contentText === 'string' ? fields.contentText.trim() : '';
  const raw = fields.rawOutput === undefined ? '' : rawOutputText(fields.rawOutput).trim();
  const parts = [content, raw].filter((part) => part.length > 0);
  const text = parts.filter((part, index) => parts.indexOf(part) === index).join('\n');
  return {
    text: text || JSON.stringify(result, null, 2),
    exitCode: toolExitCode(fields.rawOutput) ?? toolExitCode(text),
  };
}
