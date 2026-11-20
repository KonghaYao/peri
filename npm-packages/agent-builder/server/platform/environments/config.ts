import { z } from 'zod';
import { parseJsonColumn } from '../json';
import { decodeInlineFileContent } from './content';
import { normalizeEnvironmentPath } from './workspace';

export const CONTAINER_SIZES = ['small', 'medium', 'large'] as const;
export type ContainerSize = (typeof CONTAINER_SIZES)[number];

export const NETWORK_ACCESS_MODES = ['enabled', 'disabled', 'restricted'] as const;
export type NetworkAccessMode = (typeof NETWORK_ACCESS_MODES)[number];

export interface EnvironmentPackages {
  readonly python: string[];
  readonly system: string[];
  readonly npm: string[];
}

export interface StoredNetworkPolicy {
  readonly access: NetworkAccessMode;
  readonly allowed_domains: string[];
  readonly blocked_domains: string[];
}

export interface StoredDesktop {
  readonly enabled: boolean;
}

export interface StoredSetupCommand {
  readonly command: string;
  readonly cwd: string | null;
}

export type SafeSkill =
  | { readonly type: 'skill_reference'; readonly skill_id: string; readonly version: string | null }
  | { readonly type: 'inline'; readonly name: string; readonly description: string };

export type SafePlugin = {
  readonly type: 'inline';
  readonly name: string;
  readonly description: string;
};

export type SafeTemplateFile =
  | { readonly type: 'file_id'; readonly file_id: string; readonly path: string }
  | { readonly type: 'inline'; readonly path: string; readonly size_bytes: number };

/** Everything persisted for one platform environment. Confidential inputs are never echoed. */
export interface EnvironmentConfig {
  readonly packages: EnvironmentPackages;
  readonly setup_commands: StoredSetupCommand[];
  readonly network: StoredNetworkPolicy;
  readonly desktop: StoredDesktop;
  readonly env: Record<string, string>;
  readonly capability_directories: string[];
  readonly skills: SafeSkill[];
  readonly plugins: SafePlugin[];
  readonly container_size: ContainerSize | null;
  readonly workspace_directory: string | null;
  readonly vault_ids: string[];
  readonly template_id: string | null;
}

/** Everything persisted for one reusable environment template. */
export interface TemplateConfig {
  readonly packages: EnvironmentPackages;
  readonly setup_commands: StoredSetupCommand[];
  readonly network: StoredNetworkPolicy;
  readonly desktop: StoredDesktop;
  readonly env: Record<string, string>;
  readonly capability_directories: string[];
  readonly skills: SafeSkill[];
  readonly plugins: SafePlugin[];
  readonly files: SafeTemplateFile[];
}

const StringList = z.array(z.string());

export const PackagesParamSchema = z.strictObject({
  python: StringList.nullable().optional(),
  system: StringList.nullable().optional(),
  npm: StringList.nullable().optional(),
});

export const SetupCommandParamSchema = z.strictObject({
  command: z.string().max(65_536),
  cwd: z.string().max(4096).nullable().optional(),
});

export const NetworkPolicyParamSchema = z.strictObject({
  access: z.enum(NETWORK_ACCESS_MODES),
  allowed_domains: StringList.nullable().optional(),
  blocked_domains: StringList.max(100).nullable().optional(),
});

export const DesktopParamSchema = z.strictObject({
  enabled: z.boolean(),
});

const InlineArchiveSourceSchema = z.strictObject({
  type: z.literal('base64'),
  media_type: z.literal('application/zip'),
  data: z.string().min(1).max(70_254_592),
});

export const SkillParamSchema = z.discriminatedUnion('type', [
  z.strictObject({
    type: z.literal('skill_reference'),
    skill_id: z.string().min(1).max(64),
    version: z.string().max(1_048_576).nullable().optional(),
  }),
  z.strictObject({
    type: z.literal('inline'),
    name: z.string().min(1).max(64),
    description: z.string().max(1_048_576),
    source: InlineArchiveSourceSchema,
  }),
]);

