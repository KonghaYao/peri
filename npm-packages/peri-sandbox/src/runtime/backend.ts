import {
  Client,
  ProtocolError,
  type Transport,
  type JSONRPCMessage,
  type Implementation,
  type ServerCapabilities,
  type SubscriptionFilter,
} from '@modelcontextprotocol/client';
import { z } from 'zod';
import { type ChildProcessWithoutNullStreams } from 'node:child_process';
import {
  SandboxError,
  type McpBackendPort,
  type McpBackendRequestOptions,
  type McpBackendSubscription,
  type McpBackendSubscriptionOptions,
} from '../instance/types.js';
import type { DiagnosticLog } from './diagnostics.js';

const maxMessage = 32 * 1024 * 1024;
const maxDiagnostics = 32 * 1024;
const requestTimeoutMs = 60 * 60 * 1000;
const maxSubscriptions = 8;
const bashCallsPerRequest = 8;
const taskRequestsPerRequest = 4;
const otherRequestsPerRequest = 32;
const requestCancelledCode = -32800;

/**
 * Reserved `_meta` keys owned by the protocol layer. The inner `Client` emits
 * its own envelope and progress token; forwarding the host's copies would let
 * the host impersonate the bridge's protocol identity.
 */
const reservedMetaKeys = new Set([
  'io.modelcontextprotocol/protocolVersion',
  'io.modelcontextprotocol/clientCapabilities',
  'io.modelcontextprotocol/clientInfo',
  'io.modelcontextprotocol/serverInfo',
  'io.modelcontextprotocol/subscriptionId',
  'baggage',
  'traceparent',
  'tracestate',
  'progressToken',
]);

function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

/** Strip bridge-owned envelope keys, keep every other `_meta` entry verbatim. */
function forwardedParams(params: Record<string, unknown>): Record<string, unknown> {
  if (!object(params._meta)) return params;
  const kept: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(params._meta)) {
    if (!reservedMetaKeys.has(key)) kept[key] = value;
  }
  const { _meta: _ignored, ...rest } = params;
  return Object.keys(kept).length > 0 ? { ...rest, _meta: kept } : rest;
}

function requestKey(id: string | number): string {
  return `${typeof id}:${String(id)}`;
}

function taskIdOf(params: Record<string, unknown>): string | undefined {
  if (typeof params.taskId === 'string') return params.taskId;
  if (object(params.task)) {
    if (typeof params.task.taskId === 'string') return params.task.taskId;
    if (typeof params.task.task_id === 'string') return params.task.task_id;
  }
  return undefined;
}

interface ActiveSubscription {
  filter: Record<string, unknown>;
  sink: (notification: { method: string; params?: Record<string, unknown> }) => void;
  subscription?: McpBackendSubscription;
  closed: boolean;
}

interface PendingRequest {
  controller: AbortController;
  cancelled: boolean;
}

