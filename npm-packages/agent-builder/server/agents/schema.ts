import { BareHarnessConfig } from '@peri-code/sdk';
import { AgentDraft } from '@shared/agent';
import { AppError, appErrorFromZod } from '../errors';

/** The capability keys the SDK actually understands; anything else would be silently ignored. */
const META_HARNESS_KEYS = new Set(Object.keys(BareHarnessConfig));

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/** ACP transports hold live SDK objects, so they cannot be represented in stored JSON. */
function rejectLiveMcpTransports(input: unknown): void {
  if (!isRecord(input) || !isRecord(input.config)) return;
  const servers = input.config.mcpServers;
  if (!isRecord(servers)) return;

  for (const [name, server] of Object.entries(servers)) {
    if (isRecord(server) && server.type === 'acp') {
      throw new AppError(
        'unsupported_mcp_type',
        'ACP MCP servers cannot be stored; use the "http" or "stdio" transport',
        { details: { server: name, type: 'acp' } },
      );
    }
  }
}

export function parseAgentDraft(input: unknown): AgentDraft {
  rejectLiveMcpTransports(input);

  const result = AgentDraft.safeParse(input);
  if (!result.success) throw appErrorFromZod(result.error);

  const unknownKeys = Object.keys(result.data.config.metaHarness)
    .filter((key) => !META_HARNESS_KEYS.has(key))
    .sort();
  if (unknownKeys.length > 0) {
    throw new AppError('invalid_input', 'Unknown metaHarness keys', { details: { unknownKeys } });
  }

  return result.data;
}
