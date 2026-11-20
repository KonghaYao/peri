import { existsSync, lstatSync, realpathSync, statSync } from 'node:fs';
import { homedir } from 'node:os';
import path from 'node:path';
import { WorkspaceURI } from '@peri-code/rivet/rivet';
import type { SandboxReason, SandboxStatus } from '@shared/chat';
import { type ProviderType, validProviderUrl } from '@shared/provider';
import { z } from 'zod';
import { ConfigError } from './config-error';
import { createPrivateFileIfMissing, ensurePrivateDirectory } from './filesystem';
import { loadOrCreateMachineId, parseMachineId } from './identity';

const DEFAULT_STORE_URL = 'http://127.0.0.1:8091/db/local_default/agent_builder/main';

const OptionalTrimmedString = z
  .string()
  .optional()
  .transform((value) => {
    const trimmed = value?.trim();
    return trimmed === undefined || trimmed.length === 0 ? undefined : trimmed;
  });

const BooleanString = z
  .string()
  .optional()
  .transform((value) => {
    const normalized = value?.trim().toLowerCase();
    return normalized === 'true' || normalized === '1';
  });

/** Provider fields are deployment configuration, not request input: blank means absent. */
const ProviderTypeEnv = OptionalTrimmedString.pipe(z.enum(['anthropic', 'openai']).optional());

const ProviderBaseUrlEnv = OptionalTrimmedString.refine(
  (value) => value === undefined || validProviderUrl(value),
  { message: 'PERI_PROVIDER_BASE_URL 必须是合法的 provider URL' },
);

const EnvSchema = z.object({
  HOST: z.literal('127.0.0.1').default('127.0.0.1'),
  PORT: z.coerce.number().int().min(1).max(65_535).default(3000),
  DATABASE_PATH: z.string().min(1).default('.data/agent-builder.db'),
  PERI_WORKSPACE_ID: z.string().min(1).default('agent-builder'),
  PERI_WORKSPACE_PATH: z.string().min(1).default(process.cwd()),
  PERI_STORE_URL: z.url().default(DEFAULT_STORE_URL),
  PERI_STORE_TOKEN: OptionalTrimmedString,
  PERI_ENGINE_PORT: z.coerce.number().int().min(1).max(65_535).default(8961),
  /** Stable deployment identity. When absent the identity file below is used. */
  PERI_MACHINE_ID: OptionalTrimmedString,
  /** Must come from the launch environment: the native engine reads it at process start. */
  RIVETKIT_STORAGE_PATH: OptionalTrimmedString,
  PERI_SANDBOX_ENABLED: BooleanString,
  PERI_SANDBOX_PERI_PATH: OptionalTrimmedString,
  PERI_SANDBOX_SETTINGS: OptionalTrimmedString,
  PERI_SANDBOX_ROOT: OptionalTrimmedString,
  PERI_SANDBOX_PROFILE: OptionalTrimmedString,
  /** Deployment credential; when absent it is generated once into the private data directory. */
  PERI_SANDBOX_TOKEN: OptionalTrimmedString,
  /** Local RivetKit gateway credential. It is not the sandbox actor credential. */
  PERI_SANDBOX_GATEWAY_TOKEN: OptionalTrimmedString,
  /** Comma-separated extra browser origins (for example a separate local frontend). */
  PERI_ALLOWED_ORIGINS: OptionalTrimmedString,
  /** Single Provider seeded from the environment. Absent type disables seeding. */
  PERI_PROVIDER_TYPE: ProviderTypeEnv,
  PERI_PROVIDER_BASE_URL: ProviderBaseUrlEnv,
  PERI_PROVIDER_API_KEY: OptionalTrimmedString,
  PERI_PROVIDER_NAME: OptionalTrimmedString,
  PERI_SANDBOX_STARTUP_TIMEOUT_MS: z.coerce.number().int().positive().default(90_000),
  PERI_SANDBOX_STOP_TIMEOUT_MS: z.coerce.number().int().positive().default(20_000),
});

