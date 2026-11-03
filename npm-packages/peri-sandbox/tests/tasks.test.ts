import test from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { PassThrough } from 'node:stream';
import { ProtocolError } from '@modelcontextprotocol/client';
import { PeriBackend } from '../src/runtime/backend.js';
import { SandboxError } from '../src/instance/types.js';
import type { ChildProcessWithoutNullStreams } from 'node:child_process';

function makeBackend(bashLimit = 4): PeriBackend {
  const process = Object.assign(new EventEmitter(), {
    stdin: new PassThrough(),
    stdout: new PassThrough(),
    stderr: new PassThrough(),
  }) as unknown as ChildProcessWithoutNullStreams;
  return new PeriBackend({ id: 'sbx_test', runId: 'run_one', process, bashLimit });
}

test('requests and task results are forwarded verbatim; reserved meta is replaced by the bridge', async () => {
  const backend = makeBackend();
  const calls: { method: string; params: Record<string, unknown> }[] = [];
  const raw = {
    resultType: 'task',
    taskId: 'shell-internal',
    status: 'working',
    _meta: { task_id: 'shell-internal', evidence: 'preserved' },
    task: { taskId: 'shell-internal' },
    result: { _meta: { task_id: 'shell-internal' } },
  };
  backend.client.request = (async (request: { method: string; params: Record<string, unknown> }) => {
    calls.push(request);
    return raw;
  }) as typeof backend.client.request;
  const signal = new AbortController().signal;
  const created = await backend.request(
    'tools/call',
    {
      name: 'Bash',
      arguments: { command: 'sleep 5' },
      _meta: {
        'peri/taskOwner': 'owner-one',
        'peri.dev/tool-execution-v1': { id: 'x' },
        progressToken: 7,
        'io.modelcontextprotocol/protocolVersion': '2099-01-01',
      },
    },
    { signal, requestId: 1 },
  );
  assert.deepEqual(created, raw);
  assert.equal(created.taskId, 'shell-internal');
  assert.deepEqual((created._meta as { task_id: string }).task_id, 'shell-internal');
  assert.deepEqual(created.task, { taskId: 'shell-internal' });
  assert.deepEqual(calls[0].params, {
    name: 'Bash',
    arguments: { command: 'sleep 5' },
    _meta: {
      'peri/taskOwner': 'owner-one',
      'peri.dev/tool-execution-v1': { id: 'x' },
    },
  });

  const signal2 = new AbortController().signal;
  for (const method of ['tasks/get', 'tasks/update', 'tasks/cancel']) {
    await backend.request(method, { taskId: 'shell-internal', extra: true }, { signal: signal2 });
  }
  assert.deepEqual(
    calls.slice(1).map((call) => call.method),
    ['tasks/get', 'tasks/update', 'tasks/cancel'],
  );
  assert.deepEqual(calls[2].params, { taskId: 'shell-internal', extra: true });

  backend.client.request = (async (request: { method: string; params: Record<string, unknown> }) => {
    calls.push(request);
    return { resources: [] };
  }) as typeof backend.client.request;
  assert.deepEqual(await backend.request('resources/list', { cursor: 'c' }, { signal }), {
    resources: [],
  });
  await backend.request('skills/list', {}, { signal });
  assert.deepEqual(
    calls.slice(-2).map((call) => call.method),
    ['resources/list', 'skills/list'],
  );
});

test('inner protocol errors keep code, message and data', async () => {
  const backend = makeBackend();
  backend.client.request = (async () => {
    throw ProtocolError.fromError(-32602, 'bad resource', { uri: 'skill://x' });
  }) as typeof backend.client.request;
  await assert.rejects(
    () => backend.request('resources/read', { uri: 'skill://x' }, { signal: new AbortController().signal }),
    (error: unknown) =>
      error instanceof ProtocolError &&
      error.code === -32602 &&
      error.message === 'bad resource' &&
      JSON.stringify(error.data) === JSON.stringify({ uri: 'skill://x' }),
  );
});

test('Bash budget is enforced and other tools are not counted', async () => {
  const backend = makeBackend(1);
  backend.client.request = (async () => ({ resultType: 'complete' })) as typeof backend.client.request;
  const signal = new AbortController().signal;
  await backend.request('tools/call', { name: 'Read', arguments: { file_path: '/tmp/a' } }, { signal });
  await backend.request('tools/call', { name: 'Bash', arguments: { command: 'pwd' } }, { signal });
  await assert.rejects(
    () => backend.request('tools/call', { name: 'Bash', arguments: { command: 'pwd' } }, { signal }),
    (error: unknown) => error instanceof SandboxError && error.code === 'limit_exceeded',
  );
});

test('concurrency buckets limit tools/call, tasks and other methods independently', async () => {
  const backend = makeBackend(100);
  const pending: Array<(value: Record<string, unknown>) => void> = [];
  backend.client.request = ((request: { method: string }) =>
    new Promise<Record<string, unknown>>((resolve) => {
      pending.push(resolve);
    })) as typeof backend.client.request;
  const signal = new AbortController().signal;
  const calls = Array.from({ length: 8 }, (_, index) =>
    backend.request('tools/call', { name: 'Read', arguments: { index } }, { signal, requestId: index }),
  );
  await assert.rejects(
    () => backend.request('tools/call', { name: 'Read', arguments: {} }, { signal }),
    (error: unknown) => error instanceof SandboxError && error.code === 'limit_exceeded',
  );
  for (let index = 0; index < 4; index++)
    void backend.request('tasks/get', { taskId: String(index) }, { signal, requestId: 100 + index });
  await assert.rejects(
    () => backend.request('tasks/get', { taskId: 'x' }, { signal }),
    (error: unknown) => error instanceof SandboxError && error.code === 'limit_exceeded',
  );
  for (let index = 0; index < 32; index++)
    void backend.request('resources/list', {}, { signal, requestId: 200 + index });
  await assert.rejects(
    () => backend.request('prompts/list', {}, { signal }),
    (error: unknown) => error instanceof SandboxError && error.code === 'limit_exceeded',
  );
  for (const resolve of pending) resolve({ resultType: 'complete' });
  await Promise.all(calls);
});

