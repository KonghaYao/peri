# site-project 架构说明

> 本地文档预览 + Git 工作台 + 终端 + HTTP 测试器，多 iframe 协同开发工具。
> 服务端口 **23566**，前端无构建，后端 `Hono + tsx`。

---

## 1. 项目定位

一个跑在本地的「文档浏览器 / Git GUI / 终端 / HTTP 测试器」一体化工具。所有功能拆分到独立 iframe 中，父 shell 通过共享状态和 Comlink RPC 协调它们。

- **后端**：单进程 `Hono`，同时承担 REST API、静态资源、WebSocket（终端 PTY）
- **前端**：1 个父 shell + 6 个 iframe 子页面，纯 ESM，**无打包步骤**
- **依赖加载**：CDN（unpkg）+ `<script type="importmap">`，没有 node_modules 体积负担

---

## 2. 目录结构

```
site-project/
├── src/                      # 后端（Hono + tsx）
│   ├── server.ts             # 入口（端口 23566）
│   ├── terminal.ts           # WebSocket + node-pty
│   ├── lib/workspace.ts      # workspace.json 持久化
│   ├── services/             # 业务服务（File/Scm/Graph/Getman/Workspace）
│   └── routes/               # REST 路由（files/scm/graph/getman/workspace）
│
├── tests/                    # 后端测试（node:test + tsx）
│   ├── helpers.ts            # makeGitRepo / writeTestFile / makeTempDir / execGit
│   ├── scm-service.test.ts
│   ├── graph-service.test.ts
│   ├── getman-service.test.ts
│   └── workspace-service.test.ts
│
├── public/                   # 前端（直接静态服务）
│   ├── parent.html           # 父 shell
│   ├── pages/                # 6 个 iframe 子页面（仅含 head + 模块导入）
│   │   ├── file-tree.html    # 常驻：文件树
│   │   ├── preview.html      # 常驻：文件预览
│   │   ├── scm.html          # 常驻：源代码管理
│   │   ├── terminal.html     # 常驻：xterm 终端（可独立访问）
│   │   ├── graph.html        # lazy：Git graph（按需加载）
│   │   └── getman.html       # lazy：HTTP 测试器（按需加载）
│   │
│   ├── lib/                  # 公共代码层
│   │   ├── shared-state-core.js     # 框架无关 store（get/set/subscribe/hydrate）
│   │   ├── comlink-bridge.js        # Comlink wrap/expose 封装
│   │   ├── api.js                   # fetch 工具 + debounce
│   │   ├── env.js                   # 独立运行支持（buildWsUrl/isStandalone/...）
│   │   ├── solid-hooks.js           # useParentState/useTheme/useScmVersion/useParentMethod
│   │   ├── solid-components.js      # <SharedState> 包装组件
│   │   ├── components/
│   │   │   └── HostedIframe.js      # 父侧 iframe 控制组件
│   │   │
│   │   ├── ui/                 # 通用 UI 组件库（Tailwind 工具类，无业务）
│   │   │   ├── index.js            # barrel：Header / Button / IconButton / Badge / Empty / Tabs / KVInput
│   │   │   ├── Header.js
│   │   │   ├── Button.js
│   │   │   ├── Badge.js
│   │   │   ├── Empty.js
│   │   │   ├── Tabs.js
│   │   │   └── KVInput.js
│   │   │
│   │   ├── file-tree/
│   │   │   └── FileTree.js         # 文件树业务组件
│   │   ├── scm/
│   │   │   ├── ScmPanel.js          # 提交 / 暂存区业务组件
│   │   │   └── ScmGraph.js          # Git graph 业务组件
│   │   ├── terminal/
│   │   │   ├── ws.js                # WebSocket 管理器（连接 / 重连 / session 持久化）
│   │   │   └── TerminalTabs.js      # 多 tab 终端业务组件
│   │   ├── preview/
│   │   │   └── Preview.js           # 文件预览业务组件
│   │   └── getman/
│   │       └── Getman.js            # HTTP 测试器业务组件
│   │
│   └── css/
│       └── base.css          # 基础 reset（14 行）
│
├── package.json              # 类型："module"，script: dev → tsx watch src/server.ts, test → tsx --test
├── tsconfig.json
└── workspace.json            # 运行时生成：UI 状态持久化
```

---

## 3. JS 架构方式

### 3.1 不构建（unbundle）

**没有 webpack/vite/rollup/esbuild**。所有 JS 以 ESM 源码直接发送到浏览器，靠浏览器原生 `<script type="module">` 加载。

**理由**：本项目体量小（前端 lib 总和 < 400 行），构建步骤只会增加调试成本。改 lib 文件后刷新页面即生效，无编译延迟。

### 3.2 依赖加载：importmap + unpkg

第三方库通过 unpkg CDN 加载，每个 HTML 在 `<head>` 内统一声明 importmap：

```html
<script type="importmap">
{
  "imports": {
    "solid-js":     "https://unpkg.com/solid-js@1.9.3/dist/solid.js",
    "solid-js/web": "https://unpkg.com/solid-js@1.9.3/web/dist/web.js",
    "solid-js/html":"https://unpkg.com/solid-js@1.9.3/html/dist/html.js",
    "comlink":      "https://unpkg.com/comlink@4.4.1/dist/esm/comlink.mjs"
  }
}
</script>
```

