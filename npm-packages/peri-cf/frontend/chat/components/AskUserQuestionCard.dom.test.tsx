import { afterEach, describe, expect, test } from 'bun:test';
import { installDom } from '../../test/dom';

installDom();

const { act, cleanup, fireEvent, render, screen } = await import('@testing-library/react');
const { AskUserQuestionCard } = await import('./AskUserQuestionCard');
const { createElement } = await import('react');

import type { AskUserQuestion, Interaction, InteractionAnswerHandler } from '../runtime';

afterEach(() => cleanup());

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
    options: [{ value: 'a', label: 'A', description: 'desc-a' }, { value: 'b', label: 'B' }],
  },
  {
    id: 'ask_user_question_2', header: '补充', question: '补充说明？', multiSelect: false, options: [],
  },
];

function interaction(): Interaction {
  return {
    id: 'interaction:ask-1', kind: 'elicitation', sessionId: 'chat-questions', status: 'pending',
    createdAt: '2026-10-20T00:00:00Z', resolvedAt: null,
    title: '需要你的回答', message: 'Please provide the requested information', questions,
  };
}

describe('AskUserQuestion card interactions', () => {
  test('presents one question at a time and submits every answer together', async () => {
    const calls: unknown[] = [];
    const onAnswer: InteractionAnswerHandler = async (id, answer) => {
      calls.push({ id, answer });
    };
    render(createElement(AskUserQuestionCard, { interaction: interaction(), onAnswer }));

    expect(screen.getByText('选择部署环境？')).toBeTruthy();
    expect(screen.queryByText('选择功能？')).toBeNull();

    fireEvent.click(screen.getByRole('radio', { name: /生产/ }));
    fireEvent.click(screen.getByRole('button', { name: /下一题/ }));
    expect(screen.getByText('选择功能？')).toBeTruthy();

    fireEvent.click(screen.getByRole('checkbox', { name: /A/ }));
    fireEvent.click(screen.getByRole('button', { name: /下一题/ }));
    expect(screen.getByText('补充说明？')).toBeTruthy();

    const submit = () => screen.getByRole('button', { name: '提交回答' });
    expect(submit()).toHaveProperty('disabled', false);
    fireEvent.change(screen.getByLabelText('第 3 题：回答'), { target: { value: '补充内容' } });

    await act(async () => { fireEvent.click(submit()); });
    expect(calls).toEqual([{
      id: 'interaction:ask-1',
      answer: {
        kind: 'elicitation',
        decision: {
          action: 'accept',
          content: {
            ask_user_question_0: '生产',
            ask_user_question_1: ['a'],
            ask_user_question_2: '补充内容',
          },
        },
      },
    }]);
  });

  test('progress chips jump between questions and reflect answered state', () => {
    render(createElement(AskUserQuestionCard, {
      interaction: interaction(),
      onAnswer: async () => {},
    }));

    fireEvent.click(screen.getByLabelText('第 3 题，未回答'));
    expect(screen.getByText('补充说明？')).toBeTruthy();

    fireEvent.click(screen.getByLabelText('第 1 题，未回答'));
    fireEvent.click(screen.getByRole('radio', { name: /生产/ }));
    expect(screen.getByLabelText('第 1 题，已回答')).toBeTruthy();
  });

  test('submitting with missing answers jumps to the first unanswered question', () => {
    render(createElement(AskUserQuestionCard, {
      interaction: interaction(),
      onAnswer: async () => {},
    }));

    fireEvent.click(screen.getByLabelText('第 3 题，未回答'));
    fireEvent.click(screen.getByRole('button', { name: '提交回答' }));
    expect(screen.getByText('选择部署环境？')).toBeTruthy();
    expect(screen.getByRole('alert').textContent).toContain('第 1 题还没有回答');
  });

  test('declining sends an explicit decline decision', async () => {
    const calls: unknown[] = [];
    const onAnswer: InteractionAnswerHandler = async (id, answer) => {
      calls.push({ id, answer });
    };
    render(createElement(AskUserQuestionCard, { interaction: interaction(), onAnswer }));

    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: '拒绝回答' }));
    });
    expect(calls).toEqual([{
      id: 'interaction:ask-1',
      answer: { kind: 'elicitation', decision: { action: 'decline' } },
    }]);
  });

  test('declining after typing asks for confirmation', async () => {
    const calls: unknown[] = [];
    const onAnswer: InteractionAnswerHandler = async (id, answer) => {
      calls.push({ id, answer });
    };
    render(createElement(AskUserQuestionCard, { interaction: interaction(), onAnswer }));

    fireEvent.change(screen.getByLabelText('第 1 题：其他答案'), { target: { value: '预发' } });
    fireEvent.click(screen.getByRole('button', { name: '拒绝回答' }));
    expect(calls).toEqual([]);
    expect(screen.getByText('拒绝后已填写的内容不会提交。')).toBeTruthy();

    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: '确认拒绝' }));
    });
    expect(calls).toEqual([{
      id: 'interaction:ask-1',
      answer: { kind: 'elicitation', decision: { action: 'decline' } },
    }]);
  });
});
