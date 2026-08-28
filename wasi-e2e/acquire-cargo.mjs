import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { lstatSync, realpathSync } from 'node:fs';
import { lstat, mkdir, readFile, readdir, realpath, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

function trustedDirectory(directory, label) {
  const declared = path.resolve(directory);
  const metadata = lstatSync(declared);
  assert.equal(metadata.isSymbolicLink(), false, `${label} must not be a symlink`);
  assert.equal(metadata.isDirectory(), true, `${label} must be a directory`);
  const canonical = realpathSync(declared);
  assert.equal(canonical, declared, `${label} must already be canonical`);
  return canonical;
}

const PACKAGE_DIR = trustedDirectory(fileURLToPath(new URL('.', import.meta.url)), 'package root');
const ROOT_DIR = trustedDirectory(path.dirname(PACKAGE_DIR), 'repository root');
const PACKAGE_TARGET_DIR = path.join(PACKAGE_DIR, 'target');
const ACQUISITION_DIR = path.join(PACKAGE_TARGET_DIR, 'cargo-acquisition');
const VENDOR_DIR = path.join(PACKAGE_TARGET_DIR, 'cargo-vendor');
const ISOLATED_CARGO_HOME = path.join(PACKAGE_TARGET_DIR, 'cargo-home');
const ACQUISITION_MANIFEST = path.join(ACQUISITION_DIR, 'Cargo.toml');
const ACQUISITION_METADATA = path.join(ACQUISITION_DIR, 'metadata.json');
const ROOT_MANIFEST = path.join(ROOT_DIR, 'Cargo.toml');
const POLICY_DIR = path.join(ROOT_DIR, 'peri-turn-policy');
const WASI_DIR = path.join(ROOT_DIR, 'peri-wasi');
const FIXED_GENERATED_PATHS = [ACQUISITION_DIR, VENDOR_DIR, ISOLATED_CARGO_HOME];
const FINGERPRINT_INPUTS = [
  ['root-cargo-lock', path.join(ROOT_DIR, 'Cargo.lock')],
  ['root-cargo-manifest', ROOT_MANIFEST],
  ['peri-wasi-manifest', path.join(WASI_DIR, 'Cargo.toml')],
  ['peri-turn-policy-manifest', path.join(POLICY_DIR, 'Cargo.toml')],
];

function isContainedPath(candidate, root) {
  const relative = path.relative(root, candidate);
  return relative === '' || (relative !== '..' && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative));
}

async function assertSafeDescendant(target, options = {}) {
  const resolved = path.resolve(target);
  assert.equal(isContainedPath(resolved, PACKAGE_DIR), true, `generated path escaped package root: ${target}`);
  const relative = path.relative(PACKAGE_DIR, resolved);
  assert.notEqual(relative, '', 'generated path must be below package root');
  let current = PACKAGE_DIR;
  let missing = false;
  for (const [index, component] of relative.split(path.sep).entries()) {
    current = path.join(current, component);
    if (missing) continue;
    let metadata;
    try {
      metadata = await lstat(current);
    } catch (error) {
      if (error?.code !== 'ENOENT') throw error;
      assert.equal(options.allowMissing, true, `generated path is missing: ${current}`);
      missing = true;
      continue;
    }
    assert.equal(metadata.isSymbolicLink(), false, `symlink rejected in generated path: ${current}`);
    if (index < relative.split(path.sep).length - 1) {
      assert.equal(metadata.isDirectory(), true, `generated path ancestor is not a directory: ${current}`);
    }
    assert.equal(isContainedPath(await realpath(current), PACKAGE_DIR), true, `real path escaped package root: ${current}`);
  }
}

async function assertSafeTree(directory) {
  await assertSafeDescendant(directory);
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const entryPath = path.join(directory, entry.name);
    const metadata = await lstat(entryPath);
    assert.equal(metadata.isSymbolicLink(), false, `symlink rejected in generated tree: ${entryPath}`);
    assert.equal(isContainedPath(await realpath(entryPath), PACKAGE_DIR), true, `generated entry escaped package root`);
    if (metadata.isDirectory()) await assertSafeTree(entryPath);
  }
}

