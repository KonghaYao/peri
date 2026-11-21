import type { SwarmSandboxEntry } from './manager.js';

/** Best-effort probe budget; a slow probe never delays the real MCP request beyond this. */
export const PROVIDER_PROBE_TIMEOUT_MS = 2_000;

const PROTOCOL_VERSION = '2026-07-28';
const SERVER_INFO_META_KEY = 'io.modelcontextprotocol/serverInfo';

function parseJsonRpcPayload(text: string): Record<string, unknown> | null {
  const trimmed = text.trim();
  if (trimmed.length === 0) return null;
  const candidates = [trimmed];
  for (const line of trimmed.split('\n')) {
    const match = /^data:\s*(.*)$/.exec(line.trim());
    if (match?.[1] !== undefined && match[1].length > 0) candidates.push(match[1]);
  }
  for (const candidate of candidates) {
    try {
      const parsed = JSON.parse(candidate);
      if (parsed !== null && typeof parsed === 'object' && !Array.isArray(parsed))
        return parsed as Record<string, unknown>;
    } catch {
      // 下一个候选（例如 SSE data 行）。
    }
  }
  return null;
}

function providerVersionOf(payload: Record<string, unknown> | null): string | null {
  const result = payload?.result;
  if (result === null || typeof result !== 'object' || Array.isArray(result)) return null;
  const record = result as Record<string, unknown>;
  const meta = record._meta;
  const stamped =
    meta !== null && typeof meta === 'object' && !Array.isArray(meta)
      ? (meta as Record<string, unknown>)[SERVER_INFO_META_KEY]
      : undefined;
  for (const info of [record.serverInfo, stamped]) {
    if (info === null || typeof info !== 'object' || Array.isArray(info)) continue;
    const version = (info as Record<string, unknown>).version;
    if (typeof version === 'string' && version.length > 0) return version;
  }
  return null;
}

/**
 * Ask a running sandbox for the workspace provider's `serverInfo.version`.
 *
 * Uses the modern per-request `server/discover` exchange, so it needs no
 * session state and cannot change provider state. Failures return `null`:
 * version diagnostics are advisory and never gate the tool surface.
 */
export async function probeWorkspaceProviderVersion(
  entry: SwarmSandboxEntry,
  timeoutMs: number = PROVIDER_PROBE_TIMEOUT_MS,
): Promise<string | null> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  timer.unref();
  try {
    const response = await fetch(entry.mcpUrl, {
      method: 'POST',
      headers: {
        accept: 'application/json, text/event-stream',
        authorization: `Bearer ${entry.innerToken}`,
        'content-type': 'application/json',
        'mcp-method': 'server/discover',
        'mcp-protocol-version': PROTOCOL_VERSION,
      },
      body: JSON.stringify({
        jsonrpc: '2.0',
        id: 'peri-swarm-provider-version-probe',
        method: 'server/discover',
        params: {
          _meta: {
            'io.modelcontextprotocol/protocolVersion': PROTOCOL_VERSION,
            'io.modelcontextprotocol/clientCapabilities': {},
          },
        },
      }),
      signal: controller.signal,
    });
    if (!response.ok) return null;
    return providerVersionOf(parseJsonRpcPayload(await response.text()));
  } catch {
    return null;
  } finally {
    clearTimeout(timer);
  }
}