test('host cancellation aborts only the matching inner request with -32800', async () => {
  const backend = makeBackend();
  const signals: AbortSignal[] = [];
  backend.client.request = ((_request: unknown, _schema: unknown, options: { signal: AbortSignal }) => {
    signals.push(options.signal);
    return new Promise<Record<string, unknown>>((_resolve, reject) => {
      options.signal.addEventListener('abort', () => reject(options.signal.reason));
    });
  }) as typeof backend.client.request;
  const signal = new AbortController().signal;
  const cancelled = backend.request('tools/call', { name: 'Bash', arguments: {} }, { signal, requestId: 42 });
  const other = backend.request('tools/call', { name: 'Read', arguments: {} }, { signal, requestId: 43 });
  backend.cancel(42);
  await assert.rejects(
    () => cancelled,
    (error: unknown) => error instanceof ProtocolError && error.code === -32800,
  );
  assert.equal(signals[0].aborted, true);
  assert.equal(signals[1].aborted, false);
  backend.cancel(43);
  await assert.rejects(
    () => other,
    (error: unknown) => error instanceof ProtocolError,
  );
});

test('concurrent requests use independent signals and progress callbacks', async () => {
  const backend = makeBackend(3);
  const pending: {
    signal: AbortSignal;
    onprogress?: (progress: { progress: number }) => void;
    resolve: (value: Record<string, unknown>) => void;
  }[] = [];
  backend.client.request = ((
    _request: unknown,
    _schema: unknown,
    options: { signal: AbortSignal; onprogress?: (progress: { progress: number }) => void },
  ) => new Promise((resolve) => pending.push({ ...options, resolve }))) as typeof backend.client.request;
  const first = new AbortController();
  const second = new AbortController();
  const firstProgress: number[] = [];
  const secondProgress: number[] = [];
  const call = { name: 'Bash', arguments: { command: 'sleep 5' } };
  const one = backend.request('tools/call', call, {
    signal: first.signal,
    onProgress: (progress) => firstProgress.push(progress.progress),
  });
  const two = backend.request('tools/call', call, {
    signal: second.signal,
    onProgress: (progress) => secondProgress.push(progress.progress),
  });
  assert.equal(pending.length, 2);
  first.abort();
  assert.equal(pending[0].signal.aborted, true);
  assert.equal(pending[1].signal.aborted, false);
  pending[1].onprogress?.({ progress: 2 });
  pending[0].resolve({ resultType: 'complete' });
  pending[1].resolve({ resultType: 'complete' });
  await Promise.all([one, two]);
  assert.deepEqual(firstProgress, []);
  assert.deepEqual(secondProgress, [2]);
});

test('subscribe forwards the filter verbatim and routes only matching notifications', async () => {
  const backend = makeBackend();
  let honored: unknown;
  let closed = 0;
  let settle: (outcome: 'local' | 'graceful' | 'remote') => void = () => {};
  const filter = {
    toolsListChanged: true,
    resourceSubscriptions: ['skill://demo/SKILL.md'],
    taskIds: ['shell-1'],
    customExtension: true,
  };
  backend.client.listen = (async (requested: unknown) => {
    honored = requested;
    const closedPromise = new Promise<'local' | 'graceful' | 'remote'>((resolve) => {
      settle = resolve;
    });
    return {
      honoredFilter: requested,
      close: async () => {
        closed += 1;
        settle('local');
      },
      closed: closedPromise,
    };
  }) as typeof backend.client.listen;
  const received: Array<{ method: string; params?: Record<string, unknown> }> = [];
  const subscription = await backend.subscribe(filter, {
    signal: new AbortController().signal,
    requestId: 'listen-1',
    sink: (notification) => received.push(notification),
  });
  assert.deepEqual(honored, filter);
  assert.deepEqual(subscription.honoredFilter, filter);
  const notify = (method: string, params?: Record<string, unknown>) =>
    backend.client.fallbackNotificationHandler?.({ method, params } as never);

  await notify('notifications/tools/list_changed');
  await notify('notifications/resources/updated', { uri: 'skill://demo/SKILL.md' });
  await notify('notifications/resources/updated', { uri: 'skill://other/SKILL.md' });
  await notify('notifications/tasks/status', { task: { taskId: 'shell-1' } });
  await notify('notifications/tasks/status', { task: { taskId: 'shell-2' } });
  await notify('notifications/custom/extra', { value: 1 });
  await notify('notifications/subscriptions/acknowledged', { notifications: {} });
  await notify('notifications/cancelled', { requestId: 'listen:0' });
  assert.deepEqual(
    received.map((notification) => notification.method),
    [
      'notifications/tools/list_changed',
      'notifications/resources/updated',
      'notifications/tasks/status',
      'notifications/custom/extra',
    ],
  );
  assert.deepEqual(received[1].params, { uri: 'skill://demo/SKILL.md' });
  assert.deepEqual(received[2].params, { task: { taskId: 'shell-1' } });
  backend.cancel('listen-1');
  assert.equal(closed, 1);
  await subscription.closed;
});
