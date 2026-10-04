import { copyFile, cp, rm } from 'node:fs/promises';
import { resolve } from 'node:path';

const here = import.meta.dir;
const sdkRoot = resolve(here, '../..');
const source = resolve(sdkRoot, 'dist/wasm');
const dist = resolve(here, 'dist');

const build = Bun.spawn(['bun', 'run', 'build'], {
  cwd: sdkRoot, stdin: 'inherit', stdout: 'inherit', stderr: 'inherit',
});
if (await build.exited !== 0) throw new Error('SDK WASM build failed');

await rm(dist, { recursive: true, force: true });
await cp(source, dist, { recursive: true });
await copyFile(resolve(here, 'worker.js'), resolve(dist, 'worker.js'));
console.log(`Prepared Wrangler probe in ${dist}`);
