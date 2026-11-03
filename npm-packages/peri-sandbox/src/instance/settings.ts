import { createHash } from 'node:crypto';
import { existsSync, lstatSync, mkdirSync, readFileSync, realpathSync, statSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { DEFAULT_PROFILE_NAME, parseSettings, resolveProfile } from './options.js';
import { SandboxError, type SandboxProfile, type SandboxSettings } from './types.js';

export function defaultSettingsPath(): string {
  return resolve(process.env.PERI_SANDBOX_SETTINGS ?? join(homedir(), '.peri-sandbox', 'settings.json'));
}

function defaultReadPaths(): string[] {
  return [
    '/usr',
    '/bin',
    '/sbin',
    '/usr/local',
    '/opt/homebrew',
    '/System',
    '/Library',
    '/private/etc',
    '/private/var/select',
    '/var/select',
    '/etc',
    '/Applications/Xcode.app',
    '/etc/ssl/certs',
    '/etc/ld.so.cache',
    '/etc/resolv.conf',
    '/etc/hosts',
  ]
    .filter((path) => existsSync(path))
    .map((path) => resolve(path));
}

export function defaultProfile(periPath: string): SandboxProfile {
  return {
    allowedDomains: [
      'github.com',
      'api.github.com',
      'codeload.github.com',
      'objects.githubusercontent.com',
      'raw.githubusercontent.com',
      'registry.npmjs.org',
      'registry.yarnpkg.com',
      'pypi.org',
      'files.pythonhosted.org',
      'crates.io',
      'static.crates.io',
      'index.crates.io',
      'proxy.golang.org',
      'sum.golang.org',
      'deno.land',
      'jsr.io',
      'repo1.maven.org',
      'repo.maven.apache.org',
      'api.anthropic.com',
      'api.openai.com',
    ],
    runtimeReadPaths: [...new Set([...defaultReadPaths(), dirname(process.execPath)])].filter(
      (path) => path !== periPath,
    ),
    memoryMax: '1G',
    tasksMax: 128,
    cpuQuota: '100%',
  };
}

function defaultPeriPath(): string {
  return resolve(process.env.PERI_PATH ?? join(homedir(), '.peri', 'peri'));
}

function defaultSettingsFor(periPath: string, periSha256: string): SandboxSettings {
  return {
    sandboxesRoot: join(homedir(), '.peri-sandbox', 'sandboxes'),
    periPath,
    periSha256,
    defaultProfile: DEFAULT_PROFILE_NAME,
    profiles: { [DEFAULT_PROFILE_NAME]: defaultProfile(periPath) },
  };
}

/** In-memory defaults used when no settings file exists; nothing is written. */
export function defaultSandboxSettings(): SandboxSettings {
  const periPath = defaultPeriPath();
  return defaultSettingsFor(periPath, sha256Of(periPath) ?? '');
}

function sha256Of(path: string): string | null {
  try {
    return createHash('sha256').update(readFileSync(path)).digest('hex');
  } catch {
    return null;
  }
}

function validateManagedRoot(path: string, description: string): void {
  const entry = lstatSync(path, { throwIfNoEntry: false });
  if (!entry) return;
  if (entry.isSymbolicLink() || !entry.isDirectory())
    throw new SandboxError('invalid_options', `${description} must be a real directory: ${path}`);
  if (statSync(path).mode & 0o077)
    throw new SandboxError('invalid_options', `${description} must be private (0700): ${path}`);
}

function validateSettings(value: SandboxSettings, description: string): SandboxSettings {
  const profileNames = Object.keys(value.profiles);
  if (value.defaultProfile !== undefined && !profileNames.includes(value.defaultProfile))
    throw new SandboxError(
      'invalid_options',
      `${description} references unknown defaultProfile: ${value.defaultProfile}`,
    );
  return value;
}

interface LoadedSandboxSettings {
  readonly path: string;
  readonly source: 'file' | 'defaults';
  readonly settings: SandboxSettings;
}

function loadSettingsResult(file: string | undefined, digestDefaults: boolean): LoadedSandboxSettings {
  const explicit = file !== undefined;
  const path = resolve(file ?? defaultSettingsPath());
  if (!existsSync(path)) {
    if (explicit) throw new SandboxError('invalid_options', `Sandbox settings not found: ${path}`);
    const periPath = defaultPeriPath();
    // Diagnostics skip the digest: reading the Peri binary is not needed to describe a profile.
    const settings = defaultSettingsFor(periPath, digestDefaults ? (sha256Of(periPath) ?? '') : '');
    return { path, source: 'defaults', settings: validateSettings(settings, 'default settings') };
  }
  let raw: unknown;
  try {
    raw = JSON.parse(readFileSync(path, 'utf8'));
  } catch (error) {
    throw new SandboxError('invalid_options', `Cannot parse sandbox settings: ${path}`, { cause: error });
  }
  const settings = validateSettings(parseSettings(raw), 'settings');
  validateManagedRoot(settings.sandboxesRoot, 'sandboxesRoot');
  return { path, source: 'file', settings };
}

/**
 * Load settings from an explicit path, the default path, or in-memory defaults.
 *
 * An explicitly requested file must exist and parse; the default path silently
 * falls back to in-memory defaults so `run` works before `init`.
 */
export function loadSettings(file?: string): SandboxSettings {
  return loadSettingsResult(file, true).settings;
}

export interface ResolvedSandboxSettings {
  /** Absolute path considered, whether or not the file exists. */
  path: string;
  source: 'file' | 'defaults';
  profileName: string;
  profile: SandboxProfile;
}

/**
 * Describe the settings and profile the runtime would use, without starting a sandbox.
 *
 * Shares `loadSettings` (explicit-path errors, defaults fallback) and
 * `resolveProfile` (defaultProfile, first-profile fallback, unknown profile)
 * with the runtime. Defaults are built without hashing the Peri binary because
 * this runs on the monitor polling path.
 */
export function resolveSandboxSettings(
  options: { settingsPath?: string; profileName?: string } = {},
): ResolvedSandboxSettings {
  const { path, source, settings } = loadSettingsResult(options.settingsPath, false);
  const { profile, profileName } = resolveProfile(settings, { profileName: options.profileName });
  return { path, source, profileName, profile };
}

/** Verify the configured Peri binary before spawning it. */
export function verifyPeriRuntime(periPath: string, expectedSha256: string): void {
  let stats;
  try {
    stats = statSync(periPath);
  } catch (error) {
    throw new SandboxError('unsupported_environment', `Peri executable unavailable: ${periPath}`, { cause: error });
  }
  if (!stats.isFile()) throw new SandboxError('unsupported_environment', `Peri executable is not a file: ${periPath}`);
  if (!realpathSync(periPath).startsWith('/'))
    throw new SandboxError('unsupported_environment', `Invalid Peri executable path: ${periPath}`);
  const actual = sha256Of(periPath);
  if (actual === null || actual !== expectedSha256)
    throw new SandboxError('unsupported_environment', `Peri executable hash mismatch: ${periPath}`);
}

/**
 * Create `<settings dir>/settings.json` (0600) and the sandboxes root (0700).
 *
 * Existing settings are never overwritten; no token or port is generated.
 */
export async function initSettings(file?: string): Promise<string> {
  if (process.platform !== 'darwin' && process.platform !== 'linux')
    throw new SandboxError('unsupported_environment', `Unsupported platform: ${process.platform}`);
  const path = resolve(file ?? defaultSettingsPath());
  if (existsSync(path)) throw new SandboxError('conflict', `Sandbox settings already exist: ${path}`);
  let periPath: string;
  try {
    periPath = realpathSync(defaultPeriPath());
  } catch (error) {
    throw new SandboxError('unsupported_environment', `Peri executable unavailable: ${defaultPeriPath()}`, {
      cause: error,
    });
  }
  const sha256 = sha256Of(periPath);
  if (sha256 === null) throw new SandboxError('unsupported_environment', `Peri executable unreadable: ${periPath}`);
  const sandboxesRoot = join(dirname(path), 'sandboxes');
  const settings: SandboxSettings = {
    sandboxesRoot,
    periPath,
    periSha256: sha256,
    defaultProfile: DEFAULT_PROFILE_NAME,
    profiles: { [DEFAULT_PROFILE_NAME]: defaultProfile(periPath) },
  };
  mkdirSync(dirname(path), { recursive: true, mode: 0o700 });
  mkdirSync(sandboxesRoot, { recursive: true, mode: 0o700 });
  for (const directory of [dirname(path), sandboxesRoot]) {
    const stats = lstatSync(directory);
    if (!stats.isDirectory() || stats.isSymbolicLink() || stats.mode & 0o077)
      throw new SandboxError('unsupported_environment', `Managed settings directory must be private: ${directory}`);
  }
  try {
    writeFileSync(path, `${JSON.stringify(settings, null, 2)}\n`, { flag: 'wx', mode: 0o600 });
  } catch (error) {
    throw new SandboxError('conflict', `Cannot create sandbox settings: ${path}`, { cause: error });
  }
  return path;
}