- **版本必须锁定**：禁止用 `solid-js@latest` 或省略版本号
- **importmap 必须在第一个 `<script type="module">` 之前**
- **importmap 一个文档只能有一个**（嵌套 iframe 各自有自己的 importmap，互不影响）
- 全局 `<script src="...">`（如 xterm、diff2html、marked）不属于 ESM，仍走 `<script src>`，不进 importmap

**import 写法**：用裸模块名，不用路径：
```js
import { render } from 'solid-js/web';
import html from 'solid-js/html';
import { createSignal } from 'solid-js';
import { expose, wrap, windowEndpoint } from 'comlink';
```

### 3.3 框架选择：Solid + htm

- **SolidJS**：响应式原语（`createSignal`/`createEffect`/`onMount`/`onCleanup`）+ 内置组件（`<For>`/`<Show>`）
- **htm**：模板字面量写 JSX-like 语法，**没有 JSX 编译步骤**

```js
import html from 'solid-js/html';

function App() {
  const [count, setCount] = createSignal(0);
  return html`
    <div class="card">
      <button onClick=${() => setCount(c => c + 1)}>+1</button>
      <span>${count}</span>            <!-- 直接传 accessor，htm 自动追踪 -->
      <span>${() => count() * 2}</span> <!-- 显式箭头函数：动态计算 -->
    </div>
  `;
}
```

### 3.4 Solid + htm 响应式规则（**重要**）

这是踩坑最频繁的地方。**违反以下任何一条都会导致 UI 不更新或 DOM 错位**。

#### 规则 1：传 accessor，不传 value

```js
// ✅ 正确
<span>${count}</span>           // accessor 函数
<span>${() => count() * 2}</span> // 包装箭头函数

// ❌ 错误
<span>${count()}</span>         // 调用了，变成静态值
```

#### 规则 2：动态属性必须用箭头函数包装

```js
// ✅ 动态 class
<div class=${() => active() ? 'on' : 'off'}></div>

// ✅ 动态 classList（必须箭头函数）
<div classList=${() => ({ active: active(), disabled: !enabled() })}></div>

// ❌ 错误：对象字面量不会被追踪
<div classList=${{ active: active() }}></div>

// ✅ 动态 style
<div style=${() => `width:${w()}px`}></div>
```

#### 规则 3：`createEffect` 只追踪 Solid signal，**不追踪 store.get()**

```js
// ❌ 错误：store.get 不是 signal，effect 不会重新执行
createEffect(() => {
  document.documentElement.dataset.theme = store.get('theme');
});

// ✅ 正确：用 store.subscribe
store.subscribe('theme', (t) => {
  document.documentElement.dataset.theme = t;
});
```

#### 规则 4：`<For>` 按引用 keyed，替换对象会重建 DOM

`setXxx(arr => arr.map(...))` 中如果返回**新对象引用**，Solid `<For>` 会销毁旧 DOM 节点重建新节点。

```js
// ❌ 危险：如果 DOM 上挂了 xterm canvas 等重资源，会断开
setTabs(ts => ts.map(x => x.id === id ? { ...x, status: 'open' } : x));

// ✅ 正确：把易变状态（status）抽到独立 signal，tabs 数组只在 push/filter 时变
const [tabs, setTabs] = createSignal([]);            // 稳定结构
const [statuses, setStatuses] = createSignal({});    // { [id]: 'open' | 'closed' }
setStatuses(s => ({ ...s, [id]: 'open' }));
```

#### 规则 5：`onCleanup` 必须配对

任何 `setInterval` / `setTimeout` / `addEventListener` / `WebSocket` 都要在 `onCleanup` 中清理：

```js
onMount(() => {
  const id = setInterval(refresh, 5000);
  onCleanup(() => clearInterval(id));
});
```

### 3.5 公共代码层（`public/lib/`）

#### `shared-state-core.js` — 框架无关 store

跨 iframe 共享状态的**唯一真相源**。提供：

```js
createSharedStore(initial, onPersist?)
  → { get(key), set(key, val), subscribe(key, cb), hydrate(snapshot), getAll() }
```

- `set(key, val)`：写入并通知所有订阅者；触发 `onPersist`（500ms debounce）
- `subscribe(key, cb)`：订阅单 key 变化；返回 unsubscribe 函数
- `onPersist(keys, state)`：持久化回调（父 shell 中实现，PATCH 到后端）

**这个 store 不是 Solid signal**。所以 `store.get()` 在 Solid 模板中只在初始渲染时计算一次。要响应式更新，要么用 `subscribe` + Solid signal 桥接，要么通过 `solid-hooks` 中的 `useParentState` 等 hook（内部已桥接）。

#### `comlink-bridge.js` — Comlink 封装

```js
getParent()         // 子页面获取父 API（返回 Promise<parentApi>）
exposeAPI(api)      // 子页面把自己的 API 暴露给父
exposeToChild(...)  // 父侧 expose store 给子
wrapChild(...)      // 父侧 wrap 子 API
```

**唯一通道**：禁止业务代码直读 `window.parent` 或用 `postMessage`。所有跨 iframe 调用必须经过 Comlink。

#### `api.js` — fetch 工具

```js
getJSON(url)                        // GET JSON
sendJSON(url, method, body)         // POST/PATCH/DELETE JSON
debounce(fn, delay)                 // 防抖
```

#### `solid-hooks.js` — Solid 适配层

