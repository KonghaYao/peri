import { Hono } from 'hono';
import type { z } from 'zod';
import {
  buildTemplateConfig,
  CreateEnvironmentTemplateParamsSchema,
  type TemplateConfig,
  templateConfigFromColumn,
  UpdateEnvironmentTemplateParamsSchema,
  withoutConfidentialTemplateInput,
} from '../environments/config';
import { EnvironmentRequestError, handleEnvironmentErrors, requiredPathParam } from '../environments/errors';
import { buildIdListPage, environmentTemplateResource } from '../environments/serialize';
import { serializeJsonColumn } from '../json';
import { parsePageRequest } from '../pagination';
import type { PlatformRepositories } from '../repositories';
import { parsePlatformJson } from '../request';

type TemplateParams = z.infer<typeof CreateEnvironmentTemplateParamsSchema>;

function assertCapabilityDirectoriesSupported(value: readonly string[]): void {
  if (value.length > 0) {
    throw new EnvironmentRequestError(
      'environment_capability_unsupported',
      'capability_directories are not supported by the local workspace',
      400,
      'capability_directories',
    );
  }
}

/** Routes for `/v1/agents/environments/templates`. */
export function createEnvironmentTemplateRoutes(repos: PlatformRepositories): Hono {
  const routes = new Hono();

  routes.get(
    '/',
    handleEnvironmentErrors((c) => {
      const query = c.req.query();
      const page = parsePageRequest({
        limit: query.limit,
        order: query.order,
        after: query.after,
      });
      const rows = repos.environmentTemplates.list({ ...page, limit: page.limit + 1 });
      const basePage = buildIdListPage(rows, page.limit, (row) => row.id);
      const data = basePage.data.map((row) =>
        environmentTemplateResource(row, templateConfigFromColumn(row.config)),
      );
      return c.json({ ...basePage, data });
    }),
  );

  routes.post(
    '/',
    handleEnvironmentErrors(async (c) => {
      const parsed = await parsePlatformJson(c, CreateEnvironmentTemplateParamsSchema);
      const config = buildTemplateConfig({
        packages: parsed.packages,
        setup_commands: parsed.setup_commands,
        network: parsed.network,
        desktop: parsed.desktop,
        env: parsed.env,
        capability_directories: parsed.capability_directories,
        skills: parsed.skills,
        plugins: parsed.plugins,
        files: parsed.files,
      });
      assertCapabilityDirectoriesSupported(config.capability_directories);
      const row = repos.environmentTemplates.create({
        name: parsed.name ?? '',
        config: serializeJsonColumn(withoutConfidentialTemplateInput(config)),
      });
      return c.json(environmentTemplateResource(row, config), 201);
    }),
  );

  routes.get(
    '/:environment_template_id',
    handleEnvironmentErrors((c) => {
      const row = repos.environmentTemplates.get(requiredPathParam(c, 'environment_template_id'));
      if (row === null) {
        throw new EnvironmentRequestError('not_found', 'Environment template not found', 404);
      }
      return c.json(environmentTemplateResource(row, templateConfigFromColumn(row.config)));
    }),
  );

  routes.post(
    '/:environment_template_id',
    handleEnvironmentErrors(async (c) => {
      const row = repos.environmentTemplates.get(requiredPathParam(c, 'environment_template_id'));
      if (row === null) {
        throw new EnvironmentRequestError('not_found', 'Environment template not found', 404);
      }
      const parsed = await parsePlatformJson(c, UpdateEnvironmentTemplateParamsSchema);
      const current = templateConfigFromColumn(row.config);
      const config = mergeTemplateConfig(current, parsed);
      assertCapabilityDirectoriesSupported(config.capability_directories);
      const updated = repos.environmentTemplates.update(row.id, {
        name: parsed.name === undefined ? row.name : (parsed.name ?? ''),
        config: serializeJsonColumn(withoutConfidentialTemplateInput(config)),
      });
      if (updated === null) {
        throw new EnvironmentRequestError('not_found', 'Environment template not found', 404);
      }
      return c.json(environmentTemplateResource(updated, config));
    }),
  );

  routes.delete(
    '/:environment_template_id',
    handleEnvironmentErrors((c) => {
      const id = requiredPathParam(c, 'environment_template_id');
      if (!repos.environmentTemplates.delete(id)) {
        throw new EnvironmentRequestError('not_found', 'Environment template not found', 404);
      }
      return c.json({ id, object: 'agent.environment.template.deleted', deleted: true });
    }),
  );

  return routes;
}

/** Absent fields keep the stored value; explicit `null` resets the field to its default. */
function mergeTemplateConfig(current: TemplateConfig, patch: TemplateParams): TemplateConfig {
  return buildTemplateConfig({
    packages: patch.packages === undefined ? current.packages : patch.packages,
    setup_commands: patch.setup_commands === undefined ? current.setup_commands : patch.setup_commands,
    network: patch.network === undefined ? current.network : patch.network,
    desktop: patch.desktop === undefined ? current.desktop : patch.desktop,
    env: patch.env === undefined ? current.env : patch.env,
    capability_directories:
      patch.capability_directories === undefined
        ? current.capability_directories
        : patch.capability_directories,
    skills: patch.skills === undefined ? current.skills : patch.skills,
    plugins: patch.plugins === undefined ? current.plugins : patch.plugins,
    files: patch.files === undefined ? current.files : patch.files,
  });
}
