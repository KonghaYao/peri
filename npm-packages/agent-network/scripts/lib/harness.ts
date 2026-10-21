/**
 * 验收脚本的共享工具：官方 TS SDK 客户端、工具调用与断言。
 *
 * 所有脚本都通过 AGENTNET_URL 指向被测服务（默认本地 wrangler dev），
 * 因此同一份用例既能验收本地，也能验收部署后的远端。
 */

import { Client, StreamableHTTPClientTransport } from '@modelcontextprotocol/client';
import type { McpSubscription, SubscriptionFilter } from '@modelcontextprotocol/client';

export const PROTOCOL_VERSION = '2026-07-28';

export function baseUrl(): string {
  return (process.env.AGENTNET_URL ?? 'http://127.0.0.1:8792').replace(/\/+$/u, '');
}

export function mcpUrl(): URL {
  return new URL(`${baseUrl()}/mcp`);
}

export function newToken(): string {
  return `ank_${crypto.randomUUID().replaceAll('-', '')}`;
}

export function newRequestId(prefix = 'req'): string {
  return `${prefix}_${crypto.randomUUID().replaceAll('-', '')}`;
}

export class CheckFailure extends Error {
  constructor(
    readonly label: string,
    message: string,
  ) {
    super(`${label}: ${message}`);
    this.name = 'CheckFailure';
  }
}

let checks = 0;

export function check(label: string, condition: unknown, detail?: unknown): void {
  checks += 1;
  if (!condition) {
    const suffix = detail === undefined ? '' : ` (${JSON.stringify(detail)})`;
    throw new CheckFailure(label, `断言失败${suffix}`);
  }
  console.log(`  ✓ ${label}`);
}

export function checkEqual(label: string, actual: unknown, expected: unknown): void {
  check(label, Object.is(actual, expected), { actual, expected });
}

export function checksPassed(): number {
  return checks;
}

export class ToolError extends Error {
  constructor(
    readonly code: string,
    readonly body: Record<string, unknown>,
  ) {
    super(`${code}: ${String(body.message ?? '')}`);
    this.name = 'ToolError';
  }
}

export interface McpConnection {
  client: Client;
  transport: StreamableHTTPClientTransport;
  token: string;
  close(): Promise<void>;
}

export function connect(token: string): McpConnection {
  const client = new Client(
    { name: 'agent-network-acceptance', version: '0.1.0' },
    { versionNegotiation: { mode: { pin: PROTOCOL_VERSION } } },
  );
  const transport = new StreamableHTTPClientTransport(mcpUrl(), {
    authProvider: { token: async () => token },
  });
  return {
    client,
    transport,
    token,
    async close() {
      await client.close().catch(() => undefined);
    },
  };
}

export async function connectClient(token: string): Promise<McpConnection> {
  const connection = connect(token);
  await connection.client.connect(connection.transport);
  return connection;
}

interface RawToolResult {
  content?: { type: string; text?: string }[];
  structuredContent?: Record<string, unknown>;
  isError?: boolean;
}

/** 调用工具；业务错误统一抛出 `ToolError`，错误体来自服务端契约。 */
export async function callTool<T = Record<string, unknown>>(
  client: Client,
  name: string,
  args: Record<string, unknown> = {},
): Promise<T> {
  const result = (await client.callTool({ name, arguments: args })) as RawToolResult;
  const text = result.content?.find((item) => item.type === 'text')?.text;
  if (result.isError) {
    let body: Record<string, unknown> = {};
    if (text) {
      try {
        body = JSON.parse(text) as Record<string, unknown>;
      } catch {
        body = { message: text };
      }
    }
    throw new ToolError(String(body.code ?? 'UNKNOWN'), body);
  }
  if (result.structuredContent) return result.structuredContent as T;
  if (text) return JSON.parse(text) as T;
  return {} as T;
}

export async function expectToolError(
  label: string,
  promise: Promise<unknown>,
  code: string,
): Promise<ToolError> {
  try {
    await promise;
  } catch (error) {
    if (error instanceof ToolError) {
      checkEqual(label, error.code, code);
      return error;
    }
    throw error;
  }
  throw new CheckFailure(label, `期望错误 ${code}，但调用成功`);
}

