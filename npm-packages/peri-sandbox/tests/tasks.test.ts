import test from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { PassThrough } from 'node:stream';
import { PeriBackend } from '../src/runtime/backend.js';
import type { ChildProcessWithoutNullStreams } from 'node:child_process';

test('task IDs are opaque, stable per owner, and reversed only on the owning backend', async () => {
  const process = Object.assign(new EventEmitter(), {
    stdin: new PassThrough(),
    stdout: new PassThrough(),
    stderr: new PassThrough(),
  }) as unknown as ChildProcessWithoutNullStreams;
  const backend = new PeriBackend({ id: 'sbx_test', runId: 'run_old', process, bashLimit: 4 });
  const calls: unknown[] = [];
  let result: Record<string, unknown> = {
    resultType: 'task',
    taskId: 'internal',
    status: 'working',
    _meta: { task_id: 'internal', evidence: 'preserved' },
    task: { taskId: 'internal' },
  };
  backend.client.request = (async (request: unknown) => {
    calls.push(request);
    return result;
  }) as typeof backend.client.request;
  const signal = new AbortController().signal;
  const created = await backend.forward(
    'tools/call',
    { name: 'Bash', arguments: { command: 'sleep 5' } },
    'owner-one',
    signal,
  );
  const handle = created.taskId as string;
  assert.notEqual(handle, 'internal');
  assert.match(handle, /^task_[0-9a-f]{48}$/);
  assert.equal((created._meta as { task_id: string }).task_id, handle);
  assert.equal((created._meta as { evidence: string }).evidence, 'preserved');
  assert.deepEqual(created.task, { taskId: handle });
  result = {
    resultType: 'complete',
    taskId: 'internal',
    status: 'completed',
    result: { isError: true, structuredContent: { exitCode: 7 } },
  };
  const completed = await backend.forward('tasks/get', { taskId: handle }, 'owner-one', signal);
  assert.equal(completed.taskId, handle);
  assert.equal(completed.status, 'completed');
  assert.equal((completed.result as { isError: boolean }).isError, true);
  assert.deepEqual((calls[1] as { params: { taskId: string } }).params.taskId, 'internal');
  result = { resultType: 'task', taskId: 'internal', status: 'working' };
  const otherOwner = await backend.forward(
    'tools/call',
    { name: 'Read', arguments: { file_path: '/tmp/a' } },
    'owner-two',
    signal,
  );
  assert.notEqual(otherOwner.taskId, handle);
  result = { resultType: 'complete', taskId: 'internal', status: 'completed' };
  await assert.rejects(() => backend.forward('tasks/cancel', { taskId: handle }, 'owner-two', signal), /unknown task/);
  assert.equal(calls.length, 3);
  const update = await backend.forward('tasks/update', { taskId: handle }, 'owner-one', signal);
  assert.deepEqual(update, { resultType: 'complete' });
  assert.equal((calls[3] as { method: string }).method, 'tasks/get');
  assert.equal((calls[3] as { params: { taskId: string } }).params.taskId, 'internal');
  result = { resultType: 'complete', taskId: 'internal', status: 'input_required' };
  await assert.rejects(
    () => backend.forward('tasks/get', { taskId: handle }, 'owner-one', signal),
    /unsupported backend input_required/,
  );
  await assert.rejects(
    () => backend.forward('tasks/update', { taskId: handle }, 'owner-one', signal),
    /unsupported backend input_required/,
  );
});

test('allowlists and Bash budget reject before dispatch while task operations remain available', async () => {
  const process = Object.assign(new EventEmitter(), {
    stdin: new PassThrough(),
    stdout: new PassThrough(),
    stderr: new PassThrough(),
  }) as unknown as ChildProcessWithoutNullStreams;
  const backend = new PeriBackend({ id: 'sbx_test', runId: 'run_one', process, bashLimit: 1 });
  const calls: { method: string; params: Record<string, unknown> }[] = [];
  backend.client.request = (async (request: { method: string; params: Record<string, unknown> }) => {
    calls.push(request);
    return request.method === 'tools/call'
      ? { resultType: 'task', taskId: 'raw', status: 'working' }
      : { taskId: 'raw', status: 'completed', result: { isError: false } };
  }) as typeof backend.client.request;
  const signal = new AbortController().signal;
  const call = { name: 'Bash', arguments: { command: 'sleep 5' } };
  await assert.rejects(
    () => backend.forward('tools/call', { ...call, unknownField: 5 }, undefined, signal),
    /unsupported request field/,
  );
  await assert.rejects(() => backend.forward('tools/call', call, ' ', signal), /invalid peri\/taskOwner/);
  await assert.rejects(() => backend.forward('tools/list', {}, undefined, signal), /unsupported method/);
  const created = await backend.forward(
    'tools/call',
    { ...call, _meta: { progressToken: 5, 'peri/taskOwner': 'spoof' } },
    'owner',
    signal,
  );
  assert.deepEqual(calls[0].params._meta, { 'peri/taskOwner': 'owner' });
  await assert.rejects(() => backend.forward('tools/call', call, 'owner', signal), /budget exhausted/);
  await assert.rejects(
    () => backend.forward('tasks/get', { taskId: created.taskId, other: true }, 'owner', signal),
    /unsupported request field/,
  );
  await assert.rejects(
    () => backend.forward('tasks/get', { taskId: created.taskId }, undefined, signal),
    /unknown task/,
  );
  const queried = await backend.forward('tasks/get', { taskId: created.taskId }, 'owner', signal);
  assert.deepEqual(queried.result, { isError: false });
  await backend.forward('tasks/cancel', { taskId: created.taskId }, 'owner', signal);
  assert.deepEqual(
    calls.map((call) => call.method),
    ['tools/call', 'tasks/get', 'tasks/cancel'],
  );
  assert.equal(calls[2].params.taskId, 'raw');
});

test('concurrent requests use independent signals and progress callbacks', async () => {
  const process = Object.assign(new EventEmitter(), {
    stdin: new PassThrough(),
    stdout: new PassThrough(),
    stderr: new PassThrough(),
  }) as unknown as ChildProcessWithoutNullStreams;
  const backend = new PeriBackend({ id: 'sbx_test', runId: 'run_one', process, bashLimit: 3 });
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
  const one = backend.forward('tools/call', call, undefined, first.signal, (progress) =>
    firstProgress.push(progress.progress),
  );
  const two = backend.forward('tools/call', call, undefined, second.signal, (progress) =>
    secondProgress.push(progress.progress),
  );
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
