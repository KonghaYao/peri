import { createMcpHandler, type McpHttpHandler } from '@modelcontextprotocol/server';
import { Hono } from 'hono';

import { LIMITS } from './contracts/limits';
import { isBusinessError, type BusinessErrorBody } from './contracts/errors';
import { authenticate } from './mcp/auth';
import { buildServer } from './mcp/server';
import { resolveSubscriptionTopics } from './mcp/subscriptions';
import { maintain } from './maintenance';
import { TopicEventBus } from './realtime/bus';
import { drainOutbox } from './realtime/outbox';
import type { Env } from './env';

interface ListenBody {
  id?: string | number | null;
  method?: string;
  params?: {
    notifications?: { resourceSubscriptions?: string[] };
    _meta?: Record<string, unknown>;
  };
  _meta?: Record<string, unknown>;
}

const PROTOCOL_VERSION_META = 'io.modelcontextprotocol/protocolVersion';
const MODERN_VERSION = '2026-07-28';

function jsonRpcError(
  id: string | number | null | undefined,
  code: number,
  message: string,
  data?: BusinessErrorBody,
  status = 200,
): Response {
  return new Response(
    JSON.stringify({
      jsonrpc: '2.0',
      id: id ?? null,
      error: { code, message, ...(data ? { data } : {}) },
    }),
    { status, headers: { 'content-type': 'application/json' } },
  );
}

function handlerFor(env: Env, principal: Awaited<ReturnType<typeof authenticate>>, bus?: TopicEventBus): McpHttpHandler {
  return createMcpHandler(() => buildServer(env, principal), {
    legacy: 'reject',
    maxRequestBodySize: LIMITS.httpBodyBytes,
    ...(bus ? { bus } : {}),
  });
}

interface WaitUntil {
  waitUntil(promise: Promise<unknown>): void;
}

async function handleMcp(
  request: Request,
  env: Env,
  ctx: WaitUntil,
): Promise<Response> {
  let principal: Awaited<ReturnType<typeof authenticate>>;
  try {
    principal = await authenticate(env.DB, request);
  } catch (error) {
    if (isBusinessError(error)) {
      return jsonRpcError(null, -32000, error.message, error.toBody(), 401);
    }
    throw error;
  }

  if (request.method !== 'POST') {
    const response = await handlerFor(env, principal).fetch(request);
    if (response.status !== 405) return response;
    // RFC 9110 要求 405 响应声明允许的方法。
    const headers = new Headers(response.headers);
    headers.set('allow', 'POST');
    return new Response(response.body, { status: 405, statusText: response.statusText, headers });
  }

  const body = (await request.clone().json().catch(() => null)) as ListenBody | null;
  const meta = body?._meta ?? body?.params?._meta;
  const isListen =
    body !== null &&
    !Array.isArray(body) &&
    body.method === 'subscriptions/listen' &&
    meta?.[PROTOCOL_VERSION_META] === MODERN_VERSION;

  if (isListen) {
    if (!principal.actor) {
      return jsonRpcError(body.id, -32602, '建立订阅需要已注册身份', {
        code: 'CREDENTIAL_INVALID',
        message: '当前凭据尚未注册；先调用 network_register',
        retryable: false,
      });
    }
    let bus: TopicEventBus;
    try {
      const topics = await resolveSubscriptionTopics(
        env.DB,
        principal.actor.actor_id,
        body.params?.notifications?.resourceSubscriptions ?? [],
      );
      let handler: McpHttpHandler | undefined;
      bus = new TopicEventBus(env, topics, () => {
        void handler?.close();
      });
      await bus.ready();
      handler = handlerFor(env, principal, bus);
      return await handler.fetch(request);
    } catch (error) {
      if (isBusinessError(error)) {
        return jsonRpcError(body.id, -32602, error.message, error.toBody());
      }
      throw error;
    }
  }

  const handler = handlerFor(env, principal);
  const response = await handler.fetch(request);
  if (body?.method === 'tools/call') {
    ctx.waitUntil(
      drainOutbox(env).catch((error) => {
        console.error('agent-network fast-path outbox drain failed', error);
      }),
    );
  }
  return response;
}

const app = new Hono<{ Bindings: Env }>();

app.get('/health', (c) => c.json({ ok: true, env: c.env.ENVIRONMENT }));
app.all('/mcp', async (c) => {
  try {
    return await handleMcp(c.req.raw, c.env, c.executionCtx);
  } catch (error) {
    console.error('agent-network MCP request failed', error);
    throw error;
  }
});

export { NotificationChannel } from './realtime/channel';

async function runScheduled(env: Env): Promise<void> {
  try {
    const result = await drainOutbox(env);
    if (result.oldestPendingAgeMs !== null && result.oldestPendingAgeMs > 5 * 60_000) {
      console.warn('agent-network outbox backlog', result);
    }
  } catch (error) {
    console.error('agent-network outbox drain failed', error);
  }
  try {
    await maintain(env);
  } catch (error) {
    console.error('agent-network maintenance failed', error);
  }
}

export default {
  fetch: app.fetch,
  scheduled: async (_event: ScheduledEvent, env: Env, ctx: ExecutionContext) => {
    ctx.waitUntil(runScheduled(env));
  },
};
