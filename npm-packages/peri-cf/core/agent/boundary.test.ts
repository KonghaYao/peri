import { describe, expect, test } from 'bun:test';
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { dirname, relative, resolve, sep } from 'node:path';

/**
 * 模块边界回归：文件夹是模块，`index.ts` 是唯一公共出口。
 * 跨模块必须走出口；`core/agent/` 保持平台无关且不承载浏览器门面；
 * 客户端门面归 `frontend/remote/`；`frontend/` 与 `server/` 互不引用。
 */
const packageRoot = resolve(import.meta.dir, '../..');
const sourceRoots = ['core', 'frontend', 'server', 'scripts'];
const skippedDirectories = new Set(['node_modules', 'dist', '.cache', '.wrangler', 'logs']);

const moduleRoots = [
  'core/agent', 'core/agent/view',
  'core/shared',
  'server', 'server/api',
  'frontend/chat', 'frontend/chat/runtime', 'frontend/chat/components',
  'frontend/remote', 'frontend/settings',
  'frontend/hooks', 'frontend/lib',
  'frontend/components/assistant-ui',
].map((root) => root.split('/').join(sep));

function sourceFiles(directory: string): string[] {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    if (skippedDirectories.has(entry.name)) return [];
    const path = resolve(directory, entry.name);
    if (entry.isDirectory()) return sourceFiles(path);
    return entry.isFile() && /\.(ts|tsx)$/.test(entry.name) ? [path] : [];
  });
}

const files = sourceRoots.flatMap((root) => sourceFiles(resolve(packageRoot, root)));
const relativeFiles = new Set(files.map((file) => relative(packageRoot, file)));

