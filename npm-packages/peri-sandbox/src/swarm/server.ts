import { timingSafeEqual } from 'node:crypto';
import { SandboxError } from '../instance/types.js';
import { assertLogicalEnvId, SwarmError } from './errors.js';
import { forwardSandboxMcp } from './forward.js';
import type { SwarmManager, SwarmSandboxDescription } from './manager.js';

const JSON_HEADERS = { 'content-type': 'application/json; charset=utf-8' };

/** HTTP projection of a reserved, running or previously bound sandbox. Never contains the inner token. */
export interface SwarmSandboxView {
  readonly logicalEnvId: string;
  readonly sandboxId: string;
  readonly state: SwarmSandboxDescription['state'];
  readonly runId: string | null;
  readonly workspacePath: string;
  readonly mcpUrl: string | null;
  readonly lastUsedAt: string;
  readonly lastError: { code: string; message: string } | null;
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

function viewOf(description: SwarmSandboxDescription, origin: string): SwarmSandboxView {
  return {
    logicalEnvId: description.logicalEnvId,
    sandboxId: description.sandboxId,
    state: description.state,
    runId: description.runId,
    workspacePath: description.workspacePath,
    mcpUrl:
      description.state === 'stopped'
        ? null
        : new URL(`/sandboxes/${encodeURIComponent(description.logicalEnvId)}/mcp`, origin).toString(),
    lastUsedAt: description.lastUsedAt,
    lastError: description.lastError,
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
    const lazy = (body as Record<string, unknown>).lazy;
    if (lazy !== undefined && typeof lazy !== 'boolean')
      return errorResponse(400, 'invalid_request', 'lazy must be a boolean.');
    assertLogicalEnvId(logicalEnvId);
    if (lazy === true) {
      // Lazy reservation: allocate identity/path/URL only, never start SRT here.
      const reservation = await manager.reserve(logicalEnvId);
      return jsonResponse(200, { sandbox: viewOf(reservation, url.origin) });
    }
    const entry = await manager.ensure(logicalEnvId);
    const description = manager.describe(entry.logicalEnvId);
    if (description === undefined) throw new SwarmError('invalid_config', 'sandbox started without a description.');
    return jsonResponse(200, { sandbox: viewOf(description, url.origin) });
  }

  function getSandbox(url: URL, logicalEnvId: string): Response {
    const description = manager.describe(logicalEnvId);
    if (description === undefined) return errorResponse(404, 'sandbox_not_found', 'sandbox is not running.');
    return jsonResponse(200, { sandbox: viewOf(description, url.origin) });
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
    let entry = manager.get(logicalEnvId);
    if (entry === undefined || entry.instance.state !== 'ready') {
      // A real MCP request is the start trigger for a reserved environment.
      // A binding without a reservation (stopped/unknown) is not revived here.
      if (!manager.canStart(logicalEnvId)) return errorResponse(404, 'sandbox_not_found', 'sandbox is not running.');
      entry = await manager.ensure(logicalEnvId);
    }
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
