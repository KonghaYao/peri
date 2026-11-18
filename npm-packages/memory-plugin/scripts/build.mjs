import { build } from 'esbuild';
import { chmod, mkdir } from 'node:fs/promises';
await mkdir('dist', { recursive: true });
await build({ entryPoints: ['src/index.ts'], outfile: 'dist/index.js', bundle: true,
  packages: 'external', platform: 'node', target: 'node22', format: 'esm',
  banner: { js: '#!/usr/bin/env node' }, legalComments: 'none' });
await chmod('dist/index.js', 0o755);
