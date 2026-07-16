import html from 'solid-js/html';
import { createSignal, createEffect, createResource, For, Show } from 'solid-js';
import { useTheme, useScmVersion, useCurrentFile } from '/lib/solid-hooks.js';
import { exposeAPI } from '/lib/comlink-bridge.js';
import { getJSON, sendJSON, debounce } from '/lib/api.js';
import { Header, IconButton, Empty } from '/lib/ui/index.js';

const ICONS = {
  md: '\u{1F4DD}', js: '\u{1F7E8}', ts: '\u{1F7E6}', json: '\u{1F4CB}',
  html: '\u{1F310}', css: '\u{1F3A8}', py: '\u{1F40D}', rs: '\u{1F980}',
  sh: '\u{1F4BB}', sql: '\u{1F5C4}', txt: '\u{1F4C3}', yaml: '\u2699',
};
const getIcon = (name) => {
  const ext = name.includes('.') ? name.split('.').pop().toLowerCase() : '';
  return ICONS[ext] || '\u{1F4C4}';
};

// ====== lazy children 缓存 + 飞行中请求去重 ======
const childrenCache = {};
const inFlight = new Map();   // path → Promise
const [_childrenTick, setChildrenTick] = createSignal(0);  // 触发 visibleNodes 重算

async function loadChildren(p) {
  if (childrenCache[p]) return childrenCache[p];
  if (inFlight.has(p)) return inFlight.get(p);
  const promise = getJSON('/api/tree?path=' + encodeURIComponent(p)).then(ns => {
    inFlight.delete(p);
    if (!ns.error) {
      childrenCache[p] = ns;
      setChildrenTick(t => t + 1);  // 通知 UI
    }
    return ns;
  }).catch(e => {
    inFlight.delete(p);
    console.error(e);
    return [];
  });
  inFlight.set(p, promise);
  return promise;
}

function clearChildrenCache() {
  for (const k of Object.keys(childrenCache)) delete childrenCache[k];
  setChildrenTick(t => t + 1);
}

// ====== persist expandedDirs ======
const persistExpandedDirs = debounce((dirs) => {
  sendJSON('/api/workspace/fileTree', 'PATCH', { expandedDirs: dirs }).catch(e => console.error(e));
}, 500);

export function FileTree() {
  const [theme] = useTheme();
  const [scmVersion] = useScmVersion();
  const [currentFile, setCurrentFile] = useCurrentFile();

  const [expandedDirs, setExpandedDirs] = createSignal(new Set());

  // 从后端恢复展开目录列表
  createResource(async () => {
    try {
      const saved = await getJSON('/api/workspace/fileTree');
      if (saved?.expandedDirs?.length) {
        setExpandedDirs(new Set(saved.expandedDirs));
      }
    } catch (e) { /* ignore */ }
    return null;
  });

  // 文件树数据：scmVersion 变更时自动重取
  const [treeData, { refetch }] = createResource(
    () => scmVersion(),
    async () => {
      clearChildrenCache();
      const ns = await getJSON('/api/tree');
      if (ns.error) throw new Error(ns.error);
      // 恢复已展开目录的子节点（不阻塞主列表渲染）
      const dirs = expandedDirs();
      if (dirs.size > 0) {
        Promise.allSettled([...dirs].map(d => loadChildren(d)));
      }
      return ns;
    },
  );

  const nodes = () => treeData() || [];
  const loading = () => treeData.loading;
  const treeError = () => treeData.error;

  const refresh = () => refetch();
  doRefresh = refresh;

  function toggleDir(node) {
    const next = new Set(expandedDirs());
    if (next.has(node.path)) {
      next.delete(node.path);
    } else {
      next.add(node.path);
      if (!childrenCache[node.path] && !node._children) loadChildren(node.path);
    }
    setExpandedDirs(next);
    persistExpandedDirs([...next]);
  }

  function collapseAll() {
    setExpandedDirs(new Set());
    persistExpandedDirs([]);
  }

  const visibleNodes = () => {
    _childrenTick();  // 依赖 tick 以响应懒加载完成
    const result = [];
    const walk = (ns, depth) => {
      for (const n of ns) {
        result.push({ ...n, depth });
        if (n.type === 'directory' && expandedDirs().has(n.path)) {
          walk(childrenCache[n.path] || n._children || [], depth + 1);
        }
      }
    };
    walk(nodes(), 0);
    return result;
  };

  const handleNode = (n) => {
    if (n.type === 'directory') {
      toggleDir(n);
    } else {
      setCurrentFile({ path: n.path, name: n.name });
    }
  };

  return html`
    <${Header} title="\u{1F4C1} \u6587\u4EF6">
      <${IconButton} title="Collapse all" onClick=${collapseAll}>\u25C0\u25B6<//>
      <${IconButton} title="Refresh" onClick=${refresh}>\u27F3<//>
    <//>
    <div class="flex-1 overflow-y-auto py-0.5">
      <${Show} when=${() => !loading()} fallback=${() => html`<${Empty} text="\u52A0\u8F7D\u4E2D..." />`}>
        <${Show} when=${() => !treeError()} fallback=${() => html`<${Empty} text=${() => treeError()?.message || '\u52A0\u8F7D\u5931\u8D25'} />`}>
          <${Show} when=${() => visibleNodes().length > 0}
            fallback=${() => html`<${Empty} text="\u6682\u65E0\u6587\u4EF6" />`}>
            <${For} each=${visibleNodes}>${(node) => html`
              <div class="flex items-center gap-1 px-2 py-[3px] cursor-pointer text-xs whitespace-nowrap hover:bg-bg-hover"
                classList=${() => ({ 'bg-bg-active': currentFile()?.path === node.path })}
                style=${`padding-left:${8 + node.depth * 16}px`}
                onClick=${() => handleNode(node)}>
                <span class="w-[12px] text-[10px] text-text-muted shrink-0">${node.type === 'directory' ? '\u25B6' : ''}</span>
                <span class="shrink-0">${node.type === 'directory' ? '\u{1F4C1}' : getIcon(node.name)}</span>
                <span class="overflow-hidden text-ellipsis">${node.name}</span>
              </div>
            `}<//>
          <//>
        <//>
      <//>
    </div>
  `;
}

// 模块级 refresh stub：组件初始化时替换为实际 refetch
let doRefresh = () => {};
exposeAPI({
  refresh() { return doRefresh(); },
});
