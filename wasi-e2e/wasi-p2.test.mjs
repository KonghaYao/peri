import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, realpath, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { after, before, test } from 'node:test';

import {
  ToolExecutionError,
  ACQUISITION_METADATA,
  assertComponentInterface,
  assertSanitizedToolEnvironments,
  assertStableRust,
  buildComponent,
  captureAndValidateCargoTree,
  cleanOwnedArtifacts,
  extractComponentWit,
  resolvePinnedPackages,
  runExercise,
  runJco,
  transpileComponent,
  validateComponentHeader,
  validateGeneratedOutput,
  validateHostToolchain,
  validateFingerprintMetadata,
  validateSafeTreeSync,
  COMPONENT_PATH,
} from './harness.mjs';

const pinned = resolvePinnedPackages();
let fixtureDirectory;
let fixtures;

before(async () => {
  await cleanOwnedArtifacts();
  fixtureDirectory = await mkdtemp(path.join(tmpdir(), 'peri-wasi-negative-'));
  fixtures = {
    missing: path.join(fixtureDirectory, 'missing.wasm'),
    empty: path.join(fixtureDirectory, 'empty.wasm'),
    core: path.join(fixtureDirectory, 'core.wasm'),
    corrupt: path.join(fixtureDirectory, 'corrupt-component.wasm'),
  };
  await writeFile(fixtures.empty, Buffer.alloc(0));
  await writeFile(fixtures.core, Buffer.from([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]));
  await writeFile(
    fixtures.corrupt,
    Buffer.from([0x00, 0x61, 0x73, 0x6d, 0x0d, 0x00, 0x01, 0x00, 0xff]),
  );
});

after(async () => {
  if (fixtureDirectory) {
    await rm(fixtureDirectory, { force: true, recursive: true });
  }
});

test('rejects a missing Component before invoking Jco', async () => {
  await assert.rejects(validateComponentHeader(fixtures.missing), /Component does not exist/u);
});

test('rejects an empty Component before invoking Jco', async () => {
  await assert.rejects(validateComponentHeader(fixtures.empty), /Component is empty/u);
});

test('rejects a core Wasm module where a Component is required', async () => {
  await assert.rejects(validateComponentHeader(fixtures.core), /received a core Wasm module/u);
});

test('rejects a corrupt Component through the pinned Jco parser', async () => {
  await assert.rejects(
    extractComponentWit(fixtures.corrupt, pinned.jcoEntry),
    (error) => error instanceof ToolExecutionError && error.status !== 0 && error.signal === null,
  );
});

test('reports a nonzero pinned Jco subprocess execution', () => {
  assert.throws(
    () => runJco(pinned.jcoEntry, ['wit', fixtures.missing]),
    (error) => error instanceof ToolExecutionError && error.status !== 0 && error.stderr.length > 0,
  );
});

test('rejects tampered Cargo acquisition fingerprint metadata', async () => {
  const metadataCopy = path.join(fixtureDirectory, 'tampered-metadata.json');
  const metadata = JSON.parse(await readFile(ACQUISITION_METADATA, 'utf8'));
  metadata.fingerprint = '0'.repeat(64);
  await writeFile(metadataCopy, `${JSON.stringify(metadata, null, 2)}\n`);
  assert.throws(
    () => validateFingerprintMetadata(metadataCopy),
    (error) => error instanceof assert.AssertionError,
  );
});

test('rejects a symlink anywhere in a consumed vendor tree', async (context) => {
  const trustedRoot = path.join(fixtureDirectory, 'vendor-symlink-root');
  const vendor = path.join(trustedRoot, 'vendor');
  const outside = path.join(fixtureDirectory, 'outside-vendor-file');
  await mkdir(vendor, { recursive: true });
  await writeFile(outside, 'outside');
  try {
    await symlink(outside, path.join(vendor, 'redirect'));
  } catch (error) {
    if (error?.code === 'EPERM' || error?.code === 'EACCES') {
      context.skip(`symlinks unavailable: ${error.code}`);
      return;
    }
    throw error;
  }
  const canonicalRoot = await realpath(trustedRoot);
  assert.throws(
    () => validateSafeTreeSync(path.join(canonicalRoot, 'vendor'), canonicalRoot),
    /symlink rejected/u,
  );
});

test('builds, inspects, transpiles, and executes the WASI P2 Component', { timeout: 180_000 }, async () => {
  await cleanOwnedArtifacts();
  assertSanitizedToolEnvironments();
  const tools = validateHostToolchain();
  assertStableRust();
  await buildComponent();
  captureAndValidateCargoTree();
  await validateComponentHeader(COMPONENT_PATH);
  const wit = await extractComponentWit(COMPONENT_PATH, tools.jcoEntry);
  assertComponentInterface(wit);
  await transpileComponent(tools.jcoEntry);
  await validateGeneratedOutput();
  await runExercise();
});
