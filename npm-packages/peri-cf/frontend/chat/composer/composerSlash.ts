import type { ChatDisplayState } from '../runtime';

export interface ComposerCommand {
  name: string;
  description?: string;
  hint?: string;
}

export interface SlashTrigger {
  /** 行首 `/` 的位置。 */
  start: number;
  /** 光标位置；替换范围是 `[start, end)`。 */
  end: number;
  query: string;
}

const MAX_MENU_ITEMS = 8;

function nonEmptyString(value: unknown): string | null {
  return typeof value === 'string' && value.trim().length > 0 ? value : null;
}

/** 只使用 ACP 提供的命令事实；未知命令不伪造。 */
export function readAvailableCommands(state: ChatDisplayState | null): ComposerCommand[] {
  const raw = state?.session.availableCommands;
  if (!Array.isArray(raw)) return [];
  const seen = new Set<string>();
  return raw.flatMap((item) => {
    if (item === null || typeof item !== 'object' || Array.isArray(item)) return [];
    const record = item as Record<string, unknown>;
    const name = nonEmptyString(record.name);
    if (!name || seen.has(name)) return [];
    seen.add(name);
    const description = nonEmptyString(record.description) ?? undefined;
    const input = record.input;
    const hint = input !== null && typeof input === 'object' && !Array.isArray(input)
      ? nonEmptyString((input as Record<string, unknown>).hint) ?? undefined
      : undefined;
    return [{ name, ...(description ? { description } : {}), ...(hint ? { hint } : {}) }];
  });
}

/**
 * 仅当前行行首是 `/` 且查询部分没有空白时触发。
 * `caret` 之后的文本属于用户正在编辑的内容，替换时保留。
 */
export function findSlashTrigger(text: string, caret: number): SlashTrigger | null {
  const end = Math.min(Math.max(caret, 0), text.length);
  const lineStart = text.lastIndexOf('\n', Math.max(0, end - 1)) + 1;
  const segment = text.slice(lineStart, end);
  if (!segment.startsWith('/') || /\s/.test(segment)) return null;
  return { start: lineStart, end, query: segment.slice(1) };
}

export function filterComposerCommands(
  commands: readonly ComposerCommand[],
  query: string,
  limit = MAX_MENU_ITEMS,
): ComposerCommand[] {
  const needle = query.toLowerCase();
  return commands
    .filter((command) => {
      if (!needle) return true;
      return command.name.toLowerCase().includes(needle)
        || (command.description?.toLowerCase().includes(needle) ?? false);
    })
    .slice(0, limit);
}

/** 把 `/query` 替换为 `/name `，返回新文本与光标位置；不自动发送。 */
export function applyComposerCommand(
  text: string,
  trigger: SlashTrigger,
  name: string,
): { text: string; caret: number } {
  const before = text.slice(0, trigger.start);
  const after = text.slice(trigger.end);
  const inserted = `/${name} `;
  return { text: `${before}${inserted}${after}`, caret: before.length + inserted.length };
}
