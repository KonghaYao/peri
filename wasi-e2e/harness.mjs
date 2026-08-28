import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { lstatSync, readFileSync, readdirSync, realpathSync } from 'node:fs';
import { lstat, mkdir, readFile, readdir, realpath, rm, stat } from 'node:fs/promises';
import { createRequire } from 'node:module';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const require = createRequire(import.meta.url);

function trustedDirectory(directory, label) {
  const declared = path.resolve(directory);
  const metadata = lstatSync(declared);
  assert.equal(metadata.isSymbolicLink(), false, `${label} must not be a symlink`);
  assert.equal(metadata.isDirectory(), true, `${label} must be a directory`);
  const canonical = realpathSync(declared);
  assert.equal(canonical, declared, `${label} must already be a canonical path`);
  return canonical;
}

export const PACKAGE_DIR = trustedDirectory(fileURLToPath(new URL('.', import.meta.url)), 'package root');
export const ROOT_DIR = trustedDirectory(path.dirname(PACKAGE_DIR), 'repository root');
assert.equal(path.dirname(PACKAGE_DIR), ROOT_DIR, 'wasi-e2e must be a direct repository child');
assert.equal(path.basename(PACKAGE_DIR), 'wasi-e2e', 'unexpected package root');

const TARGET_DIR = path.join(ROOT_DIR, 'target');
const PACKAGE_TARGET_DIR = path.join(PACKAGE_DIR, 'target');
const ACQUISITION_DIR = path.join(PACKAGE_TARGET_DIR, 'cargo-acquisition');
const ACQUISITION_MANIFEST = path.join(ACQUISITION_DIR, 'Cargo.toml');
const ACQUISITION_LOCK = path.join(ACQUISITION_DIR, 'Cargo.lock');
export const ACQUISITION_METADATA = path.join(ACQUISITION_DIR, 'metadata.json');
const VENDOR_DIR = path.join(PACKAGE_TARGET_DIR, 'cargo-vendor');
const CARGO_HOME = path.join(PACKAGE_TARGET_DIR, 'cargo-home');
const CARGO_CONFIG = path.join(CARGO_HOME, 'config.toml');
export const COMPONENT_PATH = path.join(
  TARGET_DIR,
  'wasm32-wasip2',
  'release',
  'peri_wasi.wasm',
);
export const OUTPUT_DIR = path.join(PACKAGE_DIR, 'target', 'wasi-p2-node');
export const GENERATED_JS = path.join(OUTPUT_DIR, 'turn-policy.js');
export const EXERCISE_PATH = path.join(PACKAGE_DIR, 'exercise.mjs');
export const INTERFACE_NAME = 'peri:turn-policy/policy@0.1.0';
export const EXERCISE_ASSERTION_COUNT = 28;

