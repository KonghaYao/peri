import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, readdirSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { build } from 'vite';

const root = new URL('..', import.meta.url).pathname;
const outDir = mkdtempSync(join(tmpdir(), 'acp-hub-web-prod-'));
const productionModules = new Set();

function files(dir, base = dir) {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    return statSync(path).isDirectory() ? files(path, base) : [path.slice(base.length + 1)];
  });
}

try {
  await build({
    root,
    configFile: join(root, 'vite.config.ts'),
    plugins: [{
      name: 'capture-production-module-graph',
      generateBundle(_options, bundle) {
        for (const output of Object.values(bundle)) {
          if (output.type === 'chunk') Object.keys(output.modules).forEach((id) => productionModules.add(id));
        }
      },
    }],
    build: { outDir, emptyOutDir: true, manifest: true },
  });
  const paths = files(outDir);
  assert.deepEqual(paths.filter((path) => path.endsWith('.html')), ['index.html']);
  const source = paths.map((path) => readFileSync(join(outDir, path), 'utf8')).join('\n');
  assert.doesNotMatch(source, /visual-fixture|UI 状态验收台|静态测试数据/);
  const manifestPath = paths.find((path) => path.endsWith('manifest.json'));
  assert.ok(manifestPath, 'production manifest must exist');
  const manifest = JSON.parse(readFileSync(join(outDir, manifestPath), 'utf8'));
  assert.deepEqual(Object.keys(manifest).filter((key) => key.endsWith('.html')), ['index.html']);
  assert.ok(Object.keys(manifest).every((key) => !key.includes('visual-fixture')));
  assert.ok(productionModules.size > 0, 'production module graph must be captured');
  assert.ok([...productionModules].every((id) => !id.includes('/src/visual-fixture/')), 'production module graph must exclude fixture modules');
  console.log(`production boundary PASS (${paths.length} files, fixture excluded)`);
} finally {
  rmSync(outDir, { recursive: true, force: true });
}
