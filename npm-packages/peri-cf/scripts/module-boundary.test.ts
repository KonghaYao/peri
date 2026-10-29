import { describe, expect, test } from 'bun:test';
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { dirname, relative, resolve, sep } from 'node:path';

/**
 * 模块边界回归：除入口与内部实现目录外，文件夹是模块，`index.ts` 是唯一公共出口。
 * 跨模块必须走出口；`frontend/remote/` 承载 DTO、投影与客户端门面；
 * `frontend/` 与 `server/` 互不引用。
 *
 * 出口例外（不在 `moduleRoots` 中，因此不要求 index.ts）：
 * - `frontend/app/`：浏览器入口目录，`main.tsx` 直接启动，不做 barrel；
 * - `frontend/chat/timeline/rows/`：timeline 的内部行组件；
 * - `frontend/shims/`：Vite alias 的构建替身，不是可导入模块；
 * - `frontend/test/`：测试工具目录，不参与生产边界规则。
 *
 * 生产代码依赖方向（从稳定到易变）：
 * - `frontend/lib` / `frontend/hooks` / `frontend/components/assistant-ui` 不依赖 feature；
 * - `frontend/remote` 不依赖任何其他 frontend 模块，也不依赖 React/UI 包；
 * - `frontend/chat/runtime` 不依赖 components / timeline / composer，也不引用 chat 出口；
 * - `frontend/chat/timeline` 与 `frontend/chat/composer` 互不依赖，也不依赖 components；
 * - `frontend/chat/components` 只经 runtime 使用 remote；
 * - `frontend/chat/openui` 只经 `peri-openui` 包入口取库能力，不深层引用 `src/`；
 * - `server/api` 不依赖 registry/compose/index；
 * - `scripts` 不依赖 frontend/server。
 *
 * 生产依赖图不得有环；模块内文件不得导入自己的 index。
 * 测试文件与 `frontend/test` 不参与方向与环规则（测试可构造真实注册表）。
 */
const packageRoot = resolve(import.meta.dir, '..');
const sourceRoots = ['frontend', 'server', 'scripts'];
const skippedDirectories = new Set(['node_modules', 'dist', '.cache', '.wrangler', 'logs']);

