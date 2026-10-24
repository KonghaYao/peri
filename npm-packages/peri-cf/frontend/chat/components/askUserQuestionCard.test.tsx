import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { InteractionCard } from './InteractionCard';
import type { InteractionAnswerHandler } from '../runtime';
import type { AskUserQuestion, Interaction } from '../runtime';

const chat = { id: 'chat-questions', title: '问题面板', updatedAt: '2026-10-20T00:00:00Z' };

const questions: AskUserQuestion[] = [
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
      { value: 'a', label: 'A', description: 'desc-a' },
      { value: 'b', label: 'B' },
    ],
  },
  {
    id: 'ask_user_question_2', header: '补充', question: '补充说明？', multiSelect: false, options: [],
  },
];

function questionInteraction(overrides: Partial<Interaction> = {}): Interaction {
  return {
    id: 'interaction:ask-1', kind: 'elicitation', sessionId: chat.id, status: 'pending',
    createdAt: '2026-10-20T00:00:00Z', resolvedAt: null,
    title: '需要你的回答', message: 'Please provide the requested information', questions,
    ...overrides,
  };
}

const answer: InteractionAnswerHandler = async () => {};
const render = (interaction: Interaction, onAnswer?: InteractionAnswerHandler) =>
  renderToStaticMarkup(<InteractionCard interaction={interaction} onAnswer={onAnswer} />);

describe('AskUserQuestion card', () => {
  test('renders every question as a labelled group with its own controls', () => {
    const output = render(questionInteraction(), answer);
    expect(output).toContain('data-slot="ask-user-question"');
    expect(output).toContain('3 个问题');
    for (const question of questions) {
      expect(output).toContain(`data-question-id="${question.id}"`);
      expect(output).toContain(question.header);
      expect(output).toContain(question.question);
    }
    expect(output).toContain('type="radio"');
    expect(output).toContain('type="checkbox"');
    expect(output).toContain('value="生产"');
    expect(output).toContain('生产环境');
    expect(output).toContain('desc-a');
    expect(output).toContain('可多选');
    expect(output).toContain('自由输入');
    expect(output).toContain('aria-label="自定义回答：部署环境"');
    expect(output).toContain('aria-label="自定义回答：补充"');
    expect(output).toContain('取消');
    expect(output).toContain('提交回答');
  });

  test('blocks submission until every question is answered', () => {
    const output = render(questionInteraction(), answer);
    expect(output).toContain('还有 3 个问题未回答。');
    expect(output).toMatch(/<button[^>]*disabled=""[^>]*>提交回答<\/button>/);
  });

  test('questions stay visible but unanswerable when the connection is not ready', () => {
    const output = render(questionInteraction());
    expect(output).toContain('部署环境');
    expect(output).toContain('连接就绪后可以回答这条交互。');
    expect(output).toMatch(/<input[^>]*disabled=""/);
    expect(output).toMatch(/<button[^>]*disabled=""[^>]*>取消<\/button>/);
    expect(output).toMatch(/<button[^>]*disabled=""[^>]*>提交回答<\/button>/);
  });

  test('expired interactions keep the questions readonly', () => {
    const output = renderToStaticMarkup(
      <InteractionCard interaction={questionInteraction({ status: 'expired' })} onAnswer={answer} />,
    );
    expect(output).toContain('已失效');
    expect(output).not.toContain('提交回答');
    expect(output).toMatch(/<fieldset[^>]*disabled=""/);
  });
});
