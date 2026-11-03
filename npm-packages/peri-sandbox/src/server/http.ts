import { createServer, type ServerResponse } from 'node:http';
import { timingSafeEqual } from 'node:crypto';
import { Readable, Transform, pipeline } from 'node:stream';
import {
  SandboxError,
  type McpBackendPort,
  type McpBackendSubscription,
  type SandboxState,
} from '../instance/types.js';
import { createSandboxMcpHandler } from './mcp-handler.js';

const MAX_REQUEST_BYTES = 8 * 1024 * 1024;
const MAX_RESPONSE_BYTES = 33 * 1024 * 1024;
const MAX_LISTEN_NOTIFICATION_BYTES = 1024 * 1024;
const MAX_PRE_ACK_NOTIFICATIONS = 64;
const KEEPALIVE_MS = 10_000;
const SUBSCRIPTION_ID_META_KEY = 'io.modelcontextprotocol/subscriptionId';
const PROTOCOL_VERSION_META_KEY = 'io.modelcontextprotocol/protocolVersion';
const CLIENT_CAPABILITIES_META_KEY = 'io.modelcontextprotocol/clientCapabilities';

const respond = (res: ServerResponse, status: number, body: unknown) => {
  res.writeHead(status, { 'Content-Type': 'application/json' });
  res.end(JSON.stringify(body));
};

const validToken = (provided: string | undefined, expected: Buffer) => {
  const actual = Buffer.from(provided?.startsWith('Bearer ') ? provided.slice(7) : '');
  return actual.length === expected.length && timingSafeEqual(actual, expected);
};

function hostLiteral(host: '127.0.0.1' | '::1'): string {
  return host === '::1' ? '[::1]' : host;
}

interface ListenRequest {
  id: string | number;
  filter: Record<string, unknown>;
}

/**
 * Recognizes a modern `subscriptions/listen` request that the SDK's own
 * listen router cannot serve transparently (it only relays four standard
 * change events). Anything else falls through to `createMcpHandler` for
 * normal protocol validation.
 */
function listenRequest(body: Buffer): ListenRequest | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(body.toString('utf8'));
  } catch {
    return null;
  }
  if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) return null;
  const request = parsed as Record<string, unknown>;
  if (request.method !== 'subscriptions/listen' || !('id' in request)) return null;
  const id = request.id;
  if (typeof id !== 'string' && typeof id !== 'number') return null;
  const params = request.params;
  if (params === null || typeof params !== 'object' || Array.isArray(params)) return null;
  const { notifications, _meta: meta } = params as Record<string, unknown>;
  if (notifications === null || typeof notifications !== 'object' || Array.isArray(notifications)) return null;
  if (meta === null || typeof meta !== 'object' || Array.isArray(meta)) return null;
  const envelope = meta as Record<string, unknown>;
  if (typeof envelope[PROTOCOL_VERSION_META_KEY] !== 'string') return null;
  if (
    envelope[CLIENT_CAPABILITIES_META_KEY] === null ||
    typeof envelope[CLIENT_CAPABILITIES_META_KEY] !== 'object' ||
    Array.isArray(envelope[CLIENT_CAPABILITIES_META_KEY])
  )
    return null;
  return { id, filter: notifications as Record<string, unknown> };
}

function errorCode(error: unknown): number {
  return typeof (error as { code?: unknown })?.code === 'number' ? (error as { code: number }).code : -32603;
}

function errorBody(requestId: string | number, error: unknown): Record<string, unknown> {
  const data = (error as { data?: unknown })?.data;
  return {
    jsonrpc: '2.0',
    id: requestId,
    error: {
      code: errorCode(error),
      message: error instanceof Error ? error.message : 'subscription failed',
      ...(data !== undefined ? { data } : {}),
    },
  };
}

export interface SandboxHttpServerOptions {
  sandboxId: string;
  host: '127.0.0.1' | '::1';
  /** 0 binds an ephemeral port. */
  port: number;
  token: string;
  state(): SandboxState;
  backend(): McpBackendPort | null;
}

export interface SandboxHttpServer {
  readonly url: string;
  readonly port: number;
  close(): Promise<void>;
}

/**
 * Loopback-only HTTP MCP endpoint for one sandbox instance.
 *
 * Requests are rejected unless the bearer token matches, the Host header is
 * loopback for the bound port, the path is `/mcp`, and the instance is ready.
 * `subscriptions/listen` is served here as a direct relay to the inner
 * client so the full notification vocabulary passes through untouched.
 */