把 store 接入 Solid 响应式系统：

```js
const [val, setVal] = useParentState('key');  // 通用
const [currentFile, setCurrentFile] = useCurrentFile();   // 语义糖
const [theme] = useTheme();                 // 自动同步 documentElement.dataset.theme
const [, setScmVersion] = useScmVersion();  // 瞬时事件计数器
const openGraph = useParentMethod('openGraph');  // 调用父方法
```

每个 hook 内部：
- `onMount` 时通过 Comlink 获取 parentApi 并 `store.subscribe`
- 收到更新时调用 Solid `setSignal`，触发组件重渲染
- `onCleanup` 时取消订阅
- **5 秒超时**：parent 不可达时返回默认值并告警

#### `components/HostedIframe.js` — 父侧 iframe 控制组件

父 shell 中创建 iframe 的唯一入口。提供 8 个 API：

```js
api.open()      // 显示（lazy iframe 首次会触发 setSrc）
api.close()     // 隐藏（display:none，不卸载）
api.reload()    // 重新加载
api.call(method, ...args)  // 通过 Comlink 调用子方法
api.child       // 子 API 代理
api.loaded()    // 是否加载完成
api.status      // 'loading' | 'ready' | 'error' | 'timeout'
api.error       // 错误信息
```

属性：
```html
<${HostedIframe}
  src="/pages/file-tree.html"
  name="file-tree"           <!-- 必须，用于 window.name -->
  store=${store}              <!-- 自动 expose 给子 -->
  lazy=${false}               <!-- true：open() 才 setSrc；false：立即加载 -->
  ref=${api => fileTreeHost = api}
  onReady=${api => ...}       <!-- 可选 -->
/>
```

#### `env.js` — 独立运行基础设施

让 iframe 子页面也能脱离父 shell 直接访问（独立 HTML 页面）。提供：

```js
isStandalone()        // window.parent === window → 当前是否独立访问
buildWsUrl(sessionId?) // 基于自身 location.host 计算 ws/wss URL（terminal 用）
httpUrl(path)         // 路径补全为绝对 HTTP URL
parentMethod(name, fallback)  // 父方法调用，独立访问走 fallback
requireParent()       // 独立访问时返回 null 并告警
```

设计动机：terminal.html 被设计为可独立访问的「单页终端」，不再依赖父端 `getWsUrl`。

#### `ui/` — 通用 UI 组件库（无业务）

业务无关的纯展示组件集合。统一 Tailwind 工具类用法。

| 组件 | 用途 |
|------|------|
| `Header` | 区块头部（标题 + 操作按钮槽） |
| `Button` / `IconButton` | 主按钮 / 图标按钮 |
| `Badge` | 状态徽章（modified/added/deleted/unknown + 简写 m/a/d/u） |
| `Empty` | 空态占位 |
| `Tabs` | 顶部 tab 切换 |
| `KVInput` | Key-Value 表格输入（headers/params 编辑用） |

所有组件都不订阅 store，是纯函数式组件。统一从 `/lib/ui/index.js` barrel 导入。

#### 业务组件（`file-tree/` / `scm/` / `terminal/` / `preview/` / `getman/`）

每个 iframe 页面对应一个业务组件，封装该页面所有 Solid 逻辑、API 调用、状态订阅。HTML 页面只保留：
1. `<head>`：CDN、`@theme` 块、importmap、`base.css` 链接
2. `<body>`：根 `<div id="app">` + 6 行模块脚本（`render(() => html\`<${Component} />\`, app)`）

业务组件可直接调用 `useTheme()` / `useScmVersion()` / `useCurrentFile()` / `useParentMethod(name)` 等 hook，无需手写 `getParent()` 模板代码。

### 3.6 状态与持久化约定

#### Shared state key（camelCase 单字段）

| key | 类型 | 含义 | 持久化 |
|-----|------|------|--------|
| `currentFile` | `{ path, anchor } \| null` | 当前激活文件 | ✅（存到 `/api/workspace/ui`）|
| `theme` | `'dark' \| 'light'` | 主题 | ✅ |
| `sidebarWidth` | `number` | 侧边栏宽度（px） | ✅ |
| `scmFlex` / `terminalFlex` | `number` | 分隔比例 | ✅ |
| `scmVersion` | `number` | 瞬时事件计数器 | ❌ **不持久化** |

#### 瞬时事件 = 计数器

不能用 `emit('event')`，因为没有事件总线。约定：子 iframe 完成某操作后 `setScmVersion(v => v + 1)`，订阅者收到通知后自己重新 fetch。

典型用法：scm.html commit 后 `setScmVersion(v => v + 1)` → file-tree.html 监听到变化 → 重新拉 `/api/scm/status` 刷新 dirty 标记。

#### 持久化分桶（`/api/workspace/<key>`）

不要把所有状态塞同一个 key 下。按"主题/模块"分桶：

| key | 内容 |
|-----|------|
| `ui` | currentFile / theme / sidebarWidth / scmFlex / terminalFlex |
| `fileTree` | expandedDirs / activeFilePath |

PATCH `/api/workspace/ui` body 是字段级合并（不是整体替换）。

---

## 4. CSS 编写方式（Tailwind v4 Play CDN）

### 4.1 总体策略：无构建 + 限制性使用 Tailwind

和 JS 一样，CSS 也**不构建**。通过 Tailwind v4 浏览器编译器（Play CDN）实时编译工具类。