export async function readResourceJson<T = Record<string, unknown>>(
  client: Client,
  uri: string,
): Promise<T> {
  const result = await client.readResource({ uri });
  const first = result.contents?.[0] as { text?: string } | undefined;
  if (!first?.text) throw new CheckFailure('readResource', `资源 ${uri} 没有文本内容`);
  return JSON.parse(first.text) as T;
}

export function notificationCollector(client: Client): {
  events: { method: string; params: Record<string, unknown> }[];
  waitFor(predicate: (event: { method: string; params: Record<string, unknown> }) => boolean, label: string, timeoutMs?: number): Promise<{ method: string; params: Record<string, unknown> }>;
} {
  const events: { method: string; params: Record<string, unknown> }[] = [];
  const waiters = new Set<() => void>();
  client.setNotificationHandler('notifications/resources/updated', (notification) => {
    events.push({
      method: 'notifications/resources/updated',
      params: (notification.params ?? {}) as Record<string, unknown>,
    });
    for (const notify of waiters) notify();
  });
  return {
    events,
    async waitFor(predicate, label, timeoutMs = 15_000) {
      const deadline = Date.now() + timeoutMs;
      for (;;) {
        const found = events.find(predicate);
        if (found) return found;
        if (Date.now() > deadline) throw new CheckFailure(label, '等待通知超时');
        await new Promise<void>((resolve) => {
          const timer = setTimeout(() => {
            waiters.delete(notify);
            resolve();
          }, 250);
          const notify = () => {
            clearTimeout(timer);
            waiters.delete(notify);
            resolve();
          };
          waiters.add(notify);
        });
      }
    },
  };
}

export async function openSubscription(
  client: Client,
  uris: string[],
): Promise<McpSubscription> {
  const filter: SubscriptionFilter = { resourceSubscriptions: uris };
  return client.listen(filter);
}

export async function waitUntil(
  label: string,
  predicate: () => Promise<boolean> | boolean,
  timeoutMs = 10_000,
  intervalMs = 250,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    if (await predicate()) return;
    if (Date.now() > deadline) throw new CheckFailure(label, '等待条件超时');
    await new Promise((resolve) => setTimeout(resolve, intervalMs));
  }
}

export function subscriptionIdOf(params: Record<string, unknown>): string | undefined {
  const meta = params._meta as Record<string, unknown> | undefined;
  const value = meta?.['io.modelcontextprotocol/subscriptionId'];
  return typeof value === 'string' ? value : undefined;
}

/** 直接发原始 JSON-RPC POST，用于验证协议面（不依赖 SDK 的容错）。 */
export async function rawRpc(
  method: string,
  params: Record<string, unknown> = {},
  options: {
    token?: string;
    headers?: Record<string, string>;
    protocolVersion?: string | null;
    versionHeader?: string | null;
  } = {},
): Promise<{ status: number; body: Record<string, unknown>; headers: Headers }> {
  const headers: Record<string, string> = {
    'content-type': 'application/json',
    accept: 'application/json, text/event-stream',
    'mcp-method': method,
    ...options.headers,
  };
  if (method === 'tools/call' && typeof params.name === 'string') {
    headers['mcp-name'] = params.name;
  }
  if (options.token) headers.authorization = `Bearer ${options.token}`;
  if (options.versionHeader !== null) {
    headers['mcp-protocol-version'] = options.versionHeader ?? PROTOCOL_VERSION;
  }

  const meta: Record<string, unknown> = {};
  if (options.protocolVersion !== null) {
    meta['io.modelcontextprotocol/protocolVersion'] = options.protocolVersion ?? PROTOCOL_VERSION;
    meta['io.modelcontextprotocol/clientInfo'] = {
      name: 'agent-network-protocol-check',
      version: '0.1.0',
    };
    meta['io.modelcontextprotocol/clientCapabilities'] = {};
  }

  const response = await fetch(mcpUrl(), {
    method: 'POST',
    headers,
    body: JSON.stringify({
      jsonrpc: '2.0',
      id: newRequestId('rpc'),
      method,
      params: { ...params, _meta: meta },
    }),
  });
  const text = await response.text();
  let body: Record<string, unknown> = {};
  try {
    body = JSON.parse(text) as Record<string, unknown>;
  } catch {
    body = { raw: text };
  }
  return { status: response.status, body, headers: response.headers };
}

export async function sleep(ms: number): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, ms));
}
