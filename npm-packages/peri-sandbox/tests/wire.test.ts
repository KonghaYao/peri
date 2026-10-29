import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { Readable } from 'node:stream';
import { Client, ProtocolError, StreamableHTTPClientTransport } from '@modelcontextprotocol/client';
import type { Implementation, ServerCapabilities } from '@modelcontextprotocol/client';
import { Server, createMcpHandler } from '@modelcontextprotocol/server';
import { z } from 'zod';
import { EventEmitter } from 'node:events';
import { PassThrough } from 'node:stream';
import { PeriBackend } from '../src/runtime/backend.js';
import { startHttpServer } from '../src/server/http.js';
import type { McpBackendPort, McpBackendSubscription } from '../src/instance/types.js';
import type { ChildProcessWithoutNullStreams } from 'node:child_process';

test('stdio transport frames batched and split messages without limiting whole chunks', async () => {
  const stdout = new PassThrough();
  const process = Object.assign(new EventEmitter(), {
    stdin: new PassThrough(),
    stdout,
    stderr: new PassThrough(),
  }) as unknown as ChildProcessWithoutNullStreams;
  const backend = new PeriBackend({ id: 'sbx_test', runId: 'run_one', process, bashLimit: 2 });
  const transport = (
    backend as unknown as {
      transport: { start(): Promise<void>; onmessage?: (value: unknown) => void; onerror?: (error: Error) => void };
    }
  ).transport;
  const messages: unknown[] = [];
  const errors: string[] = [];
  transport.onmessage = (message) => messages.push(message);
  transport.onerror = (error) => errors.push(error.message);
  await transport.start();
  const line = JSON.stringify({ jsonrpc: '2.0', method: 'event', params: { text: 'x'.repeat(1024 * 1024) } }) + '\n';
  stdout.write(line + line);
  stdout.write(line.slice(0, 100));
  stdout.write(line.slice(100));
  assert.equal(messages.length, 3);
  assert.deepEqual(errors, []);
  stdout.write('x'.repeat(32 * 1024 * 1024 + 1));
  assert.match(errors[0], /message too large/);
});

test('stdio transport rejects oversized outgoing messages before writing', async () => {
  const stdin = new PassThrough();
  const process = Object.assign(new EventEmitter(), {
    stdin,
    stdout: new PassThrough(),
    stderr: new PassThrough(),
  }) as unknown as ChildProcessWithoutNullStreams;
  const backend = new PeriBackend({ id: 'sbx_test', runId: 'run_one', process, bashLimit: 2 });
  const transport = (backend as unknown as { transport: { send(message: unknown): Promise<void> } }).transport;
  let writes = 0;
  stdin.on('data', () => writes++);
  await assert.rejects(
    () => transport.send({ jsonrpc: '2.0', method: 'large', params: { data: 'x'.repeat(32 * 1024 * 1024) } }),
    /write buffer limit/,
  );
  assert.equal(writes, 0);
});

test('malformed backend JSON closes transport instead of stranding requests', async () => {
  const stdout = new PassThrough();
  const process = Object.assign(new EventEmitter(), {
    stdin: new PassThrough(),
    stdout,
    stderr: new PassThrough(),
  }) as unknown as ChildProcessWithoutNullStreams;
  const backend = new PeriBackend({ id: 'sbx_test', runId: 'run_one', process, bashLimit: 2 });
  const transport = (
    backend as unknown as {
      transport: {
        start(): Promise<void>;
        onmessage?: (message: unknown) => void;
        onclose?: () => void;
        onerror?: (error: Error) => void;
      };
    }
  ).transport;
  let closed = false;
  let failure = '';
  transport.onclose = () => {
    closed = true;
  };
  transport.onerror = (error) => {
    failure = error.message;
  };
  transport.onmessage = () => {};
  await transport.start();
  stdout.write('{invalid-json}\n');
  await new Promise((done) => setImmediate(done));
  assert.equal(closed, true);
  assert.match(failure, /invalid backend JSON-RPC/);
});