const PACKAGE_MANIFEST_PATH = path.join(PACKAGE_DIR, 'package.json');
const PACKAGE_LOCK_PATH = path.join(PACKAGE_DIR, 'package-lock.json');
const COMPONENT_HEADER = Buffer.from([0x00, 0x61, 0x73, 0x6d, 0x0d, 0x00, 0x01, 0x00]);
const CORE_WASM_HEADER = Buffer.from([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);
const EXPECTED_GENERATED_FILES = [
  'interfaces/peri-turn-policy-policy.d.ts',
  'turn-policy.core.wasm',
  'turn-policy.d.ts',
  'turn-policy.js',
];
const EXPECTED_COMPONENT_IMPORTS = [];
const EXPECTED_COMPONENT_EXPORTS = [INTERFACE_NAME];
const EXPECTED_WIT = `package root:component;

world root {
  export ${INTERFACE_NAME};
}
`;
const FORBIDDEN_DEPENDENCIES = new Set([
  'peri-acp',
  'peri-acp-types',
  'peri-agent',
  'peri-js',
  'peri-lsp',
  'peri-middlewares',
  'peri-model',
  'peri-ptc',
  'peri-resources',
  'peri-workflow',
  'reqwest',
  'rmcp',
  'sqlx',
  'sysinfo',
  'tokio',
  'wasmtime',
]);
const ALLOWED_REPOSITORY_CRATES = new Set(['peri-turn-policy', 'peri-wasi']);
const WINDOWS_RUNTIME_ENV_KEYS = ['SystemRoot', 'TEMP', 'TMP', 'WINDIR'];
const CARGO_CERT_ENV_KEYS = ['NIX_SSL_CERT_FILE', 'SSL_CERT_DIR', 'SSL_CERT_FILE'];
const EXPECTED_CARGO_CONFIG = `[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = ${JSON.stringify(VENDOR_DIR)}

[net]
offline = true
`;
const EXPECTED_ACQUISITION_MANIFEST = `[package]
name = "peri-wasi"
version = "0.2.0"
edition = "2021"
description = "WASI Preview 2 component for portable Peri turn policy"
publish = false

[lib]
path = ${JSON.stringify(path.join(ROOT_DIR, 'peri-wasi', 'src', 'lib.rs'))}
crate-type = ["cdylib"]

[dependencies]
peri-turn-policy = { path = ${JSON.stringify(path.join(ROOT_DIR, 'peri-turn-policy'))} }
wit-bindgen = { version = "=0.57.1", default-features = false, features = ["macros"] }

[workspace]
`;
const FINGERPRINT_INPUTS = [
  ['root-cargo-lock', path.join(ROOT_DIR, 'Cargo.lock')],
  ['root-cargo-manifest', path.join(ROOT_DIR, 'Cargo.toml')],
  ['peri-wasi-manifest', path.join(ROOT_DIR, 'peri-wasi', 'Cargo.toml')],
  ['peri-turn-policy-manifest', path.join(ROOT_DIR, 'peri-turn-policy', 'Cargo.toml')],
];
const DISALLOWED_ENV_KEYS = [
  'AWS_ACCESS_KEY_ID',
  'AZURE_CLIENT_SECRET',
  'GITHUB_TOKEN',
  'GOOGLE_APPLICATION_CREDENTIALS',
  'HOME',
  'HTTPS_PROXY',
  'NODE_OPTIONS',
  'NODE_PATH',
  'NPM_TOKEN',
  'OPENAI_API_KEY',
  'USERPROFILE',
  'npm_config_user_agent',
];

function trustedRustupHome() {
  const configured = process.env.RUSTUP_HOME;
  const parentHome = process.env.HOME ?? process.env.USERPROFILE;
  const declared = configured ?? (parentHome ? path.join(parentHome, '.rustup') : undefined);
  assert.equal(typeof declared, 'string', 'RUSTUP_HOME must be configured or derivable by the parent');
  return trustedDirectory(declared, 'RUSTUP_HOME');
}

const RUSTUP_HOME = trustedRustupHome();

export class ToolExecutionError extends Error {
  constructor(command, result) {
    const outcome = result.error
      ? result.error.message
      : result.signal
        ? `signal ${result.signal}`
        : `status ${result.status}`;
    super(`${command} failed with ${outcome}`);
    this.name = 'ToolExecutionError';
    this.status = result.status;
    this.signal = result.signal;
    this.stdout = result.stdout ?? '';
    this.stderr = result.stderr ?? '';
    this.cause = result.error;
  }
}

function readJson(file) {
  return JSON.parse(readFileSync(file, 'utf8'));
}

function assertExactOwnedPath(actual, expected, label) {
  assert.equal(path.resolve(actual), expected, `${label} escaped its owned path`);
}

function isContainedPath(candidate, root) {
  const relative = path.relative(root, candidate);
  return relative === '' || (relative !== '..' && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative));
}

