import { Hono } from 'hono';
import { EnvironmentFileParamSchema } from '../environments/config';
import { EnvironmentRequestError, handleEnvironmentErrors, requiredPathParam } from '../environments/errors';
import { decodeFilePage, encodeFilePage, writeInlineEnvironmentFile } from '../environments/files';
import { environmentFileResource } from '../environments/serialize';
import { normalizeDirectoryFilter, resolveDeploymentWorkspace } from '../environments/workspace';
import { paginate, parsePageRequest } from '../pagination';
import type { PlatformRepositories } from '../repositories';
import type { PlatformEnvironmentFileRow } from '../repositories/environments';
import { parsePlatformJson } from '../request';

function byPath(left: PlatformEnvironmentFileRow, right: PlatformEnvironmentFileRow): number {
  if (left.path < right.path) return -1;
  if (left.path > right.path) return 1;
  return 0;
}

/** Routes for `/v1/agents/environments/{environment_id}/files`. */
export function createEnvironmentFileRoutes(repos: PlatformRepositories): Hono {
  const routes = new Hono();

  routes.get(
    '/',
    handleEnvironmentErrors((c) => {
      const environmentId = requiredPathParam(c, 'environment_id');
      if (repos.environments.get(environmentId) === null) {
        throw new EnvironmentRequestError('not_found', 'Environment not found', 404);
      }

      const query = c.req.query();
      const page = parsePageRequest({ limit: query.limit, order: query.order });
      const directory = normalizeDirectoryFilter(query.path, 'path');
      const cursor = decodeFilePage(query.page);

      const prefix = directory.length === 0 ? '' : `${directory}/`;
      const rows = repos.environmentFiles
        .listByEnvironment(environmentId)
        .filter((row) => prefix.length === 0 || row.path.startsWith(prefix))
        .sort(byPath);
      if (page.order === 'desc') rows.reverse();

      const result = paginate(rows, { ...page, after: cursor }, (row) => row.path);
      const lastId = result.last_id;
      return c.json({
        object: 'page',
        data: result.data.map(environmentFileResource),
        next: result.has_more && lastId !== null ? encodeFilePage(lastId) : null,
        has_more: result.has_more,
      });
    }),
  );

  routes.post(
    '/',
    handleEnvironmentErrors(async (c) => {
      const environmentId = requiredPathParam(c, 'environment_id');
      if (repos.environments.get(environmentId) === null) {
        throw new EnvironmentRequestError('not_found', 'Environment not found', 404);
      }

      const body = await parsePlatformJson(c, EnvironmentFileParamSchema);
      if (body.type === 'file_id') {
        throw new EnvironmentRequestError(
          'environment_file_reference_unsupported',
          'Only inline file content is supported',
          400,
          'type',
        );
      }

      const row = writeInlineEnvironmentFile(
        repos.environmentFiles,
        environmentId,
        { path: body.path, data: body.data },
        resolveDeploymentWorkspace(),
      );
      return c.json(environmentFileResource(row), 201);
    }),
  );

  return routes;
}
