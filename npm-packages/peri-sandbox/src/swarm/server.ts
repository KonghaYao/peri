import { timingSafeEqual } from 'node:crypto';
import { SandboxError } from '../instance/types.js';
import {
  COLD_WORKSPACE_PROTOCOL_VERSION,
  COLD_WORKSPACE_PROVIDER_VERSION,
  coldDiscoverResult,
  coldInitializeResult,
  coldToolsListResult,
} from './cold-tools.js';
import { assertLogicalEnvId, SwarmError } from './errors.js';
import { forwardSandboxMcp } from './forward.js';
import type { SwarmManager, SwarmSandboxDescription, SwarmSandboxEntry } from './manager.js';
import { probeWorkspaceProviderVersion } from './provider-probe.js';

const JSON_HEADERS = { 'content-type': 'application/json; charset=utf-8' };
const MCP_PROTOCOL_HEADERS = { 'mcp-protocol-version': COLD_WORKSPACE_PROTOCOL_VERSION };

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
  /** `serverInfo.version` reported by the running provider; `null` before the first probe. */
  readonly providerVersion: string | null;
}

export interface SwarmServerOptions {
  readonly manager: SwarmManager;
  /** When set, `/v1/*` and `/sandboxes/*` require `Authorization: Bearer <token>`. */
  readonly token?: string;
  /**
   * Version the deployment expects from the workspace provider. Defaults to the
   * version recorded in the generated cold snapshot. A different value never
   * rejects the tool surface; it is reported in `_meta.peri.swarmDiagnostics`.
   */
  readonly workspaceProviderVersion?: string;
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

function viewOf(
  description: SwarmSandboxDescription,
  origin: string,
  providerVersion: string | null,
): SwarmSandboxView {
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
    providerVersion,
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
  const expectedProviderVersion = options.workspaceProviderVersion ?? COLD_WORKSPACE_PROVIDER_VERSION;
  if (expectedProviderVersion.trim().length === 0)
    throw new SwarmError('invalid_config', 'workspaceProviderVersion must not be empty.');
  if (expectedProviderVersion !== COLD_WORKSPACE_PROVIDER_VERSION)
    console.warn('cold workspace snapshot provider version differs from the deployment expectation', {
      expected: expectedProviderVersion,
      actual: COLD_WORKSPACE_PROVIDER_VERSION,
    });
  const manager = options.manager;
  const expected = options.token === undefined ? undefined : Buffer.from(options.token);
  /** Last observed provider version per logical environment, keyed by run id. */
  const observedVersions = new Map<string, { runId: string; version: string | null }>();
  const probes = new Map<string, Promise<string | null>>();

  function observedProviderVersion(logicalEnvId: string): string | null {
    const entry = manager.get(logicalEnvId);
    if (entry === undefined) return null;
    const observed = observedVersions.get(logicalEnvId);
    return observed !== undefined && observed.runId === entry.runId ? observed.version : null;
  }

  /**
   * Probe the started provider once per run. A mismatch is logged with version
   * strings only: URLs and bearer tokens never enter diagnostics or logs.
   */
  async function observeProviderVersion(entry: SwarmSandboxEntry): Promise<void> {
    const cached = observedVersions.get(entry.logicalEnvId);
    if (cached !== undefined && cached.runId === entry.runId) return;
    const key = `${entry.logicalEnvId}\u0000${entry.runId}`;
    let pending = probes.get(key);
    if (pending === undefined) {
      pending = probeWorkspaceProviderVersion(entry)
        .catch(() => null)
        .finally(() => probes.delete(key));
      probes.set(key, pending);
    }
    const version = await pending;
    observedVersions.set(entry.logicalEnvId, { runId: entry.runId, version });
    if (version !== null && version !== expectedProviderVersion)
      console.warn('workspace provider version mismatch', {
        logicalEnvId: entry.logicalEnvId,
        expected: expectedProviderVersion,
        actual: version,
      });
  }

  function jsonRpcResult(id: unknown, result: Record<string, unknown>): Response {
    return jsonResponse(200, { jsonrpc: '2.0', id, result }, MCP_PROTOCOL_HEADERS);
  }

  function notificationAccepted(): Response {
    return new Response(null, { status: 202, headers: MCP_PROTOCOL_HEADERS });
  }

  /**
   * Answer the MCP warm-up exchanges that need no sandbox.
   *
   * Returns `null` when the message needs the real provider (for example
   * `tools/call`); the caller then starts the single-flight instance and
   * forwards the original bytes.
   */
  function coldMcpResponse(text: string): Response | null {
    let parsed: unknown;
    try {
      parsed = JSON.parse(text);
    } catch {
      return null;
    }
    if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) return null;
    const message = parsed as Record<string, unknown>;
    const method = message.method;
    if (typeof method !== 'string') return null;
    // Notifications (including `notifications/initialized`) are acknowledged
    // without starting the sandbox: there is nothing to forward yet.
    if (!Object.hasOwn(message, 'id')) return notificationAccepted();
    switch (method) {
      case 'initialize':
        return jsonRpcResult(message.id, coldInitializeResult(expectedProviderVersion));
      case 'server/discover':
        return jsonRpcResult(message.id, coldDiscoverResult(expectedProviderVersion));
      case 'tools/list':
        return jsonRpcResult(message.id, coldToolsListResult(expectedProviderVersion));
      case 'ping':
        return jsonRpcResult(message.id, {});
      default:
        return null;
    }
  }

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
      return jsonResponse(200, { sandbox: viewOf(reservation, url.origin, null) });
    }
    const entry = await manager.ensure(logicalEnvId);
    const description = manager.describe(entry.logicalEnvId);
    if (description === undefined) throw new SwarmError('invalid_config', 'sandbox started without a description.');
    return jsonResponse(200, {
      sandbox: viewOf(description, url.origin, observedProviderVersion(description.logicalEnvId)),
    });
  }

  function getSandbox(url: URL, logicalEnvId: string): Response {
    const description = manager.describe(logicalEnvId);
    if (description === undefined) return errorResponse(404, 'sandbox_not_found', 'sandbox is not running.');
    return jsonResponse(200, {
      sandbox: viewOf(description, url.origin, observedProviderVersion(logicalEnvId)),
    });
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

  async function forward(entry: SwarmSandboxEntry, request: Request): Promise<Response> {
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

  function proxyMcp(logicalEnvId: string, request: Request): Promise<Response> {
    manager.beginRequest(logicalEnvId);
    let released = false;
    const release = (): void => {
      if (released) return;
      released = true;
      manager.endRequest(logicalEnvId);
    };
    return (async () => {
      try {
        // Reclaim first wins: wait for a close that already started, then route.
        await manager.whenIdleSettled(logicalEnvId);
        const response = await routeMcp(logicalEnvId, request);
        return trackResponseBody(response, release);
      } catch (error) {
        release();
        throw error;
      }
    })();
  }

  /**
   * Keep the environment busy until the proxied body is fully delivered.
   * Streaming responses (`subscriptions/listen`) must not be reclaimed mid-stream.
   */
  function trackResponseBody(response: Response, release: () => void): Response {
    const body = response.body;
    if (body === null) {
      release();
      return response;
    }
    const reader = body.getReader();
    let finished = false;
    const finish = (): void => {
      if (finished) return;
      finished = true;
      release();
    };
    const stream = new ReadableStream<Uint8Array>({
      async pull(controller) {
        try {
          const chunk = await reader.read();
          if (chunk.done) {
            finish();
            controller.close();
            return;
          }
          controller.enqueue(chunk.value);
        } catch (error) {
          finish();
          controller.error(error);
        }
      },
      async cancel(reason) {
        try {
          await reader.cancel(reason);
        } finally {
          finish();
        }
      },
    });
    return new Response(stream, {
      status: response.status,
      statusText: response.statusText,
      headers: response.headers,
    });
  }

  async function routeMcp(logicalEnvId: string, request: Request): Promise<Response> {
    const running = manager.get(logicalEnvId);
    if (running !== undefined && running.instance.state === 'ready') return await forward(running, request);

    // A binding without a reservation (stopped/unknown) is not revived here.
    if (!manager.canStart(logicalEnvId)) return errorResponse(404, 'sandbox_not_found', 'sandbox is not running.');

    // Stream/session operations have no cold answer. Starting SRT just to open
    // an idle SSE stream would defeat lazy reservation, so they get an explicit
    // 405 until a real tool call starts the instance.
    if (request.method !== 'POST')
      return errorResponse(405, 'sandbox_cold', 'sandbox is reserved; the first tools/call starts it.', {
        allow: 'POST',
      });

    let body: string;
    try {
      body = await request.text();
    } catch {
      return errorResponse(400, 'invalid_request', 'request body could not be read.');
    }
    const cold = coldMcpResponse(body);
    if (cold !== null) return cold;

    // Everything the snapshot cannot answer (tools/call and every other
    // request) starts the instance through the manager's single-flight path.
    const entry = await manager.ensure(logicalEnvId);
    if (entry.instance.state !== 'ready')
      return errorResponse(503, 'sandbox_not_ready', `sandbox is ${entry.instance.state}.`);
    await observeProviderVersion(entry);
    const forwarded = new Request(request.url, {
      method: request.method,
      headers: request.headers,
      body,
      signal: request.signal,
    });
    return await forward(entry, forwarded);
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