**「限制性使用」的含义**：

1. **Tailwind 是工具，不是宗教**。布局、间距、颜色、排版等基础样式优先用工具类
2. **复杂选择器（`:hover` / `::before` / 嵌套）用 `<style type="text/tailwindcss">`**，不强求把所有样式塞进 class 列表
3. **动态计算值（拖拽宽度、主题相关像素值）用 inline `style`**，不用 Tailwind 任意值 `[xxx]`
4. **第三方组件覆盖（xterm/Diff2Html）保持普通 CSS 文件**

### 4.2 引入 Play CDN

每个 HTML 的 `<head>` 内必须加载 Tailwind 浏览器编译器（在 importmap 之前或之后均可）：

```html
<script src="https://cdn.jsdelivr.net/npm/@tailwindcss/browser@4"></script>
```

- **版本必须锁定**：禁止 `@latest` 或省略 `@4`
- **生产环境不推荐 Play CDN**，但本地工具可接受实时编译开销（首次编译 < 100ms）

### 4.3 主题 token 注册（`@theme` 块）

**所有颜色必须通过 `@theme` 注册为 `--color-*` 命名空间**。Tailwind v4 会扫描这些变量自动生成 `bg-*` / `text-*` / `border-*` / `fill-*` 等工具类。

每个 HTML 在 `<head>` 内联一份 `<style type="text/tailwindcss">`（每个 iframe 独立，必须各自声明）：

```html
<style type="text/tailwindcss">
  @theme {
    --color-bg: #0d1117;
    --color-bg-secondary: #161b22;
    --color-bg-hover: #1c2128;
    --color-bg-active: rgba(31,111,235,0.12);

    --color-text: #e6edf3;
    --color-text-secondary: #8b949e;
    --color-text-muted: #6e7681;

    --color-border: #30363d;
    --color-accent: #58a6ff;
    --color-accent-hover: #79c0ff;
    --color-success: #3fb950;
    --color-warning: #d29922;
    --color-error: #f85149;

    --font-mono: "JetBrains Mono", "Fira Code", Menlo, monospace;
  }

  [data-theme="light"] {
    --color-bg: #ffffff;
    --color-bg-secondary: #f6f8fa;
    --color-bg-hover: #e8eaed;
    --color-bg-active: rgba(9,105,218,0.12);
    --color-text: #1f2328;
    --color-text-secondary: #57606a;
    --color-text-muted: #656d76;
    --color-border: #d0d7de;
    --color-accent: #0969da;
    --color-accent-hover: #0550ae;
    --color-success: #1a7f37;
    --color-warning: #9a6700;
    --color-error: #cf222e;
  }
</style>
```

**生成的工具类速查**：

| Token | 生成的工具类 |
|-------|-------------|
| `--color-bg` | `bg-bg` / `text-bg` / `border-bg` |
| `--color-bg-secondary` | `bg-bg-secondary` |
| `--color-text` | `text-text` / `bg-text` |
| `--color-text-muted` | `text-text-muted` |
| `--color-border` | `border-border` / `bg-border` |
| `--color-accent` | `bg-accent` / `text-accent` / `border-accent` |
| `--color-success` / `--color-error` | `text-success` / `text-error` / `bg-success` |
| `--font-mono` | `font-mono` |

> 命名注意：`bg-bg` 看起来啰嗦，但这是「以语义命名 token」的代价。`bg-bg` = "用背景色 token 作 background-color"，`text-text` = "用文本色 token 作 color"。读几次就习惯了。

主题切换：父 shell 设置 `document.documentElement.dataset.theme = 'light' | 'dark'`，**所有 iframe 各自同步自己的 data-theme**（通过 `useTheme()` hook）。由于 `[data-theme="light"]` 在 `@theme` 之外用普通 CSS 覆盖 `--color-*`，工具类会自动跟随。

### 4.4 编写规范（限制性 Tailwind）

#### 优先级

| 场景 | 用什么 | 例子 |
|------|--------|------|
| 布局 / 间距 / 字号 | Tailwind 工具类 | `class="flex items-center gap-2 px-3 py-2"` |
| 颜色（静态） | Tailwind 工具类 | `class="bg-bg-secondary text-text-muted border-border"` |
| 颜色（动态切换） | Tailwind 工具类 + `data-theme` | 同上，自动跟随主题 |
| 伪类（`:hover` / `:focus`） | Tailwind 变体工具类 | `class="hover:bg-bg-hover focus:border-accent"` |
| 动态 classList | SolidJS 条件表达式 | `class=${() => active() ? 'bg-bg-active' : ''}` |
| 兄弟选择器 / 嵌套（无法用工具类表达） | 普通 CSS（不用 @apply） | `#sidebar iframe + iframe { border-top: 1px solid var(--color-border); }` |
| 动态计算值 | inline `style` | `style=${() => \`width:${w()}px\`}` |
| 第三方覆盖（xterm/diff2html/marked） | HTML 内 `<style type="text/tailwindcss">` 普通 CSS | `.xterm { height: 100%; }` |

#### 禁止

