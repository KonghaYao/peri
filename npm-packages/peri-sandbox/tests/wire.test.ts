import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { Readable } from 'node:stream';
import { Client, StreamableHTTPClientTransport } from '@modelcontextprotocol/client';
import { Server, createMcpHandler } from '@modelcontextprotocol/server';
import { z } from 'zod';
import { EventEmitter } from 'node:events';
import { PassThrough } from 'node:stream';
import { PeriBackend } from '../src/runtime/backend.js';
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
  stdout.write('x'.repeat(8 * 1024 * 1024 + 1));
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
    () => transport.send({ jsonrpc: '2.0', method: 'large', params: { data: 'x'.repeat(8 * 1024 * 1024) } }),
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