export async function startHttpServer(options: SandboxHttpServerOptions): Promise<SandboxHttpServer> {
  const expected = Buffer.from(options.token);
  const pending = new Set<AbortController>();
  const allowedHosts = new Set<string>();
  const allowedOrigins = new Set<string>();
  let listeningPort = 0;
  let closed = false;
  let handler: ReturnType<typeof createSandboxMcpHandler> | null = null;

  async function serveListen(request: ListenRequest, res: ServerResponse): Promise<void> {
    const backend = options.backend();
    if (!backend) {
      respond(res, 503, { error: 'not_running' });
      return;
    }
    const abort = new AbortController();
    pending.add(abort);
    let ended = false;
    let headersSent = false;
    let ackSent = false;
    let keepAlive: NodeJS.Timeout | undefined;
    let subscription: McpBackendSubscription | undefined;
    const queue: Array<{ method: string; params?: Record<string, unknown> }> = [];
    let overflow = false;

    const cleanup = () => {
      if (keepAlive !== undefined) clearInterval(keepAlive);
      keepAlive = undefined;
      pending.delete(abort);
    };
    const end = () => {
      if (ended) return;
      ended = true;
      cleanup();
      if (!res.writableEnded) res.end();
    };
    res.once('close', () => {
      if (ended) return;
      abort.abort(new SandboxError('backend_exited', 'HTTP client disconnected'));
      void subscription?.close().catch(() => {});
      end();
    });
    const writeEvent = (payload: unknown): boolean => {
      if (ended || res.writableEnded) return false;
      const text = JSON.stringify(payload);
      if (Buffer.byteLength(text) > MAX_LISTEN_NOTIFICATION_BYTES) return false;
      res.write(`event: message\ndata: ${text}\n\n`);
      return true;
    };
    const notificationPayload = (notification: { method: string; params?: Record<string, unknown> }) => {
      const params = notification.params ?? {};
      const meta =
        params._meta !== null && typeof params._meta === 'object' && !Array.isArray(params._meta)
          ? (params._meta as Record<string, unknown>)
          : {};
      return {
        jsonrpc: '2.0',
        method: notification.method,
        params: { ...params, _meta: { ...meta, [SUBSCRIPTION_ID_META_KEY]: request.id } },
      };
    };
    const writeNotification = (notification: { method: string; params?: Record<string, unknown> }): boolean => {
      const payload = notificationPayload(notification);
      const text = JSON.stringify(payload);
      if (Buffer.byteLength(text) > MAX_LISTEN_NOTIFICATION_BYTES) {
        console.warn('sandbox listen notification exceeds 1 MiB; closing subscription', {
          method: notification.method,
        });
        void subscription?.close();
        end();
        return false;
      }
      if (ended || res.writableEnded) return false;
      res.write(`event: message\ndata: ${text}\n\n`);
      return true;
    };
    const sink = (notification: { method: string; params?: Record<string, unknown> }) => {
      if (ended) return;
      if (!ackSent) {
        if (queue.length >= MAX_PRE_ACK_NOTIFICATIONS) {
          overflow = true;
          return;
        }
        queue.push(notification);
        return;
      }
      writeNotification(notification);
    };

    try {
      try {
        subscription = await backend.subscribe(request.filter, {
          signal: abort.signal,
          requestId: request.id,
          sink,
        });
      } catch (error) {
        console.warn('sandbox subscriptions/listen failed', error);
        respond(res, 200, errorBody(request.id, error));
        cleanup();
        return;
      }
      if (overflow) {
        console.warn('sandbox subscriptions/listen pre-ack queue overflow');
        await subscription.close().catch(() => {});
        respond(res, 200, {
          jsonrpc: '2.0',
          id: request.id,
          error: { code: -32603, message: 'listen notification queue overflow' },
        });
        cleanup();
        return;
      }
      if (ended) {
        await subscription.close().catch(() => {});
        return;
      }
      res.writeHead(200, {
        'Content-Type': 'text/event-stream',
        'Cache-Control': 'no-cache',
        Connection: 'keep-alive',
      });
      res.flushHeaders();
      headersSent = true;
      writeEvent({
        jsonrpc: '2.0',
        method: 'notifications/subscriptions/acknowledged',
        params: {
          notifications: subscription.honoredFilter,
          _meta: { [SUBSCRIPTION_ID_META_KEY]: request.id },
        },
      });
      ackSent = true;
      for (const notification of queue.splice(0)) writeNotification(notification);
      keepAlive = setInterval(() => {
        if (!ended && !res.writableEnded) res.write(': keepalive\n\n');
      }, KEEPALIVE_MS);

      const current = subscription;
      void current.closed.then((outcome) => {
        if (ended) return;
        if (outcome === 'graceful')
          writeEvent({
            jsonrpc: '2.0',
            id: request.id,
            result: { resultType: 'complete', _meta: { [SUBSCRIPTION_ID_META_KEY]: request.id } },
          });
        end();
      });
      abort.signal.addEventListener(
        'abort',
        () => {
          void current.close().catch(() => {});
          end();
        },
        { once: true },
      );
    } catch (error) {
      console.warn('sandbox subscriptions/listen relay failed', error);
      if (!headersSent && !ended) respond(res, 200, errorBody(request.id, error));
      await subscription?.close().catch(() => {});
      cleanup();
      if (!res.writableEnded) res.end();
    }
  }

  const server = createServer(async (req, res) => {
    if (!validToken(req.headers.authorization, expected)) {
      respond(res, 401, { error: 'unauthorized' });
      return;
    }
    if (!req.headers.host || !allowedHosts.has(req.headers.host)) {
      respond(res, 403, { error: 'host not allowed' });
      return;
    }
    if (req.headers.origin && !allowedOrigins.has(req.headers.origin)) {
      respond(res, 403, { error: 'origin not allowed' });
      return;
    }
    if (new URL(req.url ?? '/', 'http://127.0.0.1').pathname !== '/mcp') {
      respond(res, 404, { error: 'not found' });
      return;
    }
    if (req.method !== 'POST' && req.method !== 'GET' && req.method !== 'DELETE') {
      res.writeHead(405, { Allow: 'POST, GET, DELETE', 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ error: 'method not allowed' }));
      return;
    }
    const backend = options.backend();
    if (options.state() !== 'ready' || !backend) {
      respond(res, 503, { error: 'not_running' });
      return;
    }
    if (Number(req.headers['content-length'] ?? 0) > MAX_REQUEST_BYTES) {
      respond(res, 413, { error: 'request too large' });
      return;
    }
    try {
      const chunks: Buffer[] = [];
      let size = 0;
      for await (const data of req) {
        const chunk = Buffer.from(data);
        size += chunk.length;
        if (size > MAX_REQUEST_BYTES) {
          respond(res, 413, { error: 'request too large' });
          return;
        }
        chunks.push(chunk);
      }
      const body = Buffer.concat(chunks);
      if (req.method === 'POST') {
        const listen = listenRequest(body);
        if (listen) {
          await serveListen(listen, res);
          return;
        }
      }
      const url = new URL(req.url!, `http://${req.headers.host}`);
      const abort = new AbortController();
      pending.add(abort);
      const disconnect = () => {
        if (!res.writableFinished) abort.abort(new SandboxError('backend_exited', 'HTTP client disconnected'));
      };
      res.once('close', disconnect);
      try {
        const request = new Request(url, {
          method: req.method,
          headers: req.headers as HeadersInit,
          signal: abort.signal,
          ...(req.method === 'POST' ? { body } : {}),
        });
        handler ??= createSandboxMcpHandler({ sandboxId: options.sandboxId, backend });
        const response = await handler.fetch(request);
        res.writeHead(response.status, Object.fromEntries(response.headers.entries()));
        if (response.body) {
          let bytes = 0;
          const limiter = new Transform({
            transform(chunk: Buffer, _encoding, done) {
              bytes += chunk.length;
              if (bytes > MAX_RESPONSE_BYTES) {
                abort.abort();
                done(new SandboxError('limit_exceeded', 'MCP response exceeds 33 MiB'));
              } else done(null, chunk);
            },
          });
          pipeline(Readable.fromWeb(response.body as never), limiter, res, (error) => {
            if (error && !abort.signal.aborted) console.error('MCP response stream failed:', error);
          });
        } else res.end();
      } finally {
        pending.delete(abort);
        res.off('close', disconnect);
      }
    } catch (error) {
      if (!res.headersSent) respond(res, 500, { error: 'internal' });
      console.error('mcp exchange failed:', error);
    }
  });
  server.maxConnections = 32;
  server.requestTimeout = 15000;
  server.headersTimeout = 10000;
  // Long tool calls and listen streams must not be killed by socket idleness;
  // request/headers timeouts still bound slow request bodies and headers.
  server.timeout = 0;
  await new Promise<void>((resolve, reject) => {
    const onError = (error: NodeJS.ErrnoException) => {
      server.off('listening', onListening);
      reject(
        new SandboxError(
          error.code === 'EADDRINUSE' || error.code === 'EACCES' ? 'conflict' : 'start_failed',
          `Cannot bind sandbox MCP endpoint on ${hostLiteral(options.host)}:${options.port}: ${error.message}`,
          { cause: error },
        ),
      );
    };
    const onListening = () => {
      server.off('error', onError);
      const address = server.address();
      if (!address || typeof address === 'string') {
        reject(new SandboxError('start_failed', 'Sandbox MCP endpoint has no TCP address.'));
        return;
      }
      listeningPort = address.port;
      resolve();
    };
    server.once('error', onError);
    server.once('listening', onListening);
    server.listen(options.port, options.host);
  }).catch((error) => {
    if (server.listening) server.close();
    throw error;
  });

  const literal = hostLiteral(options.host);
  const url = `http://${literal}:${listeningPort}`;
  allowedHosts.add(`${literal}:${listeningPort}`);
  allowedHosts.add(`localhost:${listeningPort}`);
  allowedOrigins.add(url);
  allowedOrigins.add(`http://localhost:${listeningPort}`);

  return {
    url: `${url}/mcp`,
    port: listeningPort,
    async close() {
      if (closed) return;
      closed = true;
      for (const abort of pending) abort.abort(new SandboxError('backend_exited', 'Sandbox HTTP server closed.'));
      pending.clear();
      await new Promise<void>((resolve) => {
        if (!server.listening) {
          resolve();
          return;
        }
        server.close(() => resolve());
        server.closeAllConnections();
      });
    },
  };
}