export async function assertSafeDescendant(target, trustedRoot, options = {}) {
  const canonicalRoot = await realpath(trustedRoot);
  assert.equal(canonicalRoot, trustedRoot, 'trusted root changed after initialization');
  const rootMetadata = await lstat(trustedRoot);
  assert.equal(rootMetadata.isSymbolicLink(), false, 'trusted root must not be a symlink');
  assert.equal(rootMetadata.isDirectory(), true, 'trusted root must be a directory');

  const resolvedTarget = path.resolve(target);
  const relative = path.relative(trustedRoot, resolvedTarget);
  assert.equal(relative !== '', true, 'target must be below its trusted root');
  assert.equal(isContainedPath(resolvedTarget, trustedRoot), true, 'target escaped its trusted root');

  const components = relative.split(path.sep);
  let current = trustedRoot;
  let missing = false;
  let verifiedParent = trustedRoot;
  for (const [index, component] of components.entries()) {
    current = path.join(current, component);
    if (missing) {
      continue;
    }

    let metadata;
    try {
      metadata = await lstat(current);
    } catch (error) {
      if (error?.code !== 'ENOENT') {
        throw error;
      }
      assert.equal(options.allowMissing, true, `required path is missing: ${current}`);
      missing = true;
      continue;
    }

    assert.equal(metadata.isSymbolicLink(), false, `symlink rejected in owned path: ${current}`);
    const isLeaf = index === components.length - 1;
    if (!isLeaf) {
      assert.equal(metadata.isDirectory(), true, `path ancestor is not a directory: ${current}`);
    } else if (options.leafKind === 'directory') {
      assert.equal(metadata.isDirectory(), true, `owned path must be a directory: ${current}`);
    } else if (options.leafKind === 'file') {
      assert.equal(metadata.isFile(), true, `owned path must be a file: ${current}`);
    }

    const canonicalCurrent = await realpath(current);
    assert.equal(isContainedPath(canonicalCurrent, trustedRoot), true, `real path escaped trusted root: ${current}`);
    verifiedParent = current;
  }

  return { exists: !missing, verifiedParent };
}

function copyAllowed(source, keys, destination) {
  for (const key of keys) {
    if (typeof source[key] === 'string') {
      destination[key] = source[key];
    }
  }
}

function assertSafeDescendantSync(target, trustedRoot, label) {
  const resolvedTarget = path.resolve(target);
  assert.equal(isContainedPath(resolvedTarget, trustedRoot), true, `${label} escaped its trusted root`);
  const relative = path.relative(trustedRoot, resolvedTarget);
  assert.equal(relative !== '', true, `${label} must be below its trusted root`);
  let current = trustedRoot;
  for (const component of relative.split(path.sep)) {
    current = path.join(current, component);
    let metadata;
    try {
      metadata = lstatSync(current);
    } catch (error) {
      if (error?.code === 'ENOENT') {
        throw new Error(
          `isolated Cargo acquisition is missing (${current}); run \`npm --prefix wasi-e2e run acquire:cargo\` first`,
          { cause: error },
        );
      }
      throw error;
    }
    assert.equal(metadata.isSymbolicLink(), false, `symlink rejected in ${label}: ${current}`);
    const canonical = realpathSync(current);
    assert.equal(isContainedPath(canonical, trustedRoot), true, `${label} real path escaped trusted root`);
  }
}

function assertCredentialFreeTree(directory) {
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const entryPath = path.join(directory, entry.name);
    const metadata = lstatSync(entryPath);
    assert.equal(metadata.isSymbolicLink(), false, `symlink rejected in isolated Cargo home: ${entryPath}`);
    assert.doesNotMatch(entry.name, /credential|token/iu, `credential-like file rejected: ${entryPath}`);
    if (metadata.isDirectory()) {
      assertCredentialFreeTree(entryPath);
    }
  }
}

function assertRegularFile(file, label) {
  const metadata = lstatSync(file);
  assert.equal(metadata.isSymbolicLink(), false, `${label} must not be a symlink`);
  assert.equal(metadata.isFile(), true, `${label} must be a regular file`);
}

export function validateSafeTreeSync(directory, trustedRoot) {
  assertSafeDescendantSync(directory, trustedRoot, 'validated tree');
  const visit = (current) => {
    for (const entry of readdirSync(current, { withFileTypes: true })) {
      const entryPath = path.join(current, entry.name);
      const metadata = lstatSync(entryPath);
      assert.equal(metadata.isSymbolicLink(), false, `symlink rejected in validated tree: ${entryPath}`);
      const canonical = realpathSync(entryPath);
      assert.equal(isContainedPath(canonical, trustedRoot), true, `validated tree entry escaped trusted root: ${entryPath}`);
      if (metadata.isDirectory()) visit(entryPath);
    }
  };
  visit(directory);
}

