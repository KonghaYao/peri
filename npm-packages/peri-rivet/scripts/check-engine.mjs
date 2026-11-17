#!/usr/bin/env node
// Preflight for the rivet-engine binary that rivetkit spawns: surfaces a missing platform package
// at install time instead of at the first `registry.startAndWait()`. Resolution delegates to
// @rivetkit/engine-cli so the check and rivetkit cannot drift apart.
import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const projectDir = process.cwd();
const binaryName = process.platform === 'win32' ? 'rivet-engine.exe' : 'rivet-engine';

function fail(lines) {
  for (const line of lines) console.error(line);
  process.exit(1);
}

function resolveEngineCliManifest() {
  const projectManifest = path.join(projectDir, 'package.json');
  try {
    return createRequire(projectManifest).resolve('@rivetkit/engine-cli/package.json');
  } catch {
    // Not hoisted to the project root; fall back to rivetkit's own dependency tree.
  }
  try {
    const rivetkitManifest = createRequire(projectManifest).resolve('rivetkit/package.json');
    return createRequire(rivetkitManifest).resolve('@rivetkit/engine-cli/package.json');
  } catch (error) {
    fail([
      'check-engine: cannot resolve @rivetkit/engine-cli.',
      `reason: ${error.message}`,
      '',
      'rivetkit must be installed in this project before the engine binary can be checked:',
      '  npm install rivetkit',
    ]);
  }
}

function readManifest(file) {
  return JSON.parse(readFileSync(file, 'utf8'));
}

function overrideLabel() {
  if (process.env.RIVET_ENGINE_BINARY) return `env RIVET_ENGINE_BINARY=${process.env.RIVET_ENGINE_BINARY}`;
  if (process.env.RIVET_ENGINE_BINARY_PATH)
    return `env RIVET_ENGINE_BINARY_PATH=${process.env.RIVET_ENGINE_BINARY_PATH}`;
  return null;
}

function describeSource(binaryPath, engineCliEntry) {
  const override = overrideLabel();
  if (override) return override;
  // Mirrors @rivetkit/engine-cli's own order: a binary beside the resolver wins over the npm package.
  if (binaryPath === path.join(path.dirname(engineCliEntry), binaryName))
    return 'local binary beside @rivetkit/engine-cli (dev build)';
  const platformPackage = engineCli.getPlatformPackageName();
  if (!platformPackage) return 'unknown';
  try {
    const platformManifest = readManifest(engineCliRequire.resolve(`${platformPackage}/package.json`));
    return `${platformPackage}@${platformManifest.version}`;
  } catch {
    return 'unknown';
  }
}

const engineCliManifestPath = resolveEngineCliManifest();
const engineCliManifest = readManifest(engineCliManifestPath);
const engineCliRequire = createRequire(engineCliManifestPath);
const engineCliEntry = engineCliRequire.resolve('@rivetkit/engine-cli');
const engineCli = await import(pathToFileURL(engineCliEntry).href);

const platformPackage = engineCli.getPlatformPackageName();
const expectedVersion = engineCliManifest.version;
const installCommand = platformPackage
  ? `npm install --registry=https://registry.npmjs.org --include=optional ${platformPackage}@${expectedVersion}`
  : null;

let binaryPath;
try {
  binaryPath = engineCli.getEnginePath();
} catch (error) {
  fail([
    'check-engine: rivet-engine is not available.',
    `reason: ${error.message}`,
    '',
    ...(installCommand ? ['Install the platform package that matches this host:', `  ${installCommand}`, ''] : []),
    ...(platformPackage
      ? []
      : ['This host has no @rivetkit/engine-cli platform package; point at a binary you built instead.', '']),
    'Or point at an existing binary (absolute path, file must exist):',
    '  RIVET_ENGINE_BINARY_PATH=/absolute/path/to/rivet-engine node your-app.mjs',
    'RIVET_ENGINE_BINARY is accepted as the same override.',
  ]);
}

if (!existsSync(binaryPath)) {
  fail([
    `check-engine: ${binaryPath} resolved but does not exist.`,
    '',
    ...(installCommand ? ['Reinstall the platform package for this host:', `  ${installCommand}`, ''] : []),
    'Or set RIVET_ENGINE_BINARY_PATH to an existing binary.',
  ]);
}

let version;
try {
  version = execFileSync(binaryPath, ['--version'], { encoding: 'utf8', timeout: 10_000 }).trim();
} catch (error) {
  fail([
    `check-engine: ${binaryPath} exists but could not be executed.`,
    `reason: ${error.message}`,
    '',
    'Reinstall the platform package so the binary keeps its executable bit:',
    ...(installCommand ? [`  ${installCommand}`] : ['  npm install --include=optional']),
  ]);
}

console.log(`check-engine: rivet-engine is available`);
console.log(`  path: ${binaryPath}`);
console.log(`  version: ${version}`);
console.log(`  source: ${describeSource(binaryPath, engineCliEntry)}`);
