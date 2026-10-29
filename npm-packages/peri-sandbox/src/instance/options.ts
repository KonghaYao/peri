import { createHash, randomBytes } from 'node:crypto';
import { existsSync, realpathSync } from 'node:fs';
import { basename, dirname, isAbsolute, join, resolve } from 'node:path';
import { z } from 'zod';
import { SandboxError, type SandboxProfile, type SandboxSettings, type StartSandboxOptions } from './types.js';

export const sandboxIdPattern = /^sbx_[a-f0-9]{32}$/;

const absolute = z
  .string()
  .refine((path) => isAbsolute(path) && resolve(path) === path, 'must be a normalized absolute path');
const domain = z
  .string()
  .regex(/^(?=.{1,253}$)[a-z0-9]+(?:[a-z0-9-]*[a-z0-9])?(?:\.[a-z0-9]+(?:[a-z0-9-]*[a-z0-9])?)*$/i);

export const profileSchema = z.strictObject({
  allowedDomains: z.array(domain).default([]),
  runtimeReadPaths: z.array(absolute),
  memoryMax: z.string().regex(/^[1-9]\d*[KMGT]$/),
  tasksMax: z.number().int().positive().safe(),
  cpuQuota: z.string().regex(/^[1-9]\d*%$/),
});

export const settingsSchema = z.strictObject({
  sandboxesRoot: absolute,
  periPath: absolute,
  periSha256: z.string().regex(/^[a-f0-9]{64}$/),
  defaultProfile: z.string().min(1).optional(),
  unitPrefix: z
    .string()
    .regex(/^[a-z][a-z0-9-]*-$/)
    .optional(),
  profiles: z
    .record(z.string().min(1), profileSchema)
    .refine((profiles) => Object.keys(profiles).length > 0, 'at least one profile required'),
  startupTimeoutMs: z.number().int().positive().optional(),
  stopTimeoutMs: z.number().int().positive().optional(),
});

export const DEFAULT_STARTUP_TIMEOUT_MS = 45_000;
export const DEFAULT_STOP_TIMEOUT_MS = 15_000;
export const DEFAULT_BASH_CALLS = 10_000;
export const DEFAULT_MIN_FREE_BYTES = 1024 * 1024 * 1024;
export const DEFAULT_UNIT_PREFIX = 'peri-sbx-';
export const DEFAULT_PROFILE_NAME = 'workspace-default';

export function parseProfile(value: unknown, description = 'profile'): SandboxProfile {
  try {
    return profileSchema.parse(value);
  } catch (error) {
    throw new SandboxError('invalid_options', `Invalid ${description}.`, { cause: error });
  }
}

export function parseSettings(value: unknown): SandboxSettings {
  try {
    return settingsSchema.parse(value);
  } catch (error) {
    throw new SandboxError('invalid_options', 'Invalid sandbox settings.', { cause: error });
  }
}

/// Derive an id from `sessionKey` without keeping a registry.
export function deriveSandboxId(sessionKey: string): string {
  if (sessionKey.trim().length === 0 || Buffer.byteLength(sessionKey) > 1024)
    throw new SandboxError('invalid_options', 'sessionKey must be a non-empty string below 1024 bytes.');
  return `sbx_${createHash('sha256').update(`peri-sandbox-session:${sessionKey}`).digest('hex').slice(0, 32)}`;
}

export function resolveSandboxId(options: Pick<StartSandboxOptions, 'sandboxId' | 'sessionKey'>): string {
  if (options.sandboxId !== undefined) {
    if (!sandboxIdPattern.test(options.sandboxId))
      throw new SandboxError('invalid_options', 'sandboxId must match sbx_[a-f0-9]{32}.');
    return options.sandboxId;
  }
  if (options.sessionKey !== undefined) return deriveSandboxId(options.sessionKey);
  return `sbx_${randomBytes(16).toString('hex')}`;
}

/**
 * Resolve symlinked ancestors without requiring the leaf to exist.
 *
 * SRT compares literal paths; a workspace under `/var/...` must be presented
 * as `/private/var/...` or the sandbox cannot resolve it.
 */
export function canonicalizePath(path: string): string {
  let current = resolve(path);
  const suffix: string[] = [];
  while (!existsSync(current)) {
    const parent = dirname(current);
    if (parent === current) return resolve(path);
    suffix.unshift(basename(current));
    current = parent;
  }
  return join(realpathSync(current), ...suffix);
}

export function assertSandboxId(id: string): string {
  if (!sandboxIdPattern.test(id)) throw new SandboxError('invalid_options', 'sandboxId must match sbx_[a-f0-9]{32}.');
  return id;
}

