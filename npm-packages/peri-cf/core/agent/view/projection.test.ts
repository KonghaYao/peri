import { describe, expect, test } from 'bun:test';
import type { InteractionCallbacks } from '@peri-code/sdk/portable';
import { ChatInteractionQueue } from './interactions';
import { ChatProjection } from './projection';

const chat = { id: 'session-1', title: '投影测试', updatedAt: '2026-10-20T00:00:00Z' };

function elicitation(properties: Record<string, unknown>) {
  return {
    mode: 'form', sessionId: chat.id, message: 'Please provide the requested information',
    requestedSchema: { type: 'object', properties },
  };
}

const askUserPayload = (index = 0) => elicitation({
  [`ask_user_question_${index}`]: {
    type: 'string', title: '部署环境', description: '选择部署环境？',
    oneOf: [{ const: '生产', title: '生产' }, { const: '测试', title: '测试' }],
  },
});

const genericPayload = elicitation({ name: { type: 'string', title: '名字' } });

function setup() {
  const projection = new ChatProjection();
  projection.setRunState({ chat, running: true, executionBlocked: false });
  const queue = new ChatInteractionQueue();
  const callbacks: InteractionCallbacks = {
    onPermissionRequest: (request) => queue.requestPermission(request),
    onElicitation: (request) => queue.requestElicitation(request),
  };
  return { projection, queue, callbacks };
}

const tick = () => new Promise<void>((resolve) => queueMicrotask(() => queueMicrotask(resolve)));

describe('ChatProjection AskUserQuestion bridge', () => {
  test('publishes a question card and round-trips the exact accept content', async () => {
    const { projection, queue, callbacks } = setup();
    const response = projection.handleRequest('elicitation/create', askUserPayload(), chat.id, callbacks);
    const pending = projection.state().pendingInteractions;
    expect(pending).toHaveLength(1);
    expect(pending[0]).toMatchObject({
      kind: 'elicitation', sessionId: chat.id, status: 'pending',
      questions: [{
        id: 'ask_user_question_0', header: '部署环境', question: '选择部署环境？', multiSelect: false,
        options: [{ value: '生产', label: '生产' }, { value: '测试', label: '测试' }],
      }],
    });

    expect(queue.answer(chat.id, pending[0]!.id, {
      kind: 'elicitation',
      decision: { action: 'accept', content: { ask_user_question_0: '生产' } },
    })).toBe('accepted');
    expect(await response).toEqual({ action: 'accept', content: { ask_user_question_0: '生产' } });
    expect(projection.state().pendingInteractions).toHaveLength(0);
    // 重复回答同一个交互不再生效（不会二次结算）。
    expect(queue.answer(chat.id, pending[0]!.id, {
      kind: 'elicitation', decision: { action: 'accept', content: { ask_user_question_0: '测试' } },
    })).toBe('unknown');
  });

  test('keeps concurrent requests independent and settles each once', async () => {
    const { projection, queue, callbacks } = setup();
    const first = projection.handleRequest('elicitation/create', askUserPayload(0), chat.id, callbacks);
    const second = projection.handleRequest('elicitation/create', askUserPayload(1), chat.id, callbacks);
    const pending = projection.state().pendingInteractions;
    expect(pending).toHaveLength(2);
    expect(pending[0]!.id).not.toBe(pending[1]!.id);
    const [firstId, secondId] = pending.map((interaction) => interaction.id);

    expect(queue.answer(chat.id, firstId!, {
      kind: 'elicitation', decision: { action: 'accept', content: { ask_user_question_0: '生产' } },
    })).toBe('accepted');
    expect(await first).toEqual({ action: 'accept', content: { ask_user_question_0: '生产' } });
    expect(projection.state().pendingInteractions.map((interaction) => interaction.id)).toEqual([secondId]);

    expect(queue.answer(chat.id, secondId!, {
      kind: 'elicitation', decision: { action: 'decline' },
    })).toBe('accepted');
    expect(await second).toEqual({ action: 'decline' });
    expect(projection.state().pendingInteractions).toHaveLength(0);
  });

  test('turn completion aborts pending questions and removes their cards', async () => {
    const { projection, queue, callbacks } = setup();
    const response = projection.handleRequest('elicitation/create', askUserPayload(), chat.id, callbacks);
    expect(projection.state().pendingInteractions).toHaveLength(1);

    await projection.complete('cancelled');
    expect(await response).toEqual({ action: 'decline' });
    expect(projection.state().pendingInteractions).toHaveLength(0);
    // 已失效的交互不再接受迟到回答。
    expect(queue.answer(chat.id, 'interaction:missing', { kind: 'elicitation', decision: { action: 'decline' } }))
      .toBe('unknown');
  });

  test('runtime disposal aborts pending questions and rejects later requests', async () => {
    const { projection, callbacks } = setup();
    const response = projection.handleRequest('elicitation/create', askUserPayload(), chat.id, callbacks);
    projection.dispose();
    expect(await response).toEqual({ action: 'decline' });
    await expect(projection.handleRequest('elicitation/create', askUserPayload(), chat.id, callbacks))
      .rejects.toThrow('disposed');
  });

  test('malformed AskUserQuestion payload is declined without prompting the user', async () => {
    let called = 0;
    const projection = new ChatProjection();
    projection.setRunState({ chat, running: true, executionBlocked: false });
    const callbacks: InteractionCallbacks = {
      onElicitation: async () => { called++; return { action: 'decline' }; },
    };
    const broken = elicitation({
      ask_user_question_0: { type: 'array', title: 'A', description: 'Q' },
    });
    expect(await projection.handleRequest('elicitation/create', broken, chat.id, callbacks))
      .toEqual({ action: 'decline' });
    expect(called).toBe(0);
    expect(projection.state().pendingInteractions).toHaveLength(0);
  });

  test('accept content that does not match the question types is declined', async () => {
    const projection = new ChatProjection();
    projection.setRunState({ chat, running: true, executionBlocked: false });
    const callbacks: InteractionCallbacks = {
      onElicitation: async () => ({
        action: 'accept', content: { ask_user_question_0: ['生产'] },
      }),
    };
    expect(await projection.handleRequest('elicitation/create', askUserPayload(), chat.id, callbacks))
      .toEqual({ action: 'decline' });
    expect(projection.state().pendingInteractions).toHaveLength(0);
  });

  test('generic elicitation still flows through the SDK form projection', async () => {
    const { projection, queue, callbacks } = setup();
    const response = projection.handleRequest('elicitation/create', genericPayload, chat.id, callbacks);
    await tick();
    const pending = projection.state().pendingInteractions;
    expect(pending).toHaveLength(1);
    expect(pending[0]!.questions).toBeUndefined();
    expect(pending[0]!.fields).toEqual([
      { key: 'name', title: '名字', description: '', type: 'string', required: false },
    ]);

    expect(queue.answer(chat.id, pending[0]!.id, {
      kind: 'elicitation', decision: { action: 'accept', content: { name: 'Ada' } },
    })).toBe('accepted');
    expect(await response).toEqual({ action: 'accept', content: { name: 'Ada' } });
  });

  test('rejects cross-session elicitation and unknown reverse methods', async () => {
    const { projection, callbacks } = setup();
    await expect(projection.handleRequest(
      'elicitation/create', { ...askUserPayload(), sessionId: 'other' }, chat.id, callbacks,
    )).rejects.toThrow('invalid sessionId');
    expect(projection.state().pendingInteractions).toHaveLength(0);
    await expect(projection.handleRequest('session/unknown', { sessionId: chat.id }, chat.id, callbacks))
      .rejects.toThrow('Unsupported ACP client request');
  });
});

