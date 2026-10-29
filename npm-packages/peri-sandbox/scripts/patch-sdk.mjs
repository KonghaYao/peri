import { existsSync, readFileSync, writeFileSync, readdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const sdkDist = (pkg) => dirname(fileURLToPath(import.meta.resolve(`@modelcontextprotocol/${pkg}`)));

function patch(file, before, after) {
  const source = readFileSync(file, 'utf8');
  if (source.includes(after)) return;
  if (!source.includes(before)) throw new Error(`SDK 2.4.0 compatibility anchor missing: ${file}`);
  writeFileSync(file, source.replace(before, after));
}

for (const pkg of ['client', 'server']) {
  const dir = sdkDist(pkg);
  for (const file of readdirSync(dir).filter((name) => /^src-.*\.(mjs|cjs)$/.test(name))) {
    const previous =
      'if (rawResultType === "task" && method === "tools/call" && typeof raw.taskId === "string") return { kind: "complete", result: raw };';
    const previousNested =
      'if (rawResultType === "task" && method === "tools/call" && raw.task && typeof raw.task.taskId === "string") return { kind: "complete", result: raw };';
    const updated =
      'if (rawResultType === "task" && method === "tools/call" && (typeof raw.taskId === "string" || raw.task && typeof raw.task.taskId === "string")) return { kind: "complete", result: raw };';
    if (readFileSync(join(dir, file), 'utf8').includes(previous)) patch(join(dir, file), previous, updated);
    if (readFileSync(join(dir, file), 'utf8').includes(previousNested)) patch(join(dir, file), previousNested, updated);
    patch(
      join(dir, file),
      'if (rawResultType !== "complete") return {',
      `${updated}\n\t\tif (rawResultType !== "complete") return {`,
    );
    if (pkg === 'server')
      patch(
        join(dir, file),
        'if (provided === "complete") return result;',
        'if (provided === "complete" || provided === "task" && method === "tools/call") return result;',
      );
  }
}
const dir = sdkDist('server');
for (const file of readdirSync(dir).filter((name) => /^mcp-.*\.(mjs|cjs)$/.test(name))) {
  const prefix = file.endsWith('.cjs') ? 'require_src.' : '';
  const previous = 'result?.resultType === "task" && typeof result.taskId === "string"';
  const previousNested = 'result?.resultType === "task" && result.task && typeof result.task.taskId === "string"';
  const updated =
    'result?.resultType === "task" && (typeof result.taskId === "string" || result.task && typeof result.task.taskId === "string")';
  if (readFileSync(join(dir, file), 'utf8').includes(previous)) patch(join(dir, file), previous, updated);
  if (readFileSync(join(dir, file), 'utf8').includes(previousNested)) patch(join(dir, file), previousNested, updated);
  patch(
    join(dir, file),
    `if (${prefix}isInputRequiredResult(result)) return result;\n\t\t\tconst normalizedResult`,
    `if (${prefix}isInputRequiredResult(result) || ${updated}) return result;\n\t\t\tconst normalizedResult`,
  );
}

// SRT 的代理环境变量使用 localhost；沙箱内 localhost 会先解析到 ::1，而
// 代理只监听 127.0.0.1，导致 curl/git 连接挂起或快速失败。统一改为 IPv4。
const srtRoot = dirname(fileURLToPath(import.meta.resolve('@anthropic-ai/sandbox-runtime')));
const srtUtils = [
  join(srtRoot, 'sandbox', 'sandbox-utils.js'),
  join(srtRoot, 'dist', 'sandbox', 'sandbox-utils.js'),
].find((file) => existsSync(file));
if (!srtUtils) throw new Error('sandbox-runtime sandbox-utils.js not found');
const srtSource = readFileSync(srtUtils, 'utf8');
if (!srtSource.includes('${auth}127.0.0.1:')) {
  if (!srtSource.includes('${auth}localhost:'))
    throw new Error(`sandbox-runtime proxy anchor missing: ${srtUtils}`);
  writeFileSync(srtUtils, srtSource.replaceAll('${auth}localhost:', '${auth}127.0.0.1:'));
}
