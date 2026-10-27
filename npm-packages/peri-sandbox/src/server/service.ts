import { createServer, type IncomingMessage, type ServerResponse } from 'node:http';
import { createConnection, createServer as createUnixServer } from 'node:net';
import {
  readFileSync,
  readdirSync,
  mkdirSync,
  chmodSync,
  existsSync,
  lstatSync,
  unlinkSync,
  openSync,
  closeSync,
} from 'node:fs';
import { randomUUID, timingSafeEqual } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { join } from 'node:path';
import { Readable, Transform, pipeline } from 'node:stream';
import { Server, createMcpHandler } from '@modelcontextprotocol/server';
import { z } from 'zod';
import type { Config } from '../config.js';
import { Control } from '../control/control.js';
import { PeriBackend } from '../runtime/backend.js';
import { Registry } from '../control/registry.js';
import { SystemdProvider, type Provider } from '../runtime/provider.js';
import { MacProvider } from '../runtime/mac-provider.js';
import { ControlError } from '../control/types.js';
import { DiagnosticLog } from '../runtime/diagnostics.js';
import { pickPort } from './pick-port.js';

const respond = (res: ServerResponse, status: number, body: unknown) => {
  res.writeHead(status, { 'Content-Type': 'application/json' });
  res.end(JSON.stringify(body));
};
const validToken = (provided: string | undefined, expected: Buffer) => {
  const actual = Buffer.from(provided?.startsWith('Bearer ') ? provided.slice(7) : '');
  return actual.length === expected.length && timingSafeEqual(actual, expected);
};
const objects = z.object({}).passthrough();

export interface ServerHandle {
  close(): Promise<void>;
}

