import html from 'solid-js/html';
import { createSignal, createResource, For, Show } from 'solid-js';
import { useTheme, useScmVersion, useCurrentFile } from '/lib/solid-hooks.js';
import { exposeAPI } from '/lib/comlink-bridge.js';
import { getJSON, sendJSON, debounce } from '/lib/api.js';
import { Header, IconButton, Empty, Icon } from '/lib/ui/index.js';

// 扩展名 → [图标, 颜色 class]
const ICON_MAP = {
  md: ['book', 'text-info'],
  js: ['code', 'text-warning'], jsx: ['code', 'text-warning'], mjs: ['code', 'text-warning'],
  ts: ['code', 'text-info'], tsx: ['code', 'text-info'],
  json: ['hash', 'text-warning'],
  html: ['globe', 'text-error'], htm: ['globe', 'text-error'],
  css: ['code', 'text-info'], scss: ['code', 'text-info'], less: ['code', 'text-info'],
  py: ['code', 'text-success'],
  rs: ['code', 'text-warning'],
  go: ['code', 'text-info'],
  sh: ['terminal', 'text-success'], bash: ['terminal', 'text-success'], zsh: ['terminal', 'text-success'],
  sql: ['database', 'text-info'],
  yml: ['hash', 'text-text-muted'], yaml: ['hash', 'text-text-muted'], toml: ['hash', 'text-text-muted'],
  txt: ['file-text', 'text-text-muted'],
  png: ['image', 'text-text-muted'], jpg: ['image', 'text-text-muted'], jpeg: ['image', 'text-text-muted'], gif: ['image', 'text-text-muted'], svg: ['image', 'text-text-muted'],
};
const getFileIcon = (name) => {
  const ext = name.includes('.') ? name.split('.').pop().toLowerCase() : '';
  return ICON_MAP[ext] || ['file', 'text-text-muted'];
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
    <${Header} title="文件">
      <${IconButton} title="全部折叠" onClick=${collapseAll}><${Icon} name="collapse" class="w-3.5 h-3.5" /><//>
      <${IconButton} title="刷新" onClick=${refresh}><${Icon} name="refresh" class="w-3.5 h-3.5" /><//>
    <//>
    <div class="flex-1 overflow-y-auto overflow-x-hidden py-1">
      <${Show} when=${() => !loading()} fallback=${() => html`<${Empty} text="加载中..." />`}>
        <${Show} when=${() => !treeError()} fallback=${() => html`<${Empty} icon="alert" text=${() => treeError()?.message || '加载失败'} />`}>
          <${Show} when=${() => visibleNodes().length > 0}
            fallback=${() => html`<${Empty} icon="folder" text="暂无文件" />`}>
            <${For} each=${visibleNodes}>${(node) => {
              const isDir = node.type === 'directory';
              const [iconName, iconCls] = isDir ? ['folder', 'text-accent'] : getFileIcon(node.name);
              return html`
              <div
                class=${() => [
                  'flex items-center gap-1.5 mx-1 px-1.5 h-[26px] rounded-md cursor-pointer text-[12px] whitespace-nowrap transition-colors duration-100',
                  currentFile()?.path === node.path ? 'bg-bg-active text-text' : 'text-text-secondary hover:bg-bg-hover hover:text-text',
                ].join(' ')}
                style=${`padding-left:${6 + node.depth * 14}px`}
                onClick=${() => handleNode(node)}>
                <span class=${'w-3 h-3 inline-flex items-center justify-center shrink-0 text-text-muted transition-transform duration-150 ' + (isDir && expandedDirs().has(node.path) ? 'rotate-90' : '')}>
                  ${isDir ? html`<${Icon} name="chevron-right" class="w-3 h-3" />` : null}
                </span>
                <${Icon} name=${iconName} class=${'w-3.5 h-3.5 shrink-0 ' + iconCls} />
                <span class="overflow-hidden text-ellipsis">${node.name}</span>
              </div>`;
            }}<//>
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
