import { Hono } from 'hono';
import type { z } from 'zod';
import { assertExecutableEnvironment } from '../environments/capabilities';
import {
  buildEnvironmentConfig,
  CreateEnvironmentBodySchema,
  type EnvironmentConfig,
  type EnvironmentConfigurationSchema,
  environmentConfigFromColumn,
  type TemplateConfig,
  templateConfigFromColumn,
  templateDeclaresNetwork,
  withoutConfidentialEnvironmentInput,
} from '../environments/config';
import { EnvironmentRequestError, handleEnvironmentErrors, requiredPathParam } from '../environments/errors';
import { writeInlineEnvironmentFile } from '../environments/files';
import { buildIdListPage, environmentResource } from '../environments/serialize';
import { executableEnvironmentType } from '../environments/type';
import { assertDeploymentWorkspace } from '../environments/workspace';
import { PlatformRequestError } from '../errors';
import { serializeJsonColumn } from '../json';
import { parsePageRequest } from '../pagination';
import type { PlatformRepositories } from '../repositories';
import type {
  PlatformEnvironmentRow,
  PlatformEnvironmentsRepository,
  PlatformEnvironmentType,
} from '../repositories/environments';
import { parsePlatformJson } from '../request';

type EnvironmentConfiguration = z.infer<typeof EnvironmentConfigurationSchema>;

interface LoadedTemplate {
  readonly config: TemplateConfig;
  readonly declaresNetwork: boolean;
}

function loadTemplate(repos: PlatformRepositories, templateId: string): LoadedTemplate {
  const row = repos.environmentTemplates.get(templateId);
  if (row === null) {
    throw new EnvironmentRequestError('not_found', 'Environment template not found', 404);
  }
  return {
    config: templateConfigFromColumn(row.config),
    declaresNetwork: templateDeclaresNetwork(row.config),
  };
}

/**
 * The list filter keeps the hosting types this deployment stores. Hosted types match nothing
 * instead of being rewritten to a local filter, and undocumented values are invalid input.
 */
function listTypeFilter(requested: string | undefined): readonly PlatformEnvironmentType[] | null {
  if (requested === undefined || requested.length === 0) return null;
  if (requested === 'self_hosted') return ['self_hosted', 'local'];
  if (requested === 'none') return ['none'];
  if (requested === 'openai_hosted') return [];
  throw new PlatformRequestError('Unknown environment type', 'type');
}

function assertCapabilityDirectoriesSupported(value: unknown, param: string): void {
  if (Array.isArray(value) && value.length > 0) {
    throw new EnvironmentRequestError(
      'environment_capability_unsupported',
      'capability_directories are not supported by the local workspace',
      400,
      param,
    );
  }
}

/**
 * Merges the referenced template with inline configuration. Arrays and settings replace
 * the template value; environment variables merge with inline values taking precedence.
 */
function buildMergedEnvironmentConfig(
  input: EnvironmentConfiguration,
  templateId: string | null,
  templateConfig: TemplateConfig | null,
  workspaceDirectory: string,
  vaultIds: readonly string[],
): EnvironmentConfig {
  const packages = input.packages === undefined ? templateConfig?.packages : input.packages;
  const setupCommands =
    input.setup_commands === undefined ? templateConfig?.setup_commands : input.setup_commands;
  const network = input.network === undefined ? templateConfig?.network : input.network;
  const desktop = input.desktop === undefined ? templateConfig?.desktop : input.desktop;
  const env =
    input.env === undefined || input.env === null
      ? (templateConfig?.env ?? {})
      : { ...(templateConfig?.env ?? {}), ...input.env };
  const capabilityDirectories =
    input.capability_directories === undefined
      ? (templateConfig?.capability_directories ?? [])
      : (input.capability_directories ?? []);
  const skills = input.skills === undefined ? templateConfig?.skills : input.skills;
  const plugins = input.plugins === undefined ? templateConfig?.plugins : input.plugins;
  const containerSize = input.container_size ?? null;

  return buildEnvironmentConfig({
    packages,
    setup_commands: setupCommands,
    network,
    desktop,
    env,
    capability_directories: capabilityDirectories,
    skills,
    plugins,
    container_size: containerSize,
    workspace_directory: workspaceDirectory,
    vault_ids: vaultIds,
    template_id: templateId,
  });
}

function environmentById(repositories: PlatformEnvironmentsRepository, id: string): PlatformEnvironmentRow {
  const row = repositories.get(id);
  if (row === null) throw new EnvironmentRequestError('not_found', 'Environment not found', 404);
  return row;
}

/** Routes for `/v1/agents/environments`. */
export function createEnvironmentRoutes(repos: PlatformRepositories): Hono {
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
      const types = listTypeFilter(query.type);
      const rows =
        types === null || types.length > 0
          ? repos.environments.list({ ...page, limit: page.limit + 1 }, types)
          : [];
      const basePage = buildIdListPage(rows, page.limit, (row) => row.id);
      const data = basePage.data.map((row) =>
        environmentResource(
          row,
          environmentConfigFromColumn(row.config),
          repos.environmentFiles.listByEnvironment(row.id),
        ),
      );
      return c.json({ ...basePage, data });
    }),
  );

  routes.post(
    '/',
    handleEnvironmentErrors(async (c) => {
      const body = await parsePlatformJson(c, CreateEnvironmentBodySchema);
      const nested = 'environment' in body;
      const input = nested ? body.environment : body;
      const name = body.name ?? input.name ?? '';
      const vaultIds = body.vault_ids ?? [];
      const type = executableEnvironmentType(input.type, 'type');
      const templateId = input.environment_template_id ?? input.template_id ?? null;
      const template = templateId === null ? null : loadTemplate(repos, templateId);

      assertCapabilityDirectoriesSupported(input.capability_directories, 'capability_directories');
      const workspaceDirectory = assertDeploymentWorkspace(input.workspace_directory, 'workspace_directory');
      const config = buildMergedEnvironmentConfig(
        input,
        templateId,
        template?.config ?? null,
        workspaceDirectory,
        vaultIds,
      );
      assertCapabilityDirectoriesSupported(config.capability_directories, 'capability_directories');
      assertExecutableEnvironment(config, {
        networkRequested:
          (input.network !== undefined && input.network !== null) || template?.declaresNetwork === true,
        vaultIds,
      });

      const persistedConfig = withoutConfidentialEnvironmentInput(config);

      // Creating the environment and materializing its inline files is one unit: a rejected
      // file must not leave an environment with a partial configuration.
      const row = repos.environments.db.transaction(() => {
        const created = repos.environments.create({
          type,
          status: 'ready',
          name,
          config: serializeJsonColumn(persistedConfig),
        });
        for (const file of input.files ?? []) {
          if (file.type === 'file_id') {
            throw new EnvironmentRequestError(
              'environment_file_reference_unsupported',
              'Only inline file content is supported',
              400,
              'files',
            );
          }
          writeInlineEnvironmentFile(
            repos.environmentFiles,
            created.id,
            { path: file.path, data: file.data },
            workspaceDirectory,
          );
        }
        return created;
      })();

      return c.json(
        environmentResource(row, persistedConfig, repos.environmentFiles.listByEnvironment(row.id)),
        201,
      );
    }),
  );

  routes.get(
    '/:environment_id',
    handleEnvironmentErrors((c) => {
      const row = environmentById(repos.environments, requiredPathParam(c, 'environment_id'));
      const config = environmentConfigFromColumn(row.config);
      return c.json(environmentResource(row, config, repos.environmentFiles.listByEnvironment(row.id)));
    }),
  );

  return routes;
}
