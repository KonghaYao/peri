import { describe, expect, test } from 'bun:test';
import { ChatUserAction } from '@peri-code/sdk';
import { UserError } from 'rivetkit';
import type { ChatExecuteResult, ChatOpenResult } from '../src/rivet/index.js';
import {
  agentChunkTexts,
  chatTarget,
  createChatHarness,
  createStubChatRuntime,
  noticeMethods,
  waitForFacts,
} from './support/chat-stub.js';

async function openAndCreate(
  harness: ReturnType<typeof createChatHarness>,
  requestId: string,
): Promise<string> {
  const opened = (await harness.actions.open(harness.context, chatTarget())) as ChatOpenResult;
  expect(opened.binding).toBeNull();
  const created = (await harness.actions.execute(
    harness.context,
    ChatUserAction.createNewSession({ requestId }),
  )) as ChatExecuteResult;
  if (created.type !== 'create_new_session') {
    throw new Error(`expected create_new_session, received ${created.type}`);
  }
  return created.result.sessionId;
}

async function send(
  harness: ReturnType<typeof createChatHarness>,
  requestId: string,
  text: string,
): Promise<void> {
  const sent = (await harness.actions.execute(
    harness.context,
    ChatUserAction.send({ text, requestId }),
  )) as ChatExecuteResult;
  if (sent.type !== 'send') {
    throw new Error(`expected send, received ${sent.type}`);
  }
  expect(sent.inputId.length).toBeGreaterThan(0);
}

describe('chat actor loop over a transport runtime', () => {
  test('runs open → create_new_session → send → facts → close', async () => {
    const stub = createStubChatRuntime();
    const harness = createChatHarness(stub.runtime);

    const sessionId = await openAndCreate(harness, 'req_create_1');
    expect(sessionId.startsWith('stub_session_')).toBe(true);

    await send(harness, 'req_send_1', 'hello');
    const facts = await waitForFacts(
      harness.actions,
      harness.context,
      (current) => agentChunkTexts(current).includes('reply: hello'),
      'the assistant reply',
    );

    expect(facts.binding).not.toBeNull();
    expect(facts.overflow).toBe(false);
    expect(agentChunkTexts(facts)).toEqual(['reply: hello']);
    expect(noticeMethods(facts.envelopes)).toContain('peri/agent_event_done');

    await expect(harness.actions.close(harness.context)).resolves.toEqual({ closed: true });
    expect(stub.requests).toContain('session/close');
    expect(harness.databaseCloses()).toBe(1);
    expect(harness.diagnostics).toEqual([]);
  });

  test('keeps multiple turns in one activation and closes the session once', async () => {
    const stub = createStubChatRuntime();
    const harness = createChatHarness(stub.runtime);

    await openAndCreate(harness, 'req_create_1');
    await send(harness, 'req_send_1', 'first');
    await waitForFacts(
      harness.actions,
      harness.context,
      (current) => agentChunkTexts(current).includes('reply: first'),
      'the first reply',
    );
    await send(harness, 'req_send_2', 'second');
    const facts = await waitForFacts(
      harness.actions,
      harness.context,
      (current) => agentChunkTexts(current).includes('reply: second'),
      'the second reply',
    );

    expect(agentChunkTexts(facts)).toEqual(['reply: first', 'reply: second']);

    await expect(harness.actions.close(harness.context)).resolves.toEqual({ closed: true });
    expect(stub.requests.filter((method) => method === 'session/close')).toHaveLength(1);
    expect(harness.databaseCloses()).toBe(1);
    expect(harness.diagnostics).toEqual([]);
  });

  test('reports close_incomplete and keeps the activation retryable when session/close rejects', async () => {
    const stub = createStubChatRuntime({ failClose: true });
    const harness = createChatHarness(stub.runtime);

    await openAndCreate(harness, 'req_create_1');

    for (let attempt = 1; attempt <= 2; attempt += 1) {
      let failure: unknown;
      try {
        await harness.actions.close(harness.context);
      } catch (error) {
        failure = error;
      }
      expect(failure).toBeInstanceOf(UserError);
      expect((failure as UserError).code).toBe('close_incomplete');
      expect((failure as UserError).metadata).toMatchObject({ incomplete: true });
      expect(stub.requests.filter((method) => method === 'session/close')).toHaveLength(attempt);
      expect(harness.context.vars.chat).not.toBeNull();
    }
    expect(harness.databaseCloses()).toBe(0);
  });

  for (const hook of ['sleep', 'destroy'] as const) {
    test(`${hook} keeps the real SDK handle retryable when session/close rejects`, async () => {
      const options = { failClose: true };
      const stub = createStubChatRuntime(options);
      const harness = createChatHarness(stub.runtime);

      await openAndCreate(harness, `req_create_${hook}`);
      const retained = harness.context.vars.chat;

      const failure = await harness[hook]().catch((error: unknown) => error);
      expect(failure).toBeInstanceOf(UserError);
      expect((failure as UserError).code).toBe('close_incomplete');
      expect((failure as UserError).metadata).toMatchObject({ incomplete: true });
      // The SDK does not retry session/close internally: one hook call is one request.
      expect(stub.requests.filter((method) => method === 'session/close')).toHaveLength(1);
      expect(harness.context.vars.chat).toBe(retained);
      expect(harness.context.vars.database).not.toBeNull();
      expect(harness.databaseCloses()).toBe(0);

      options.failClose = false;
      await harness[hook]();
      expect(stub.requests.filter((method) => method === 'session/close')).toHaveLength(2);
      expect(harness.databaseCloses()).toBe(1);
      expect(harness.context.vars).toEqual({ chat: null, database: null, open: null });
    });
  }

  test('sleep keeps a confirmed session close and retries only the database', async () => {
    const stub = createStubChatRuntime();
    const harness = createChatHarness(stub.runtime, 'chat_stub_1', { databaseCloseFailures: 1 });

    await openAndCreate(harness, 'req_create_db');

    await expect(harness.sleep()).rejects.toThrow('database close failed');
    expect(stub.requests.filter((method) => method === 'session/close')).toHaveLength(1);
    expect(harness.databaseCloses()).toBe(1);
    expect(harness.context.vars.chat).not.toBeNull();
    expect(harness.context.vars.database).not.toBeNull();

    await harness.sleep();
    expect(stub.requests.filter((method) => method === 'session/close')).toHaveLength(1);
    expect(harness.databaseCloses()).toBe(2);
    expect(harness.context.vars).toEqual({ chat: null, database: null, open: null });
  });

  test('a failed sleep and a queued close serialize without deadlocking', async () => {
    const stub = createStubChatRuntime({ failCloseCount: 1 });
    const harness = createChatHarness(stub.runtime);
    await openAndCreate(harness, 'req_create_queue');

    const sleeping = harness.sleep().catch((error: unknown) => error);
    const closing = harness.actions.close(harness.context);
    const [sleepFailure, closed] = await Promise.all([sleeping, closing]);

    expect(sleepFailure).toBeInstanceOf(UserError);
    expect((sleepFailure as UserError).code).toBe('close_incomplete');
    expect(closed).toEqual({ closed: true });
    expect(stub.requests.filter((method) => method === 'session/close')).toHaveLength(2);
    expect(harness.databaseCloses()).toBe(1);
    expect(harness.context.vars).toEqual({ chat: null, database: null, open: null });
    await expect(harness.actions.close(harness.context)).resolves.toEqual({ closed: true });
  });
});
