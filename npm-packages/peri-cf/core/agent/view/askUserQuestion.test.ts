import { describe, expect, test } from 'bun:test';
import {
  buildAskUserQuestionContent, MAX_ASK_USER_QUESTIONS, normalizeAskUserQuestionDecision,
  parseAskUserQuestionElicitation, type AskUserQuestionDrafts,
} from './askUserQuestion';
import type { AskUserQuestionDTO as AskUserQuestion } from '../../shared';

/** 与 peri-acp transport_broker 的 build_elicitation_params 同构的 wire payload。 */
function elicitation(properties: Record<string, unknown>, overrides: Record<string, unknown> = {}) {
  return {
    mode: 'form',
    sessionId: 'session-1',
    message: 'Please provide the requested information',
    requestedSchema: { type: 'object', properties },
    ...overrides,
  };
}

function questions(): AskUserQuestion[] {
  return [
    {
      id: 'ask_user_question_0', header: '部署环境', question: '选择部署环境？', multiSelect: false,
      options: [
        { value: '生产', label: '生产', description: '生产环境' },
        { value: '测试', label: '测试' },
      ],
    },
    {
      id: 'ask_user_question_1', header: '功能', question: '选择功能？', multiSelect: true,
      options: [
        { value: 'a', label: 'a', description: 'desc-a' },
        { value: 'b', label: 'b' },
      ],
    },
    {
      id: 'ask_user_question_2', header: '补充', question: '补充说明？', multiSelect: false, options: [],
    },
  ];
}

const payload = () => elicitation({
  ask_user_question_0: {
    type: 'string', title: '部署环境', description: '选择部署环境？',
    oneOf: [
      { const: '生产', title: '生产', description: '生产环境' },
      { const: '测试', title: '测试' },
    ],
  },
  ask_user_question_1: {
    type: 'array', title: '功能', description: '选择功能？',
    items: { type: 'string', anyOf: [
      { const: 'a', title: 'a', description: 'desc-a' },
      { const: 'b', title: 'b' },
    ] },
  },
  ask_user_question_2: { type: 'string', title: '补充', description: '补充说明？' },
});

describe('parseAskUserQuestionElicitation', () => {
  test('parses the exact single/multi/free-text wire shape', () => {
    const parsed = parseAskUserQuestionElicitation(payload());
    expect(parsed.kind).toBe('ask-user');
    if (parsed.kind !== 'ask-user') return;
    expect(parsed.request.questions).toEqual(questions());
    expect(parsed.request.message).toBe('Please provide the requested information');
  });

  test('sorts questions by numeric index instead of object order', () => {
    const source = payload();
    const properties = source.requestedSchema.properties as Record<string, unknown>;
    const shuffled = {
      ...source,
      requestedSchema: {
        type: 'object',
        properties: {
          ask_user_question_2: properties.ask_user_question_2,
          ask_user_question_0: properties.ask_user_question_0,
          ask_user_question_1: properties.ask_user_question_1,
        },
      },
    };
    const parsed = parseAskUserQuestionElicitation(shuffled);
    expect(parsed.kind).toBe('ask-user');
    if (parsed.kind !== 'ask-user') return;
    expect(parsed.request.questions.map((question) => question.id))
      .toEqual(['ask_user_question_0', 'ask_user_question_1', 'ask_user_question_2']);
  });

  test('leaves generic MCP elicitation for the SDK form path', () => {
    expect(parseAskUserQuestionElicitation(elicitation({ name: { type: 'string', title: '名字' } })).kind)
      .toBe('not-ask-user');
    expect(parseAskUserQuestionElicitation({ mode: 'url', sessionId: 's', url: 'https://example.com' }).kind)
      .toBe('not-ask-user');
    expect(parseAskUserQuestionElicitation(null).kind).toBe('not-ask-user');
  });

  test('rejects malformed AskUserQuestion payloads instead of rendering them', () => {
    const cases: Array<[string, unknown]> = [
      ['mixed keys', elicitation({
        ask_user_question_0: { type: 'string', title: 'A', description: 'Q' },
        other: { type: 'string', title: 'B', description: 'Q' },
      })],
      ['missing description', elicitation({
        ask_user_question_0: { type: 'string', title: 'A' },
      })],
      ['unsupported enum', elicitation({
        ask_user_question_0: { type: 'string', title: 'A', description: 'Q', enum: ['a', 'b'] },
      })],
      ['empty oneOf', elicitation({
        ask_user_question_0: { type: 'string', title: 'A', description: 'Q', oneOf: [] },
      })],
      ['bad array items', elicitation({
        ask_user_question_0: { type: 'array', title: 'A', description: 'Q', items: { type: 'string' } },
      })],
      ['too many questions', elicitation(Object.fromEntries(
        Array.from({ length: MAX_ASK_USER_QUESTIONS + 1 }, (_, index) => [
          `ask_user_question_${index}`, { type: 'string', title: `H${index}`, description: `Q${index}` },
        ]),
      ))],
    ];
    for (const [label, value] of cases) {
      const parsed = parseAskUserQuestionElicitation(value);
      expect(parsed.kind, label).toBe('invalid');
    }
  });
});