export function computeAcquisitionMetadata(inputs = FINGERPRINT_INPUTS) {
  const combined = createHash('sha256');
  const metadataInputs = [];
  for (const [label, file] of inputs) {
    const labelBytes = Buffer.from(label, 'utf8');
    const contents = readFileSync(file);
    const lengths = Buffer.alloc(16);
    lengths.writeBigUInt64BE(BigInt(labelBytes.length), 0);
    lengths.writeBigUInt64BE(BigInt(contents.length), 8);
    combined.update(lengths.subarray(0, 8));
    combined.update(labelBytes);
    combined.update(lengths.subarray(8));
    combined.update(contents);
    metadataInputs.push({
      label,
      length: contents.length,
      sha256: createHash('sha256').update(contents).digest('hex'),
    });
  }
  return {
    schema: 1,
    algorithm: 'sha256-framed-label-length-content',
    inputs: metadataInputs,
    fingerprint: combined.digest('hex'),
  };
}

export function validateFingerprintMetadata(metadataPath = ACQUISITION_METADATA, inputs = FINGERPRINT_INPUTS) {
  assertRegularFile(metadataPath, 'acquisition metadata');
  assert.deepEqual(JSON.parse(readFileSync(metadataPath, 'utf8')), computeAcquisitionMetadata(inputs));
}

export function validateIsolatedCargoHome() {
  assert.equal(isContainedPath(CARGO_HOME, PACKAGE_TARGET_DIR), true, 'CARGO_HOME must be below wasi-e2e/target');
  assertSafeDescendantSync(CARGO_HOME, PACKAGE_DIR, 'isolated Cargo home');
  assertSafeDescendantSync(VENDOR_DIR, PACKAGE_DIR, 'Cargo vendor directory');
  assertSafeDescendantSync(ACQUISITION_DIR, PACKAGE_DIR, 'Cargo acquisition workspace');
  assertSafeDescendantSync(ACQUISITION_MANIFEST, PACKAGE_DIR, 'Cargo acquisition manifest');
  assertSafeDescendantSync(ACQUISITION_LOCK, PACKAGE_DIR, 'Cargo acquisition lock');
  assertSafeDescendantSync(ACQUISITION_METADATA, PACKAGE_DIR, 'Cargo acquisition metadata');
  assertRegularFile(ACQUISITION_LOCK, 'acquisition Cargo.lock');
  assertRegularFile(ACQUISITION_METADATA, 'acquisition metadata');
  validateFingerprintMetadata();
  assert.equal(readFileSync(ACQUISITION_MANIFEST, 'utf8'), EXPECTED_ACQUISITION_MANIFEST, 'acquisition manifest drifted');
  const acquisitionWit = path.join(ACQUISITION_DIR, 'wit', 'world.wit');
  assertSafeDescendantSync(acquisitionWit, PACKAGE_DIR, 'Cargo acquisition WIT');
  assert.equal(
    readFileSync(acquisitionWit, 'utf8'),
    readFileSync(path.join(ROOT_DIR, 'peri-wasi', 'wit', 'world.wit'), 'utf8'),
    'acquisition WIT is stale; rerun acquire:cargo',
  );
  assertSafeDescendantSync(CARGO_CONFIG, PACKAGE_DIR, 'isolated Cargo config');
  assert.equal(readFileSync(CARGO_CONFIG, 'utf8'), EXPECTED_CARGO_CONFIG, 'isolated Cargo config drifted');
  assert.doesNotMatch(readFileSync(CARGO_CONFIG, 'utf8'), /credential|password|token/iu);
  assertCredentialFreeTree(CARGO_HOME);
  validateSafeTreeSync(VENDOR_DIR, PACKAGE_DIR);
}

export function jcoEnvironment(source = process.env) {
  const env = {};
  copyAllowed(source, ['PATH'], env);
  if (process.platform === 'win32') {
    copyAllowed(source, WINDOWS_RUNTIME_ENV_KEYS, env);
  }
  env.NO_COLOR = '1';
  return env;
}

