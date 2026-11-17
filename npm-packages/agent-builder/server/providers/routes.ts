import { ProviderCreate, ProviderUpdate } from '@shared/provider';
import { Hono } from 'hono';
import { AppError, readJsonBody } from '../errors';
import type { ProviderService } from './service';

export function readRevision(value: string | undefined): number {
  if (!value || !/^(?:[1-9]\d*|"[1-9]\d*")$/.test(value))
    throw new AppError('revision_conflict', 'If-Match revision is required');
  const revision = Number(value.replaceAll('"', ''));
  if (!Number.isSafeInteger(revision)) throw new AppError('revision_conflict', 'Invalid If-Match revision');
  return revision;
}

export function createProvidersRoutes(service: ProviderService): Hono {
  const routes = new Hono();
  routes.use('*', async (c, next) => {
    c.header('Cache-Control', 'no-store');
    await next();
  });
  routes.get('/', (c) => c.json({ providers: service.list() }));
  routes.post('/', async (c) =>
    c.json({ provider: service.create(ProviderCreate.parse(await readJsonBody(c))) }, 201),
  );
  routes.get('/:id', (c) => c.json({ provider: service.get(c.req.param('id')) }));
  routes.put('/:id', async (c) => {
    const revision = readRevision(c.req.header('If-Match'));
    const input = ProviderUpdate.parse(await readJsonBody(c));
    return c.json({ provider: service.update(c.req.param('id'), input, revision) });
  });
  routes.delete('/:id', (c) => {
    service.delete(c.req.param('id'), readRevision(c.req.header('If-Match')));
    return c.body(null, 204);
  });
  return routes;
}
