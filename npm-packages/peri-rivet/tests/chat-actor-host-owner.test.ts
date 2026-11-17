import { describe, expect, test } from 'bun:test';
import { type ChatConfig, ChatUserAction } from '@peri-code/sdk';
import { UserError } from 'rivetkit';
import { createHostResourceOwner } from '../src/rivet/host-resource-owner.js';
import { type ChatOpenResult, createMemoryChatOwnership } from '../src/rivet/index.js';
import { chatTarget, createChatHarness, createStubChatRuntime } from './support/chat-stub.js';

const CHAT_ID = 'chat_host_owner_1';

function closedVars(): { chat: null; database: null; open: null } {
  return { chat: null, database: null, open: null };
}

async function expectUserError(promise: Promise<unknown>): Promise<UserError> {
  const raised = await promise.catch((error: unknown) => error);
  expect(raised).toBeInstanceOf(UserError);
  return raised as UserError;
}

async function openAndCreate(harness: ReturnType<typeof createChatHarness>): Promise<void> {
  await harness.actions.open(harness.context, chatTarget(CHAT_ID));
  await harness.actions.execute(
    harness.context,
    ChatUserAction.createNewSession({ requestId: `${CHAT_ID}:create` }),
  );
}

describe('chat actor host owner', () => {
  test('a failed sleep keeps the SDK handle after vars are replaced and blocks the next open', async () => {
    const stub = createStubChatRuntime({ failCloseCount: 2 });
    const owner = createHostResourceOwner();
    const harness = createChatHarness(stub.runtime, CHAT_ID, { owner, closeDrainTimeoutMs: 1 });
    await openAndCreate(harness);

    const sleepError = await expectUserError(harness.sleep());
    expect(sleepError.code).toBe('close_incomplete');
    expect(harness.databaseCloses()).toBe(0);
    const oldVars = harness.context.vars;
    const pendingAfterSleep = owner.pending()[0];
    if (pendingAfterSleep === undefined) throw new Error('the failed sleep did not retain a host record');
    expect(pendingAfterSleep.activationRef).not.toBe(oldVars);

    // RivetKit clears activation vars in its hook finally; the host record must remain reachable.
    harness.context.vars = closedVars();
    const opensBefore = harness.databaseOpens();
    const blocked = await expectUserError(harness.actions.open(harness.context, chatTarget(CHAT_ID)));
    expect(blocked.code).toBe('close_incomplete');
    expect(harness.databaseOpens()).toBe(opensBefore);
    expect(owner.pending()).toHaveLength(1);

    // Host retry uses the opaque pending handle, never the discarded vars object.
    const pending = owner.pending()[0];
    if (pending === undefined) throw new Error('the failed open did not retain a host record');
    const lease = owner.lease(pending.activationRef);
    if (lease === undefined) throw new Error('the old activation is no longer owned by the host');
    await lease.close();
    expect(owner.pending()).toHaveLength(0);
    expect(harness.databaseCloses()).toBe(1);

    const reopened = (await harness.actions.open(harness.context, chatTarget(CHAT_ID))) as ChatOpenResult;
    expect(reopened.reused).toBe(false);
    expect(harness.databaseOpens()).toBe(opensBefore + 1);
    await harness.actions.close(harness.context);
  });

  test('closeAll retries an incomplete SDK close within its attempt bound', async () => {
    const stub = createStubChatRuntime({ failCloseCount: 2 });
    const owner = createHostResourceOwner();
    const harness = createChatHarness(stub.runtime, CHAT_ID, { owner, closeDrainTimeoutMs: 1 });
    await openAndCreate(harness);

    const sleepError = await expectUserError(harness.sleep());
    expect(sleepError.code).toBe('close_incomplete');
    expect(harness.databaseCloses()).toBe(0);

    // The first closeAll round consumes the second failed session/close; the second round confirms.
    const report = await owner.closeAll({ timeoutMs: 1_000, maxAttempts: 2 });
    expect(report.complete).toBe(true);
    expect(report.pending).toBe(0);
    expect(harness.databaseCloses()).toBe(1);
    expect(owner.pending()).toHaveLength(0);
  });

  test('a database-only failure keeps the chat-closed progress and retries only the database', async () => {
    const stub = createStubChatRuntime();
    const owner = createHostResourceOwner();
    const harness = createChatHarness(stub.runtime, CHAT_ID, {
      owner,
      databaseCloseFailures: 1,
      closeDrainTimeoutMs: 1,
    });
    await openAndCreate(harness);

    await expect(harness.sleep()).rejects.toThrow('database close failed');
    expect(harness.databaseCloses()).toBe(1);
    expect(stub.requests.filter((method) => method === 'session/close')).toHaveLength(1);
    expect(owner.pending()).toHaveLength(1);

    await harness.actions.close(harness.context);
    expect(harness.databaseCloses()).toBe(2);
    expect(stub.requests.filter((method) => method === 'session/close')).toHaveLength(1);
    expect(harness.context.vars).toEqual(closedVars());
    expect(owner.pending()).toHaveLength(0);
  });

  test('a failed construction whose database close also fails stays host-owned and retryable', async () => {
    const owner = createHostResourceOwner();
    const invalidRuntime = {} as ChatConfig['runtime'];
    const harness = createChatHarness(invalidRuntime, CHAT_ID, {
      owner,
      databaseCloseFailures: 1,
    });

    const rejected = await harness.actions.open(harness.context, chatTarget(CHAT_ID)).catch((error) => error);
    expect(rejected).toBeInstanceOf(TypeError);
    expect(harness.databaseOpens()).toBe(1);
    expect(harness.databaseCloses()).toBe(1);
    expect(harness.context.vars).toEqual(closedVars());
    expect(owner.pending()).toHaveLength(1);
    const pending = owner.pending()[0];
    if (pending === undefined) throw new Error('the failed construction is not host-owned');
    expect(pending.activationRef).not.toBe(harness.context.vars);
    expect(owner.lease(pending.activationRef)).toBeDefined();

    const report = await owner.closeAll({ timeoutMs: 1_000, maxAttempts: 2 });
    expect(report.complete).toBe(true);
    expect(harness.databaseCloses()).toBe(2);
    expect(owner.pending()).toHaveLength(0);
  });

  test('beginShutdown during a waiting replacement open creates no database or ownership reset', async () => {
    const owner = createHostResourceOwner();
    const key = [CHAT_ID];
    let finishOld: (() => void) | undefined;
    let closeStarted: (() => void) | undefined;
    const oldCloseBegan = new Promise<void>((resolve) => {
      closeStarted = resolve;
    });
    await owner.open(key, {}, async (track) => {
      track({
        close: () =>
          new Promise<void>((resolve) => {
            finishOld = resolve;
            closeStarted?.();
          }),
      });
      return 'old-activation';
    });

    const backing = createMemoryChatOwnership();
    let resets = 0;
    const ownership = {
      ...backing,
      getItem: async (name: string): Promise<unknown> => {
        resets += 1;
        return await backing.getItem(name);
      },
    };
    const stub = createStubChatRuntime();
    const harness = createChatHarness(stub.runtime, CHAT_ID, { owner, ownership });

    const opening = harness.actions
      .open(harness.context, chatTarget(CHAT_ID))
      .catch((error: unknown) => error);
    // Deterministic barrier: the replacement open has reached the old lease close and is waiting.
    await oldCloseBegan;
    owner.beginShutdown();
    finishOld?.();
    const rejected = await opening;

    expect(rejected).toBeInstanceOf(UserError);
    expect((rejected as UserError).code).toBe('closed');
    expect(harness.databaseOpens()).toBe(0);
    expect(resets).toBe(0);
    expect(harness.context.vars).toEqual(closedVars());

    const report = await owner.closeAll({ timeoutMs: 1_000 });
    expect(report.complete).toBe(true);
    expect(report.pending).toBe(0);
    expect(owner.pending()).toHaveLength(0);
  });

  test('beginShutdown closes execute admission but an explicit close still confirms resources', async () => {
    const stub = createStubChatRuntime();
    const owner = createHostResourceOwner();
    const harness = createChatHarness(stub.runtime, CHAT_ID, { owner });
    await openAndCreate(harness);

    owner.beginShutdown();
    const rejected = await expectUserError(
      harness.actions.execute(
        harness.context,
        ChatUserAction.send({ text: 'after shutdown', requestId: `${CHAT_ID}:send` }),
      ),
    );
    expect(rejected.code).toBe('closed');
    const openRejected = await expectUserError(harness.actions.open(harness.context, chatTarget(CHAT_ID)));
    expect(openRejected.code).toBe('closed');
    expect(harness.diagnostics).toHaveLength(0);

    await harness.actions.close(harness.context);
    expect(harness.databaseCloses()).toBe(1);
    expect(harness.context.vars).toEqual(closedVars());
    const report = await owner.closeAll({ timeoutMs: 1_000 });
    expect(report.complete).toBe(true);
  });

  test('a retired activation cannot reopen the chat after its confirmed close removed the record', async () => {
    const stub = createStubChatRuntime();
    const owner = createHostResourceOwner();
    const harness = createChatHarness(stub.runtime, CHAT_ID, { owner });
    await openAndCreate(harness);
    const opensBefore = harness.databaseOpens();

    owner.retire(harness.context.vars);
    await expect(harness.actions.close(harness.context)).resolves.toEqual({ closed: true });
    expect(harness.context.vars).toEqual(closedVars());
    expect(owner.pending()).toHaveLength(0);

    const rejected = await expectUserError(harness.actions.open(harness.context, chatTarget(CHAT_ID)));
    expect(rejected.code).toBe('closed');
    // The owner refuses before any ownership reset, database or SDK handle is built.
    expect(harness.databaseOpens()).toBe(opensBefore);
    expect(owner.pending()).toHaveLength(0);
  });
});