test('pinned 2026-07-28 HTTP codec preserves a native task result and call arguments', async () => {
  let argument: unknown;
  const handler = createMcpHandler(
    () => {
      const server = new Server(
        { name: 'fixture', version: '1' },
        { capabilities: { tools: {}, extensions: { 'io.modelcontextprotocol/tasks': {} } } },
      );
      server.setRequestHandler('tools/list', async () => ({
        tools: [{ name: 'Bash', inputSchema: { type: 'object' } }],
      }));
      server.setRequestHandler(
        'tools/call',
        {
          params: z.object({ name: z.string(), arguments: z.record(z.string(), z.unknown()).optional() }),
          result: z.record(z.string(), z.unknown()),
        },
        async (params) => {
          argument = params;
          return {
            resultType: 'task',
            taskId: 'raw-id',
            status: 'working',
            createdAt: '2026-10-26T00:00:00Z',
            lastUpdatedAt: '2026-10-26T00:00:00Z',
            ttlMs: null,
          };
        },
      );
      server.setRequestHandler(
        'tasks/get',
        { params: z.object({ taskId: z.string() }), result: z.record(z.string(), z.unknown()) },
        async (params) => ({ taskId: params.taskId, status: 'completed', result: { isError: true } }),
      );
      server.setRequestHandler(
        'tasks/update',
        { params: z.object({ taskId: z.string() }), result: z.record(z.string(), z.unknown()) },
        async () => ({}),
      );
      server.setRequestHandler(
        'tasks/cancel',
        { params: z.object({ taskId: z.string() }), result: z.record(z.string(), z.unknown()) },
        async () => ({}),
      );
      return server;
    },
    { legacy: 'reject' },
  );
  const http = createServer(async (req, res) => {
    const body: Buffer[] = [];
    for await (const chunk of req) body.push(Buffer.from(chunk));
    const response = await handler.fetch(
      new Request(`http://127.0.0.1${req.url}`, {
        method: req.method,
        headers: req.headers as HeadersInit,
        body: req.method === 'POST' ? Buffer.concat(body) : undefined,
      }),
    );
    res.writeHead(response.status, Object.fromEntries(response.headers.entries()));
    if (response.body) Readable.fromWeb(response.body as never).pipe(res);
    else res.end();
  });
  await new Promise<void>((resolve) => http.listen(0, '127.0.0.1', resolve));
  const address = http.address();
  assert(address && typeof address !== 'string');
  const client = new Client(
    { name: 'fixture-client', version: '1' },
    {
      capabilities: { extensions: { 'io.modelcontextprotocol/tasks': {} } },
      versionNegotiation: { mode: { pin: '2026-07-28' } },
    },
  );
  try {
    await client.connect(new StreamableHTTPClientTransport(new URL(`http://127.0.0.1:${address.port}/mcp`)));
    const result = await client.request(
      { method: 'tools/call', params: { name: 'Bash', arguments: { command: 'pwd' } } },
      z.record(z.string(), z.unknown()),
    );
    assert.deepEqual(argument, { name: 'Bash', arguments: { command: 'pwd' } });
    assert.equal(result.resultType, 'task');
    assert.equal(result.taskId, 'raw-id');
    const queried = await client.request(
      { method: 'tasks/get', params: { taskId: 'raw-id' } },
      z.record(z.string(), z.unknown()),
    );
    assert.equal(queried.taskId, 'raw-id');
    const updated = await client.request(
      { method: 'tasks/update', params: { taskId: 'raw-id' } },
      z.record(z.string(), z.unknown()),
    );
    const cancelled = await client.request(
      { method: 'tasks/cancel', params: { taskId: 'raw-id' } },
      z.record(z.string(), z.unknown()),
    );
    assert.equal(
      Object.keys(updated).every((key) => key === '_meta'),
      true,
    );
    assert.equal(
      Object.keys(cancelled).every((key) => key === '_meta'),
      true,
    );
  } finally {
    await client.close();
    await new Promise<void>((resolve) => http.close(() => resolve()));
  }
});

