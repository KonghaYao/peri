import { SwarmError } from './errors.js';
import { resolveSwarmSandboxesRoot, SwarmManager } from './manager.js';
import { createSwarmRequestHandler } from './server.js';

const DEFAULT_BIND = '127.0.0.1:8081';
const LOOPBACK_HOSTS = new Set(['127.0.0.1', '::1', 'localhost']);

export interface SwarmBindAddress {
  readonly hostname: string;
  readonly port: number;
}

/** Parse `PERI_SWARM_BUN_BIND`; the service is loopback-only by construction. */
export function parseSwarmBind(value: string): SwarmBindAddress {
  const trimmed = value.trim();
  let hostname: string;
  let portText: string;
  if (trimmed.startsWith('[')) {
    const end = trimmed.indexOf(']');
    if (end === -1 || trimmed[end + 1] !== ':')
      throw new SwarmError('invalid_config', `PERI_SWARM_BUN_BIND must be host:port: ${value}`);
    hostname = trimmed.slice(1, end);
    portText = trimmed.slice(end + 2);
  } else {
    const separator = trimmed.lastIndexOf(':');
    if (separator <= 0) throw new SwarmError('invalid_config', `PERI_SWARM_BUN_BIND must be host:port: ${value}`);
    hostname = trimmed.slice(0, separator);
    portText = trimmed.slice(separator + 1);
  }
  if (!LOOPBACK_HOSTS.has(hostname))
    throw new SwarmError('invalid_config', `PERI_SWARM_BUN_BIND host must be loopback: ${hostname}`);
  if (!/^\d{1,5}$/.test(portText))
    throw new SwarmError('invalid_config', `PERI_SWARM_BUN_BIND port must be an integer: ${value}`);
  const port = Number(portText);
  if (port < 1 || port > 65_535)
    throw new SwarmError('invalid_config', `PERI_SWARM_BUN_BIND port must be between 1 and 65535: ${value}`);
  return { hostname, port };
}

function tokenFromEnv(value: string | undefined): string | undefined {
  if (value === undefined) return undefined;
  if (value.length === 0 || /\s/.test(value))
    throw new SwarmError('invalid_config', 'PERI_SWARM_BUN_TOKEN must be non-empty and contain no whitespace.');
  return value;
}

/**
 * Start the local swarm sandbox HTTP service.
 *
 * The sandbox lifecycle, settings and `PERI_PATH` handling stay in
 * `startSandbox()`; this entrypoint only adds the HTTP surface and bindings.
 */
export async function main(): Promise<void> {
  const bind = parseSwarmBind(process.env.PERI_SWARM_BUN_BIND ?? DEFAULT_BIND);
  const token = tokenFromEnv(process.env.PERI_SWARM_BUN_TOKEN);
  const root = resolveSwarmSandboxesRoot(process.env.PERI_SWARM_SANDBOXES_ROOT);
  const manager = new SwarmManager({ root });
  const handler = createSwarmRequestHandler({ manager, ...(token === undefined ? {} : { token }) });

  const server = Bun.serve({
    hostname: bind.hostname,
    port: bind.port,
    // Long MCP calls and `subscriptions/listen` streams must survive idle windows.
    idleTimeout: 255,
    fetch: (request) => handler(request),
  });
  console.log(
    JSON.stringify({
      event: 'swarm_server_listening',
      url: `http://${bind.hostname}:${server.port}`,
      sandboxesRoot: manager.root,
      auth: token !== undefined,
      pid: process.pid,
    }),
  );

  let shuttingDown = false;
  const shutdown = async (signal: string): Promise<void> => {
    if (shuttingDown) return;
    shuttingDown = true;
    console.log(JSON.stringify({ event: 'swarm_server_stopping', signal }));
    server.stop(true);
    try {
      await manager.closeAll();
    } catch (error) {
      console.error('swarm shutdown did not close every sandbox', error);
    }
    process.exit(0);
  };
  process.once('SIGINT', () => void shutdown('SIGINT'));
  process.once('SIGTERM', () => void shutdown('SIGTERM'));
}

if (import.meta.main) {
  main().catch((error: unknown) => {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  });
}