export const PluginParamSchema = z.strictObject({
  type: z.literal('inline'),
  name: z.string().min(1).max(64),
  description: z.string().max(1_048_576),
  source: InlineArchiveSourceSchema,
});

export const EnvironmentFileParamSchema = z.discriminatedUnion('type', [
  z.strictObject({
    type: z.literal('file_id'),
    file_id: z.string().min(1).max(256),
    path: z.string().min(1).max(4096),
  }),
  z.strictObject({
    type: z.literal('inline'),
    data: z.string().max(9_500_000),
    path: z.string().min(1).max(4096),
  }),
]);

const ENVIRONMENT_CONFIG_SHAPE = {
  type: z.string().min(1).max(64),
  name: z.string().min(1).max(256).optional(),
  workspace_directory: z.string().min(1).max(4096).optional(),
  packages: PackagesParamSchema.nullable().optional(),
  setup_commands: z.array(SetupCommandParamSchema).max(16).nullable().optional(),
  network: NetworkPolicyParamSchema.nullable().optional(),
  desktop: DesktopParamSchema.nullable().optional(),
  env: z.record(z.string().min(1).max(256), z.string().max(1_048_576)).nullable().optional(),
  capability_directories: z.array(z.string().max(1_048_576)).max(16_384).nullable().optional(),
  skills: z.array(SkillParamSchema).max(200).nullable().optional(),
  plugins: z.array(PluginParamSchema).max(32).nullable().optional(),
  files: z.array(EnvironmentFileParamSchema).max(50).nullable().optional(),
  environment_template_id: z.string().min(1).max(64).optional(),
  /** Local alias for `environment_template_id`; the documented name wins when both appear. */
  template_id: z.string().min(1).max(64).optional(),
  container_size: z.enum(CONTAINER_SIZES).nullable().optional(),
};

export const EnvironmentConfigurationSchema = z.strictObject(ENVIRONMENT_CONFIG_SHAPE);

const VAULT_IDS_SHAPE = z.array(z.string().min(1).max(1_048_576)).max(10).nullable().optional();

/**
 * Accepts both the documented nested body (`{ environment, vault_ids }`) and a flat
 * environment object. Local callers frequently skip the wrapper; both shapes stay strict.
 */
export const CreateEnvironmentBodySchema = z.union([
  z.strictObject({
    environment: EnvironmentConfigurationSchema,
    vault_ids: VAULT_IDS_SHAPE,
    name: z.string().min(1).max(256).optional(),
  }),
  z.strictObject({
    ...ENVIRONMENT_CONFIG_SHAPE,
    vault_ids: VAULT_IDS_SHAPE,
  }),
]);

const TEMPLATE_SHAPE = {
  name: z.string().min(1).max(256).nullable().optional(),
  packages: PackagesParamSchema.nullable().optional(),
  setup_commands: z.array(SetupCommandParamSchema).max(16).nullable().optional(),
  network: NetworkPolicyParamSchema.nullable().optional(),
  desktop: DesktopParamSchema.nullable().optional(),
  env: z.record(z.string().min(1).max(256), z.string().max(1_048_576)).nullable().optional(),
  capability_directories: z.array(z.string().max(1_048_576)).max(16_384).nullable().optional(),
  skills: z.array(SkillParamSchema).max(200).nullable().optional(),
  plugins: z.array(PluginParamSchema).max(32).nullable().optional(),
  files: z.array(EnvironmentFileParamSchema).max(50).nullable().optional(),
};

export const CreateEnvironmentTemplateParamsSchema = z.strictObject(TEMPLATE_SHAPE);
export const UpdateEnvironmentTemplateParamsSchema = z.strictObject(TEMPLATE_SHAPE);

function asRecord(value: unknown): Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
}

function stringArray(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value.filter((item): item is string => typeof item === 'string');
}

function optionalString(value: unknown): string | null {
  return typeof value === 'string' ? value : null;
}