export function cargoEnvironment(source = process.env) {
  validateIsolatedCargoHome();
  const env = {};
  copyAllowed(source, ['PATH'], env);
  if (process.platform === 'win32') {
    copyAllowed(source, WINDOWS_RUNTIME_ENV_KEYS, env);
  } else {
    copyAllowed(source, ['TMPDIR'], env);
  }
  copyAllowed(source, CARGO_CERT_ENV_KEYS, env);
  env.CARGO_HOME = CARGO_HOME;
  env.RUSTUP_HOME = RUSTUP_HOME;
  env.CARGO_NET_OFFLINE = 'true';
  env.CARGO_TERM_COLOR = 'never';
  env.NO_COLOR = '1';
  env.TERM = 'dumb';
  return env;
}

export function assertSanitizedToolEnvironments() {
  const sentinelSource = Object.fromEntries(DISALLOWED_ENV_KEYS.map((key) => [key, `sentinel-${key}`]));
  sentinelSource.PATH = process.env.PATH ?? '';
  sentinelSource.TMPDIR = process.env.TMPDIR ?? '/tmp';
  const environments = [jcoEnvironment(sentinelSource), cargoEnvironment(sentinelSource)];
  for (const env of environments) {
    for (const disallowed of DISALLOWED_ENV_KEYS) {
      assert.equal(Object.hasOwn(env, disallowed), false, `${disallowed} leaked into a tool environment`);
    }
  }
  assert.deepEqual(Object.keys(jcoEnvironment(sentinelSource)).sort(), ['NO_COLOR', 'PATH']);
  assert.equal(environments[1].CARGO_NET_OFFLINE, 'true');
  assert.equal(environments[1].CARGO_HOME, CARGO_HOME);
  assert.equal(environments[1].RUSTUP_HOME, RUSTUP_HOME);
}

function run(command, args, options = {}) {
  assert.notEqual(options.env, undefined, `explicit environment required for ${command}`);
  const result = spawnSync(command, args, {
    cwd: options.cwd ?? ROOT_DIR,
    encoding: 'utf8',
    env: options.env,
    input: options.input,
    maxBuffer: 16 * 1024 * 1024,
    shell: false,
    stdio: options.stdio ?? ['ignore', 'pipe', 'pipe'],
    timeout: options.timeout ?? 30_000,
  });
  if (result.error || result.status !== 0 || result.signal !== null) {
    throw new ToolExecutionError([command, ...args].join(' '), result);
  }
  return result;
}

export function runJco(jcoEntry, args, options = {}) {
  return run(process.execPath, [jcoEntry, ...args], {
    ...options,
    env: options.env ?? jcoEnvironment(),
  });
}

export function resolvePinnedPackages() {
  const jcoPackagePath = path.resolve(path.dirname(require.resolve('@bytecodealliance/jco')), '..', 'package.json');
  const shimPackagePath = path.resolve(
    path.dirname(require.resolve('@bytecodealliance/preview2-shim')),
    '..',
    '..',
    'package.json',
  );
  const jcoPackage = readJson(jcoPackagePath);
  const shimPackage = readJson(shimPackagePath);

  assert.equal(jcoPackage.name, '@bytecodealliance/jco');
  assert.equal(jcoPackage.version, '1.32.1');
  assert.deepEqual(jcoPackage.bin, { jco: 'dist/jco.js' });
  assert.equal(shimPackage.name, '@bytecodealliance/preview2-shim');
  assert.equal(shimPackage.version, '0.22.0');

  const jcoPackageDir = path.dirname(jcoPackagePath);
  const jcoEntry = path.resolve(jcoPackageDir, jcoPackage.bin.jco);
  assert.equal(
    path.relative(jcoPackageDir, jcoEntry),
    path.join('dist', 'jco.js'),
    'Jco bin must resolve inside the pinned package',
  );
  assert.equal(readFileSync(jcoEntry, 'utf8').length > 0, true, 'Jco bin must be nonempty');

  return { jcoEntry };
}