export async function startServer(config: Config): Promise<ServerHandle> {
  const provider: Provider = process.platform === 'darwin' ? new MacProvider(config) : new SystemdProvider(config);
  mkdirSync(config.controlRoot, { recursive: true, mode: 0o700 });
  mkdirSync(config.sandboxesRoot, { recursive: true, mode: 0o700 });
  const lockFd = openSync(join(config.controlRoot, '.daemon.lock'), 'a', 0o600);
  const lockResult =
    process.platform === 'darwin'
      ? spawnSync('/usr/bin/lockf', ['-t', '0', '3'], { stdio: ['ignore', 'ignore', 'pipe', lockFd] })
      : spawnSync('flock', ['-n', '3'], { stdio: ['ignore', 'ignore', 'pipe', lockFd] });
  if (lockResult.error || lockResult.status !== 0) {
    closeSync(lockFd);
    throw new ControlError('conflict', 'another daemon owns the state root');
  }
  const stopOwned = async () => {
    const failures: unknown[] = [];
    for (const unit of await provider.listOwned()) {
      try {
        await provider.stop(unit);
      } catch (error) {
        failures.push(error);
      }
    }
    if (failures.length) throw new AggregateError(failures, 'could not confirm cleanup of every owned unit');
  };
  await stopOwned();
  await provider.doctor(true);
  let registry: Registry;
  try {
    if (!existsSync(join(config.controlRoot, 'registry.db')) && readdirSync(config.sandboxesRoot).length) {
      throw new ControlError(
        'unsupported_environment',
        'registry missing while sandbox data exists; restore a consistent backup',
      );
    }
    registry = new Registry(join(config.controlRoot, 'registry.db'));
  } catch (error) {
    try {
      await stopOwned();
    } catch (cleanupError) {
      throw new ControlError(
        'stop_incomplete',
        `registry unavailable and unit cleanup unconfirmed: ${String(cleanupError)}`,
      );
    }
    throw error;
  }
  const control = new Control(config, registry, provider, async (id, generation, handle) => {
    const backend = new PeriBackend(
      id,
      generation,
      handle.process,
      config.limits.bashCalls,
      new DiagnosticLog(config.controlRoot, id),
    );
    try {
      await backend.ready();
      return backend;
    } catch (error) {
      await backend.close().catch((closeError) => console.error('backend initialization cleanup failed:', closeError));
      throw error;
    }
  });
  try {
    await control.recover();
  } catch (error) {
    try {
      await stopOwned();
    } catch (cleanupError) {
      throw new ControlError(
        'stop_incomplete',
        `recovery failed and unit cleanup unconfirmed: ${String(cleanupError)}`,
      );
    }
    throw error;
  }
  const token = Buffer.from(readFileSync(config.auth.tokenFile, 'utf8').trim());
  const routes = new Map<
    string,
    { generation: string; backend: PeriBackend; handler: ReturnType<typeof createMcpHandler> }
  >();
  let haltPromise: Promise<void> | undefined;
  const haltExecution = (): Promise<void> =>
    (haltPromise ??= (async () => {
      for (const [id, backend] of control.backends as Map<string, PeriBackend>) {
        control.backends.delete(id);
        routes.delete(id);
        try {
          await backend.close();
        } catch (error) {
          console.error('backend close after registry failure:', error);
        }
        try {
          await provider.stop(provider.unit(id, backend.generation));
        } catch (error) {
          console.error('unit cleanup after registry failure unconfirmed:', id, error);
        }
      }
      try {
        for (const unit of await provider.listOwned()) {
          try {
            await provider.stop(unit);
          } catch (error) {
            console.error('owned unit cleanup after registry failure unconfirmed:', unit, error);
          }
        }
      } catch (error) {
        console.error('owned unit enumeration after registry failure unconfirmed:', error);
      }
    })());
  const route = (id: string, backend: PeriBackend) => {
    const cached = routes.get(id);
    if (cached?.generation === backend.generation && cached.backend === backend) return cached.handler;
    const handler = createMcpHandler(
      () => {
        const server = new Server(
          { name: 'peri-sandbox', version: '0.1.0' },
          { capabilities: { tools: {}, extensions: { 'io.modelcontextprotocol/tasks': {} } } },
        );
        server.setRequestHandler('tools/list', async () => backend.listTools() as never);
        for (const method of ['tools/call', 'tasks/get', 'tasks/update', 'tasks/cancel']) {
          server.setRequestHandler(method, { params: objects, result: objects }, async (params, context) => {
            const correlationId = randomUUID(),
              begun = Date.now();
            let outcome = 'error';
            try {
              const owner = context.mcpReq._meta?.['peri/taskOwner'];
              if (
                owner !== undefined &&
                (typeof owner !== 'string' || !owner.trim() || Buffer.byteLength(owner, 'utf8') > 512)
              )
                throw new ControlError('conflict', 'invalid peri/taskOwner');
              const envelope = context.mcpReq.envelope as Record<string, unknown> | undefined;
              const capabilities = envelope?.['io.modelcontextprotocol/clientCapabilities'] as
                | { extensions?: Record<string, unknown> }
                | undefined;
              const arguments_ = params as Record<string, unknown>;
              if (
                (method !== 'tools/call' || arguments_.name === 'Bash') &&
                !capabilities?.extensions?.['io.modelcontextprotocol/tasks']
              )
                throw new ControlError('conflict', 'client must declare Tasks capability');
              const token = context.mcpReq._meta?.progressToken as number | string | undefined;
              const result = await backend.forward(
                method,
                arguments_,
                owner as string | undefined,
                context.mcpReq.signal,
                token === undefined
                  ? undefined
                  : (progress) => {
                      void context.mcpReq
                        .notify({ method: 'notifications/progress', params: { ...progress, progressToken: token } })
                        .catch(() => {});
                    },
              );
              outcome = result.resultType === 'task' ? 'task' : 'complete';
              return result;
            } catch (error) {
              outcome = error instanceof ControlError ? error.code : 'interrupted';
              throw error;
            } finally {
              console.info(
                JSON.stringify({
                  event: 'mcp_request',
                  sandboxId: id,
                  generation: backend.generation,
                  correlationId,
                  method,
                  durationMs: Date.now() - begun,
                  outcome,
                }),
              );
            }
          });
        }
        return server;
      },
      { legacy: 'reject' },
    );
    routes.set(id, { generation: backend.generation, backend, handler });
    return handler;
  };
  const http = createServer(async (req, res) => {
    if (!validToken(req.headers.authorization, token)) {
      respond(res, 401, { error: 'unauthorized' });
      return;
    }
    if (registry.unhealthy) {
      await haltExecution();
      respond(res, 503, { error: 'registry unavailable' });
      return;
    }
    if (!req.headers.host || !config.http.allowedHosts.includes(req.headers.host)) {
      respond(res, 403, { error: 'host not allowed' });
      return;
    }
    if (req.headers.origin && !config.http.allowedOrigins.includes(req.headers.origin)) {
      respond(res, 403, { error: 'origin not allowed' });
      return;
    }
    const found = /^\/sandboxes\/(sbx_[a-f0-9]{32})\/mcp$/.exec(req.url ?? '');
    if (!found) {
      respond(res, 404, { error: 'not found' });
      return;
    }
    const id = found[1];
    let record;
    try {
      record = await control.inspect(id);
    } catch (error) {
      if (error instanceof ControlError && error.code === 'not_found') {
        respond(res, 404, { error: 'not found' });
        return;
      }
      registry.unhealthy = true;
      await haltExecution();
      respond(res, 503, { error: 'registry unavailable' });
      return;
    }
    if (!record) {
      respond(res, 404, { error: 'not found' });
      return;
    }
    if (record.state === 'deleted') {
      respond(res, 410, { error: 'deleted' });
      return;
    }
    if (record.state !== 'ready') {
      respond(res, 503, { error: 'not_running' });
      return;
    }
    const backend = control.backends.get(id) as PeriBackend | undefined;
    if (!backend || backend.generation !== record.generation) {
      respond(res, 503, { error: 'not_running' });
      return;
    }
    if (Number(req.headers['content-length'] ?? 0) > 1048576) {
      respond(res, 413, { error: 'request too large' });
      return;
    }
    try {
      const chunks: Buffer[] = [];
      let size = 0;
      for await (const data of req) {
        const chunk = Buffer.from(data);
        size += chunk.length;
        if (size > 1048576) {
          respond(res, 413, { error: 'request too large' });
          return;
        }
        chunks.push(chunk);
      }
      const url = new URL(req.url!, `http://${req.headers.host}`);
      const abort = new AbortController();
      const disconnect = () => {
        if (!res.writableFinished) abort.abort(new ControlError('interrupted', 'HTTP client disconnected'));
      };
      res.once('close', disconnect);
      const request = new Request(url, {
        method: req.method,
        headers: req.headers as HeadersInit,
        signal: abort.signal,
        ...(req.method === 'POST' ? { body: Buffer.concat(chunks) } : {}),
      });
      const response = await route(id, backend).fetch(request);
      res.writeHead(response.status, Object.fromEntries(response.headers.entries()));
      if (response.body) {
        let bytes = 0;
        const limiter = new Transform({
          transform(chunk: Buffer, _encoding, done) {
            bytes += chunk.length;
            if (bytes > 8 * 1024 * 1024) {
              abort.abort();
              done(new ControlError('limit_exceeded', 'MCP response exceeds 8 MiB'));
            } else done(null, chunk);
          },
        });
        pipeline(Readable.fromWeb(response.body as never), limiter, res, (error) => {
          if (error && !abort.signal.aborted) console.error('MCP response stream failed:', id, error);
        });
      } else res.end();
    } catch (error) {
      if (!res.headersSent) respond(res, 500, { error: 'internal' });
      console.error('mcp exchange failed:', error);
    }
  });
  http.maxConnections = 32;
  http.requestTimeout = 15000;
  http.headersTimeout = 10000;
  http.timeout = 15000;
  const socketPath = join(config.controlRoot, 'control.sock');
  if (existsSync(socketPath)) {
    if (!lstatSync(socketPath).isSocket()) throw new ControlError('conflict', 'control socket path is not a socket');
    await new Promise<void>((resolve, reject) => {
      const probe = createConnection(socketPath);
      probe.once('connect', () => {
        probe.end();
        reject(new ControlError('conflict', 'control socket already accepts connections'));
      });
      probe.once('error', (error: NodeJS.ErrnoException) =>
        error.code === 'ECONNREFUSED' ? resolve() : reject(error),
      );
    });
    unlinkSync(socketPath);
  }
  const socket = createUnixServer((connection) => {
    let input = '';
    connection.setTimeout(30000, () => connection.destroy());
    connection.on('data', async (data) => {
      input += data.toString('utf8');
      if (input.length > 65536) {
        connection.destroy();
        return;
      }
      if (!input.includes('\n')) return;
      const line = input.slice(0, input.indexOf('\n'));
      input = '';
      const begun = Date.now();
      let method = 'invalid',
        requestId = '',
        outcome = 'error';
      try {
        const request = JSON.parse(line);
        method = typeof request.method === 'string' ? request.method.slice(0, 64) : 'invalid';
        requestId = typeof request.requestId === 'string' ? request.requestId : '';
        const { id, spec, deleteData } = request;
        let result: unknown;
        switch (method) {
          case 'create':
            result = await control.create(spec, { requestId });
            break;
          case 'start':
            result = await control.start(id, { requestId });
            break;
          case 'stop':
            result = await control.stop(id, { requestId });
            routes.delete(id);
            break;
          case 'destroy':
            result = await control.destroy(id, { requestId, deleteData });
            routes.delete(id);
            break;
          case 'inspect':
            result = await control.inspect(id);
            break;
          case 'list':
            result = await control.list();
            break;
          default:
            throw new ControlError('conflict', 'unknown method');
        }
        connection.end(JSON.stringify({ result }) + '\n');
        outcome = 'complete';
      } catch (error) {
        const failure = error instanceof ControlError ? error : new ControlError('interrupted', String(error));
        outcome = failure.code;
        connection.end(JSON.stringify({ error: { code: failure.code, message: failure.message } }) + '\n');
      } finally {
        console.info(
          JSON.stringify({
            event: 'control_request',
            method,
            requestId: requestId || undefined,
            durationMs: Date.now() - begun,
            outcome,
          }),
        );
        if (registry.unhealthy)
          void haltExecution().catch((error) => console.error('registry failure cleanup failed:', error));
      }
    });
  });
  try {
    await new Promise<void>((resolve, reject) => socket.once('error', reject).listen(socketPath, () => resolve()));
    chmodSync(socketPath, 0o600);
    const port = await pickPort(http, config.listen.host, config.listen.port, process.platform === 'darwin');
    if (port !== config.listen.port) {
      const baseUrl = new URL(config.publicBaseUrl);
      const originalHost = `${baseUrl.hostname}:${config.listen.port}`;
      baseUrl.port = String(port);
      config.listen.port = port;
      config.publicBaseUrl = baseUrl.toString();
      config.http.allowedHosts = config.http.allowedHosts.map((host) =>
        host === originalHost ? `${baseUrl.hostname}:${port}` : host,
      );
    }
    if (process.platform === 'darwin') registry.rebaseMcpUrls(config.publicBaseUrl.replace(/\/$/, ''));
  } catch (error) {
    if (socket.listening) socket.close();
    if (http.listening) http.close();
    registry.close();
    closeSync(lockFd);
    throw error;
  }
  let shutdownPromise: Promise<void> | undefined;
  const shutdown = (): Promise<void> =>
    (shutdownPromise ??= (async () => {
      const httpClosed = new Promise<void>((resolve) => http.close(() => resolve()));
      http.closeAllConnections();
      const socketClosed = new Promise<void>((resolve) => socket.close(() => resolve()));
      let cleanupFailed = false;
      try {
        for (const record of await control.list())
          if (record.state === 'ready' || record.state === 'starting') {
            await control.stop(record.id, { requestId: `shutdown:${Date.now()}:${record.id}` }).catch((error) => {
              cleanupFailed = true;
              console.error('stop failed', error);
            });
          }
      } catch (error) {
        cleanupFailed = true;
        console.error('registry unavailable during shutdown:', error);
      }
      try {
        for (const unit of await provider.listOwned()) {
          try {
            await provider.stop(unit);
          } catch (error) {
            cleanupFailed = true;
            console.error('owned unit cleanup failed:', unit, error);
          }
        }
      } catch (error) {
        cleanupFailed = true;
        console.error('owned unit enumeration failed:', error);
      }
      for (const backend of control.backends.values()) {
        try {
          await backend.close();
        } catch (error) {
          cleanupFailed = true;
          console.error('backend close failed:', error);
        }
      }
      try {
        await Promise.all([httpClosed, socketClosed]);
      } finally {
        registry.close();
        closeSync(lockFd);
      }
      if (cleanupFailed) throw new ControlError('stop_incomplete', 'sandbox cleanup failed during shutdown');
    })());
  return { close: shutdown };
}

export async function serve(config: Config): Promise<void> {
  const server = await startServer(config);
  handleShutdown(server);
}

export function handleShutdown(server: ServerHandle): void {
  const onSignal = () => {
    process.off('SIGTERM', onSignal);
    process.off('SIGINT', onSignal);
    void server.close().catch((error) => {
      console.error('shutdown failed:', error);
      process.exitCode = 1;
    });
  };
  process.once('SIGTERM', onSignal);
  process.once('SIGINT', onSignal);
}