export function normalizePackages(value: unknown): EnvironmentPackages {
  const record = asRecord(value);
  return {
    python: stringArray(record.python),
    system: stringArray(record.system),
    npm: stringArray(record.npm),
  };
}

export function normalizeNetwork(value: unknown): StoredNetworkPolicy {
  const record = asRecord(value);
  const access = record.access;
  return {
    access: access === 'enabled' || access === 'restricted' || access === 'disabled' ? access : 'disabled',
    allowed_domains: stringArray(record.allowed_domains),
    blocked_domains: stringArray(record.blocked_domains),
  };
}

export function normalizeDesktop(value: unknown): StoredDesktop {
  return { enabled: asRecord(value).enabled === true };
}

export function normalizeSetupCommands(value: unknown): StoredSetupCommand[] {
  if (!Array.isArray(value)) return [];
  const commands: StoredSetupCommand[] = [];
  for (const item of value) {
    const record = asRecord(item);
    if (typeof record.command !== 'string') continue;
    commands.push({ command: record.command, cwd: optionalString(record.cwd) });
  }
  return commands;
}

export function normalizeEnvMap(value: unknown): Record<string, string> {
  const record = asRecord(value);
  const env: Record<string, string> = {};
  for (const [key, item] of Object.entries(record)) {
    if (typeof item === 'string') env[key] = item;
  }
  return env;
}

/** Drops inline archive contents and keeps only safe skill metadata. */
export function normalizeSkills(value: unknown): SafeSkill[] {
  if (!Array.isArray(value)) return [];
  const skills: SafeSkill[] = [];
  for (const item of value) {
    const record = asRecord(item);
    if (record.type === 'skill_reference' && typeof record.skill_id === 'string') {
      skills.push({
        type: 'skill_reference',
        skill_id: record.skill_id,
        version: optionalString(record.version),
      });
    } else if (record.type === 'inline' && typeof record.name === 'string') {
      skills.push({
        type: 'inline',
        name: record.name,
        description: typeof record.description === 'string' ? record.description : '',
      });
    }
  }
  return skills;
}

/** Drops inline archive contents and keeps only safe plugin metadata. */
export function normalizePlugins(value: unknown): SafePlugin[] {
  if (!Array.isArray(value)) return [];
  const plugins: SafePlugin[] = [];
  for (const item of value) {
    const record = asRecord(item);
    if (record.type === 'inline' && typeof record.name === 'string') {
      plugins.push({
        type: 'inline',
        name: record.name,
        description: typeof record.description === 'string' ? record.description : '',
      });
    }
  }
  return plugins;
}

/**
 * Strips inline file contents; sizes are recomputed from the payload when possible.
 * Paths are stored workspace-relative so the resource can render them absolutely,
 * matching the `EnvironmentFile` contract.
 */
export function normalizeTemplateFiles(value: unknown): SafeTemplateFile[] {
  if (!Array.isArray(value)) return [];
  const files: SafeTemplateFile[] = [];
  for (const item of value) {
    const record = asRecord(item);
    if (typeof record.path !== 'string') continue;
    const path = normalizeEnvironmentPath(record.path, 'files.path');
    if (record.type === 'file_id' && typeof record.file_id === 'string') {
      files.push({ type: 'file_id', file_id: record.file_id, path });
      continue;
    }
    if (record.type !== 'inline') continue;
    if (typeof record.size_bytes === 'number' && Number.isSafeInteger(record.size_bytes)) {
      files.push({ type: 'inline', path, size_bytes: record.size_bytes });
    } else if (typeof record.data === 'string') {
      files.push({
        type: 'inline',
        path,
        size_bytes: decodeInlineFileContent(record.data, 'files.data').byteLength,
      });
    }
  }
  return files;
}

function containerSize(value: unknown): ContainerSize | null {
  return value === 'small' || value === 'medium' || value === 'large' ? value : null;
}

