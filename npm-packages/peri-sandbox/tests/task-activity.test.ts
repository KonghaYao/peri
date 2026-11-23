import assert from 'node:assert/strict';
import test from 'node:test';
import { ProtocolError } from '@modelcontextprotocol/client';
import { TaskActivity } from '../src/runtime/task-activity.js';

type Payload = Record<string, unknown>;
const flush = () => new Promise<void>((resolve) => setImmediate(resolve));

test('task status pins the run across clients; acknowledgments and unrelated results cannot settle it', () => {
  let settled = 0;
  const activity = new TaskActivity(
    async () => ({}),
    () => settled++,
  );
  activity.observe('tools/call', {}, { resultType: 'task', taskId: 'one', status: 'working' });
  activity.observe('tools/call', {}, { resultType: 'task', task: { taskId: 'two', status: 'input_required' } });
  activity.observe('tasks/cancel', { taskId: 'one' }, { resultType: 'complete' });
  activity.observe('tasks/get', { taskId: 'one' }, { taskId: 'two', status: 'failed' });
  activity.observe('tools/call', {}, { resultType: 'complete', taskId: 'one', status: 'completed' });
  activity.observe('tasks/get', { taskId: 'one' }, { resultType: 'complete', taskId: 'one', status: 'completed' });
  assert.equal(activity.busy(), true);
  assert.equal(settled, 0);
  activity.observe('notifications/tasks', {}, { taskId: 'two', status: 'cancelled' });
  assert.equal(activity.busy(), false);
  assert.equal(settled, 1);
});

test('orphaned tasks poll with the initiating owner, provider interval, single-flight and fair retries', async () => {
  let now = 0;
  let settled = 0;
  const calls: Payload[] = [];
  const replies: Array<(value: Payload) => void> = [];
  const activity = new TaskActivity(
    async (method, params) => {
      assert.equal(method, 'tasks/get');
      calls.push(params);
      if (calls.length === 2) throw new Error('temporary transport failure');
      if (calls.length === 4) throw ProtocolError.fromError(-32602, 'unknown task');
      return new Promise((resolve) => replies.push(resolve));
    },
    () => settled++,
    () => now,
  );
  for (const [id, owner] of [
    ['a', 'session-A'],
    ['b', 'session-B'],
  ]) {
    activity.observe(
      'tools/call',
      { _meta: { 'peri/taskOwner': owner, 'peri/threadOwner': owner, secret: 'discard' } },
      { resultType: 'task', taskId: id, status: 'working', pollIntervalMs: 2000 },
    );
  }
  now = 1999;
  assert.equal(activity.busy(), true);
  await flush();
  assert.equal(calls.length, 0);
  now = 3000;
  activity.busy();
  now = 5000;
  activity.busy();
  assert.equal(calls.length, 1);
  assert.deepEqual(calls[0], {
    taskId: 'a',
    _meta: { 'peri/taskOwner': 'session-A', 'peri/threadOwner': 'session-A' },
  });
  replies.shift()!({ taskId: 'a', status: 'input_required' });
  await flush();
  activity.busy();
  await flush();
  assert.equal(calls[1].taskId, 'b');
  now = 6000;
  activity.busy();
  replies.shift()!({ taskId: 'a', status: 'failed' });
  await flush();
  assert.equal(settled, 0);
  now = 7000;
  activity.busy();
  await flush();
  assert.equal(calls[3].taskId, 'b');
  assert.equal(settled, 1);
  assert.equal(activity.busy(), false);
});

test('interrupted Bash startup reconciles all owners and ignores a stale empty snapshot', async () => {
  let now = 0;
  let settled = 0;
  const replies: Array<(value: Payload) => void> = [];
  const activity = new TaskActivity(
    async (method, params) => {
      assert.equal(method, 'tools/call');
      assert.deepEqual(params, { name: 'shell_task_list', arguments: {} });
      return new Promise((resolve) => replies.push(resolve));
    },
    () => settled++,
    () => now,
  );
  const settle = async (list: unknown) => {
    replies.shift()!({ content: [{ type: 'text', text: JSON.stringify(list) }] });
    await flush();
    now += 1000;
  };
  activity.interruptedBash();
  activity.busy();
  activity.interruptedBash();
  await settle({ unknown: false, tasks: [] });
  for (const list of [
    { unknown: true, tasks: [] },
    { unknown: false, tasks: [{ task_id: 'running' }] },
  ]) {
    assert.equal(activity.busy(), true);
    await settle(list);
  }
  assert.equal(activity.busy(), true);
  await settle({ unknown: false, tasks: [] });
  assert.equal(activity.busy(), false);
  assert.equal(settled, 1);
});

test('an interrupted launch rejoins the owner-scoped round robin with twelve other task IDs', async () => {
  let now = 0;
  let settled = 0;
  const probes: string[] = [];
  const activity = new TaskActivity(
    async (method, params) => {
      if (method === 'tools/call') {
        assert.deepEqual(params, { name: 'shell_task_list', arguments: {} });
        return {
          content: [
            {
              type: 'text',
              text: JSON.stringify({
                unknown: false,
                tasks: [{ taskId: 'orphan', owner: 'owner-orphan', threadOwner: 'thread-orphan' }],
              }),
            },
          ],
        };
      }
      assert.equal(method, 'tasks/get');
      const id = String(params.taskId);
      probes.push(id);
      if (id === 'orphan')
        assert.deepEqual(params._meta, { 'peri/taskOwner': 'owner-orphan', 'peri/threadOwner': 'thread-orphan' });
      return { taskId: id, status: id === 'held' ? 'working' : 'completed' };
    },
    () => settled++,
    () => now,
  );
  for (const id of ['held', ...Array.from({ length: 11 }, (_, n) => String(n))]) {
    activity.observe('tools/call', {}, { resultType: 'task', taskId: id, status: 'working' });
  }
  activity.interruptedBash();
  for (let tick = 0; tick < 14; tick++) {
    now += 1000;
    // Several overlapping sweeps within each tick must still send one request.
    for (let i = 0; i < 20; i++) assert.equal(activity.busy(), true);
    await flush();
  }
  assert.deepEqual(probes, ['held', ...Array.from({ length: 11 }, (_, n) => String(n)), 'orphan']);
  assert.equal(settled, 0);
  activity.observe('notifications/tasks', {}, { taskId: 'held', status: 'completed' });
  assert.equal(activity.busy(), false);
});

test('late status requests and notifications cannot retire a reused task ID', async () => {
  let now = 0;
  let settleProbe!: (result: Payload) => void;
  const activity = new TaskActivity(
    async () =>
      new Promise((resolve) => {
        settleProbe = resolve;
      }),
    () => {},
    () => now,
  );
  const start = (createdAt: string) =>
    activity.observe('tools/call', {}, { resultType: 'task', taskId: 'same', status: 'working', createdAt });
  start('first');
  const oldGet = activity.observeRequest('tasks/get', { taskId: 'same' });
  now += 1000;
  activity.busy();
  activity.observe('notifications/tasks', {}, { taskId: 'same', status: 'completed', createdAt: 'first' });
  start('second');
  oldGet({ taskId: 'same', status: 'completed' });
  settleProbe({ taskId: 'same', status: 'completed' });
  await flush();
  activity.observe('notifications/tasks', {}, { taskId: 'same', status: 'failed', createdAt: 'first' });
  assert.equal(activity.busy(), true);
  activity.observeRequest('tasks/get', { taskId: 'same' })({
    taskId: 'same',
    status: 'completed',
    createdAt: 'second',
  });
  assert.equal(activity.busy(), false);
});