export function validateHostToolchain() {
  const nodeParts = process.versions.node.split('.').map(Number);
  assert.equal(nodeParts.length, 3, 'Node must report a three-part version');
  assert.equal(nodeParts[0], 22, 'Node major version must be 22');
  assert.equal(nodeParts[1] >= 20, true, 'Node must be at least v22.20.0');

  const manifest = readJson(PACKAGE_MANIFEST_PATH);
  const lock = readJson(PACKAGE_LOCK_PATH);
  assert.equal(manifest.name, 'peri-wasi-e2e');
  assert.equal(manifest.version, '0.0.0');
  assert.equal(manifest.private, true);
  assert.equal(manifest.type, 'module');
  assert.equal(manifest.packageManager, 'npm@10.9.3');
  assert.equal(manifest.engines.node, '>=22.20.0 <23');
  assert.deepEqual(manifest.devDependencies, {
    '@bytecodealliance/jco': '1.32.1',
    '@bytecodealliance/preview2-shim': '0.22.0',
  });
  assert.equal(lock.lockfileVersion, 3);
  assert.equal(lock.name, manifest.name);
  assert.equal(lock.version, manifest.version);
  assert.deepEqual(lock.packages[''].devDependencies, manifest.devDependencies);

  const npmUserAgent = process.env.npm_config_user_agent ?? '';
  const npmVersion = /(?:^|\s)npm\/([^\s]+)/u.exec(npmUserAgent)?.[1];
  assert.equal(npmVersion, '10.9.3', 'run the gate through pinned npm 10.9.3');

  const pinned = resolvePinnedPackages();
  assert.equal(runJco(pinned.jcoEntry, ['--version']).stdout.trim(), '1.32.1');
  assert.match(runJco(pinned.jcoEntry, ['wit', '--help']).stdout, /extract the WIT/u);
  const transpileHelp = runJco(pinned.jcoEntry, ['transpile', '--help']).stdout;
  assert.match(transpileHelp, /--name <name>/u);
  assert.match(transpileHelp, /--base64-cutoff <bytes>/u);
  return pinned;
}

export async function cleanOwnedArtifacts() {
  assertExactOwnedPath(COMPONENT_PATH, path.join(ROOT_DIR, 'target', 'wasm32-wasip2', 'release', 'peri_wasi.wasm'), 'Component');
  assertExactOwnedPath(OUTPUT_DIR, path.join(PACKAGE_DIR, 'target', 'wasi-p2-node'), 'transpile output');
  await assertSafeDescendant(COMPONENT_PATH, ROOT_DIR, { allowMissing: true, leafKind: 'file' });
  await assertSafeDescendant(OUTPUT_DIR, PACKAGE_DIR, { allowMissing: true, leafKind: 'directory' });
  await rm(COMPONENT_PATH, { force: true });
  await rm(OUTPUT_DIR, { force: true, recursive: true });
}

