import { copyFile, mkdir } from 'node:fs/promises';
import { resolve } from 'node:path';

const here = import.meta.dir;
const root = resolve(here, '../..');
const source = resolve(root, 'target/wasm32-unknown-emscripten/release');
const dist = resolve(here, 'dist');

const build = Bun.spawn([
  resolve(root, 'scripts/cargo-wasm.sh'), 'build', '--locked', '--release',
  '-p', 'peri-wasm', '--target', 'wasm32-unknown-emscripten',
], { cwd: root, stdin: 'inherit', stdout: 'inherit', stderr: 'inherit' });
if (await build.exited !== 0) throw new Error('peri-wasm release build failed');

await mkdir(dist, { recursive: true });
for (const name of ['peri-wasm.js', 'peri_wasm.wasm']) {
  await copyFile(resolve(source, name), resolve(dist, name));
}
await copyFile(resolve(here, 'worker.js'), resolve(dist, 'worker.js'));
console.log(`Prepared Wrangler probe in ${dist}`);