function parentToolHome(name, fallback) {
  const parentHome = process.env.HOME ?? process.env.USERPROFILE;
  const declared = process.env[name] ?? (parentHome ? path.join(parentHome, fallback) : undefined);
  assert.equal(typeof declared, 'string', `${name} is required for Cargo acquisition`);
  return trustedDirectory(declared, name);
}

function acquisitionEnvironment() {
  const env = {};
  for (const key of ['PATH', 'TMPDIR', 'NIX_SSL_CERT_FILE', 'SSL_CERT_DIR', 'SSL_CERT_FILE']) {
    if (typeof process.env[key] === 'string') env[key] = process.env[key];
  }
  if (process.platform === 'win32') {
    for (const key of ['SystemRoot', 'TEMP', 'TMP', 'WINDIR']) {
      if (typeof process.env[key] === 'string') env[key] = process.env[key];
    }
  }
  env.CARGO_HOME = parentToolHome('CARGO_HOME', '.cargo');
  env.RUSTUP_HOME = parentToolHome('RUSTUP_HOME', '.rustup');
  env.CARGO_NET_OFFLINE = 'true';
  env.CARGO_TERM_COLOR = 'never';
  env.NO_COLOR = '1';
  env.TERM = 'dumb';
  return env;
}

function runCargo(args, cwd = ROOT_DIR) {
  const result = spawnSync('cargo', ['+1.96.1', ...args], {
    cwd,
    encoding: 'utf8',
    env: acquisitionEnvironment(),
    maxBuffer: 16 * 1024 * 1024,
    shell: false,
    stdio: ['ignore', 'pipe', 'pipe'],
    timeout: 120_000,
  });
  if (result.error || result.status !== 0 || result.signal !== null) {
    const detail = [result.stderr, result.stdout].filter(Boolean).join('\n').trim();
    throw new Error(
      `offline Cargo acquisition command failed: cargo +1.96.1 ${args.join(' ')}${detail ? `\n${detail}` : ''}\n` +
      'Required crate sources must already exist in the parent Cargo cache; no network fallback is permitted.',
      { cause: result.error },
    );
  }
  return result.stdout;
}

function registrySet(tree, localPackages) {
  const packages = new Set();
  for (const line of tree.split(/\r?\n/u)) {
    const match = /(?:^|[│├└─ ]+)([a-zA-Z0-9_-]+) v([^\s]+)/u.exec(line);
    if (match && !localPackages.has(match[1])) packages.add(`${match[1]}@${match[2]}`);
  }
  return [...packages].sort();
}

function cargoConfig() {
  return `[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = ${JSON.stringify(VENDOR_DIR)}

[net]
offline = true
`;
}

async function fingerprintMetadata() {
  const combined = createHash('sha256');
  const inputs = [];
  for (const [label, file] of FINGERPRINT_INPUTS) {
    const labelBytes = Buffer.from(label, 'utf8');
    const contents = await readFile(file);
    const lengths = Buffer.alloc(16);
    lengths.writeBigUInt64BE(BigInt(labelBytes.length), 0);
    lengths.writeBigUInt64BE(BigInt(contents.length), 8);
    combined.update(lengths.subarray(0, 8));
    combined.update(labelBytes);
    combined.update(lengths.subarray(8));
    combined.update(contents);
    inputs.push({
      label,
      length: contents.length,
      sha256: createHash('sha256').update(contents).digest('hex'),
    });
  }
  return {
    schema: 1,
    algorithm: 'sha256-framed-label-length-content',
    inputs,
    fingerprint: combined.digest('hex'),
  };
}