/** 诊断只展示这些 schema 字段名；其余 issue path 一律丢弃。 */
const ENV_FIELD_NAMES = Object.keys(EnvSchema.shape);
const ENV_FIELD_NAME_SET = new Set(ENV_FIELD_NAMES);

export interface SandboxEnv {
  readonly enabled: boolean;
  readonly mode: SandboxStatus['mode'];
  readonly reasons: readonly SandboxReason[];
  /** Resolved peri binary; undefined when the default path does not exist. */
  readonly periPath: string | undefined;
  /** Existing settings file; undefined means the runtime generates one in `dataDir/sandbox`. */
  readonly settingsPath: string | undefined;
  readonly sandboxesRoot: string | undefined;
  readonly profileName: string;
  readonly token: string | undefined;
  readonly gatewayToken: string;
  readonly startupTimeoutMs: number;
  readonly stopTimeoutMs: number;
  readonly dataDir: string;
  /** True only when the sandbox workspace is a real 0700 directory (never a symlink). */
  readonly workspacePathPrivate: boolean;
}

export interface ProviderEnv {
  /** Undefined disables env seeding; the stored provider (if any) is left untouched. */
  readonly type: ProviderType | undefined;
  readonly baseUrl: string | undefined;
  readonly apiKey: string | undefined;
  readonly name: string;
}

export interface ServerEnv {
  readonly host: '127.0.0.1';
  readonly port: number;
  readonly databasePath: string;
  readonly workspaceId: string;
  /** Canonical absolute directory; this is the only workspace the app is authorized to use. */
  readonly workspacePath: string;
  /** Canonical `peri+sandbox://` URI; the sole workspace identity at actor boundaries. */
  readonly workspaceUri: string;
  readonly storeUrl: string;
  readonly storeToken: string | undefined;
  readonly enginePort: number;
  readonly machineId: string;
  readonly rivetStoragePath: string | undefined;
  readonly sandbox: SandboxEnv;
  /** Single Provider desired state; `type` absent means no seeding. */
  readonly provider: ProviderEnv;
  readonly allowedOrigins: readonly string[];
}

export interface LoadEnvOptions {
  readonly identityDir?: string;
  readonly generateMachineId?: () => string;
}

function isMemoryDatabase(databasePath: string): boolean {
  return databasePath === ':memory:' || databasePath.startsWith('file::memory:');
}

function invalidEnvFields(error: z.ZodError): string[] {
  const invalid = new Set<string>();
  for (const issue of error.issues) {
    const field = issue.path[0];
    if (typeof field === 'string' && ENV_FIELD_NAME_SET.has(field)) invalid.add(field);
  }
  return ENV_FIELD_NAMES.filter((name) => invalid.has(name));
}

function parseConfiguredMachineId(value: string): string {
  try {
    return parseMachineId(value);
  } catch (error) {
    // 原始异常只留在 cause；公共输出只有稳定 code 与提示。
    throw new ConfigError('config_machine_id_invalid', { cause: error });
  }
}

// 只拒绝、不改写：realpath 归一化会让同一目录出现第二种授权写法并改变 workspace URI identity。
function canonicalWorkspace(resolved: string): string {
  const entry = lstatSync(resolved, { throwIfNoEntry: false });
  if (!entry) throw new ConfigError('workspace_path_missing');
  if (entry.isSymbolicLink()) throw new ConfigError('workspace_path_symlink');
  if (!statSync(resolved).isDirectory()) throw new ConfigError('workspace_path_not_directory');
  const real = realpathSync(resolved);
  if (real !== resolved) throw new ConfigError('workspace_path_not_canonical');
  if (real === path.parse(real).root) throw new ConfigError('workspace_path_filesystem_root');
  return real;
}