describe('ChatProjection revision and summary', () => {
  test('increments revision on every publish and keeps the epoch stable', () => {
    const projection = new ChatProjection();
    projection.setRunState({ chat, running: false, executionBlocked: false });
    const first = projection.state();
    expect(first.revision).toBe(1);
    expect(first.projectionEpoch).toBeTruthy();

    projection.setRunState({ chat, running: true, executionBlocked: false });
    const second = projection.state();
    expect(second.revision).toBe(2);
    expect(second.projectionEpoch).toBe(first.projectionEpoch);
  });

  test('summary counts pending questions without materializing the state', async () => {
    const { projection, callbacks } = setup();
    expect(projection.summary()).toEqual({
      pendingInteractionCount: 0, lastTurnStatus: null, lastCompletedAt: null,
    });
    const response = projection.handleRequest('elicitation/create', askUserPayload(), chat.id, callbacks);
    expect(projection.summary().pendingInteractionCount).toBe(1);
    projection.dispose();
    expect(await response).toEqual({ action: 'decline' });
  });

  test('lastCompletedAt tracks the last successfully completed turn, not the active one', async () => {
    const projection = new ChatProjection();
    projection.setRunState({ chat, running: true, executionBlocked: false });

    await projection.start('第一轮');
    await projection.complete('completed');
    const firstCompletedAt = projection.state().turns[0]?.completedAt ?? null;
    expect(firstCompletedAt).toBeTruthy();
    expect(projection.summary().lastCompletedAt).toBe(firstCompletedAt);

    // cancelled 是终态但不是「已完成」，不得覆盖最后成功完成时间。
    await projection.start('第二轮');
    await projection.complete('cancelled');
    expect(projection.summary().lastCompletedAt).toBe(firstCompletedAt);

    // 新一轮运行中时，仍保留上一轮成功完成时间。
    await projection.start('第三轮');
    expect(projection.summary()).toMatchObject({
      lastTurnStatus: 'accepting', lastCompletedAt: firstCompletedAt,
    });

    await projection.complete('completed');
    const thirdCompletedAt = projection.state().turns.at(-1)?.completedAt ?? null;
    expect(thirdCompletedAt).toBeTruthy();
    expect(projection.summary().lastCompletedAt).toBe(thirdCompletedAt);
    projection.dispose();
  });
});