async function main() {
  await assertSafeDescendant(PACKAGE_TARGET_DIR, { allowMissing: true });
  await mkdir(PACKAGE_TARGET_DIR, { recursive: true });
  await assertSafeDescendant(PACKAGE_TARGET_DIR);
  for (const generated of FIXED_GENERATED_PATHS) {
    await assertSafeDescendant(generated, { allowMissing: true });
    await rm(generated, { force: true, recursive: true });
  }

  await mkdir(path.join(ACQUISITION_DIR, 'wit'), { recursive: true });
  await assertSafeDescendant(ACQUISITION_DIR);
  const manifest = `[package]
name = "peri-wasi"
version = "0.2.0"
edition = "2021"
description = "WASI Preview 2 component for portable Peri turn policy"
publish = false

[lib]
path = ${JSON.stringify(path.join(WASI_DIR, 'src', 'lib.rs'))}
crate-type = ["cdylib"]

[dependencies]
peri-turn-policy = { path = ${JSON.stringify(POLICY_DIR)} }
wit-bindgen = { version = "=0.57.1", default-features = false, features = ["macros"] }

[workspace]
`;
  await writeFile(ACQUISITION_MANIFEST, manifest, { encoding: 'utf8', flag: 'wx' });
  await writeFile(
    path.join(ACQUISITION_DIR, 'wit', 'world.wit'),
    await readFile(path.join(WASI_DIR, 'wit', 'world.wit')),
    { flag: 'wx' },
  );

  runCargo(['generate-lockfile', '--manifest-path', ACQUISITION_MANIFEST, '--offline']);
  const rootTree = runCargo([
    'tree', '--manifest-path', ROOT_MANIFEST, '-p', 'peri-wasi', '--target', 'wasm32-wasip2',
    '--edges', 'normal', '--frozen',
  ]);
  const acquisitionTree = runCargo([
    'tree', '--manifest-path', ACQUISITION_MANIFEST, '--target', 'wasm32-wasip2',
    '--edges', 'normal', '--locked', '--offline',
  ]);
  const rootRegistry = registrySet(rootTree, new Set(['peri-turn-policy', 'peri-wasi']));
  const acquisitionRegistry = registrySet(acquisitionTree, new Set(['peri-turn-policy', 'peri-wasi']));
  if (JSON.stringify(rootRegistry) !== JSON.stringify(acquisitionRegistry)) {
    throw new Error(
      `standalone Cargo registry closure drifted\nroot=${JSON.stringify(rootRegistry)}\nacquisition=${JSON.stringify(acquisitionRegistry)}`,
    );
  }

  runCargo([
    'vendor', '--manifest-path', ACQUISITION_MANIFEST, '--locked', '--offline',
    '--respect-source-config', '--versioned-dirs', VENDOR_DIR,
  ]);
  await assertSafeTree(VENDOR_DIR);
  const vendorEntries = (await readdir(VENDOR_DIR, { withFileTypes: true }))
    .filter((entry) => entry.isDirectory())
    .map((entry) => entry.name)
    .sort();
  const expectedVendorEntries = rootRegistry.map((entry) => entry.replace('@', '-')).sort();
  assert.deepEqual(vendorEntries, expectedVendorEntries, 'versioned vendor directory set drifted');

  await mkdir(ISOLATED_CARGO_HOME, { recursive: true });
  await assertSafeDescendant(ISOLATED_CARGO_HOME);
  await writeFile(path.join(ISOLATED_CARGO_HOME, 'config.toml'), cargoConfig(), { encoding: 'utf8', flag: 'wx' });
  await assertSafeTree(ISOLATED_CARGO_HOME);
  const cargoHomeEntries = await readdir(ISOLATED_CARGO_HOME);
  assert.deepEqual(cargoHomeEntries, ['config.toml']);
  assert.doesNotMatch(cargoConfig(), /credential|password|token/iu);

  await writeFile(
    ACQUISITION_METADATA,
    `${JSON.stringify(await fingerprintMetadata(), null, 2)}\n`,
    { encoding: 'utf8', flag: 'wx' },
  );
  await assertSafeDescendant(ACQUISITION_METADATA);

  process.stdout.write(`CARGO_VENDOR_REGISTRY_PACKAGES=${rootRegistry.length}\n`);
}

try {
  await main();
} catch (error) {
  process.stderr.write(`Cargo acquisition failed: ${error.message}\n`);
  process.exitCode = 1;
}