describe('buildAskUserQuestionContent', () => {
  const drafts = (overrides: AskUserQuestionDrafts = {}): AskUserQuestionDrafts => ({
    ask_user_question_0: { selected: ['生产'], custom: '' },
    ask_user_question_1: { selected: ['a'], custom: '其它' },
    ask_user_question_2: { selected: [], custom: '自由补充' },
    ...overrides,
  });

  test('builds string / string[] answers keyed by question id', () => {
    expect(buildAskUserQuestionContent(questions(), drafts())).toEqual({
      ok: true,
      content: {
        ask_user_question_0: '生产',
        ask_user_question_1: ['a', '其它'],
        ask_user_question_2: '自由补充',
      },
    });
  });

  test('custom text overrides a single-select option and trims whitespace', () => {
    expect(buildAskUserQuestionContent(questions(), drafts({
      ask_user_question_0: { selected: ['生产'], custom: '  预发  ' },
    }))).toEqual({
      ok: true,
      content: {
        ask_user_question_0: '预发',
        ask_user_question_1: ['a', '其它'],
        ask_user_question_2: '自由补充',
      },
    });
  });

  test('reports every unanswered question and drops stale option values', () => {
    const built = buildAskUserQuestionContent(questions(), {
      ask_user_question_0: { selected: ['已废弃'], custom: '   ' },
      ask_user_question_1: { selected: [], custom: '' },
      ask_user_question_2: { selected: [], custom: '' },
    });
    expect(built).toEqual({
      ok: false,
      missing: ['ask_user_question_0', 'ask_user_question_1', 'ask_user_question_2'],
    });
  });
});

describe('normalizeAskUserQuestionDecision', () => {
  test('accepts only complete, exactly typed content', () => {
    expect(normalizeAskUserQuestionDecision({
      action: 'accept', content: {
        ask_user_question_0: '生产', ask_user_question_1: ['a', 'b'], ask_user_question_2: '补充',
      },
    }, questions())).toEqual({
      action: 'accept', content: {
        ask_user_question_0: '生产', ask_user_question_1: ['a', 'b'], ask_user_question_2: '补充',
      },
    });
  });

  test('declines missing/extra keys and wrong value types', () => {
    const base = { ask_user_question_0: '生产', ask_user_question_1: ['a'], ask_user_question_2: '补充' };
    expect(normalizeAskUserQuestionDecision({ action: 'accept', content: { ...base, extra: 'x' } }, questions()))
      .toEqual({ action: 'decline' });
    expect(normalizeAskUserQuestionDecision({ action: 'accept', content: { ask_user_question_0: '生产' } }, questions()))
      .toEqual({ action: 'decline' });
    expect(normalizeAskUserQuestionDecision({
      action: 'accept', content: { ...base, ask_user_question_1: 'a' },
    }, questions())).toEqual({ action: 'decline' });
    expect(normalizeAskUserQuestionDecision({
      action: 'accept', content: { ...base, ask_user_question_0: ['生产'] },
    }, questions())).toEqual({ action: 'decline' });
  });

  test('passes through decline/cancel and rejects unknown actions', () => {
    expect(normalizeAskUserQuestionDecision({ action: 'decline' }, questions())).toEqual({ action: 'decline' });
    expect(normalizeAskUserQuestionDecision({ action: 'cancel' }, questions())).toEqual({ action: 'cancel' });
    expect(normalizeAskUserQuestionDecision({ action: 'other' }, questions())).toEqual({ action: 'decline' });
    expect(normalizeAskUserQuestionDecision(null, questions())).toEqual({ action: 'decline' });
  });
});