- **`@apply`**：**完全禁用**，一次都不许用。要么把工具类直接写在 SolidJS 模板的 class 里，要么写普通 CSS
- **硬编码颜色**：`#58a6ff` / `rgb(...)` 直接写在 class 或 style 里（除非 `[data-theme]` 中注册 token）
- **CSS 类堆砌工具类**：`.foo { display: flex; align-items: center; gap: 8px; }` 这种应该直接 `<div class="flex items-center gap-2">`
- **`!important`**：除非覆盖第三方（Diff2Html / xterm）
- **`position: absolute` 撑布局**：除非真的需要（如 `.terminals > div`）
- **媒体查询**：本地工具，固定桌面宽度
- **HTML 内重复 `* {}` / `html, body {}` reset**：已由 `css/base.css` 提供

#### 鼓励

```html
<!-- ✅ 工具类直接写到 SolidJS html`` 模板 -->
<div class="flex items-center gap-2 px-3 py-2 bg-bg-secondary border-b border-border">
  <span class="text-text-muted text-xs">label</span>
  <button class="text-accent hover:text-accent-hover hover:bg-bg-hover">click</button>
</div>

<!-- ✅ 动态 classList 改为条件工具类 -->
<div class=${() => active() ? 'bg-bg-active' : ''}>...</div>

<!-- ✅ 动态值用 inline style -->
<div style=${() => `width:${store.get('sidebarWidth')}px`}></div>

<!-- ✅ 兄弟选择器等无法工具类表达的，用普通 CSS（无 @apply） -->
<style type="text/tailwindcss">
  @theme { /* ... */ }
  [data-theme="light"] { /* ... */ }
  #sidebar iframe + iframe { border-top: 1px solid var(--color-border); }
</style>
```

### 4.5 CSS 文件分层（更新）

| 层级 | 文件 | 用途 |
|------|------|------|
| **全局 reset** | `css/base.css` | box-sizing + html/body 基础（16 行，Tailwind preflight 已接管大部分） |
| **主题 token** | HTML 内 `<style type="text/tailwindcss">@theme` | 颜色 token 注册，**每个 HTML 必须声明** |
| **第三方 CSS** | CDN `<link>` | xterm.css / diff2html.css（按需引入） |
| **页面内工具类组合** | HTML 元素 / SolidJS 模板 `class="..."` | Tailwind 工具类（首选，覆盖布局/间距/颜色/伪类） |
| **页面内自定义类** | HTML 内 `<style type="text/tailwindcss">` | 仅兄弟选择器、第三方覆盖等无法工具类表达的部分（**普通 CSS，禁 @apply**） |

> 模块化 CSS 文件（sidebar.css / statusbar.css / terminal.css 等）已全部删除，所有样式都用 Tailwind 工具类直接写在 SolidJS 模板里。`public/css/` 现在只剩 `base.css` 一个文件。

```html
<!-- 标准页面头 -->
<head>
  <meta charset="UTF-8">
  <script src="https://cdn.jsdelivr.net/npm/@tailwindcss/browser@4"></script>
  <link rel="stylesheet" href="/css/base.css">
  <link rel="stylesheet" href="https://unpkg.com/xterm@5.3.0/css/xterm.css">   <!-- 第三方按需 -->

  <script type="importmap">{ ... }</script>

  <style type="text/tailwindcss">
    @theme { /* 颜色 token，每个 HTML 必须声明 */ }
    [data-theme="light"] { /* 覆盖 */ }
    /* 页面专属自定义类（伪类、嵌套） */
  </style>
</head>
```

### 4.6 主题切换的常见坑

- **xterm 主题**：xterm 不读 CSS 变量，必须通过 `term.options.theme = DARK_THEME | LIGHT_THEME` 显式切换。订阅 theme 后遍历所有 term 实例更新
- **Diff2Html**：自带浅色样式，通过包裹元素 `.diff2html[data-theme]` 控制可读性，或在 HTML 内 `<style>` 中用普通 CSS 覆盖
- **`<input>` / `<textarea>` / `<select>`**：Tailwind preflight 会清空它们的样式，必须显式 `class="bg-bg-secondary text-text border border-border"` 或自定义类补充
- **Tailwind 工具类跟随主题**：`bg-bg` 等会自动随 `--color-bg` 的 `[data-theme]` 覆盖变化，**不需要额外处理**
- **`@theme` 变量是静态编译的**：不要在 `@theme` 块内写 `var(--xxx)` 自引用。运行时切换的值要放到 `@theme` 之外的 `[data-theme]` 中

---

## 5. iframe 通信契约

### 5.1 父 → 子（Comlink wrap）

```js
// parent.html
const childApi = wrapChild(iframeEl, 'child-name');
await childApi.someMethod(args);
```

### 5.2 子 → 父（Comlink expose + getParent）

```js
// 父侧：expose store 到所有子 iframe
exposeToChild(store, iframeEl);

// 子页面：获取父 API
const parentApi = await getParent();
await parentApi.openGraph();
await parentApi.set('theme', 'light');   // 直接操作父 store
// 注意：ws URL 不再走父方法，用 buildWsUrl() 自计算（见 lib/env.js）
```

### 5.3 父 shell 必须暴露的方法清单

`parent.html` 中 `store` 对象上挂载的所有方法：

| 方法 | 签名 | 用途 |
|------|------|------|
| `openGraph()` / `closeGraph()` | | 切换 graph overlay |
| `openGetman()` / `closeGetman()` | | 切换 getman overlay |
| `setStatusText(text)` | | 写底部状态栏 |
| `set(key, val)` / `get(key)` / `subscribe(key, cb)` | | 共享状态读写（来自 createSharedStore） |