export interface EnvironmentConfigInputs {
  readonly packages: unknown;
  readonly setup_commands: unknown;
  readonly network: unknown;
  readonly desktop: unknown;
  readonly env: unknown;
  readonly capability_directories: unknown;
  readonly skills: unknown;
  readonly plugins: unknown;
  readonly container_size: unknown;
  readonly workspace_directory: string | null;
  readonly vault_ids: readonly string[];
  readonly template_id: string | null;
}

export function buildEnvironmentConfig(inputs: EnvironmentConfigInputs): EnvironmentConfig {
  return {
    packages: normalizePackages(inputs.packages),
    setup_commands: normalizeSetupCommands(inputs.setup_commands),
    network: normalizeNetwork(inputs.network),
    desktop: normalizeDesktop(inputs.desktop),
    env: normalizeEnvMap(inputs.env),
    capability_directories: stringArray(inputs.capability_directories),
    skills: normalizeSkills(inputs.skills),
    plugins: normalizePlugins(inputs.plugins),
    container_size: containerSize(inputs.container_size),
    workspace_directory: inputs.workspace_directory,
    vault_ids: [...inputs.vault_ids],
    template_id: inputs.template_id,
  };
}

export interface TemplateConfigInputs {
  readonly packages: unknown;
  readonly setup_commands: unknown;
  readonly network: unknown;
  readonly desktop: unknown;
  readonly env: unknown;
  readonly capability_directories: unknown;
  readonly skills: unknown;
  readonly plugins: unknown;
  readonly files: unknown;
}

export function buildTemplateConfig(inputs: TemplateConfigInputs): TemplateConfig {
  return {
    packages: normalizePackages(inputs.packages),
    setup_commands: normalizeSetupCommands(inputs.setup_commands),
    network: normalizeNetwork(inputs.network),
    desktop: normalizeDesktop(inputs.desktop),
    env: normalizeEnvMap(inputs.env),
    capability_directories: stringArray(inputs.capability_directories),
    skills: normalizeSkills(inputs.skills),
    plugins: normalizePlugins(inputs.plugins),
    files: normalizeTemplateFiles(inputs.files),
  };
}

/**
 * Confidential inputs are never persisted: `setup_commands` and `env` are validated, then dropped
 * before the configuration is stored.
 */
export function withoutConfidentialEnvironmentInput(config: EnvironmentConfig): EnvironmentConfig {
  return { ...config, setup_commands: [], env: {} };
}

export function withoutConfidentialTemplateInput(config: TemplateConfig): TemplateConfig {
  return { ...config, setup_commands: [], env: {} };
}

export function environmentConfigFromColumn(raw: string | null | undefined): EnvironmentConfig {
  const record = asRecord(parseJsonColumn<unknown>(raw, {}));
  return buildEnvironmentConfig({
    packages: record.packages,
    setup_commands: record.setup_commands,
    network: record.network,
    desktop: record.desktop,
    env: record.env,
    capability_directories: record.capability_directories,
    skills: record.skills,
    plugins: record.plugins,
    container_size: record.container_size,
    workspace_directory: optionalString(record.workspace_directory),
    vault_ids: stringArray(record.vault_ids),
    template_id: optionalString(record.template_id),
  });
}

/** Whether a stored template declared a network policy; older rows simply lack the field. */
/**
 * Stored template configs always carry a normalized `network` object, so presence alone is not a
 * declaration; only an active policy means the template asks for enforced network access.
 */
export function templateDeclaresNetwork(raw: string | null | undefined): boolean {
  const record = asRecord(parseJsonColumn<unknown>(raw, {}));
  const network = asRecord(record.network);
  return network.access === 'enabled' || network.access === 'restricted';
}

export function templateConfigFromColumn(raw: string | null | undefined): TemplateConfig {
  const record = asRecord(parseJsonColumn<unknown>(raw, {}));
  return buildTemplateConfig({
    packages: record.packages,
    setup_commands: record.setup_commands,
    network: record.network,
    desktop: record.desktop,
    env: record.env,
    capability_directories: record.capability_directories,
    skills: record.skills,
    plugins: record.plugins,
    files: record.files,
  });
}
