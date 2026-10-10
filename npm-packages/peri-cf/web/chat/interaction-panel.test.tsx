import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { ChatPanels, type InteractionAnswerHandler } from './Panels';
import { buildElicitationContent, initialElicitationValues } from './interactionForm';
import type { Interaction, SyncState } from './types';

const chat = { id: 'chat-interactions', title: '交互回归', updatedAt: '2026-10-10T00:00:00Z' };

function state(interactions: Interaction[]): SyncState {
  return {
    chat, running: false, executionBlocked: false, readiness: 'ready',
    entries: [], toolCalls: [], turns: [], activeTurn: { turnId: null, status: null }, session: {}, tasks: [],
    plans: { current: [], byTurn: {} }, interactions,
    pendingInteractions: interactions.filter((interaction) => interaction.status === 'pending'),
    inputQueue: null,
  };
}

function permission(overrides: Partial<Interaction> = {}): Interaction {
  return {
    id: 'interaction:1', kind: 'permission', sessionId: chat.id, status: 'pending',
    createdAt: '2026-10-10T00:00:00Z', resolvedAt: null, title: 'Write file',
    options: [{ optionId: 'allow_once', name: '允许一次' }, { optionId: 'reject_once', name: '拒绝' }],
    ...overrides,
  };
}

function elicitation(overrides: Partial<Interaction> = {}): Interaction {
  return {
    id: 'interaction:2', kind: 'elicitation', sessionId: chat.id, status: 'pending',
    createdAt: '2026-10-10T00:00:00Z', resolvedAt: null, message: '请填写表单',
    fields: [
      { key: 'name', title: '名字', description: '', type: 'string', required: true },
      { key: 'level', title: '级别', description: '', type: 'string', required: false, enum: ['a', 'b'] },
      { key: 'flag', title: '开关', description: '', type: 'boolean', required: false },
    ],
    ...overrides,
  };
}

/** 回答句柄代表「当前会话已同步」；未同步时面板不提供回答入口。 */
const answer: InteractionAnswerHandler = async () => {};
const render = (interactions: Interaction[], onAnswer?: InteractionAnswerHandler) =>
  renderToStaticMarkup(<ChatPanels state={state(interactions)} onAnswer={onAnswer} />);

describe('interaction answer surface (P4)', () => {
  test('pending permission offers the supported decisions as commands', () => {
    const output = render([permission()], answer);
    expect(output).toContain('Write file');
    expect(output).toContain('等待回答');
    expect(output).toContain('允许一次');
    expect(output).toContain('拒绝');
    expect(output).toContain('button');
  });

  test('unsupported permission options stay visible but are not submittable', () => {
    const output = render([permission({ options: [{ optionId: 'allow_always', name: '总是允许' }] })], answer);
    expect(output).toContain('总是允许（暂不支持）');
    expect(output).not.toContain('>总是允许<');
  });

  test('elicitation form renders typed inputs, required marks and enum select', () => {
    const output = render([elicitation()], answer);
    expect(output).toContain('名字（必填）');
    expect(output).toContain('type="text"');
    expect(output).toContain('<select');
    expect(output).toContain('type="checkbox"');
    expect(output).toContain('提交');
    expect(output).toContain('拒绝回答');
  });

  test('unanswerable interactions state the reason and offer no controls', () => {
    const unsupported = render([elicitation({ formUnsupported: 'unsupported_property_type', fields: [] })], answer);
    expect(unsupported).toContain('该表单类型暂不支持回答');
    expect(unsupported).not.toContain('interaction-form');
    const offline = render([permission()]);
    expect(offline).toContain('连接就绪后可以回答这条交互');
    const noOptions = render([permission({ options: [] })], answer);
    expect(noOptions).toContain('这条交互当前不可回答');
  });

  test('settled and expired interactions are read-only', () => {
    const resolved = render([permission({ status: 'resolved', outcome: 'allow_once' })], answer);
    expect(resolved).toContain('已处理：allow_once');
    expect(resolved).toContain('回答闭环已在服务端结算');
    expect(resolved).not.toContain('interaction-actions');
    const expired = render([elicitation({ status: 'expired', fields: [] })], answer);
    expect(expired).toContain('已失效');
  });
});

describe('elicitation form values', () => {
  test('builds content with defaults and reports missing required fields', () => {
    const fields = elicitation().fields!;
    const initial = initialElicitationValues(fields);
    expect(initial).toEqual({ level: 'a', flag: false });
    const content = buildElicitationContent(fields, { ...initial, name: 'Ada' });
    expect(content).toEqual({ ok: true, content: { level: 'a', flag: false, name: 'Ada' } });
    const missing = buildElicitationContent(fields, { ...initial });
    expect(missing).toEqual({ ok: false, missing: ['name'] });
    const numeric = buildElicitationContent([{ key: 'n', title: 'N', description: '', type: 'integer', required: true }], { n: '1.5' });
    expect(numeric).toEqual({ ok: false, missing: ['n'] });
    expect(buildElicitationContent([{ key: 'n', title: 'N', description: '', type: 'number', required: true }], { n: '1.5' }))
      .toEqual({ ok: true, content: { n: 1.5 } });
  });
});
