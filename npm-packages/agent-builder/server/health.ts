import type { EngineHealth, HealthResponse } from '@shared/api';
import { Hono } from 'hono';

export type EngineHealthProvider = () => EngineHealth | Promise<EngineHealth>;

const notConfigured: EngineHealthProvider = () => ({ mode: 'not_configured', status: 'unknown' });

/**
 * The engine provider is the seam where M3 plugs in the rivet registry health. It may probe the
 * Store, so the handler awaits it; the probe owns a 2s budget and never waits indefinitely.
 */
export function createHealthRoutes(getEngineHealth: EngineHealthProvider = notConfigured): Hono {
  const routes = new Hono();

  routes.get('/', async (c) => {
    const body: HealthResponse = {
      ok: true,
      engine: await getEngineHealth(),
      runtime: { name: 'bun', version: Bun.version },
    };
    return c.json(body);
  });

  return routes;
}
