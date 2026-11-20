import { Hono } from 'hono';
import type { z } from 'zod';
import {
  buildEnvironmentConfig,
  CreateEnvironmentBodySchema,
  degradationReasons,
  type EnvironmentConfig,
  type EnvironmentConfigurationSchema,
  environmentConfigFromColumn,
  LOCAL_ENVIRONMENT_TYPES,
  type LocalEnvironmentType,
  type TemplateConfig,
  templateConfigFromColumn,
  withoutConfidentialEnvironmentInput,
} from '../environments/config';
import { EnvironmentRequestError, handleEnvironmentErrors, requiredPathParam } from '../environments/errors';
import { writeInlineEnvironmentFile } from '../environments/files';
import { buildIdListPage, environmentResource, setDegradationHeader } from '../environments/serialize';
import { assertDeploymentWorkspace } from '../environments/workspace';
import { serializeJsonColumn } from '../json';
import { parsePageRequest } from '../pagination';
import type { PlatformRepositories } from '../repositories';
import type { PlatformEnvironmentRow, PlatformEnvironmentsRepository } from '../repositories/environments';
import { parsePlatformJson } from '../request';

type EnvironmentConfiguration = z.infer<typeof EnvironmentConfigurationSchema>;

function isLocalEnvironmentType(value: string): value is LocalEnvironmentType {
  return (LOCAL_ENVIRONMENT_TYPES as readonly string[]).includes(value);
}

interface MappedType {
  readonly type: LocalEnvironmentType;
  readonly mapped: boolean;
}

/** Types outside the locally executable set run as `local`; callers are told via a header. */
function mapEnvironmentType(requested: string | undefined): MappedType {
  if (requested === undefined) return { type: 'local', mapped: false };
  if (isLocalEnvironmentType(requested)) return { type: requested, mapped: false };
  return { type: 'local', mapped: true };
}

function loadTemplateConfig(repos: PlatformRepositories, templateId: string): TemplateConfig {
  const row = repos.environmentTemplates.get(templateId);
  if (row === null) {
    throw new EnvironmentRequestError('not_found', 'Environment template not found', 404);
  }
  return templateConfigFromColumn(row.config);
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
    degradation: [],
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
      const requestedType = query.type;
      const filterMapped =
        requestedType !== undefined && requestedType.length > 0 && !isLocalEnvironmentType(requestedType);
      const typeFilter: LocalEnvironmentType | null =
        requestedType === undefined || requestedType.length === 0
          ? null
          : isLocalEnvironmentType(requestedType)
            ? requestedType
            : 'local';

      const rows = repos.environments.list({ ...page, limit: page.limit + 1 }, typeFilter);
      const basePage = buildIdListPage(rows, page.limit, (row) => row.id);
      const data = basePage.data.map((row) =>
        environmentResource(
          row,
          environmentConfigFromColumn(row.config),
          repos.environmentFiles.listByEnvironment(row.id),
        ),
      );
      if (filterMapped) setDegradationHeader(c, ['environment_type_filter_mapped_to_local']);
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
      const requestedType = input.type;
      const { type, mapped } = mapEnvironmentType(requestedType);
      const templateId = input.environment_template_id ?? input.template_id ?? null;

      assertCapabilityDirectoriesSupported(input.capability_directories, 'capability_directories');
      const workspaceDirectory = assertDeploymentWorkspace(input.workspace_directory, 'workspace_directory');
      const templateConfig = templateId === null ? null : loadTemplateConfig(repos, templateId);
      const config = buildMergedEnvironmentConfig(
        input,
        templateId,
        templateConfig,
        workspaceDirectory,
        vaultIds,
      );
      assertCapabilityDirectoriesSupported(config.capability_directories, 'capability_directories');

      const reasons = degradationReasons(config, {
        typeMapped: mapped,
        hasVaults: vaultIds.length > 0,
      });
      const persistedConfig: EnvironmentConfig = {
        ...withoutConfidentialEnvironmentInput(config),
        degradation: reasons,
      };

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

      setDegradationHeader(c, persistedConfig.degradation);
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
      setDegradationHeader(c, config.degradation);
      return c.json(environmentResource(row, config, repos.environmentFiles.listByEnvironment(row.id)));
    }),
  );

  return routes;
}