> Terminal WebSocket URL 不再走父方法：子页面直接用 `lib/env.js::buildWsUrl()` 基于自身 `location.host` 计算，让 terminal.html 可独立访问。

**子页面调用父方法**：用 `useParentMethod('xxx')` hook，不要直接 `parentApi.xxx()`（hook 处理了 5 秒超时和未连接时的 fallback）。

### 5.4 子页面也可 expose（让父主动调用）

```js
// child.html
exposeAPI({
  reload: () => location.reload(),
  refresh: () => refreshData(),
});
```

```js
// parent.html
graphHost.call('refresh');
```

---

## 6. 开发注意事项

### 6.1 新增 iframe 页面 checklist

1. 在 `public/pages/<name>.html` 创建文件，复制标准头（含 importmap + base.css + `@theme` 块）
2. 在 `public/lib/<name>/` 创建业务组件 `<Name>.js`，把 Solid 逻辑、API 调用、状态订阅全部封装进去
3. HTML 页面只保留 `<div id="app">` + 6 行模块脚本：`render(() => html\`<${Component} />\`, app)`
4. 在 `parent.html` 用 `<HostedIframe>` 挂载，决定 lazy 还是常驻
5. 如果是 lazy：在 store 上加 `openXxx/closeXxx` 方法 + `<HostedIframe lazy=${true}>`
6. 如果是常驻：放在 `#sidebar` 或 `#center-area` 内，正确设置 flex 比例
7. 如果需要持久化：在 `shared-state-core.js` 中加 key，在 `parent.html` 中决定持久化分桶
8. 通用 UI 元素（按钮、徽章、空态、tab）必须从 `/lib/ui/index.js` 复用，不要在业务组件内重写

### 6.2 新增 shared state key checklist

1. 决定 key 名（camelCase，语义清晰，如 `currentFile` / `sidebarWidth`）
2. 在 `parent.html` 的 `createSharedStore` 默认值中声明
3. 在 `solid-hooks.js` 中加语义糖 hook（可选，如 `useCurrentFile`）
4. 决定是否持久化：持久化 → 加到 `onPersist` 过滤白名单；瞬时 → 加到 `keys.filter(k => k !== 'xxx')` 黑名单
5. 子页面用 `useParentState('key')` 订阅

### 6.3 资源清理规则

每个 iframe 都可能被父侧 reload 或卸载，必须清理：

- `setInterval` / `setTimeout` → `onCleanup` 中 `clearXxx`
- `addEventListener` → `onCleanup` 中 `removeEventListener`
- `WebSocket` → `onCleanup` 中 `.close()`
- `ResizeObserver` / `MutationObserver` → `onCleanup` 中 `.disconnect()`
- xterm `Terminal` 实例 → `onCleanup` 中 `.dispose()`

### 6.4 错误处理标准

```js
// fetch 必须包 try-catch，错误时显示空态而不是白屏
try {
  const data = await getJSON('/api/xxx');
  setData(data);
} catch (e) {
  console.error('[xxx] fetch failed:', e);
  setError(e.message || String(e));
}
```

- **不要 swallow catch**：`catch (e) {}` 至少 `console.error` 一下
- **错误状态用独立 signal**：`const [error, setError] = createSignal('')`，不要和 data 混在一起

### 6.5 避免的反模式

| 反模式 | 正确做法 |
|--------|----------|
| 业务代码直读 `window.parent.document` | 通过 Comlink 父 API |
| `localStorage` 跨 iframe 通信 | 通过 shared state store |
| `postMessage` 手写消息 | 通过 Comlink |
| 在 Solid `<For>` 中替换对象引用 | 抽离易变字段到独立 signal |
| 多个 HTML 共享 JS 文件用 `<script src>` | 用 ESM `import` |
| HTML 内联 `<script>`（非 module） | 用 `<script type="module">` |
| 全局变量挂 `window.xxx` | 用模块 export |
| 不加版本号的 CDN URL | 必须锁版本 |

---

## 7. 后端约定

### 7.1 路由组织

```
GET    /api/tree                      # 文件树
GET    /api/file?path=<rel>           # 读文件内容
GET    /api/stat?path=<rel>           # 文件元数据

GET    /api/scm/status                # Git status
POST   /api/scm/stage                 # 暂存 / 取消暂存
POST   /api/scm/discard               # 丢弃改动
POST   /api/scm/commit                # 提交
GET    /api/scm/diff?file=<rel>&staged=<bool>
GET    /api/scm/graph?max=<n>         # commit 列表
GET    /api/scm/commit-diff?hash=<h>  # 某次 commit 的 diff

POST   /api/getman/proxy              # HTTP 代理（注意 SSRF）
POST   /api/getman/parse-curl         # cURL 解析

GET    /api/workspace/<key>           # 读取持久化分桶（不存在返回 404）
PATCH  /api/workspace/<key>           # 字段级合并
DELETE /api/workspace/<key>           # 删除分桶

WS     /ws?cols=<n>&rows=<n>&session=<id>   # 终端 PTY
```

### 7.2 服务层模式

每个领域一个 `services/<name>-service.ts`，提供纯业务方法（不依赖 Hono）。路由层只做参数提取 + 调用 service + JSON 序列化。

### 7.2.1 后端测试（`tests/`）