class FakeSandboxBackend implements McpBackendPort {
  readonly runId = 'run_test';
  capabilities: ServerCapabilities = {
    tools: {},
    resources: { subscribe: true, listChanged: true },
    prompts: { listChanged: true },
    extensions: {
      'io.modelcontextprotocol/skills': {},
      'io.modelcontextprotocol/tasks': {},
    },
  };
  info: Implementation = { name: 'inner-workspace', version: '9.9.9' };
  instructionsValue: string | undefined = 'inner instructions';
  readonly requests: Array<{
    method: string;
    params: Record<string, unknown>;
    requestId?: string | number;
  }> = [];
  readonly cancelled: Array<string | number> = [];
  readonly filters: Record<string, unknown>[] = [];
  subscriptionCloses = 0;
  private subscription?:
    | {
        sink: (notification: { method: string; params?: Record<string, unknown> }) => void;
      }
    | undefined;
  handler: (method: string, params: Record<string, unknown>) => Promise<Record<string, unknown>> = async () => ({
    resultType: 'complete',
  });
  requestImpl?:
    | ((
        method: string,
        params: Record<string, unknown>,
        options: { requestId?: string | number },
      ) => Promise<Record<string, unknown>>)
    | undefined;
  subscribeImpl?:
    | ((
        filter: Record<string, unknown>,
        options: {
          sink: (notification: { method: string; params?: Record<string, unknown> }) => void;
        },
      ) => Promise<McpBackendSubscription>)
    | undefined;
  serverCapabilities(): ServerCapabilities {
    return this.capabilities;
  }
  serverInfo(): Implementation {
    return this.info;
  }
  instructions(): string | undefined {
    return this.instructionsValue;
  }
  async request(
    method: string,
    params: Record<string, unknown>,
    options: { requestId?: string | number },
  ): Promise<Record<string, unknown>> {
    this.requests.push({ method, params, requestId: options.requestId });
    if (this.requestImpl) return this.requestImpl(method, params, options);
    return this.handler(method, params);
  }
  async notify(): Promise<void> {}
  async subscribe(
    filter: Record<string, unknown>,
    options: {
      requestId: string | number;
      sink: (notification: { method: string; params?: Record<string, unknown> }) => void;
    },
  ): Promise<McpBackendSubscription> {
    this.filters.push(filter);
    if (this.subscribeImpl) return this.subscribeImpl(filter, options);
    let settle: (outcome: 'local' | 'graceful' | 'remote') => void = () => {};
    const closed = new Promise<'local' | 'graceful' | 'remote'>((resolve) => {
      settle = resolve;
    });
    this.subscription = { sink: options.sink };
    return {
      honoredFilter: filter,
      close: async () => {
        this.subscriptionCloses += 1;
        settle('local');
      },
      closed,
    };
  }
  emit(notification: { method: string; params?: Record<string, unknown> }): void {
    this.subscription?.sink(notification);
  }
  cancel(requestId: string | number): void {
    this.cancelled.push(requestId);
    this.cancelImpl?.(requestId);
  }
  cancelImpl?: (requestId: string | number) => void;
  async close(): Promise<void> {}
}

async function sandboxEndpoint(backend: McpBackendPort): Promise<{ url: string; close(): Promise<void> }> {
  const http = await startHttpServer({
    sandboxId: 'sbx_test',
    host: '127.0.0.1',
    port: 0,
    token: 'test-token',
    state: () => 'ready',
    backend: () => backend,
  });
  return {
    url: http.url,
    close: () => http.close(),
  };
}

async function connectSandbox(url: string): Promise<Client> {
  const client = new Client(
    { name: 'sandbox-test-client', version: '1' },
    {
      capabilities: { extensions: { 'io.modelcontextprotocol/tasks': {} } },
      versionNegotiation: { mode: { pin: '2026-07-28' } },
    },
  );
  await client.connect(
    new StreamableHTTPClientTransport(new URL(url), {
      requestInit: { headers: { Authorization: 'Bearer test-token' } },
    }),
  );
  return client;
}

function request(client: Client, method: string, params?: Record<string, unknown>): Promise<Record<string, unknown>> {
  return client.request({ method, params } as never, z.record(z.string(), z.unknown()));
}

test('outer discover mirrors inner capabilities, identity and instructions', async () => {
  const backend = new FakeSandboxBackend();
  const endpoint = await sandboxEndpoint(backend);
  const client = await connectSandbox(endpoint.url);
  try {
    assert.deepEqual(client.getServerCapabilities(), backend.capabilities);
    assert.deepEqual(client.getServerVersion(), backend.info);
    assert.equal(client.getInstructions(), 'inner instructions');
  } finally {
    await client.close();
    await endpoint.close();
  }
});

