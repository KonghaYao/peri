import { timingSafeEqual } from 'node:crypto';
import { SandboxError, type SandboxState } from '../instance/types.js';
import { assertLogicalEnvId, SwarmError } from './errors.js';
import { forwardSandboxMcp } from './forward.js';
import type { SwarmManager, SwarmSandboxEntry } from './manager.js';

const JSON_HEADERS = { 'content-type': 'application/json; charset=utf-8' };

/** HTTP projection of a running or previously bound sandbox. Never contains the inner token. */
export interface SwarmSandboxView {
  readonly logicalEnvId: string;
  readonly sandboxId: string;
  readonly state: SandboxState;
  readonly runId: string;
  readonly workspacePath: string;
  readonly mcpUrl: string | null;
  readonly lastUsedAt: string;
}

export interface SwarmServerOptions {
  readonly manager: SwarmManager;
  /** When set, `/v1/*` and `/sandboxes/*` require `Authorization: Bearer <token>`. */
  readonly token?: string;
}

export type SwarmRequestHandler = (request: Request) => Promise<Response>;

function jsonResponse(status: number, body: unknown, headers: Record<string, string> = {}): Response {
  return new Response(JSON.stringify(body), { status, headers: { ...JSON_HEADERS, ...headers } });
}

function errorResponse(status: number, code: string, message: string, headers: Record<string, string> = {}): Response {
  return jsonResponse(status, { error: { code, message } }, headers);
}

function tokenMatches(request: Request, expected: Buffer): boolean {
  const header = request.headers.get('authorization');
  if (header === null || !header.startsWith('Bearer ')) return false;
  const actual = Buffer.from(header.slice('Bearer '.length));
  return actual.length === expected.length && timingSafeEqual(actual, expected);
}

function decodeSegment(value: string): string | null {
  try {
    return decodeURIComponent(value);
  } catch {
    return null;
  }
}

function viewOf(entry: SwarmSandboxEntry, origin: string): SwarmSandboxView {
  return {
    logicalEnvId: entry.logicalEnvId,
    sandboxId: entry.sandboxId,
    state: entry.instance.state,
    runId: entry.runId,
    workspacePath: entry.workspacePath,
    mcpUrl: new URL(`/sandboxes/${encodeURIComponent(entry.logicalEnvId)}/mcp`, origin).toString(),
    lastUsedAt: entry.lastUsedAt.toISOString(),
  };
}

function mapError(error: unknown): Response {
  if (error instanceof SwarmError) {
    const status = error.code === 'invalid_logical_env_id' || error.code === 'invalid_config' ? 400 : 500;
    return errorResponse(status, error.code, error.message);
  }
  if (error instanceof SandboxError) {
    switch (error.code) {
      case 'invalid_options':
        return errorResponse(400, error.code, error.message);
      case 'conflict':
        return errorResponse(409, error.code, error.message);
      case 'start_failed':
      case 'backend_exited':
        return errorResponse(502, error.code, error.message);
      default:
        return errorResponse(500, error.code, error.message);
    }
  }
  console.error('swarm request failed', error);
  return errorResponse(500, 'internal_error', 'unexpected server error');
}

/**
 * Build the swarm HTTP handler.
 *
 * Routes: `GET /healthz`, `POST /v1/sandboxes`,
 * `GET|DELETE /v1/sandboxes/{logicalEnvId}`, and
 * `POST|GET|DELETE /sandboxes/{logicalEnvId}/mcp` (reverse proxy).
 */
