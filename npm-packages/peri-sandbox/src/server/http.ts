import { createServer, type ServerResponse } from 'node:http';
import { timingSafeEqual } from 'node:crypto';
import { Readable, Transform, pipeline } from 'node:stream';
import { SandboxError, type McpBackendPort, type SandboxState } from '../instance/types.js';
import { createSandboxMcpHandler } from './mcp-handler.js';

const MAX_REQUEST_BYTES = 1024 * 1024;
const MAX_RESPONSE_BYTES = 8 * 1024 * 1024;

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
 */
export async function startHttpServer(options: SandboxHttpServerOptions): Promise<SandboxHttpServer> {
  const expected = Buffer.from(options.token);
  const pending = new Set<AbortController>();
  const allowedHosts = new Set<string>();
  const allowedOrigins = new Set<string>();
  let listeningPort = 0;
  let closed = false;
  let handler: ReturnType<typeof createSandboxMcpHandler> | null = null;

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
          ...(req.method === 'POST' ? { body: Buffer.concat(chunks) } : {}),
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
                done(new SandboxError('limit_exceeded', 'MCP response exceeds 8 MiB'));
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
  server.timeout = 15000;
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