test('resources, prompts, skills, tools, tasks and unknown methods are forwarded verbatim', async () => {
  const backend = new FakeSandboxBackend();
  backend.handler = async (method, params) => {
    switch (method) {
      case 'tools/list':
        return { tools: [] };
      case 'resources/list':
        return { resources: [] };
      case 'resources/read':
        return {
          contents: [{ uri: String(params.uri), mimeType: 'text/plain', text: 'ok' }],
        };
      case 'prompts/list':
        return { prompts: [] };
      case 'prompts/get':
        return { description: 'demo', messages: [] };
      case 'tasks/get':
        return {
          taskId: String(params.taskId),
          status: 'working',
          createdAt: '2026-10-26T00:00:00Z',
          lastUpdatedAt: '2026-10-26T00:00:00Z',
          ttlMs: null,
        };
      case 'tasks/update':
      case 'tasks/cancel':
        return {};
      default:
        return { echoed: method, params };
    }
  };
  const endpoint = await sandboxEndpoint(backend);
  const client = await connectSandbox(endpoint.url);
  try {
    await request(client, 'tools/list');
    await request(client, 'resources/list');
    await request(client, 'resources/read', { uri: 'skill://demo/SKILL.md' });
    await request(client, 'prompts/list');
    await request(client, 'prompts/get', { name: 'demo', arguments: { topic: 'mcp' } });
    await request(client, 'skills/list');
    await request(client, 'skills/get', { uri: 'skill://demo/SKILL.md' });
    await request(client, 'x/custom/method', { anything: [1, 2, 3] });
    await request(client, 'tasks/update', { taskId: 'shell-1', extra: true });
    await request(client, 'tasks/get', { taskId: 'shell-1' });
    await request(client, 'tasks/cancel', { taskId: 'shell-1' });
    assert.deepEqual(
      backend.requests.map((request) => request.method),
      [
        'tools/list',
        'resources/list',
        'resources/read',
        'prompts/list',
        'prompts/get',
        'skills/list',
        'skills/get',
        'x/custom/method',
        'tasks/update',
        'tasks/get',
        'tasks/cancel',
      ],
    );
    assert.deepEqual(backend.requests[2].params, { uri: 'skill://demo/SKILL.md' });
    assert.deepEqual(backend.requests[4].params, { name: 'demo', arguments: { topic: 'mcp' } });
    assert.deepEqual(backend.requests[7].params, { anything: [1, 2, 3] });
    assert.deepEqual(backend.requests[8].params, { taskId: 'shell-1', extra: true });
  } finally {
    await client.close();
    await endpoint.close();
  }
});

test('native task results are not rewritten and inner errors keep code/message/data', async () => {
  const backend = new FakeSandboxBackend();
  backend.handler = async (method) =>
    method === 'tools/call'
      ? {
          resultType: 'task',
          taskId: 'shell-<uuid>',
          status: 'working',
          createdAt: '2026-10-26T00:00:00Z',
          lastUpdatedAt: '2026-10-26T00:00:00Z',
          ttlMs: null,
          _meta: { task_id: 'shell-<uuid>' },
        }
      : { resultType: 'complete' };
  const endpoint = await sandboxEndpoint(backend);
  const client = await connectSandbox(endpoint.url);
  try {
    const created = (await request(client, 'tools/call', {
      name: 'Bash',
      arguments: { command: 'sleep 5' },
    })) as { resultType: string; taskId: string; _meta: { task_id: string } };
    assert.equal(created.resultType, 'task');
    assert.equal(created.taskId, 'shell-<uuid>');
    assert.equal(created._meta.task_id, 'shell-<uuid>');
    backend.requestImpl = async () => {
      throw ProtocolError.fromError(-32602, 'resource denied', { uri: 'file:///etc/passwd' });
    };
    await assert.rejects(
      () => request(client, 'resources/read', { uri: 'file:///etc/passwd' }),
      (error: unknown) =>
        error instanceof ProtocolError &&
        error.code === -32602 &&
        error.message === 'resource denied' &&
        JSON.stringify(error.data) === JSON.stringify({ uri: 'file:///etc/passwd' }),
    );
  } finally {
    await client.close();
    await endpoint.close();
  }
});