const moduleRoots = [
  'server', 'server/api',
  'frontend/chat', 'frontend/chat/runtime', 'frontend/chat/components',
  'frontend/chat/timeline', 'frontend/chat/composer',
  'frontend/chat/openui',
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
  if (specifier.startsWith('@/')) base = resolve(packageRoot, 'frontend', specifier.slice(2));
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

const modulePath = (path: string): string => path.split('/').join(sep);
const frontendTestRoot = modulePath('frontend/test');

function isProduction(path: string): boolean {
  return !/\.test\.(ts|tsx)$/.test(path) && !within(frontendTestRoot, path);
}

const production = new Set(
  files.map((file) => relative(packageRoot, file)).filter(isProduction),
);

interface ProductionImport {
  importer: string;
  specifier: string;
  target: string | undefined;
}

const productionImports: ProductionImport[] = files.flatMap((file) => {
  const importer = relative(packageRoot, file);
  if (!production.has(importer)) return [];
  return importsOf(withoutComments(readFileSync(file, 'utf8')))
    .filter((specifier) => !specifier.includes('?') && !/\.(css|wasm|js)$/.test(specifier))
    .map((specifier) => ({ importer, specifier, target: resolveImport(specifier, importer) }));
});

const inModule = (module: string, path: string): boolean => within(modulePath(module), path);

const featureModules = ['frontend/remote', 'frontend/chat', 'frontend/settings', 'frontend/app'];

/** 只统计生产文件中“从 importer 到已解析 target”的违规边。 */
function directionViolations(
  importerIn: (importer: string) => boolean,
  targetIn: (target: string) => boolean,
): string[] {
  return productionImports
    .filter(({ importer, target }) => target !== undefined
      && importerIn(importer) && targetIn(target))
    .map(({ importer, specifier }) => `${importer} → ${specifier}`);
}

/** Tarjan：返回所有大小 > 1 或带自环的强连通分量。 */
function cyclicComponents(edges: ReadonlyMap<string, readonly string[]>): string[][] {
  let nextIndex = 0;
  const indices = new Map<string, number>();
  const lowLinks = new Map<string, number>();
  const stack: string[] = [];
  const onStack = new Set<string>();
  const components: string[][] = [];

  const visit = (node: string): void => {
    indices.set(node, nextIndex);
    lowLinks.set(node, nextIndex);
    nextIndex += 1;
    stack.push(node);
    onStack.add(node);
    for (const successor of edges.get(node) ?? []) {
      const known = indices.get(successor);
      if (known === undefined) {
        visit(successor);
        lowLinks.set(node, Math.min(lowLinks.get(node)!, lowLinks.get(successor)!));
      } else if (onStack.has(successor)) {
        lowLinks.set(node, Math.min(lowLinks.get(node)!, known));
      }
    }
    if (lowLinks.get(node) !== indices.get(node)) return;
    const component: string[] = [];
    for (;;) {
      const member = stack.pop()!;
      onStack.delete(member);
      component.push(member);
      if (member === node) break;
    }
    if (component.length > 1 || (edges.get(node) ?? []).includes(node)) {
      components.push(component.sort());
    }
  };

  for (const node of edges.keys()) if (!indices.has(node)) visit(node);
  return components;
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

describe('production dependency direction', () => {
  test('frontend/remote does not import any other frontend module', () => {
    const violations = directionViolations(
      (importer) => inModule('frontend/remote', importer),
      (target) => inModule('frontend', target) && !inModule('frontend/remote', target),
    );
    expect(violations).toEqual([]);
  });

  test('frontend/remote does not import React or UI packages', () => {
    const packages = productionImports
      .filter(({ importer, target }) => target === undefined && inModule('frontend/remote', importer))
      .map(({ specifier }) => specifier)
      .filter((specifier) => /^react(\/|$)/.test(specifier)
        || /^react-dom(\/|$)/.test(specifier)
        || /^@tanstack\//.test(specifier)
        || /^lucide-react(\/|$)/.test(specifier)
        || /^@assistant-ui\//.test(specifier));
    expect(packages).toEqual([]);
  });

  test('leaf frontend modules do not import any feature', () => {
    const leaves = ['frontend/lib', 'frontend/hooks', 'frontend/components/assistant-ui'];
    const violations = directionViolations(
      (importer) => leaves.some((leaf) => inModule(leaf, importer)),
      (target) => featureModules.some((feature) => inModule(feature, target)),
    );
    expect(violations).toEqual([]);
  });

  test('chat/runtime does not import components, timeline or composer', () => {
    const violations = directionViolations(
      (importer) => inModule('frontend/chat/runtime', importer),
      (target) => ['frontend/chat/components', 'frontend/chat/timeline', 'frontend/chat/composer']
        .some((view) => inModule(view, target))
        || target === modulePath('frontend/chat/index.ts'),
    );
    expect(violations).toEqual([]);
  });

  test('timeline and composer do not depend on each other or on components', () => {
    const forbiddenFor = (sibling: string): string[] => directionViolations(
      (importer) => inModule(sibling, importer),
      (target) => inModule('frontend/chat/components', target)
        || ['frontend/chat/timeline', 'frontend/chat/composer']
          .filter((view) => view !== sibling)
          .some((view) => inModule(view, target)),
    );
    expect([...forbiddenFor('frontend/chat/timeline'),
      ...forbiddenFor('frontend/chat/composer')]).toEqual([]);
  });

  test('chat/openui does not import timeline, components, runtime or composer', () => {
    const violations = directionViolations(
      (importer) => inModule('frontend/chat/openui', importer),
      (target) => ['frontend/chat/timeline', 'frontend/chat/components', 'frontend/chat/runtime', 'frontend/chat/composer']
        .some((view) => inModule(view, target)),
    );
    expect(violations).toEqual([]);
  });

  test('chat/openui takes library capabilities only from the peri-openui entrypoints', () => {
    const deepLibraryImport = /^peri-openui\/src(\/|$)/;
    const violations = productionImports
      .filter(({ importer, specifier }) => inModule('frontend/chat/openui', importer)
        && deepLibraryImport.test(specifier))
      .map(({ importer, specifier }) => `${importer} → ${specifier}`);
    expect(violations).toEqual([]);
  });

  test('chat/components reaches remote only through chat/runtime', () => {
    const violations = directionViolations(
      (importer) => inModule('frontend/chat/components', importer),
      (target) => inModule('frontend/remote', target),
    );
    expect(violations).toEqual([]);
  });

  test('server/api does not import registry, compose or index', () => {
    const orchestration = new Set([
      modulePath('server/registry.ts'),
      modulePath('server/compose.ts'),
      modulePath('server/index.ts'),
    ]);
    const violations = directionViolations(
      (importer) => inModule('server/api', importer),
      (target) => orchestration.has(target),
    );
    expect(violations).toEqual([]);
  });

  test('scripts do not import frontend or server code', () => {
    const violations = directionViolations(
      (importer) => inModule('scripts', importer),
      (target) => inModule('frontend', target) || inModule('server', target),
    );
    expect(violations).toEqual([]);
  });

  test('module files do not import their own entry', () => {
    const violations: string[] = [];
    for (const { importer, specifier, target } of productionImports) {
      if (target === undefined || target === importer) continue;
      const containing = moduleRoots.filter((root) => within(root, importer));
      const own = containing.reduce<string | undefined>(
        (deepest, root) => (deepest === undefined || root.length >= deepest.length ? root : deepest),
        undefined,
      );
      if (!own) continue;
      const entries = [`${own}${sep}index.ts`, `${own}${sep}index.tsx`];
      if (entries.includes(target)) violations.push(`${importer} → ${specifier}`);
    }
    expect(violations).toEqual([]);
  });

  test('production dependency graph is acyclic', () => {
    const edges = new Map<string, string[]>();
    for (const path of production) edges.set(path, []);
    for (const { importer, target } of productionImports) {
      if (target !== undefined && production.has(target)) edges.get(importer)!.push(target);
    }
    expect(cyclicComponents(edges)).toEqual([]);
  });
});

describe('legacy chat transport removal', () => {
  test('production frontend sources contain no SSE, AgentManager, or old command/event routes', () => {
    const legacy = [/\bEventSource\b/, /\bAgentManager\b/, /\/commands\b/, /\/events\b/,
      /\bRemoteChatRuntime\b/, /\bChatCommand\b/, /\bSyncState\b/];
    const violations: string[] = [];
    for (const file of files) {
      const path = relative(packageRoot, file);
      if (!/^frontend[\\/]/.test(path) || /\.test\.(ts|tsx)$/.test(path)) continue;
      const code = withoutComments(readFileSync(file, 'utf8'));
      for (const pattern of legacy) if (pattern.test(code)) violations.push(`${path} → ${pattern}`);
    }
    expect(violations).toEqual([]);
  });
});
