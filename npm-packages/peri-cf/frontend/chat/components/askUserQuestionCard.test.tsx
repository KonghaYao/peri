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
  test('renders only the first question as a labelled group with its own controls', () => {
    const output = render(questionInteraction(), answer);
    expect(output).toContain('data-slot="ask-user-question"');
    expect(output).toContain('第 1 / 3 题');
    expect(output).toContain('data-question-id="ask_user_question_0"');
    expect(output).not.toContain('data-question-id="ask_user_question_1"');
    expect(output).toContain('部署环境');
    expect(output).toContain('选择部署环境？');
    expect(output).toContain('type="radio"');
    expect(output).toContain('value="生产"');
    expect(output).toContain('生产环境');
    expect(output).toContain('单选');
    expect(output).toContain('其他答案（可选，替代所选）');
    expect(output).toContain('aria-label="第 1 题：其他答案"');
    expect(output).toContain('拒绝回答');
    expect(output).toContain('下一题');
    expect(output).not.toContain('提交回答');
  });

  test('progress marks every question and a single question submits directly', () => {
    const output = render(questionInteraction(), answer);
    expect(output).toContain('aria-label="第 1 题，未回答"');
    expect(output).toContain('aria-label="第 2 题，未回答"');
    expect(output).toContain('aria-label="第 3 题，未回答"');
    expect(output).toContain('aria-current="step"');

    const single = renderToStaticMarkup(
      <InteractionCard
        interaction={questionInteraction({ questions: [questions[0]!] })}
        onAnswer={answer}
      />,
    );
    expect(single).toContain('1 个问题');
    expect(single).toContain('提交回答');
    expect(single).not.toMatch(/<button[^>]*disabled=""[^>]*>提交回答/);
    expect(single).not.toContain('下一题');
  });

  test('questions stay unanswerable when the connection is not ready', () => {
    const output = render(questionInteraction());
    expect(output).toContain('部署环境');
    expect(output).toContain('连接就绪后可以回答这条交互。');
    expect(output).toMatch(/<input[^>]*disabled=""/);
    expect(output).toMatch(/<button[^>]*disabled=""[^>]*>拒绝回答<\/button>/);
    expect(output).toMatch(/<button[^>]*disabled=""[^>]*>下一题/);
  });

  test('free-form questions render a textarea and require an answer', () => {
    const output = renderToStaticMarkup(
      <InteractionCard
        interaction={questionInteraction({ questions: [questions[2]!] })}
        onAnswer={answer}
      />,
    );
    expect(output).toContain('回答（必填）');
    expect(output).toContain('<textarea');
    expect(output).toContain('aria-label="第 1 题：回答"');
    expect(output).toContain('提交回答');
  });

  test('expired interactions keep the first question readonly', () => {
    const output = renderToStaticMarkup(
      <InteractionCard interaction={questionInteraction({ status: 'expired' })} onAnswer={answer} />,
    );
    expect(output).toContain('已失效');
    expect(output).not.toContain('提交回答');
    expect(output).not.toContain('下一题');
    expect(output).toMatch(/<fieldset[^>]*disabled=""/);
  });
});