用 `node:test` + `node:assert/strict` + `tsx` 运行 TypeScript 测试，不引入 vitest/jest。

```
npm test    # → tsx --test tests/**/*.test.ts
```

| 文件 | 覆盖范围 |
|------|----------|
| `helpers.ts` | `makeGitRepo()` / `writeTestFile()` / `makeTempDir()` / `execGit()` 共享工厂 |
| `scm-service.test.ts` | detect / status / stage / commit / discard / diff / branches / 路径穿越校验 |
| `graph-service.test.ts` | graph 拓扑 / HEAD 标记 / remotes / commit-diff / hash 校验 |
| `getman-service.test.ts` | parseCurl 全分支 / proxyRequest 错误路径 |
| `workspace-service.test.ts` | getState / updateState / getKey / setKey / deleteWorkspaceKey |

测试约定：
- **隔离**：每个 case 用 `makeGitRepo()` / `makeTempDir()` 创建临时目录，不污染源码
- **错误路径**：每条核心功能至少 1 条错误路径 assert（如空参数 / 路径穿越 / 非法输入）
- **不依赖网络**：getman proxyRequest 测试只覆盖 URL/method 校验，不实际发请求

### 7.3 WebSocket 协议（终端）

- 客户端连接 `/ws?cols=80&rows=24`（首次）/ `/ws?session=<id>`（重连）
- 服务端推送三种消息：
  - 字符串原始终端输出 → `term.write(data)`
  - `{type: 'session', id}` → 客户端持久化到 sessionStorage
  - `{type: 'stats', cpu, memKB}` → 监控数据（前端可选显示）
- 客户端发送：
  - 字符串 → PTY 输入
  - `{type: 'resize', cols, rows}` → PTY 尺寸

---

## 8. App Shell 架构（macOS 风格桌面）

> **状态**: 设计阶段 · **目标**: 将静态分栏布局替换为 macOS 风格的桌面 + Dock + 浮动窗口范式。

### 8.1 设计理念

当前 `parent.html` 是一个**固定分栏布局**：左侧边栏（文件树 + SCM）/ 中间（终端 + 预览）/ 覆盖层（Graph + Getman）。所有区域始终占用屏幕空间，不可移动、不可关闭。

新设计模仿 macOS 桌面范式：
- **桌面**是工作空间（浮动窗口的容器）
- **Dock** 是应用启动器（底部常驻图标栏）
- **AppWindow** 是每个应用的独立容器（可拖拽、缩放、最小化）
- 只打开你需要的 app，未打开的 app 不占用空间

### 8.2 视觉隐喻

```
┌──────────────────────────────────────────────────────────┐
│ Desktop（工作区）                                          │
│                                                           │
│   ┌─ Terminal ────────[─][□][×]─┐  ┌─ Preview ─[─][□][×] │
│   │                              │  │                      │
│   │  >_                          │  │  📄 README.md         │
│   │                              │  │                      │
│   └──────────────────────────────┘  └──────────────────────┘
│   ┌─ 版本控制 ────────[─][□][×]─┐
│   │  staged / unstaged / diff    │
│   │                              │
│   └──────────────────────────────┘
│                                                           │
│                        ┌─── Dock ────────────────────┐    │
│  ┌──────┐ ┌──────┐ ┌──────┐ ┌──────┐ ┌──────┐ ┌──────┐ │
│  │ 📁    │ │ 📄    │ │ >_    │ │ ⎇    │ │ ◉    │ │ ⚡   │ │
│  │文件   │ │预览   │ │终端   │ │版本   │ │图谱   │ │ API  │ │
│  └──────┘ └──────┘ └──────┘ └──────┘ └──────┘ └──────┘ │
│     ●                   ●                               │
└──────────────────────────────────────────────────────────┘
```

### 8.3 组件树

```
parent.html
└── AppShell
    ├── Desktop                        # 浮动窗口容器（flex-1, relative）
    │   └── AppWindow × N              # 每个打开的 app 一个窗口
    │       ├── TitleBar               # 可拖拽标题栏 + 红绿灯
    │       └── iframe (app 内容)      # 首次 open 时加载 src
    └── Dock                           # 底部固定，36px 高
        └── AppIcon × 6              # 点击打开/激活对应 app
```

### 8.4 App 定义（6 个）

| id | 名称 | 图标 | src | 默认打开 | 备注 |
|----|------|------|-----|----------|------|
| `files` | 文件 | 📁 | `/pages/file-tree.html` | ✅ | 文件树，点击文件可打开 Preview |
| `preview` | 预览 | 📄 | `/pages/preview.html` | ❌ | 文件内容预览，由文件树触发 |
| `terminal` | 终端 | `>_` | `/pages/terminal.html` | ✅ | xterm 多 tab 终端 |
| `scm` | 版本控制 | `⎇` | `/pages/scm.html` | ❌ | Git 暂存/提交/diff |
| `graph` | Git 图谱 | `◉` | `/pages/graph.html` | ❌ | Git 历史图谱可视化 |
| `getman` | API 测试 | `⚡` | `/pages/getman.html` | ❌ | HTTP 请求构造与响应查看 |

### 8.5 AppWindow 行为契约

