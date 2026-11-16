import { expect, test } from 'bun:test';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  realpathSync,
  renameSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import os from 'node:os';
import path from 'node:path';

// @ts-expect-error 轻量套件脚本是 Node ESM，生产与测试共用同一实现。
import { writeLocalKit } from '../../scripts/local-kit.mjs';

test('ABV-08 four small fixtures move as one kit and installer uses its own directory', () => {
  const root = realpathSync(mkdtempSync(path.join(os.tmpdir(), 'agent-builder-abv-kit-')));
  try {
    const producer = path.join(root, 'producer');
    const kit = path.join(root, 'kit');
    mkdirSync(producer);
    mkdirSync(kit);
    const archives = Object.fromEntries(
      ['agentBuilder', 'sdk', 'rivet', 'sandbox'].map((name) => {
        const file = path.join(producer, `${name}.tgz`);
        writeFileSync(file, `small-fixture-${name}`);
        return [name, { path: file, version: '0.0.0' }];
      }),
    );
    writeLocalKit(kit, archives, {
      provenance: { commit: 'fixture', upstream: 'synthetic fixture; not real packages' },
    });
    expect(readdirSync(kit).filter((name) => name.endsWith('.tgz'))).toHaveLength(4);
    const moved = path.join(root, 'customer kit with spaces');
    renameSync(kit, moved);
    renameSync(producer, path.join(root, 'producer-moved'));
    execFileSync('sh', ['-n', path.join(moved, 'install.sh')]);
    const output = execFileSync('sh', [path.join(moved, 'install.sh'), '--dry-run'], {
      cwd: root,
      encoding: 'utf8',
    });
    const result = JSON.parse(output) as {
      archives: Record<string, string>;
      overrides: Record<string, string>;
    };
    for (const file of Object.values(result.archives)) {
      expect(path.dirname(file)).toBe(moved);
      expect(existsSync(file)).toBe(true);
    }
    expect(Object.values(result.overrides).every((value) => value.startsWith(`file:${moved}/`))).toBe(true);
    const manifestText = readFileSync(path.join(moved, 'agent-builder-pack-manifest.json'), 'utf8');
    expect(manifestText.includes(producer)).toBe(false);
    expect(readFileSync(path.join(moved, 'install.sh'), 'utf8').includes(root)).toBe(false);
    const manifest = JSON.parse(manifestText) as {
      tarballs: Record<string, { path: string; bytes: number; sha256: string }>;
    };
    for (const item of Object.values(manifest.tarballs)) {
      expect(path.isAbsolute(item.path)).toBe(false);
      const bytes = readFileSync(path.join(moved, item.path));
      expect(item.bytes).toBe(bytes.length);
      expect(item.sha256).toBe(createHash('sha256').update(bytes).digest('hex'));
    }
    expect(readdirSync(root).sort()).toEqual(['customer kit with spaces', 'producer-moved']);
    rmSync(path.join(moved, 'sdk.tgz'));
    expect(() =>
      execFileSync('sh', [path.join(moved, 'install.sh'), '--dry-run'], { stdio: 'pipe' }),
    ).toThrow();
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
  expect(existsSync(root)).toBe(false);
});