class ProcessTransport implements Transport {
  onmessage?: (message: JSONRPCMessage) => void;
  onerror?: (error: Error) => void;
  onclose?: () => void;
  private buffer = Buffer.alloc(0);
  private closed = false;
  private pendingBytes = 0;
  constructor(
    readonly process: ChildProcessWithoutNullStreams,
    private readonly diagnostics?: DiagnosticLog,
    private readonly onClosed?: () => void,
  ) {}
  private fail(error: Error): void {
    this.onerror?.(error);
    void this.close();
    this.process.kill?.('SIGTERM');
  }
  async start(): Promise<void> {
    this.process.stdout.on('data', (chunk: Buffer) => {
      if (this.closed) return;
      let offset = 0;
      while (offset < chunk.length) {
        const end = chunk.indexOf(10, offset);
        const part = chunk.subarray(offset, end < 0 ? undefined : end);
        if (this.buffer.length + part.length > maxMessage) {
          this.fail(new Error('stdio message too large'));
          return;
        }
        if (end < 0) {
          this.buffer = this.buffer.length ? Buffer.concat([this.buffer, part]) : Buffer.from(part);
          break;
        }
        const line = this.buffer.length ? Buffer.concat([this.buffer, part]) : part;
        this.buffer = Buffer.alloc(0);
        try {
          const message = JSON.parse(line.toString('utf8')) as JSONRPCMessage;
          this.onmessage?.(message);
        } catch {
          this.fail(new Error('invalid backend JSON-RPC'));
          return;
        }
        offset = end + 1;
      }
    });
    this.process.stdout.on('end', () => {
      void this.close();
    });
    this.process.stdout.on('close', () => {
      void this.close();
    });
    this.process.on('error', (error) => this.onerror?.(error));
    this.process.on('close', () => {
      void this.close();
    });
    let diagnosticBytes = 0;
    this.process.stderr.on('data', (data: Buffer) => {
      try {
        this.diagnostics?.write(data);
      } catch (error) {
        this.onerror?.(error as Error);
      }
      const remaining = maxDiagnostics - diagnosticBytes;
      if (remaining <= 0) return;
      const portion = data.subarray(0, remaining);
      diagnosticBytes += portion.length;
      if (!process.stderr.write(portion)) diagnosticBytes = maxDiagnostics;
    });
  }
  async send(message: JSONRPCMessage): Promise<void> {
    if (this.closed || !this.process.stdin.writable)
      throw new SandboxError('backend_exited', 'Sandbox backend is closed.');
    const serialized = JSON.stringify(message) + '\n';
    const size = Buffer.byteLength(serialized);
    if (size > maxMessage || this.pendingBytes + size > maxMessage * 2)
      throw new SandboxError('limit_exceeded', 'stdio write buffer limit');
    this.pendingBytes += size;
    try {
      await new Promise<void>((resolve, reject) =>
        this.process.stdin.write(serialized, (error) => (error ? reject(error) : resolve())),
      );
    } finally {
      this.pendingBytes -= size;
    }
  }
  async close(): Promise<void> {
    if (this.closed) return;
    this.closed = true;
    this.process.stdin.end();
    this.diagnostics?.close();
    this.onclose?.();
    this.onClosed?.();
  }
}

export interface PeriBackendOptions {
  id: string;
  runId: string;
  process: ChildProcessWithoutNullStreams;
  bashLimit: number;
  diagnostics?: DiagnosticLog;
  onExit?: (error: Error) => void;
}