function privateDirectoryReason(directory: string, reason: SandboxReason): SandboxReason | null {
  const entry = lstatSync(directory, { throwIfNoEntry: false });
  if (!entry) return null;
  if (entry.isSymbolicLink() || !entry.isDirectory()) return reason;
  return (statSync(directory).mode & 0o077) === 0 ? null : reason;
}

function withinOrEqual(candidate: string, root: string): boolean {
  const rel = path.relative(root, candidate);
  return rel === '' || (!rel.startsWith('..') && !path.isAbsolute(rel));
}

function resolveSandbox(
  parsed: z.output<typeof EnvSchema>,
  dataDir: string,
  workspacePath: string,
  machineId: string,
): SandboxEnv {
  const enabled = parsed.PERI_SANDBOX_ENABLED;
  const reasons: SandboxReason[] = [];
  const periCandidate = path.resolve(parsed.PERI_SANDBOX_PERI_PATH ?? path.join(homedir(), '.peri', 'peri'));
  const periPath = existsSync(periCandidate) ? periCandidate : undefined;
  const settingsPath =
    parsed.PERI_SANDBOX_SETTINGS === undefined
      ? undefined
      : path.resolve(process.cwd(), parsed.PERI_SANDBOX_SETTINGS);
  const sandboxesRoot = path.resolve(
    process.cwd(),
    parsed.PERI_SANDBOX_ROOT ?? path.join(dataDir, 'sandbox', 'sandboxes'),
  );
  const workspacePathPrivate = (statSync(workspacePath).mode & 0o077) === 0;

  if (!enabled) {
    return Object.freeze({
      enabled: false,
      mode: 'disabled',
      reasons: [],
      periPath,
      settingsPath,
      sandboxesRoot,
      profileName: parsed.PERI_SANDBOX_PROFILE ?? 'default',
      token: undefined,
      gatewayToken: parsed.PERI_SANDBOX_GATEWAY_TOKEN ?? 'default',
      startupTimeoutMs: parsed.PERI_SANDBOX_STARTUP_TIMEOUT_MS,
      stopTimeoutMs: parsed.PERI_SANDBOX_STOP_TIMEOUT_MS,
      dataDir,
      workspacePathPrivate,
    });
  }

  if (periPath === undefined) reasons.push('sandbox_peri_missing');
  else if (!statSync(periPath).isFile()) reasons.push('sandbox_peri_missing');

  if (settingsPath !== undefined) {
    const settingStat = lstatSync(settingsPath, { throwIfNoEntry: false });
    if (!settingStat?.isFile()) reasons.push('sandbox_settings_missing');
    else if ((settingStat.mode & 0o077) !== 0) reasons.push('sandbox_settings_not_private');
  }

  const workspaceReason = privateDirectoryReason(workspacePath, 'sandbox_workspace_not_private');
  if (workspaceReason !== null) reasons.push(workspaceReason);
  if (withinOrEqual(workspacePath, sandboxesRoot) || withinOrEqual(sandboxesRoot, workspacePath)) {
    reasons.push('sandbox_root_overlaps_workspace');
  }
  const rootReason = privateDirectoryReason(sandboxesRoot, 'sandbox_root_not_private');
  if (rootReason !== null) reasons.push(rootReason);

  let token: string | undefined;
  try {
    token =
      parsed.PERI_SANDBOX_TOKEN ??
      createPrivateFileIfMissing(
        path.join(dataDir, 'sandbox-token'),
        `${Buffer.from(crypto.getRandomValues(new Uint8Array(32))).toString('hex')}\n`,
      ).trim();
    if (token.length < 16 || token.length > 512 || /\s/.test(token)) {
      reasons.push('sandbox_token_missing');
      token = undefined;
    }
  } catch {
    reasons.push('sandbox_token_missing');
  }

  // Reuse the URI constructor so a malformed machine id or non-canonical path fails at boot.
  new WorkspaceURI({ machineId, runtimePath: workspacePath }).toString();

  return Object.freeze({
    enabled: true,
    mode: reasons.length === 0 ? 'ready' : 'misconfigured',
    reasons,
    periPath,
    settingsPath,
    sandboxesRoot,
    profileName: parsed.PERI_SANDBOX_PROFILE ?? 'default',
    token,
    gatewayToken: parsed.PERI_SANDBOX_GATEWAY_TOKEN ?? 'default',
    startupTimeoutMs: parsed.PERI_SANDBOX_STARTUP_TIMEOUT_MS,
    stopTimeoutMs: parsed.PERI_SANDBOX_STOP_TIMEOUT_MS,
    dataDir,
    workspacePathPrivate,
  });
}

