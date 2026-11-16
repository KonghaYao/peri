import { createHash } from 'node:crypto';
import { copyFileSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import path from 'node:path';

/** 四包只记录相对路径；provenance 由调用方提供，不把生产机器路径当安装依赖。 */
export function writeLocalKit(outDir, archives, provenance) {
  const tarballs = {};
  for (const [name, archive] of Object.entries(archives)) {
    const filename = path.basename(archive.path);
    const target = path.join(outDir, filename);
    if (path.resolve(archive.path) !== path.resolve(target)) copyFileSync(archive.path, target);
    tarballs[name] = {
      path: filename,
      version: archive.version,
      bytes: statSync(target).size,
      sha256: createHash('sha256').update(readFileSync(target)).digest('hex'),
    };
  }
  const filenames = Object.fromEntries(Object.entries(tarballs).map(([key, value]) => [key, value.path]));
  const installScript = `#!/bin/sh
set -eu
kit=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
target=\${1:-peri-agent-builder}
node - "$kit" "$target" <<'NODE'
const fs = require('node:fs');
const path = require('node:path');
const kit = process.argv[2];
const files = ${JSON.stringify(filenames)};
const archives = Object.fromEntries(Object.entries(files).map(([key, filename]) => [key, path.join(kit, filename)]));
for (const file of Object.values(archives)) {
  if (!fs.statSync(file).isFile()) throw new Error('Kit archive is not a file');
}
const overrides = {
  '@peri-code/sdk': 'file:' + archives.sdk,
  '@peri-code/rivet': 'file:' + archives.rivet,
  'peri-sandbox': 'file:' + archives.sandbox,
};
if (process.argv[3] === '--dry-run') {
  console.log(JSON.stringify({ archives, overrides }));
} else {
  const target = path.resolve(process.argv[3]);
  fs.mkdirSync(target, { recursive: true, mode: 0o700 });
  const file = path.join(target, 'package.json');
  if (fs.existsSync(file)) throw new Error('Install target must not contain package.json');
  fs.writeFileSync(file, JSON.stringify({ private: true, overrides }, null, 2) + '\\n', { flag: 'wx', mode: 0o600 });
}
NODE
if [ "$target" = '--dry-run' ]; then exit 0; fi
cd -- "$target"
bun add "$kit/${filenames.agentBuilder}"
bun pm trust peri-sandbox @openuidev/lang-core
bun install
printf '%s\\n' 'installed. Next: cp node_modules/@peri-code/agent-builder/.env.example .env'
`;
  writeFileSync(path.join(outDir, 'install.sh'), installScript, { mode: 0o755 });
  const result = {
    ...provenance,
    tarballs,
    install: {
      pathBase: 'kit directory',
      overrides: {
        '@peri-code/sdk': `file:./${filenames.sdk}`,
        '@peri-code/rivet': `file:./${filenames.rivet}`,
        'peri-sandbox': `file:./${filenames.sandbox}`,
      },
      commands: ['sh install.sh --dry-run', 'sh install.sh ~/peri-agent-builder'],
    },
  };
  writeFileSync(path.join(outDir, 'agent-builder-pack-manifest.json'), `${JSON.stringify(result, null, 2)}\n`);
  return result;
}
