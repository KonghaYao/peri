import type { ToolCall } from '../runtime';
import { argumentText, exitCodeOf, rawOutputText, record } from './toolText';

export type ToolCategory =
  | 'command' | 'read' | 'write' | 'edit' | 'search' | 'web' | 'task' | 'ask' | 'plan' | 'other';

interface ToolPresentation {
  category: ToolCategory;
  /** 动作标题，例如「运行命令」。 */
  action: string;
  /** 动作目标，例如命令、路径或搜索词。 */
  target?: string;
}

/** MCP 工具名 `mcp__server__Tool` 取末段；其余保持原名。 */
function effectiveToolName(name: string): string {
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
    .map((key) => argumentText(args?.[key]))
    .find((value): value is string => value !== undefined);
  return { category: mapping.category, action: mapping.action, ...(target ? { target } : {}) };
}

interface ToolResultPresentation {
  /** 可直接展示的纯文本；不把 contentText 包进 JSON 引号与转义。 */
  text: string;
  /** 从文本或结构化 rawOutput 提取的退出码。 */
  exitCode: string | null;
}

/**
 * 工具结果展示：优先标准 `contentText`，其次 stdout/stderr 等纯文本；
 * 只有完全没有可读文本时才退回 JSON，避免真实错误被引号与转义包住。
 */
export function toolResultPresentation(result: unknown): ToolResultPresentation {
  if (result === undefined || result === null) return { text: '', exitCode: null };
  if (typeof result === 'string') return { text: result, exitCode: exitCodeOf(result) };
  if (typeof result !== 'object') {
    const value = String(result);
    return { text: value, exitCode: exitCodeOf(value) };
  }
  const fields = result as Record<string, unknown>;
  const content = typeof fields.contentText === 'string' ? fields.contentText.trim() : '';
  const raw = fields.rawOutput === undefined ? '' : rawOutputText(fields.rawOutput).trim();
  const parts = [content, raw].filter((part) => part.length > 0);
  const text = parts.filter((part, index) => parts.indexOf(part) === index).join('\n');
  return {
    text: text || JSON.stringify(result, null, 2),
    exitCode: exitCodeOf(fields.rawOutput) ?? exitCodeOf(text),
  };
}