/** MCP stdio bridge to the Peri backend running inside SRT. */
export class PeriBackend implements McpBackendPort {
  readonly client: Client;
  readonly id: string;
  readonly runId: string;
  private readonly transport: ProcessTransport;
  private readonly bashLimit: number;
  private readonly onExit?: (error: Error) => void;
  private readonly requests = new Map<string, PendingRequest>();
  private readonly subscriptions = new Map<string, McpBackendSubscription>();
  private readonly sinks = new Set<ActiveSubscription>();
  private bashCalls = 0;
  private calls = 0;
  private taskRequests = 0;
  private otherRequests = 0;
  private closing = false;
  constructor(options: PeriBackendOptions) {
    this.id = options.id;
    this.runId = options.runId;
    this.bashLimit = options.bashLimit;
    this.onExit = options.onExit;
    this.transport = new ProcessTransport(options.process, options.diagnostics, () => this.handleClosed());
    this.client = new Client(
      { name: 'peri-sandbox', version: '0.1.0' },
      {
        versionNegotiation: { mode: { pin: '2026-07-28' } },
        capabilities: { extensions: { 'io.modelcontextprotocol/tasks': {} } },
        inputRequired: { autoFulfill: false },
      },
    );
    this.client.fallbackNotificationHandler = async (notification) => {
      this.routeNotification({
        method: notification.method,
        params: object(notification.params) ? notification.params : undefined,
      });
    };
  }
  private handleClosed(): void {
    if (this.closing) return;
    this.onExit?.(new SandboxError('backend_exited', 'Sandbox backend exited unexpectedly.'));
  }
  async ready(): Promise<void> {
    await this.client.connect(this.transport);
    const capabilities = this.client.getServerCapabilities();
    if (!capabilities?.tools) throw new SandboxError('start_failed', 'backend lacks tools capability');
    const listed = await this.client.listTools();
    if (
      !listed.tools.some((tool) => tool.name === 'Bash') ||
      !listed.tools.some((tool) => tool.name === 'Read') ||
      !listed.tools.some((tool) => tool.name === 'Write')
    )
      throw new SandboxError('start_failed', 'required tools missing');
    if (!capabilities.extensions?.['io.modelcontextprotocol/tasks'])
      console.warn('sandbox backend does not declare the Tasks extension');
  }
  serverCapabilities(): ServerCapabilities | undefined {
    return this.client.getServerCapabilities();
  }
  serverInfo(): Implementation | undefined {
    return this.client.getServerVersion();
  }
  instructions(): string | undefined {
    return this.client.getInstructions();
  }
  private enter(bucket: 'call' | 'task' | 'other'): void {
    if (bucket === 'call') {
      if (this.calls >= bashCallsPerRequest) throw new SandboxError('limit_exceeded', 'concurrency limit');
      this.calls++;
    } else if (bucket === 'task') {
      if (this.taskRequests >= taskRequestsPerRequest) throw new SandboxError('limit_exceeded', 'concurrency limit');
      this.taskRequests++;
    } else {
      if (this.otherRequests >= otherRequestsPerRequest) throw new SandboxError('limit_exceeded', 'concurrency limit');
      this.otherRequests++;
    }
  }
  private leave(bucket: 'call' | 'task' | 'other'): void {
    if (bucket === 'call') this.calls--;
    else if (bucket === 'task') this.taskRequests--;
    else this.otherRequests--;
  }
  async request(
    method: string,
    params: Record<string, unknown>,
    options: McpBackendRequestOptions,
  ): Promise<Record<string, unknown>> {
    const bucket = method === 'tools/call' ? 'call' : method.startsWith('tasks/') ? 'task' : 'other';
    this.enter(bucket);
    try {
      if (method === 'tools/call' && params.name === 'Bash') {
        if (this.bashCalls >= this.bashLimit) throw new SandboxError('limit_exceeded', 'Bash call budget exhausted');
        this.bashCalls++;
      }
      const pending: PendingRequest | undefined =
        options.requestId === undefined ? undefined : { controller: new AbortController(), cancelled: false };
      const key = options.requestId === undefined ? undefined : requestKey(options.requestId);
      if (key !== undefined && pending) this.requests.set(key, pending);
      const signal = pending ? AbortSignal.any([options.signal, pending.controller.signal]) : options.signal;
      try {
        return await this.client.request(
          { method, params: forwardedParams(params) },
          z.record(z.string(), z.unknown()),
          { signal, timeout: requestTimeoutMs, onprogress: options.onProgress },
        );
      } catch (error) {
        if (pending?.cancelled) throw new ProtocolError(requestCancelledCode, 'Request cancelled');
        throw error;
      } finally {
        if (key !== undefined && pending && this.requests.get(key) === pending) this.requests.delete(key);
      }
    } finally {
      this.leave(bucket);
    }
  }
  async notify(notification: { method: string; params?: Record<string, unknown> }): Promise<void> {
    await this.client.notification(notification);
  }
  cancel(requestId: string | number): void {
    const key = requestKey(requestId);
    const pending = this.requests.get(key);
    if (pending) {
      pending.cancelled = true;
      pending.controller.abort(new ProtocolError(requestCancelledCode, 'Request cancelled'));
      return;
    }
    const subscription = this.subscriptions.get(key);
    if (subscription) {
      this.subscriptions.delete(key);
      void subscription.close();
    }
  }
  async subscribe(
    filter: Record<string, unknown>,
    options: McpBackendSubscriptionOptions,
  ): Promise<McpBackendSubscription> {
    if (this.sinks.size >= maxSubscriptions) throw new SandboxError('limit_exceeded', 'subscription limit');
    const active: ActiveSubscription = { filter, sink: options.sink, closed: false };
    this.sinks.add(active);
    const key = requestKey(options.requestId);
    let subscription: McpBackendSubscription;
    try {
      const opened = await this.client.listen(filter as SubscriptionFilter, {
        signal: options.signal,
        timeout: 10_000,
      });
      subscription = {
        honoredFilter: opened.honoredFilter,
        close: async () => {
          this.releaseSubscription(active, key);
          try {
            await opened.close();
          } catch (error) {
            // 后端已退出时关闭订阅失败是预期结果；不能变成未处理的 rejection。
            console.debug('sandbox subscription close failed', error);
          }
        },
        closed: opened.closed,
      };
    } catch (error) {
      this.sinks.delete(active);
      throw error;
    }
    active.subscription = subscription;
    this.subscriptions.set(key, subscription);
    void subscription.closed.then(() => this.releaseSubscription(active, key));
    return subscription;
  }
  private releaseSubscription(active: ActiveSubscription, key: string): void {
    if (active.closed) return;
    active.closed = true;
    this.sinks.delete(active);
    if (this.subscriptions.get(key) === active.subscription) this.subscriptions.delete(key);
  }
  private routeNotification(notification: { method: string; params?: Record<string, unknown> }): void {
    const { method, params = {} } = notification;
    // Listen control plane: the inner client already consumed its own ack.
    // An unmatched ack/cancel has no host subscription to route to.
    if (method === 'notifications/subscriptions/acknowledged' || method === 'notifications/cancelled') return;
    const targets = new Set<ActiveSubscription>();
    const routeFiltered = (matches: (filter: Record<string, unknown>) => boolean) => {
      for (const active of this.sinks) if (matches(active.filter)) targets.add(active);
    };
    if (method === 'notifications/tools/list_changed') {
      routeFiltered((filter) => filter.toolsListChanged === true);
    } else if (method === 'notifications/prompts/list_changed') {
      routeFiltered((filter) => filter.promptsListChanged === true);
    } else if (method === 'notifications/resources/list_changed') {
      routeFiltered((filter) => filter.resourcesListChanged === true);
    } else if (method === 'notifications/resources/updated') {
      const uri = typeof params.uri === 'string' ? params.uri : undefined;
      if (uri !== undefined)
        routeFiltered(
          (filter) => Array.isArray(filter.resourceSubscriptions) && filter.resourceSubscriptions.includes(uri),
        );
    } else if (method === 'notifications/tasks' || method === 'notifications/tasks/status') {
      const taskId = taskIdOf(params);
      if (taskId !== undefined)
        routeFiltered(
          (filter) =>
            Array.isArray(filter.taskIds) && filter.taskIds.some((id) => typeof id === 'string' && id === taskId),
        );
    } else {
      for (const active of this.sinks) targets.add(active);
    }
    if (targets.size === 0) {
      console.debug('sandbox notification had no host subscription', { method });
      return;
    }
    for (const active of targets) {
      if (active.closed) continue;
      active.sink({ method, params });
    }
  }
  async close(): Promise<void> {
    this.closing = true;
    const closing = [...this.sinks].map(async (active) => {
      active.closed = true;
      try {
        await active.subscription?.close();
      } catch {
        // The inner client is about to be closed; a failed sub close is not
        // actionable here and must not prevent transport shutdown.
      }
    });
    await Promise.allSettled(closing);
    this.sinks.clear();
    this.subscriptions.clear();
    try {
      await this.client.close();
    } finally {
      await this.transport.close();
    }
  }
}