export function createSwarmRequestHandler(options: SwarmServerOptions): SwarmRequestHandler {
  if (options.token !== undefined && options.token.length === 0)
    throw new SwarmError('invalid_config', 'PERI_SWARM_BUN_TOKEN must not be empty.');
  const manager = options.manager;
  const expected = options.token === undefined ? undefined : Buffer.from(options.token);

  async function ensureSandbox(request: Request, url: URL): Promise<Response> {
    let body: unknown;
    try {
      body = await request.json();
    } catch {
      return errorResponse(400, 'invalid_request', 'request body must be JSON.');
    }
    if (body === null || typeof body !== 'object' || Array.isArray(body))
      return errorResponse(400, 'invalid_request', 'request body must be a JSON object.');
    const logicalEnvId = (body as Record<string, unknown>).logicalEnvId;
    if (typeof logicalEnvId !== 'string')
      return errorResponse(400, 'invalid_request', 'logicalEnvId must be a string.');
    assertLogicalEnvId(logicalEnvId);
    const entry = await manager.ensure(logicalEnvId);
    return jsonResponse(200, { sandbox: viewOf(entry, url.origin) });
  }

  function getSandbox(url: URL, logicalEnvId: string): Response {
    const entry = manager.get(logicalEnvId);
    if (entry !== undefined) return jsonResponse(200, { sandbox: viewOf(entry, url.origin) });
    const binding = manager.binding(logicalEnvId);
    if (binding !== undefined) {
      const sandbox: SwarmSandboxView = {
        logicalEnvId,
        sandboxId: binding.sandboxId,
        state: 'stopped',
        runId: binding.lastRunId,
        workspacePath: binding.workspacePath,
        mcpUrl: null,
        lastUsedAt: binding.lastUsedAt,
      };
      return jsonResponse(200, { sandbox });
    }
    return errorResponse(404, 'sandbox_not_found', 'sandbox is not running.');
  }

  async function deleteSandbox(url: URL, logicalEnvId: string): Promise<Response> {
    const raw = url.searchParams.get('deleteData');
    let deleteData = false;
    if (raw !== null) {
      if (raw === 'true') deleteData = true;
      else if (raw !== 'false') return errorResponse(400, 'invalid_request', 'deleteData must be true or false.');
    }
    const result = await manager.close(logicalEnvId, { deleteData });
    if (!result.found) return errorResponse(404, 'sandbox_not_found', 'sandbox is not running.');
    return jsonResponse(200, {
      logicalEnvId,
      closed: result.closed,
      deletedData: result.deletedData,
    });
  }

  async function proxyMcp(logicalEnvId: string, request: Request): Promise<Response> {
    const entry = manager.get(logicalEnvId);
    if (entry === undefined) return errorResponse(404, 'sandbox_not_found', 'sandbox is not running.');
    if (entry.instance.state !== 'ready')
      return errorResponse(503, 'sandbox_not_ready', `sandbox is ${entry.instance.state}.`);
    try {
      return await forwardSandboxMcp(entry, request);
    } catch (error) {
      return errorResponse(
        502,
        'mcp_unavailable',
        error instanceof Error ? error.message : 'sandbox MCP request failed.',
      );
    }
  }

  async function route(request: Request): Promise<Response> {
    let url: URL;
    try {
      url = new URL(request.url);
    } catch {
      return errorResponse(400, 'invalid_request', 'request URL is invalid.');
    }
    const path = url.pathname;

    if (path === '/healthz') {
      if (request.method !== 'GET')
        return errorResponse(405, 'method_not_allowed', 'method not allowed.', { allow: 'GET' });
      return jsonResponse(200, { status: 'ok' });
    }

    const guarded =
      path === '/v1' || path.startsWith('/v1/') || path === '/sandboxes' || path.startsWith('/sandboxes/');
    if (guarded && expected !== undefined && !tokenMatches(request, expected))
      return errorResponse(401, 'unauthorized', 'missing or invalid bearer token.');

    if (path === '/v1/sandboxes') {
      if (request.method !== 'POST')
        return errorResponse(405, 'method_not_allowed', 'method not allowed.', { allow: 'POST' });
      return await ensureSandbox(request, url);
    }

    const sandbox = /^\/v1\/sandboxes\/([^/]+)$/.exec(path);
    if (sandbox !== null) {
      if (request.method !== 'GET' && request.method !== 'DELETE')
        return errorResponse(405, 'method_not_allowed', 'method not allowed.', { allow: 'GET, DELETE' });
      const logicalEnvId = decodeSegment(sandbox[1] ?? '');
      if (logicalEnvId === null)
        return errorResponse(400, 'invalid_request', 'logicalEnvId is not valid URL encoding.');
      assertLogicalEnvId(logicalEnvId);
      return request.method === 'GET' ? getSandbox(url, logicalEnvId) : await deleteSandbox(url, logicalEnvId);
    }

    const proxy = /^\/sandboxes\/([^/]+)\/mcp$/.exec(path);
    if (proxy !== null) {
      if (request.method !== 'POST' && request.method !== 'GET' && request.method !== 'DELETE')
        return errorResponse(405, 'method_not_allowed', 'method not allowed.', { allow: 'POST, GET, DELETE' });
      const logicalEnvId = decodeSegment(proxy[1] ?? '');
      if (logicalEnvId === null)
        return errorResponse(400, 'invalid_request', 'logicalEnvId is not valid URL encoding.');
      assertLogicalEnvId(logicalEnvId);
      return await proxyMcp(logicalEnvId, request);
    }

    return errorResponse(404, 'not_found', 'route not found.');
  }

  return async (request: Request): Promise<Response> => {
    try {
      return await route(request);
    } catch (error) {
      return mapError(error);
    }
  };
}