function withoutComments(source: string): string {
  return source.replace(/\/\*[\s\S]*?\*\//g, '').replace(/\/\/.*$/gm, '');
}

function importsOf(source: string): string[] {
  const found: string[] = [];
  for (const match of source.matchAll(/\bfrom\s+["']([^"']+)["']/g)) found.push(match[1]!);
  for (const match of source.matchAll(/\bimport\s*\(\s*["']([^"']+)["']\s*\)/g)) found.push(match[1]!);
  for (const match of source.matchAll(/\bimport\s+["']([^"']+)["']/g)) found.push(match[1]!);
  return found;
}

function resolveImport(specifier: string, importer: string): string | undefined {
  let base: string;
  if (specifier.startsWith('@core/')) base = resolve(packageRoot, 'core', specifier.slice(6));
  else if (specifier.startsWith('@/')) base = resolve(packageRoot, 'frontend', specifier.slice(2));
  else if (specifier.startsWith('.')) base = resolve(dirname(resolve(packageRoot, importer)), specifier);
  else return undefined;
  const baseRelative = relative(packageRoot, base);
  return [
    baseRelative,
    `${baseRelative}.ts`,
    `${baseRelative}.tsx`,
    `${baseRelative}${sep}index.ts`,
    `${baseRelative}${sep}index.tsx`,
  ].find((candidate) => relativeFiles.has(candidate));
}

function within(parent: string, child: string): boolean {
  const prefix = parent.endsWith(sep) ? parent : `${parent}${sep}`;
  return child === parent || child.startsWith(prefix);
}

/** 跨模块引用应落到的出口；同模块内部引用返回目标本身。 */
function boundaryOf(target: string, importer: string): string {
  const containing = moduleRoots.filter((moduleRoot) => within(moduleRoot, target));
  if (containing.length === 0) return target;
  const outermost = containing.reduce((a, b) => (a.length <= b.length ? a : b));
  if (!within(outermost, importer)) return `${outermost}${sep}index.ts`;
  const nested = containing.filter((moduleRoot) => !within(moduleRoot, importer));
  if (nested.length === 0) return target;
  return `${nested.reduce((a, b) => (a.length >= b.length ? a : b))}${sep}index.ts`;
}

describe('folder module boundary', () => {
  test('every module folder exposes index.ts', () => {
    const missing = moduleRoots.filter((root) => !existsSync(resolve(packageRoot, root, 'index.ts')));
    expect(missing.map((root) => relative(packageRoot, root))).toEqual([]);
  });

  test('cross-module imports go through the module entry', () => {
    const violations: string[] = [];
    for (const file of files) {
      const importer = relative(packageRoot, file);
      const source = withoutComments(readFileSync(file, 'utf8'));
      for (const specifier of importsOf(source)) {
        if (specifier.includes('?') || /\.(css|wasm|js)$/.test(specifier)) continue;
        const target = resolveImport(specifier, importer);
        if (!target) continue; // 包依赖由 typecheck/运行时负责。
        const boundary = boundaryOf(target, importer);
        if (target !== boundary) {
          violations.push(`${importer} → ${specifier} (expected ${relative(packageRoot, boundary)})`);
        }
      }
    }
    expect(violations).toEqual([]);
  });
});

describe('frontend/server separation', () => {
  test('frontend and server do not import each other', () => {
    const violations: string[] = [];
    for (const file of files) {
      const importer = relative(packageRoot, file);
      if (/\.test\.(ts|tsx)$/.test(importer)) continue;
      const side = within('frontend', importer) ? 'frontend' : within('server', importer) ? 'server' : undefined;
      if (!side) continue;
      const other = side === 'frontend' ? 'server' : 'frontend';
      for (const specifier of importsOf(withoutComments(readFileSync(file, 'utf8')))) {
        const target = resolveImport(specifier, importer);
        if (target && within(other, target)) violations.push(`${importer} → ${specifier}`);
      }
    }
    expect(violations).toEqual([]);
  });
});

describe('agent module purity', () => {
  const agentProduction = files.filter((file) => {
    const path = relative(packageRoot, file);
    return path.startsWith(`core${sep}agent${sep}`) && !/\.test\.(ts|tsx)$/.test(path);
  });

  test('only imports its own sources, shared DTOs, and the SDK', () => {
    const violations: string[] = [];
    for (const file of agentProduction) {
      const importer = relative(packageRoot, file);
      for (const specifier of importsOf(withoutComments(readFileSync(file, 'utf8')))) {
        if (specifier.startsWith('@peri-code/sdk')) continue;
        const target = resolveImport(specifier, importer);
        if (target && (within(`core${sep}agent`, target) || within(`core${sep}shared`, target))) continue;
        violations.push(`${importer} → ${specifier}`);
      }
    }
    expect(violations).toEqual([]);
  });

  test('does not touch browser storage, DOM, or React', () => {
    const forbidden = [
      /\blocalStorage\b/,
      /\bsessionStorage\b/,
      /\bindexedDB\b/,
      /\bglobalThis\.location\b/,
      /\bdocument\./,
      /\bwindow\./,
      /\bnavigator\./,
      /\bfrom\s+["']react/,
      /\bfrom\s+["']@assistant-ui/,
      /\bfrom\s+["']@tanstack/,
    ];
    const violations: string[] = [];
    for (const file of agentProduction) {
      const code = withoutComments(readFileSync(file, 'utf8'));
      for (const pattern of forbidden) {
        if (pattern.test(code)) violations.push(`${relative(packageRoot, file)} → ${pattern}`);
      }
    }
    expect(violations).toEqual([]);
  });

  test('keeps the browser client facade in frontend/remote', () => {
    const agentEntry = withoutComments(readFileSync(resolve(packageRoot, 'core/agent/index.ts'), 'utf8'));
    for (const symbol of ['AgentManager', 'AgentView', 'ChatView', 'Phase', 'EventSource'])
      expect(agentEntry).not.toMatch(new RegExp(`\\b${symbol}\\b`));

    const remoteEntry = withoutComments(readFileSync(resolve(packageRoot, 'frontend/remote/index.ts'), 'utf8'));
    for (const symbol of ['ChatController', 'ChatView', 'Phase', 'ChatLoadState'])
      expect(remoteEntry).toMatch(new RegExp(`\\b${symbol}\\b`));
    for (const symbol of ['AgentManager', 'RemoteChatRuntime', 'createRemoteAgent'])
      expect(remoteEntry).not.toMatch(new RegExp(`\\b${symbol}\\b`));
  });
});

describe('legacy chat transport removal', () => {
  test('production frontend/core sources contain no SSE, AgentManager, or old command/event routes', () => {
    const legacy = [/\bEventSource\b/, /\bAgentManager\b/, /\/commands\b/, /\/events\b/,
      /\bRemoteChatRuntime\b/, /\bChatCommand\b/, /\bSyncState\b/];
    const violations: string[] = [];
    for (const file of files) {
      const path = relative(packageRoot, file);
      if (!/^(frontend|core)[\\/]/.test(path) || /\.test\.(ts|tsx)$/.test(path)) continue;
      const code = withoutComments(readFileSync(file, 'utf8'));
      for (const pattern of legacy) if (pattern.test(code)) violations.push(`${path} → ${pattern}`);
    }
    expect(violations).toEqual([]);
  });
});
