import assert from 'node:assert/strict';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, realpathSync, rmSync, symlinkSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { resolveExplicitWorkspace, resolveOptions } from '../src/instance/options.js';
import { SandboxError, type SandboxProfile, type SandboxSettings } from '../src/instance/types.js';
import { makeDirectories } from '../src/instance/workspace.js';
import { policyFor } from '../src/runtime/policy.js';

const id = `sbx_${'c'.repeat(32)}`;
const profile: SandboxProfile = {
  allowedDomains: [],
  runtimeReadPaths: ['/usr'],
  memoryMax: '1G',
  tasksMax: 32,
  cpuQuota: '100%',
};

function privateDirectory(path: string): string {
  mkdirSync(path, { recursive: true, mode: 0o700 });
  chmodSync(path, 0o700);
  return realpathSync(path);
}

function fixture(): { root: string; sandboxesRoot: string; workspace: string; settings: SandboxSettings } {
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'peri-explicit-workspace-')));
  const sandboxesRoot = privateDirectory(join(root, 'sandboxes'));
  const workspace = privateDirectory(join(root, 'project'));
  return {
    root,
    sandboxesRoot,
    workspace,
    settings: {
      sandboxesRoot,
      periPath: process.execPath,
      periSha256: 'a'.repeat(64),
      defaultProfile: 'default',
      profiles: { default: profile },
    },
  };
}

function invalid(action: () => unknown): void {
  assert.throws(action, (error: unknown) => error instanceof SandboxError && error.code === 'invalid_options');
}

test('resolveOptions uses an explicit workspace verbatim and does not join it under sandboxesRoot', () => {
  const { root, sandboxesRoot, workspace, settings } = fixture();
  try {
    const resolved = resolveOptions({ sandboxId: id, workspacePath: workspace }, settings);
    assert.equal(resolved.workspacePath, workspace);
    assert.equal(resolved.homePath, join(sandboxesRoot, id, 'home'));
    assert.notEqual(resolved.workspacePath, join(resolved.sandboxRoot, 'workspace'));
    assert.equal(resolved.sandboxesRoot, sandboxesRoot);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('an explicit workspace without a caller handle gets a fresh opaque runtime id', () => {
  const { root, sandboxesRoot, workspace, settings } = fixture();
  try {
    const first = resolveOptions({ workspacePath: workspace }, settings);
    const second = resolveOptions({ workspacePath: workspace }, settings);
    assert.match(first.sandboxId, /^sbx_[a-f0-9]{32}$/);
    assert.notEqual(first.sandboxId, second.sandboxId);
    // The handle only names managed state; it never locates the workspace.
    assert.equal(first.workspacePath, workspace);
    assert.equal(first.homePath, join(sandboxesRoot, first.sandboxId, 'home'));
    assert.notEqual(first.workspacePath, join(first.sandboxRoot, 'workspace'));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('resolveExplicitWorkspace rejects relative, unnormalized, missing and symlinked workspaces', () => {
  const { root, sandboxesRoot, workspace } = fixture();
  try {
    invalid(() => resolveExplicitWorkspace('relative/workspace', sandboxesRoot));
    invalid(() => resolveExplicitWorkspace(`${workspace}/../project`, sandboxesRoot));
    invalid(() => resolveExplicitWorkspace('/', sandboxesRoot));
    invalid(() => resolveExplicitWorkspace(join(root, 'missing'), sandboxesRoot));

    const link = join(root, 'project-link');
    symlinkSync(workspace, link);
    invalid(() => resolveExplicitWorkspace(link, sandboxesRoot));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('resolveExplicitWorkspace rejects non-private directories', () => {
  const { root, sandboxesRoot, workspace } = fixture();
  try {
    chmodSync(workspace, 0o755);
    invalid(() => resolveExplicitWorkspace(workspace, sandboxesRoot));
  } finally {
    chmodSync(workspace, 0o700);
    rmSync(root, { recursive: true, force: true });
  }
});

test('resolveExplicitWorkspace rejects overlap with managed sandbox state in both directions', () => {
  const { root, sandboxesRoot } = fixture();
  try {
    const inside = privateDirectory(join(sandboxesRoot, 'inside'));
    invalid(() => resolveExplicitWorkspace(inside, sandboxesRoot));
    // root contains sandboxesRoot, so authorizing root would expose managed state.
    invalid(() => resolveExplicitWorkspace(root, sandboxesRoot));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('makeDirectories leaves an explicit workspace alone and still creates home', () => {
  const { root, sandboxesRoot, workspace } = fixture();
  try {
    makeDirectories(sandboxesRoot, id, workspace);
    assert.equal(existsSync(workspace), true);
    assert.equal(existsSync(join(sandboxesRoot, id, 'home')), true);
    assert.equal(existsSync(join(sandboxesRoot, id, 'workspace')), false);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('policyFor grants read/write on the explicit workspace instead of the derived path', () => {
  const { root, sandboxesRoot, workspace } = fixture();
  try {
    const policy = JSON.parse(
      policyFor({
        sandboxesRoot,
        sandboxId: id,
        workspacePath: workspace,
        homePath: join(sandboxesRoot, id, 'home'),
        profile,
        periPath: process.execPath,
      }).json,
    );
    assert(policy.filesystem.allowRead.includes(workspace));
    assert(policy.filesystem.allowWrite.includes(workspace));
    assert.equal(policy.filesystem.allowRead.includes(join(sandboxesRoot, id, 'workspace')), false);
    assert(policy.filesystem.allowWrite.includes(join(sandboxesRoot, id, 'home')));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
