import { describe, expect, test } from 'bun:test';
import {
  buildAskUserQuestionContent, type AskUserQuestionDrafts,
} from './askUserQuestion';
import type { AskUserQuestionDTO as AskUserQuestion } from '../../remote';

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

  test('does not duplicate a multi-select value repeated in custom text', () => {
    expect(buildAskUserQuestionContent(questions(), drafts({
      ask_user_question_1: { selected: ['a'], custom: 'a' },
    }))).toEqual({
      ok: true,
      content: {
        ask_user_question_0: '生产',
        ask_user_question_1: ['a'],
        ask_user_question_2: '自由补充',
      },
    });
  });
});
