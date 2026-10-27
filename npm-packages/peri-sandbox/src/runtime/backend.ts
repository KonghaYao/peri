import { Client, type Transport, type JSONRPCMessage } from '@modelcontextprotocol/client';
import { z } from 'zod';
import { randomBytes } from 'node:crypto';
import { type ChildProcessWithoutNullStreams } from 'node:child_process';
import { ControlError } from '../control/types.js';
import type { Backend } from '../control/control.js';
import type { DiagnosticLog } from './diagnostics.js';

const maxMessage = 8 * 1024 * 1024;
const maxDiagnostics = 32 * 1024;

function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function allowed(params: Record<string, unknown>, keys: string[]): void {
  if (Object.keys(params).some((key) => !keys.includes(key)))
    throw new ControlError('conflict', 'unsupported request field');
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
    if (this.closed || !this.process.stdin.writable) throw new ControlError('interrupted', 'backend closed');
    const serialized = JSON.stringify(message) + '\n';
    const size = Buffer.byteLength(serialized);
    if (size > maxMessage || this.pendingBytes + size > maxMessage * 2)
      throw new ControlError('limit_exceeded', 'stdio write buffer limit');
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
  }
}

export class PeriBackend implements Backend {
  readonly client: Client;
  private readonly transport: ProcessTransport;
  private readonly taskIds = new Map<string, { internal: string; owner?: string }>();
  private readonly reversed = new Map<string, string>();
  private bashCalls = 0;
  private inflight = 0;
  private queries = 0;
  private toolsCache: unknown;
  private advertisedTools = new Set(['Bash', 'Read', 'Write']);
  constructor(
    readonly id: string,
    readonly generation: string,
    process: ChildProcessWithoutNullStreams,
    readonly bashLimit: number,
    diagnostics?: DiagnosticLog,
  ) {
    this.transport = new ProcessTransport(process, diagnostics);
    this.client = new Client(
      { name: 'peri-sandbox', version: '0.1.0' },
      {
        versionNegotiation: { mode: { pin: '2026-07-28' } },
        capabilities: { extensions: { 'io.modelcontextprotocol/tasks': {} } },
        inputRequired: { autoFulfill: false },
      },
    );
  }
  async ready(): Promise<void> {
    await this.client.connect(this.transport);
    const capabilities = this.client.getServerCapabilities();
    if (!capabilities?.tools || !capabilities.extensions?.['io.modelcontextprotocol/tasks'])
      throw new ControlError('startup_failed', 'backend lacks tools or Tasks capability');
    const listed = await this.client.listTools();
    if (
      !listed.tools.some((tool) => tool.name === 'Bash') ||
      !listed.tools.some((tool) => tool.name === 'Read') ||
      !listed.tools.some((tool) => tool.name === 'Write')
    )
      throw new ControlError('startup_failed', 'required tools missing');
    this.advertisedTools = new Set(listed.tools.map((tool) => tool.name));
    this.toolsCache = { tools: listed.tools };
  }
  listTools(): unknown {
    return this.toolsCache;
  }
  private internal(external: string, owner?: string): string {
    const mapping = this.taskIds.get(external);
    if (!mapping || mapping.owner !== owner)
      throw new ControlError('not_found', 'unknown task for this generation/owner');
    return mapping.internal;
  }
  private external(internal: string, owner?: string): string {
    const key = JSON.stringify([owner ?? null, internal]);
    let external = this.reversed.get(key);
    if (!external) {
      if (this.taskIds.size >= this.bashLimit) throw new ControlError('limit_exceeded', 'task mapping limit');
      external = `task_${randomBytes(24).toString('hex')}`;
      this.taskIds.set(external, { internal, owner });
      this.reversed.set(key, external);
    }
    return external;
  }
  async forward(
    method: string,
    params: Record<string, unknown>,
    owner: string | undefined,
    signal: AbortSignal,
    onProgress?: (progress: { progress: number; total?: number; message?: string }) => void,
  ): Promise<Record<string, unknown>> {
    const isQuery = method === 'tasks/get' || method === 'tasks/update' || method === 'tasks/cancel';
    if (method === 'tools/call') {
      allowed(params, ['name', 'arguments', '_meta']);
      if (typeof params.name !== 'string' || !this.advertisedTools.has(params.name) || !object(params.arguments))
        throw new ControlError('conflict', 'unsupported tool call');
    } else if (isQuery) {
      allowed(params, ['taskId', '_meta']);
      if (typeof params.taskId !== 'string' || !params.taskId) throw new ControlError('conflict', 'invalid task ID');
    } else throw new ControlError('conflict', 'unsupported method');
    if (owner !== undefined && (typeof owner !== 'string' || !owner.trim() || Buffer.byteLength(owner) > 512))
      throw new ControlError('conflict', 'invalid peri/taskOwner');
    if (isQuery ? this.queries >= 4 : this.inflight >= 8) throw new ControlError('limit_exceeded', 'concurrency limit');
    if (method === 'tools/call' && params.name === 'Bash') {
      if (this.bashCalls >= this.bashLimit) throw new ControlError('limit_exceeded', 'Bash call budget exhausted');
      this.bashCalls++;
    }
    if (isQuery) this.queries++;
    else this.inflight++;
    try {
      const forwarded = isQuery
        ? { taskId: this.internal(params.taskId as string, owner) }
        : { name: params.name, arguments: params.arguments };
      const meta = owner === undefined ? undefined : { 'peri/taskOwner': owner };
      const backendMethod = method === 'tasks/update' ? 'tasks/get' : method;
      const result = await this.client.request(
        { method: backendMethod, params: { ...forwarded, ...(meta ? { _meta: meta } : {}) } },
        z.record(z.string(), z.unknown()),
        { signal, timeout: 60 * 60 * 1000, onprogress: onProgress },
      );
      if (result.resultType === 'input_required' || result.status === 'input_required')
        throw new ControlError('conflict', 'unsupported backend input_required');
      if (method === 'tasks/update') return { resultType: 'complete' };
      const output = { ...result };
      if (typeof output.taskId === 'string') output.taskId = this.external(output.taskId, owner);
      if (output._meta && typeof output._meta === 'object') {
        const oldMeta = output._meta as Record<string, unknown>;
        if (typeof oldMeta.task_id === 'string')
          output._meta = { ...oldMeta, task_id: this.external(oldMeta.task_id, owner) };
      }
      if (output.task && typeof output.task === 'object') {
        const task = output.task as Record<string, unknown>;
        if (typeof task.taskId === 'string') output.task = { ...task, taskId: this.external(task.taskId, owner) };
      }
      if (output.result && typeof output.result === 'object') {
        const toolResult = output.result as Record<string, unknown>;
        if (toolResult._meta && typeof toolResult._meta === 'object') {
          const oldMeta = toolResult._meta as Record<string, unknown>;
          if (typeof oldMeta.task_id === 'string')
            output.result = { ...toolResult, _meta: { ...oldMeta, task_id: this.external(oldMeta.task_id, owner) } };
        }
      }
      return output;
    } finally {
      if (isQuery) this.queries--;
      else this.inflight--;
    }
  }
  async close(): Promise<void> {
    await this.client.close();
    await this.transport.close();
  }
}
