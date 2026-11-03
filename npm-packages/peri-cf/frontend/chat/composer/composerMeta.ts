import type { ChatDisplayState } from '../runtime';

export interface ComposerOption {
  value: string;
  name: string;
  description?: string;
}

/** 可写配置项；只有 ACP 真正给了 select 选项时才可交互。 */
export interface ComposerSelector {
  id: string;
  label: string;
  currentValue: string;
  currentLabel: string;
  options: ComposerOption[];
}

export interface ComposerMeta {
  model?: ComposerSelector;
  mode?: ComposerSelector;
  thinking?: ComposerSelector;
  /** 无 configOptions 时的只读回退；不构造可点击选项。 */
  modelLabel?: string;
  modeLabel?: string;
}

const CATEGORIES = {
  model: { label: '模型', key: 'model' },
  mode: { label: '模式', key: 'mode' },
  thought_level: { label: '推理', key: 'thinking' },
} as const;

type SelectorKey = (typeof CATEGORIES)[keyof typeof CATEGORIES]['key'];

function nonEmptyString(value: unknown): string | null {
  return typeof value === 'string' && value.trim().length > 0 ? value : null;
}

function readOptions(value: unknown): ComposerOption[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((raw) => {
    if (raw === null || typeof raw !== 'object' || Array.isArray(raw)) return [];
    const record = raw as Record<string, unknown>;
    const optionValue = nonEmptyString(record.value);
    const name = nonEmptyString(record.name);
    if (!optionValue || !name) return [];
    const description = nonEmptyString(record.description);
    return [{ value: optionValue, name, ...(description ? { description } : {}) }];
  });
}

function readSelector(
  raw: Record<string, unknown>,
  label: string,
): ComposerSelector | null {
  const id = nonEmptyString(raw.id);
  const currentValue = nonEmptyString(raw.currentValue);
  if (!id || !currentValue || raw.type !== 'select') return null;
  const options = readOptions(raw.options);
  const current = options.find((option) => option.value === currentValue);
  return {
    id,
    label,
    currentValue,
    currentLabel: current?.name ?? currentValue,
    options,
  };
}

function categorySelector(
  configOptions: readonly Record<string, unknown>[] | undefined,
  category: string,
  label: string,
): ComposerSelector | null {
  for (const raw of configOptions ?? []) {
    if (raw.category !== category) continue;
    const selector = readSelector(raw, label);
    if (selector) return selector;
  }
  return null;
}

function fallbackLabel(value: unknown): string | undefined {
  return nonEmptyString(value) ?? undefined;
}

/**
 * 解析状态栏元信息：优先 configOptions（ACP 事实），缺失时回退
 * `session.model/mode` 的只读文本；坏 option 被忽略，不伪造标签。
 */
export function readComposerMeta(state: ChatDisplayState | null): ComposerMeta {
  if (!state) return {};
  const configOptions = state.session.configOptions;
  const selectors: Partial<Record<SelectorKey, ComposerSelector>> = {
    model: categorySelector(configOptions, 'model', CATEGORIES.model.label) ?? undefined,
    mode: categorySelector(configOptions, 'mode', CATEGORIES.mode.label) ?? undefined,
    thinking: categorySelector(configOptions, 'thought_level', CATEGORIES.thought_level.label) ?? undefined,
  };
  const meta: ComposerMeta = {};
  if (selectors.model) meta.model = selectors.model;
  if (selectors.mode) meta.mode = selectors.mode;
  if (selectors.thinking) meta.thinking = selectors.thinking;

  // 只有 configOptions 未提供该项时才回退；已提供但损坏也回退到会话文本。
  if (!meta.model) meta.modelLabel = fallbackLabel(state.session.model);
  if (!meta.mode) meta.modeLabel = fallbackLabel(state.session.mode);
  return meta;
}
