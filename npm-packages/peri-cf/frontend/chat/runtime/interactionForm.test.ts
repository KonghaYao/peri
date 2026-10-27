import { describe, expect, test } from 'bun:test';
import { buildElicitationContent, initialElicitationValues } from './interactionForm';
import type { InteractionFieldView } from '@peri-code/sdk/view';

/** 表单值构造与校验（自 interaction-panel.test.tsx 迁移）：面板组件已换 registry 实现，规则保持不变。 */
function fields(): InteractionFieldView[] {
  return [
    { key: 'name', title: '名字', description: '', type: 'string', required: true },
    { key: 'level', title: '级别', description: '', type: 'string', required: false, enum: ['a', 'b'] },
    { key: 'flag', title: '开关', description: '', type: 'boolean', required: false },
  ];
}

describe('elicitation form values', () => {
  test('builds content with defaults and reports missing required fields', () => {
    const initial = initialElicitationValues(fields());
    expect(initial).toEqual({ level: 'a', flag: false });
    const content = buildElicitationContent(fields(), { ...initial, name: 'Ada' });
    expect(content).toEqual({ ok: true, content: { level: 'a', flag: false, name: 'Ada' } });
    const missing = buildElicitationContent(fields(), { ...initial });
    expect(missing).toEqual({ ok: false, missing: ['name'] });
    const numeric = buildElicitationContent([{ key: 'n', title: 'N', description: '', type: 'integer', required: true }], { n: '1.5' });
    expect(numeric).toEqual({ ok: false, missing: ['n'] });
    expect(buildElicitationContent([{ key: 'n', title: 'N', description: '', type: 'number', required: true }], { n: '1.5' }))
      .toEqual({ ok: true, content: { n: 1.5 } });
  });
});