| 操作 | 触发 | 行为 |
|------|------|------|
| 打开 | 点击 Dock 图标 | 创建窗口（默认位置／大小），首次加载 iframe src |
| 聚焦 | 点击窗口任意位置 | 提升 z-index 到最前，标题栏高亮 |
| 关闭 | 点击红绿灯 × | 关闭窗口，卸载 iframe。再次打开时重新加载 |
| 最小化 | 点击红绿灯 − | 窗口收起（仅 Dock 图标下面的 ● 指示运行中） |
| 全屏 | 点击红绿灯 □ | 窗口扩展到整个 Desktop 区域 |
| 拖拽 | 拖拽标题栏 | 重定位窗口（x, y 存入 state） |
| 缩放 | 拖拽窗口边缘 | 调整大小（w, h 存入 state） |

### 8.6 状态模型

```js
// store 新增 keys
'appWindows'  // AppWindowState[]
'appOrder'    // string[]   z-index 排序（id 列表，最后面的最前）

// AppWindowState
{
  id: string,           // 'terminal'
  x: number,            // 窗口 left（px，相对于 Desktop）
  y: number,            // 窗口 top（px）
  w: number,            // 窗口宽度（px）
  h: number,            // 窗口高度（px）
  minimized: boolean,   // 是否最小化
  fullscreen: boolean,  // 是否全屏
}
```

**持久化**：`appWindows` 存到 workspace `/api/workspace/ui`，重启恢复窗口布局。`appOrder` 不持久化（重启后默认 z-order）。

### 8.7 AppIcon 交互

| 操作 | 行为 |
|------|------|
| 点击已关闭的 app | 创建默认位置/大小的窗口，加载 src |
| 点击已打开但非活跃的 app | 提升该窗口到最前 |
| 点击已打开且活跃的 app | 最小化（macOS 行为） |
| 右键 / 长按 | 未来扩展：关闭、重新加载 |

### 8.8 渐进迁移计划

**Phase 1（立即）**：
1. 新建 `AppShell` 组件替换当前 `parent.html` 固定布局
2. 新建 `Dock` + `AppIcon` 组件
3. 新建 `AppWindow` 组件（基本版：标题栏 + iframe，暂不支持拖拽/缩放）
4. 将全部 6 个 iframe 页面转为 AppWindow
5. `files`、`terminal` 默认打开（布局模拟当前分栏效果）

**Phase 2（窗口管理）**：
6. 拖拽标题栏 → 窗口移动
7. 边缘/角落 resize 手柄
8. 最小化/全屏按钮
9. z-index 层叠管理

**Phase 3（持久化）**：
10. 窗口位置/大小/最小化状态 → persist
11. 重启恢复布局

### 8.9 与原架构的兼容点

| 保留 | 修改 |
|------|------|
| 所有 iframe 子页面（HTML + JS 业务组件）**不改动** | `parent.html` 布局完全重建 |
| `HostedIframe` 组件可复用（`lazy` + `open`/`close`） | `useParentMethod` 的 `openGraph`/`closeGraph` → 改为 `toggleApp` |
| store / Comlink / postMessage 机制不变 | 新增 `appWindows` / `appOrder` 状态 key |
| theme 穿透机制不变 | overlay 概念移除（Graph/Getman 不再全屏浮窗） |
| API 路由不变 | 无 |

---
## 9. 明确不做的事（YAGNI）

| 不做 | 理由 |
|------|------|
| 构建步骤 | 项目体量小，调试延迟比构建延迟贵 |
| npm 包（共享给外部） | 内部工具 |
| 状态快照导出/导入 | 没需求 |
| 热重载（HMR） | 改了 lib 刷新页面即可 |
| undo/redo 历史栈 | 没需求 |
| `.d.ts` 类型定义文件 | JSDoc 内联即可 |
| EventBus / 装饰器 / DI | 瞬时事件用计数器够了 |
| transport 抽象层 | Comlink over windowEndpoint 是唯一通道 |
| 向后兼容垫片 | 旧代码直接删 |
| 支持其他前端框架 | 只有 Solid 适配 |
| 跨浏览器兼容 | 本地 Chrome/Safari 最新版即可 |

如果某项功能不够用，**改这个架构**，不要补丁式扩展。

---

## 10. 关键约束速查

修改代码前对照检查：

- [ ] import 全部用裸模块名（`'solid-js'`），不是 `/vendor/xxx`
- [ ] 颜色全部用 Tailwind 工具类（`bg-bg` / `text-text` / `border-border`）或 `var(--color-xxx)`，无硬编码色值
- [ ] **无 `@apply`**（完全禁用）。工具类直接写到 SolidJS html\`\` 模板的 class 里，复杂选择器写普通 CSS
- [ ] `<style type="text/tailwindcss">` 内只保留 `@theme` + `[data-theme]` + 极少量兄弟/伪类 CSS（普通属性，无 @apply）
- [ ] 不在 HTML 内重复 `* {}` / `html, body {}` reset（已由 `css/base.css` 提供）
- [ ] Solid 动态属性都用箭头函数包装
- [ ] `createEffect` 内只读 Solid signal，不读 `store.get()`
- [ ] `<For>` 中数组对象不被替换引用（易变字段抽独立 signal）
- [ ] 所有 setInterval/setTimeout/WebSocket 都有 onCleanup
- [ ] 跨 iframe 调用都走 Comlink，无 `window.parent` 直读
- [ ] shared state key 是 camelCase 单字段
- [ ] 瞬时事件计数器（如 scmVersion）不进 onPersist
- [ ] 错误路径有 try-catch 和 console.error，不会白屏