export function assertStableRust() {
  const env = cargoEnvironment();
  const rustc = run('rustc', ['+1.96.1', '--version', '--verbose'], { env });
  assert.match(rustc.stdout, /^rustc 1\.96\.1 \(/u);
  assert.match(rustc.stdout, /^release: 1\.96\.1$/mu);
  const installedTargets = run(
    'rustup',
    ['target', 'list', '--installed', '--toolchain', '1.96.1'],
    { env },
  ).stdout.trim().split(/\r?\n/u);
  assert.equal(installedTargets.includes('wasm32-wasip2'), true, 'wasm32-wasip2 must be installed for 1.96.1');
}

export async function buildComponent() {
  assertExactOwnedPath(TARGET_DIR, path.join(ROOT_DIR, 'target'), 'Cargo target directory');
  await assertSafeDescendant(TARGET_DIR, ROOT_DIR, { allowMissing: true, leafKind: 'directory' });
  await assertSafeDescendant(COMPONENT_PATH, ROOT_DIR, { allowMissing: true, leafKind: 'file' });
  await mkdir(TARGET_DIR, { recursive: true });
  await assertSafeDescendant(TARGET_DIR, ROOT_DIR, { leafKind: 'directory' });
  await assertSafeDescendant(COMPONENT_PATH, ROOT_DIR, { allowMissing: true, leafKind: 'file' });

  const result = run(
    'cargo',
    [
      '+1.96.1',
      '--config',
      CARGO_CONFIG,
      'build',
      '--manifest-path',
      ACQUISITION_MANIFEST,
      '--target-dir',
      TARGET_DIR,
      '--target',
      'wasm32-wasip2',
      '--release',
      '--frozen',
      '-p',
      'peri-wasi',
    ],
    { env: cargoEnvironment(), timeout: 120_000 },
  );
  await assertSafeDescendant(COMPONENT_PATH, ROOT_DIR, { leafKind: 'file' });
  return result;
}

export function captureAndValidateCargoTree() {
  const result = run(
    'cargo',
    [
      '+1.96.1',
      '--config',
      CARGO_CONFIG,
      'tree',
      '--manifest-path',
      ACQUISITION_MANIFEST,
      '--target',
      'wasm32-wasip2',
      '--edges',
      'normal',
      '--frozen',
      '-p',
      'peri-wasi',
    ],
    { env: cargoEnvironment(), timeout: 60_000 },
  );
  const tree = result.stdout;
  assert.equal(tree.trim().length > 0, true, 'Cargo tree must be nonempty');

  const packageNames = new Set(
    tree
      .split('\n')
      .map((line) => /(?:^|[│├└─ ]+)([a-zA-Z0-9_-]+) v\d/u.exec(line)?.[1])
      .filter(Boolean),
  );
  for (const forbidden of FORBIDDEN_DEPENDENCIES) {
    assert.equal(packageNames.has(forbidden), false, `forbidden dependency in WASI closure: ${forbidden}`);
  }

  const repositoryCrates = new Set();
  for (const match of tree.matchAll(/([a-zA-Z0-9_-]+) v[^\s]+ \((\/[\S][^)]*|[a-zA-Z]:\\[^)]*)\)/gu)) {
    const sourcePath = path.resolve(match[2]);
    const relative = path.relative(ROOT_DIR, sourcePath);
    if (relative !== '' && !relative.startsWith(`..${path.sep}`) && relative !== '..') {
      repositoryCrates.add(match[1]);
    }
  }
  assert.deepEqual([...repositoryCrates].sort(), [...ALLOWED_REPOSITORY_CRATES].sort());
  return tree;
}

export async function validateComponentHeader(componentPath) {
  let bytes;
  try {
    bytes = await readFile(componentPath);
  } catch (error) {
    if (error?.code === 'ENOENT') {
      throw new Error(`Component does not exist: ${componentPath}`, { cause: error });
    }
    throw error;
  }
  if (bytes.length === 0) {
    throw new Error(`Component is empty: ${componentPath}`);
  }
  if (bytes.subarray(0, CORE_WASM_HEADER.length).equals(CORE_WASM_HEADER)) {
    throw new Error(`Expected a Component, received a core Wasm module: ${componentPath}`);
  }
  if (bytes.length < COMPONENT_HEADER.length || !bytes.subarray(0, COMPONENT_HEADER.length).equals(COMPONENT_HEADER)) {
    throw new Error(`Invalid WASI Preview 2 Component header: ${componentPath}`);
  }
  return bytes;
}

export async function extractComponentWit(componentPath, jcoEntry) {
  if (path.resolve(componentPath) === COMPONENT_PATH) {
    await assertSafeDescendant(COMPONENT_PATH, ROOT_DIR, { leafKind: 'file' });
  }
  await validateComponentHeader(componentPath);
  const result = runJco(jcoEntry, ['wit', componentPath]);
  return `${result.stdout.trimEnd()}\n`;
}

export function assertComponentInterface(wit) {
  assert.equal(wit, EXPECTED_WIT, 'Jco WIT projection drifted');
  const imports = [...wit.matchAll(/^\s*import\s+([^;]+);$/gmu)].map((match) => match[1]);
  const exports = [...wit.matchAll(/^\s*export\s+([^;]+);$/gmu)].map((match) => match[1]);
  assert.deepEqual(imports, EXPECTED_COMPONENT_IMPORTS, 'Component import allowlist drifted');
  assert.deepEqual(exports, EXPECTED_COMPONENT_EXPORTS, 'Component business exports drifted');
  assert.doesNotMatch(wit, /wasi:(?:cli\/(?:arguments|environment)|filesystem|http|sockets)/u);
}