function positive(value: number | undefined, fallback: number, field: string): number {
  if (value === undefined) return fallback;
  if (!Number.isSafeInteger(value) || value <= 0)
    throw new SandboxError('invalid_options', `${field} must be a positive integer.`);
  return value;
}

function nonNegative(value: number | undefined, fallback: number, field: string): number {
  if (value === undefined) return fallback;
  if (!Number.isSafeInteger(value) || value < 0)
    throw new SandboxError('invalid_options', `${field} must be a non-negative integer.`);
  return value;
}

export interface ResolvedSandboxOptions {
  readonly sandboxId: string;
  readonly sandboxesRoot: string;
  readonly sandboxRoot: string;
  readonly workspacePath: string;
  readonly homePath: string;
  readonly profile: SandboxProfile;
  readonly profileName: string;
  readonly periPath: string;
  readonly periSha256: string;
  readonly token: string | undefined;
  readonly tokenFile: string | undefined;
  readonly host: '127.0.0.1' | '::1';
  readonly port: number;
  readonly bashCalls: number;
  readonly startupTimeoutMs: number;
  readonly stopTimeoutMs: number;
  readonly minFreeBytes: number;
  readonly unitPrefix: string;
  readonly signal: AbortSignal | undefined;
}

/**
 * Select the effective profile exactly as `startSandbox` does.
 *
 * Exported so read-only diagnostics share the runtime rule instead of
 * re-implementing `defaultProfile` fallback or unknown-name handling.
 */
export function resolveProfile(
  settings: SandboxSettings,
  options: Pick<StartSandboxOptions, 'profile' | 'profileName'>,
): { profile: SandboxProfile; profileName: string } {
  const name = options.profileName ?? settings.defaultProfile ?? Object.keys(settings.profiles)[0];
  if (name === undefined) throw new SandboxError('invalid_options', 'Sandbox settings define no profile.');
  const selected = settings.profiles[name];
  if (!selected) throw new SandboxError('invalid_options', `Unknown sandbox profile: ${name}.`);
  return { profile: options.profile ? parseProfile(options.profile, 'profile') : selected, profileName: name };
}

/**
 * Normalize caller options against loaded settings.
 *
 * `sandboxId`/`sessionKey` are resolved here; the id stays stable for the
 * lifetime of the instance and for explicit restarts with the same options.
 */
export function resolveOptions(options: StartSandboxOptions, settings: SandboxSettings): ResolvedSandboxOptions {
  const sandboxesRoot = canonicalizePath(options.sandboxesRoot ?? settings.sandboxesRoot);
  const sandboxId = resolveSandboxId(options);
  const sandboxRoot = join(sandboxesRoot, sandboxId);
  const { profile, profileName } = resolveProfile(settings, options);

  if (options.token !== undefined && options.tokenFile !== undefined)
    throw new SandboxError('invalid_options', 'token and tokenFile are mutually exclusive.');
  if (options.token !== undefined && verifyToken(options.token) === false)
    throw new SandboxError('invalid_options', 'token must be 16-512 printable characters without whitespace.');

  const host = options.host ?? '127.0.0.1';
  if (host !== '127.0.0.1' && host !== '::1')
    throw new SandboxError('invalid_options', 'host must be 127.0.0.1 or ::1.');

  const port = options.port ?? 0;
  if (!Number.isInteger(port) || port < 0 || port > 65535)
    throw new SandboxError('invalid_options', 'port must be an integer between 0 and 65535.');

  return {
    sandboxId,
    sandboxesRoot,
    sandboxRoot,
    workspacePath: join(sandboxRoot, 'workspace'),
    homePath: join(sandboxRoot, 'home'),
    profile,
    profileName,
    periPath: resolve(options.periPath ?? settings.periPath),
    periSha256: options.periSha256 ?? settings.periSha256,
    token: options.token,
    tokenFile: options.tokenFile === undefined ? undefined : canonicalizePath(resolve(options.tokenFile)),
    host,
    port,
    bashCalls: positive(options.bashCalls, DEFAULT_BASH_CALLS, 'bashCalls'),
    startupTimeoutMs: positive(
      options.startupTimeoutMs ?? settings.startupTimeoutMs,
      DEFAULT_STARTUP_TIMEOUT_MS,
      'startupTimeoutMs',
    ),
    stopTimeoutMs: positive(options.stopTimeoutMs ?? settings.stopTimeoutMs, DEFAULT_STOP_TIMEOUT_MS, 'stopTimeoutMs'),
    minFreeBytes: nonNegative(options.minFreeBytes, DEFAULT_MIN_FREE_BYTES, 'minFreeBytes'),
    unitPrefix: settings.unitPrefix ?? DEFAULT_UNIT_PREFIX,
    signal: options.signal,
  };
}

export function verifyToken(token: string): boolean {
  return /^[\x21-\x7e]{16,512}$/.test(token);
}