function defaultAllowedOrigins(port: number): string[] {
  return [`http://127.0.0.1:${port}`, `http://localhost:${port}`];
}

export function parseAllowedOrigins(raw: string | undefined, defaults: readonly string[]): string[] {
  const origins = new Set(defaults);
  for (const part of raw?.split(',') ?? []) {
    const value = part.trim();
    if (value.length === 0) continue;
    origins.add(value.replace(/\/$/, ''));
  }
  return [...origins];
}

export function loadEnv(
  source: Record<string, string | undefined> = process.env,
  options: LoadEnvOptions = {},
): ServerEnv {
  const parsed = EnvSchema.safeParse(source);
  if (!parsed.success) {
    throw new ConfigError('config_invalid', {
      cause: parsed.error,
      fields: invalidEnvFields(parsed.error),
    });
  }

  const databasePath = path.resolve(process.cwd(), parsed.data.DATABASE_PATH);
  const dataDir = isMemoryDatabase(parsed.data.DATABASE_PATH)
    ? path.resolve(process.cwd(), '.data')
    : path.dirname(databasePath);
  // Validate before touching the data directory: a rejected workspace must not create private
  // directories, and its actionable code must not be masked by a database-path error.
  const workspacePath = canonicalWorkspace(path.resolve(process.cwd(), parsed.data.PERI_WORKSPACE_PATH));
  if (!isMemoryDatabase(parsed.data.DATABASE_PATH)) ensurePrivateDirectory(dataDir);

  const machineId =
    parsed.data.PERI_MACHINE_ID === undefined
      ? loadOrCreateMachineId(
          options.identityDir ?? dataDir,
          options.generateMachineId ?? (() => crypto.randomUUID()),
        )
      : parseConfiguredMachineId(parsed.data.PERI_MACHINE_ID);

  const workspaceUri = new WorkspaceURI({ machineId, runtimePath: workspacePath }).toString();
  const sandbox = resolveSandbox(parsed.data, dataDir, workspacePath, machineId);

  return Object.freeze({
    host: parsed.data.HOST,
    port: parsed.data.PORT,
    databasePath,
    workspaceId: parsed.data.PERI_WORKSPACE_ID,
    workspacePath,
    workspaceUri,
    storeUrl: parsed.data.PERI_STORE_URL,
    storeToken: parsed.data.PERI_STORE_TOKEN,
    enginePort: parsed.data.PERI_ENGINE_PORT,
    machineId,
    rivetStoragePath:
      parsed.data.RIVETKIT_STORAGE_PATH === undefined
        ? undefined
        : path.resolve(process.cwd(), parsed.data.RIVETKIT_STORAGE_PATH),
    sandbox,
    provider: Object.freeze({
      type: parsed.data.PERI_PROVIDER_TYPE,
      baseUrl: parsed.data.PERI_PROVIDER_BASE_URL,
      apiKey: parsed.data.PERI_PROVIDER_API_KEY,
      name: parsed.data.PERI_PROVIDER_NAME ?? 'env',
    }),
    allowedOrigins: parseAllowedOrigins(
      parsed.data.PERI_ALLOWED_ORIGINS,
      defaultAllowedOrigins(parsed.data.PORT),
    ),
  });
}