export async function transpileComponent(jcoEntry) {
  assertExactOwnedPath(OUTPUT_DIR, path.join(PACKAGE_DIR, 'target', 'wasi-p2-node'), 'transpile output');
  await assertSafeDescendant(COMPONENT_PATH, ROOT_DIR, { leafKind: 'file' });
  await assertSafeDescendant(OUTPUT_DIR, PACKAGE_DIR, { allowMissing: true, leafKind: 'directory' });
  await mkdir(OUTPUT_DIR, { recursive: true });
  await assertSafeDescendant(OUTPUT_DIR, PACKAGE_DIR, { leafKind: 'directory' });
  const result = runJco(
    jcoEntry,
    [
      'transpile',
      COMPONENT_PATH,
      '--out-dir',
      OUTPUT_DIR,
      '--name',
      'turn-policy',
      '--base64-cutoff',
      '0',
    ],
    { timeout: 60_000 },
  );
  await assertSafeDescendant(OUTPUT_DIR, PACKAGE_DIR, { leafKind: 'directory' });
  return result;
}

async function listGeneratedFiles(directory, relative = '') {
  const entries = await readdir(path.join(directory, relative), { withFileTypes: true });
  const files = [];
  for (const entry of entries) {
    const entryRelative = path.join(relative, entry.name);
    if (entry.isDirectory()) {
      files.push(...await listGeneratedFiles(directory, entryRelative));
    } else {
      assert.equal(entry.isFile(), true, `unexpected generated entry: ${entryRelative}`);
      files.push(entryRelative.split(path.sep).join('/'));
    }
  }
  return files;
}

export async function validateGeneratedOutput() {
  await assertSafeDescendant(OUTPUT_DIR, PACKAGE_DIR, { leafKind: 'directory' });
  assert.deepEqual((await listGeneratedFiles(OUTPUT_DIR)).sort(), [...EXPECTED_GENERATED_FILES].sort());
  assert.equal(await realpath(OUTPUT_DIR), OUTPUT_DIR);

  for (const relative of EXPECTED_GENERATED_FILES) {
    const generatedPath = path.resolve(OUTPUT_DIR, relative);
    assert.equal(path.relative(OUTPUT_DIR, generatedPath).startsWith('..'), false);
    await assertSafeDescendant(generatedPath, PACKAGE_DIR, { leafKind: 'file' });
    assert.equal((await stat(generatedPath)).size > 0, true, `${relative} must be nonempty`);
  }

  const coreWasm = await readFile(path.join(OUTPUT_DIR, 'turn-policy.core.wasm'));
  assert.equal(coreWasm.length > CORE_WASM_HEADER.length, true, 'generated core Wasm must be nonempty');
  assert.equal(coreWasm.subarray(0, CORE_WASM_HEADER.length).equals(CORE_WASM_HEADER), true);

  const worldTypes = await readFile(path.join(OUTPUT_DIR, 'turn-policy.d.ts'), 'utf8');
  assert.equal(
    worldTypes,
    `// world root:component/root\nexport * as policy from './interfaces/peri-turn-policy-policy.js'; // export ${INTERFACE_NAME}\n`,
  );
}

function exerciseEnvironment() {
  const env = {};
  copyAllowed(process.env, ['PATH'], env);
  if (process.platform === 'win32') {
    copyAllowed(process.env, WINDOWS_RUNTIME_ENV_KEYS, env);
  }
  return env;
}

export async function runExercise() {
  assertExactOwnedPath(GENERATED_JS, path.join(OUTPUT_DIR, 'turn-policy.js'), 'generated module');
  await assertSafeDescendant(GENERATED_JS, PACKAGE_DIR, { leafKind: 'file' });
  const result = run(
    process.execPath,
    [EXERCISE_PATH, pathToFileURL(GENERATED_JS).href],
    {
      cwd: OUTPUT_DIR,
      env: exerciseEnvironment(),
      stdio: ['ignore', 'pipe', 'pipe'],
      timeout: 10_000,
    },
  );
  assert.equal(result.status, 0);
  assert.equal(result.signal, null);
  assert.equal(result.stderr, '');
  assert.equal(result.stdout, `WASI_P2_ASSERTIONS=${EXERCISE_ASSERTION_COUNT}\n`);
}
