import { Hono } from 'hono';
import { platformError } from '../errors';
import { serializeMetadataColumn } from '../json';
import { paginate, parsePageRequest } from '../pagination';
import type { PlatformRepositories } from '../repositories';
import { parsePlatformJson } from '../request';
import { CreateVaultBody, UpdateVaultBody } from '../vaults/schemas';
import { toVaultResource } from '../vaults/serializers';

/** Routes for `/v1/vaults` and the single-vault resource. */
export function createVaultRoutes(repos: PlatformRepositories): Hono {
  const routes = new Hono();

  routes.get('/', (c) => {
    const request = parsePageRequest(c.req.query());
    // One extra row distinguishes "page is full" from "there is another page".
    const rows = repos.vaults.list({ ...request, limit: request.limit + 1 });
    const page = paginate(rows, { ...request, after: null }, (row) => row.id);
    return c.json({ ...page, data: page.data.map(toVaultResource) });
  });

  routes.post('/', async (c) => {
    const body = await parsePlatformJson(c, CreateVaultBody);
    const row = repos.vaults.create({
      name: body.name,
      metadata: serializeMetadataColumn(body.metadata ?? {}),
    });
    return c.json(toVaultResource(row), 201);
  });

  routes.get('/:vault_id', (c) => {
    const row = repos.vaults.get(c.req.param('vault_id'));
    if (row === null) return platformError(c, 'not_found', 'Vault not found', 404);
    return c.json(toVaultResource(row));
  });

  routes.post('/:vault_id', async (c) => {
    const id = c.req.param('vault_id');
    if (repos.vaults.get(id) === null) return platformError(c, 'not_found', 'Vault not found', 404);

    const body = await parsePlatformJson(c, UpdateVaultBody);
    const row = repos.vaults.update(id, {
      // The column is NOT NULL, so clearing the documented nullable name stores an empty string.
      name: body.name === undefined ? null : (body.name ?? ''),
      metadata: body.metadata === undefined ? null : serializeMetadataColumn(body.metadata),
    });
    if (row === null) return platformError(c, 'not_found', 'Vault not found', 404);
    return c.json(toVaultResource(row));
  });

  routes.delete('/:vault_id', (c) => {
    const id = c.req.param('vault_id');
    if (!repos.vaults.delete(id)) return platformError(c, 'not_found', 'Vault not found', 404);
    return c.json({ id, object: 'vault.deleted', deleted: true });
  });

  return routes;
}
