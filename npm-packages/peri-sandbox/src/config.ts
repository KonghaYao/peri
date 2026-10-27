import { readFileSync, realpathSync, statSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { isAbsolute, relative, resolve } from 'node:path';
import { z } from 'zod';
import { ControlError } from './control/types.js';

const absolute = z
  .string()
  .refine((path) => isAbsolute(path) && resolve(path) === path, 'must be a normalized absolute path');
const domain = z
  .string()
  .regex(/^(?=.{1,253}$)[a-z0-9]+(?:[a-z0-9-]*[a-z0-9])?(?:\.[a-z0-9]+(?:[a-z0-9-]*[a-z0-9])?)*$/i);
const profile = z.strictObject({
  allowedDomains: z.array(domain).default([]),
  runtimeReadPaths: z.array(absolute),
  memoryMax: z.string().regex(/^[1-9]\d*[KMGT]$/),
  tasksMax: z.number().int().positive().safe(),
  cpuQuota: z.string().regex(/^[1-9]\d*%$/),
});
export const configSchema = z
  .strictObject({
    controlRoot: absolute,
    sandboxesRoot: absolute,
    listen: z.strictObject({ host: z.string(), port: z.number().int().min(1).max(65535) }),
    publicBaseUrl: z.string().url(),
    auth: z.strictObject({ tokenFile: absolute }),
    http: z.strictObject({ allowedHosts: z.array(z.string()).min(1), allowedOrigins: z.array(z.string()) }),
    runtime: z.strictObject({ periPath: absolute, periSha256: z.string().regex(/^[a-f0-9]{64}$/) }),
    provider: z.strictObject({
      apiUnit: z.string().regex(/^[\w@.-]+\.service$/),
      unitPrefix: z.string().regex(/^[a-z][a-z0-9-]*-$/),
    }),
    limits: z.strictObject({
      maxSandboxes: z.number().int().positive(),
      startupMs: z.number().int().positive().default(30000),
      stopMs: z.number().int().positive().default(10000),
      bashCalls: z.number().int().positive().default(10000),
      minFreeBytes: z.number().int().nonnegative().safe().default(1073741824),
    }),
    profiles: z
      .record(z.string(), profile)
      .refine((profiles) => Object.keys(profiles).length > 0, 'at least one profile required'),
  })
  .superRefine((value, context) => {
    const left = resolve(value.controlRoot),
      right = resolve(value.sandboxesRoot);
    if (left === right || !relative(left, right).startsWith('..') || !relative(right, left).startsWith('..'))
      context.addIssue({ code: 'custom', message: 'controlRoot and sandboxesRoot must not overlap' });
    for (const [name, selected] of Object.entries(value.profiles))
      for (const path of selected.runtimeReadPaths) {
        const read = resolve(path);
        for (const root of [left, right])
          if (read === root || !relative(read, root).startsWith('..') || !relative(root, read).startsWith('..')) {
            context.addIssue({ code: 'custom', message: `profile ${name} overlaps managed root via ${path}` });
          }
      }
    if (value.listen.host !== '127.0.0.1' && value.listen.host !== '::1')
      context.addIssue({
        code: 'custom',
        message: 'public binding requires a separately verified TLS proxy; bind loopback',
      });
    if (
      value.http.allowedHosts.some((host) => host === '*') ||
      value.http.allowedOrigins.some((origin) => origin === '*')
    )
      context.addIssue({ code: 'custom', message: 'wildcard host/origin forbidden' });
  });
export type Config = z.infer<typeof configSchema>;

export function verifyRuntime(config: Config): void {
  for (const [path, expected] of [[config.runtime.periPath, config.runtime.periSha256]]) {
    if (!statSync(path).isFile()) throw new ControlError('unsupported_environment', `${path} is not a file`);
    if (!realpathSync(path).startsWith('/'))
      throw new ControlError('unsupported_environment', `invalid runtime path: ${path}`);
    if (createHash('sha256').update(readFileSync(path)).digest('hex') !== expected)
      throw new ControlError('unsupported_environment', `runtime hash mismatch: ${path}`);
  }
}

export function loadConfig(file: string): Config {
  const config = configSchema.parse(JSON.parse(readFileSync(file, 'utf8')));
  for (const root of [config.controlRoot, config.sandboxesRoot]) {
    const stats = statSync(root, { throwIfNoEntry: false });
    if (stats && (!stats.isDirectory() || realpathSync(root) !== root || stats.mode & 0o077))
      throw new ControlError('unsupported_environment', `managed root must be private, real directory: ${root}`);
  }
  verifyRuntime(config);
  const tokenStat = statSync(config.auth.tokenFile);
  if (!tokenStat.isFile() || tokenStat.mode & 0o077)
    throw new ControlError('unsupported_environment', 'token file must be private (0600)');
  if (readFileSync(config.auth.tokenFile, 'utf8').trim().length < 32)
    throw new ControlError('unsupported_environment', 'token must contain at least 32 characters');
  return config;
}
