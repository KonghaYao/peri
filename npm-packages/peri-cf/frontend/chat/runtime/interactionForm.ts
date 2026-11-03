import type { InteractionAnswer, InteractionFieldView } from '@peri-code/sdk/view';

/** 交互回答经当前会话的命令通道提交；未就绪时不提供回答入口。 */
export type InteractionAnswerHandler = (interactionId: string, answer: InteractionAnswer) => Promise<void>;

/** SDK 契约支持的权限决定；其它 optionId 暂不支持（不误发）。 */
export const SUPPORTED_PERMISSION_OPTIONS = new Set(['allow_once', 'reject_once']);

export type ElicitationValues = Record<string, string | boolean | number>;

/** 表单控件取值 → 提交内容：空串视为未填；不满足约束返回 null（与 SDK 校验同一权威）。 */
export function buildElicitationContent(
  fields: InteractionFieldView[],
  values: ElicitationValues,
): { ok: true; content: Record<string, unknown> } | { ok: false; missing: string[] } {
  const content: Record<string, unknown> = {};
  const missing: string[] = [];
  for (const field of fields) {
    const raw = values[field.key];
    if (field.type === 'boolean') {
      content[field.key] = raw === true;
      continue;
    }
    if (raw === undefined || raw === '') {
      if (field.required) missing.push(field.key);
      continue;
    }
    if (field.type === 'string') content[field.key] = String(raw);
    else {
      const numeric = Number(raw);
      if (!Number.isFinite(numeric) || (field.type === 'integer' && !Number.isInteger(numeric))) { missing.push(field.key); continue; }
      content[field.key] = numeric;
    }
  }
  return missing.length ? { ok: false, missing } : { ok: true, content };
}

/** 初始值：boolean 默认 false，enum 默认首项，其余留空（必填项由用户填写）。 */
export function initialElicitationValues(fields: InteractionFieldView[]): ElicitationValues {
  const values: ElicitationValues = {};
  for (const field of fields) {
    if (field.type === 'boolean') values[field.key] = false;
    else if (field.enum?.length) values[field.key] = String(field.enum[0]);
  }
  return values;
}
