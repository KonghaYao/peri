import type { InteractionFieldView } from '@peri-code/sdk/view';

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