test('subscriptions/listen relays inner filters and notifications without rewriting', async () => {
  const backend = new FakeSandboxBackend();
  backend.subscribeImpl = async (filter, options) => {
    let settle: (outcome: 'local' | 'graceful' | 'remote') => void = () => {};
    const closed = new Promise<'local' | 'graceful' | 'remote'>((resolve) => {
      settle = resolve;
    });
    setImmediate(() => {
      options.sink({
        method: 'notifications/tasks',
        params: { task: { taskId: 'shell-1' }, status: 'working' },
      });
      options.sink({
        method: 'notifications/resources/updated',
        params: { uri: 'skill://demo/SKILL.md' },
      });
    });
    let closedOnce = false;
    return {
      honoredFilter: { ...filter },
      close: async () => {
        if (closedOnce) return;
        closedOnce = true;
        backend.subscriptionCloses += 1;
        settle('local');
      },
      closed,
    };
  };
  const endpoint = await sandboxEndpoint(backend);
  const client = await connectSandbox(endpoint.url);
  const events: Array<{ method: string; params?: Record<string, unknown> }> = [];
  client.setNotificationHandler('notifications/tasks', { params: z.object({}).passthrough() }, (params) => {
    events.push({ method: 'notifications/tasks', params: params as never });
  });
  client.setNotificationHandler('notifications/resources/updated', { params: z.object({}).passthrough() }, (params) => {
    events.push({ method: 'notifications/resources/updated', params: params as never });
  });
  try {
    const subscription = await client.listen(
      {
        toolsListChanged: true,
        resourceSubscriptions: ['skill://demo/SKILL.md'],
        taskIds: ['shell-1'],
      } as never,
      { timeout: 5000 },
    );
    assert.deepEqual(subscription.honoredFilter, {
      toolsListChanged: true,
      resourceSubscriptions: ['skill://demo/SKILL.md'],
      taskIds: ['shell-1'],
    });
    assert.deepEqual(backend.filters[0], {
      toolsListChanged: true,
      resourceSubscriptions: ['skill://demo/SKILL.md'],
      taskIds: ['shell-1'],
    });
    const deadline = Date.now() + 5000;
    while (events.length < 2 && Date.now() < deadline) await new Promise((resolve) => setTimeout(resolve, 10));
    assert.deepEqual(
      events.map((event) => event.method),
      ['notifications/tasks', 'notifications/resources/updated'],
    );
    const subscriptionIds = events.map(
      (event) =>
        (event.params?._meta as Record<string, unknown> | undefined)?.['io.modelcontextprotocol/subscriptionId'],
    );
    assert.match(String(subscriptionIds[0]), /^listen:\d+$/);
    assert.deepEqual(subscriptionIds, [subscriptionIds[0], subscriptionIds[0]]);
    await subscription.close();
    assert.equal(backend.subscriptionCloses, 1);
  } finally {
    await client.close();
    await endpoint.close();
  }
});

test('host cancellation reaches the matching inner request', async () => {
  const backend = new FakeSandboxBackend();
  let rejectInner: ((error: Error) => void) | undefined;
  backend.requestImpl = async () =>
    new Promise<Record<string, unknown>>((_resolve, reject) => {
      rejectInner = reject;
    });
  backend.cancelImpl = () => rejectInner?.(new ProtocolError(-32800, 'Request cancelled'));
  const endpoint = await sandboxEndpoint(backend);
  const client = await connectSandbox(endpoint.url);
  try {
    const pending = client.request(
      { method: 'tools/call', params: { name: 'Bash', arguments: { command: 'sleep 60' } } },
      { timeout: 30_000 },
    );
    const deadline = Date.now() + 5000;
    while (backend.requests.length === 0 && Date.now() < deadline)
      await new Promise((resolve) => setTimeout(resolve, 10));
    const hostRequestId = backend.requests[0].requestId;
    assert.notEqual(hostRequestId, undefined);
    await client.notification({
      method: 'notifications/cancelled',
      params: { requestId: hostRequestId! },
    });
    await assert.rejects(
      () => pending,
      (error: unknown) => error instanceof ProtocolError && error.code === -32800,
    );
    assert.deepEqual(backend.cancelled, [hostRequestId]);
  } finally {
    await client.close();
    await endpoint.close();
  }
});

test('long requests survive the previous 15s socket idle window', async () => {
  const backend = new FakeSandboxBackend();
  backend.handler = async () => {
    await new Promise((resolve) => setTimeout(resolve, 16_000));
    return { content: [], isError: false };
  };
  const endpoint = await sandboxEndpoint(backend);
  const client = await connectSandbox(endpoint.url);
  try {
    const begun = Date.now();
    await request(client, 'tools/call', { name: 'Bash', arguments: { command: 'sleep 15' } });
    assert(Date.now() - begun >= 15_000);
  } finally {
    await client.close();
    await endpoint.close();
  }
});

test('a 10 MiB resource result crosses the HTTP bridge', async () => {
  const backend = new FakeSandboxBackend();
  const text = 'x'.repeat(10 * 1024 * 1024);
  backend.handler = async () => ({
    contents: [{ uri: 'skill://big/SKILL.md', mimeType: 'text/markdown', text }],
  });
  const endpoint = await sandboxEndpoint(backend);
  const client = await connectSandbox(endpoint.url);
  try {
    const result = (await request(client, 'resources/read', {
      uri: 'skill://big/SKILL.md',
    })) as { contents: Array<{ text: string }> };
    assert.equal(result.contents.length, 1);
    assert.equal(result.contents[0].text.length, text.length);
  } finally {
    await client.close();
    await endpoint.close();
  }
});
