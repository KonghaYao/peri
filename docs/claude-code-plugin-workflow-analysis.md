# Claude Code Plugin 子命令完整工作流程分析

> 分析目标：理解 Claude Code 的 `plugin marketplace add` 和 `plugin install` 的完整工作流程，对比 perihelion 实现找出关键分歧。

---

## 1. 架构概览：双层分离

Claude Code 的 marketplace 管理使用**双层分离**架构：

```
settings.json                         known_marketplaces.json
┌─────────────────────────┐           ┌──────────────────────────────────┐
│ extraKnownMarketplaces  │──意图层──→│ "hindsight": {                  │
│   "hindsight": {        │  写入     │   source: "github:vectorize-io/  │
│     source: "github:    │           │            hindsight",            │
│       vectorize-io/     │           │   installLocation: "/Users/.../  │
│       hindsight"        │           │     marketplaces/hindsight",     │
│   }                     │           │   lastUpdated: "2026-07-24T..."  │
└─────────────────────────┘           │ }                                │
                                      └──────────────────────────────────┘
                                                   │
                                                   ▼ 状态层（已物化）
                                          ~/.claude/plugins/marketplaces/
                                            └── hindsight/
                                                └── .claude-plugin/
                                                    └── marketplace.json
```

| 层 | 文件 | 含义 |
|----|------|------|
| Intent（意图层） | `settings.json` → `extraKnownMarketplaces` | 用户/项目**想要**的市场列表 |
| State（状态层） | `known_marketplaces.json` | 实际**已物化**（clone/download）的市场 |

**Reconciler** (`src/utils/plugins/reconciler.ts`) 在启动时对比两层：
- `diffMarketplaces()` 比较 settings.json 和 known_marketplaces.json
- 缺失的 marketplace → 自动 `addMarketplaceSource()` 补全
- 源变更的 → 自动覆盖

---

## 2. `plugin marketplace add <source>` 完整流程

### 2.1 CLI 入口
**文件**: `src/cli/handlers/plugins.ts:422-497`

```typescript
marketplaceAddHandler(source, { scope = 'user' }) {
  1. parseMarketplaceInput(source) → MarketplaceSource
  2. addMarketplaceSource(source, onProgress) → { name, resolvedSource }
  3. saveMarketplaceToSettings(name, resolvedSource, scope)   // 写入 settings.json
  4. clearAllCaches()
}
```

### 2.2 `parseMarketplaceInput()` — 输入解析
**文件**: `src/utils/plugins/parseMarketplaceInput.ts:23-169`

支持的 6 种输入格式：

| 输入格式 | 示例 | 解析结果 |
|----------|------|----------|
| GitHub 简写 | `owner/repo` | `{ source: 'github', repo: 'owner/repo' }` |
| SSH URL | `git@github.com:owner/repo.git` | `{ source: 'git', url: '...' }` |
| HTTPS URL (git) | `https://github.com/owner/repo.git` | `{ source: 'git', url: '...' }` |
| HTTPS URL (direct) | `https://example.com/marketplace.json` | `{ source: 'url', url: '...' }` |
| 本地文件 | `./my-marketplace.json` | `{ source: 'file', path: '...' }` |
| 本地目录 | `~/my-plugins/` | `{ source: 'directory', path: '...' }` |

### 2.3 `addMarketplaceSource()` — 核心物化逻辑
**文件**: `src/utils/plugins/marketplaceManager.ts:1782-1923`

```
addMarketplaceSource(source, onProgress) {
  1. 策略检查: isSourceAllowedByPolicy()
     - blocklist 检查
     - allowlist 检查 (strictKnownMarketplaces)
     - 被阻止 → 抛出明确错误

  2. 幂等检查: 遍历 existingConfig
     - 相同 source 已存在 → return { alreadyMaterialized: true }
     - 跳过重复 clone

  3. loadAndCacheMarketplace(source):
     ├─ github: SSH 优先 → HTTPS 回退 → git clone --depth 1
     │           → 读取 .claude-plugin/marketplace.json
     ├─ git:    git clone --depth 1 → 读取 manifest
     ├─ url:    HTTP GET → Schema 校验 → 写入 {name}.json
     ├─ file:   本地读取 + Schema 校验
     └─ directory: 目录内查找 marketplace.json

  4. 名称冲突检测:
     - 同名不同源 → 覆盖 (settings intent wins)
     - seed-managed → 拒绝覆盖

  5. 写入 known_marketplaces.json:
     config[marketplace.name] = {
       source: resolvedSource,
       installLocation: cachePath,    // 实际的缓存路径
       lastUpdated: new Date().toISOString()
     }

  6. return { name: marketplace.name, alreadyMaterialized: false, resolvedSource }
}
```

**关键点**：
- `addMarketplaceSource()` **立即**执行 clone/download，不是等下次启动
- Marketplace 名称来自 manifest 的 `name` 字段，不是从 source 提取
- 相同 source 已存在时做幂等跳过（不重复 clone）

### 2.4 `saveMarketplaceToSettings()` — 意图持久化
**文件**: `src/utils/plugins/marketplaceManager.ts:226-238`

```typescript
saveMarketplaceToSettings(name, entry, settingSource) {
  settings.extraKnownMarketplaces[name] = { source, ... }
}
```

写入 `settings.json` → `extraKnownMarketplaces`，供 reconciler 在启动时重新物化。

### 2.5 自动注册官方 Marketplace
**文件**: `src/utils/plugins/marketplaceManager.ts:161-192` (`getDeclaredMarketplaces()`)

官方 marketplace 通过 `getDeclaredMarketplaces()` 隐式声明：
```typescript
getDeclaredMarketplaces() {
  // 任何启用的插件引用 @claude-plugins-official →
  // 自动声明官方 marketplace（带 sourceIsFallback: true）
  for (const [pluginId, value] of Object.entries(enabledPlugins)) {
    if (value && parsePluginIdentifier(pluginId).marketplace === 'claude-plugins-official') {
      声明 OFFICIAL_MARKETPLACE_SOURCE = { source: 'github', repo: 'anthropics/claude-plugins-official' }
    }
  }
  // 优先级: implicit < --add-dir < settings.json
  return merge(implicit, addDir, settings)
}
```

---

## 3. `plugin install <name>[@<marketplace>]` 完整流程

### 3.1 CLI 入口
**文件**: `src/cli/handlers/plugins.ts:630-663`

```typescript
pluginInstallHandler(plugin, { scope = 'user' }) {
  1. parsePluginIdentifier(plugin) → { name, marketplace? }
  2. installPlugin(plugin, scope)
}
```

### 3.2 `installPlugin()` — CLI 包装
**文件**: `src/services/plugins/pluginCliCommands.ts:102-142`

```typescript
installPlugin(plugin, scope) {
  1. console.log("Installing plugin...")
  2. result = await installPluginOp(plugin, scope)   // 核心操作
  3. 输出成功/失败消息
}
```

### 3.3 `installPluginOp()` — 核心安装逻辑 ⭐
**文件**: `src/services/plugins/pluginOperations.ts:321-418`

```typescript
installPluginOp(plugin, scope) {
  1. parsePluginIdentifier(plugin) → { name: pluginName, marketplace: marketplaceName }

  2. 插件查找 — 两个分支:
     ┌─ 如果指定了 marketplaceName:
     │    getPluginById("name@marketplace") → 只查指定市场
     │
     └─ 如果裸名（无 @）:
          遍历 known_marketplaces.json 中所有 marketplace:
            for (const [mktName, mktConfig] of Object.entries(marketplaces)) {
              marketplace = getMarketplace(mktName)      // 读缓存或 fetch
              pluginEntry = marketplace.plugins.find(p => p.name === pluginName)
              if (pluginEntry) { found; break }          // 第一个匹配就停止
            }

  3. 未找到 → return { success: false, "Plugin not found in any configured marketplace" }

  4. 构建 pluginId = "name@foundMarketplace"

  5. installResolvedPlugin({ pluginId, entry, scope }):
     ├─ 策略检查 (blocklist/allowlist)
     ├─ 依赖解析 (递归)
     ├─ cachePlugin() → 下载到缓存
     ├─ copyPluginToVersionedCache() → 复制到版本化路径
     └─ addInstalledPlugin() → 更新 installed_plugins.json + enabledPlugins

  6. return { success: true, message, pluginId }
}
```

**这是最关键的差异点**：当没有指定 `@marketplace` 时，Claude Code **遍历所有已知 marketplace** 查找插件。

### 3.4 `getMarketplace()` — Marketplace 内容获取
**文件**: `src/utils/plugins/marketplaceManager.ts:2122-2177`

```typescript
// memoized 函数，内存缓存避免重复读盘
getMarketplace(name) {
  1. loadKnownMarketplacesConfig() → 获取 entry
  2. 检查 source 路径合法性
  3. cache hit: readCachedMarketplace(installLocation)
     - 读 {installLocation}/.claude-plugin/marketplace.json (git 源)
     - 或直接读 installLocation (url/file 源)
  4. cache miss: loadAndCacheMarketplace(source) → 重新 fetch
  5. 更新 lastUpdated 时间戳
}
```

### 3.5 `parsePluginIdentifier()` — 名称解析
**文件**: `src/utils/plugins/pluginIdentifier.ts:51-57`

```typescript
parsePluginIdentifier(plugin: string) {
  if (plugin.includes('@')) {
    const parts = plugin.split('@')
    return { name: parts[0], marketplace: parts[1] }
  }
  return { name: plugin }   // 没有 @ → marketplace 为 undefined
}
```

---

## 4. `plugin install` 名称解析完整决策树

```
plugin install "hindsight-memory"
  │
  ├─ parsePluginIdentifier("hindsight-memory")
  │   → { name: "hindsight-memory", marketplace: undefined }
  │
  ├─ marketplaceName 为 undefined → 进入遍历分支
  │
  ├─ loadKnownMarketplacesConfig()
  │   → { "claude-plugins-official": {...}, "hindsight": {...} }
  │
  ├─ 遍历:
  │   ├─ "claude-plugins-official":
  │   │    getMarketplace("claude-plugins-official")
  │   │    → marketplace.plugins.find(p => p.name === "hindsight-memory")
  │   │    → undefined (没找到)
  │   │
  │   └─ "hindsight":
  │        getMarketplace("hindsight")
  │        → marketplace.plugins.find(p => p.name === "hindsight-memory")
  │        → foundPlugin = { name: "hindsight-memory", ... }
  │        → foundMarketplace = "hindsight"
  │        → break
  │
  └─ pluginId = "hindsight-memory@hindsight" → 继续安装流程
```

```
plugin install "hindsight-memory@hindsight"
  │
  ├─ parsePluginIdentifier("hindsight-memory@hindsight")
  │   → { name: "hindsight-memory", marketplace: "hindsight" }
  │
  └─ marketplaceName 有值 → getPluginById("hindsight-memory@hindsight")
      → 直接在 hindsight 市场中查找
```

---

## 5. 文件存储布局

```
~/.claude/
├── settings.json                         # 用户设置（含 extraKnownMarketplaces）
│
└── plugins/
    ├── known_marketplaces.json           # 市场注册表（状态层）
    │   {
    │     "hindsight": {
    │       "source": { "source": "github", "repo": "vectorize-io/hindsight" },
    │       "installLocation": "/Users/.../marketplaces/hindsight",
    │       "lastUpdated": "2026-07-24T..."
    │     }
    │   }
    │
    ├── marketplaces/                     # 市场缓存
    │   ├── hindsight/                    # git clone 的内容
    │   │   └── .claude-plugin/
    │   │       └── marketplace.json      # 插件清单
    │   └── claude-plugins-official/      # 官方市场
    │       └── .claude-plugin/
    │           └── marketplace.json
    │
    ├── installed_plugins.json            # 已安装插件（V2 格式）
    │   {
    │     "version": 2,
    │     "plugins": {
    │       "hindsight-memory@hindsight": [{
    │         "scope": "user",
    │         "installPath": "/Users/.../cache/hindsight/hindsight-memory/v1.0.0/",
    │         "version": "abc1234",
    │         ...
    │       }]
    │     }
    │   }
    │
    └── cache/                            # 版本化安装
        └── hindsight/
            └── hindsight-memory/
                └── abc1234/              # 版本（git SHA 前7位）
                    └── .claude-plugin/
                        └── plugin.json
```

---

## 6. Perihelion 现状与关键分歧

### 分歧 1: `plugin install` 不搜索所有 marketplace ⚠️ **最严重**

| | Claude Code | Perihelion |
|--|-------------|------------|
| 指定 `@marketplace` | 只在指定市场搜索 | 同 |
| 裸名（无 `@`） | **遍历所有 known marketplaces** | **硬编码默认 `claude-plugins-official`** |

**位置**: `peri-tui/src/cli_plugin.rs:89-91`
```rust
let (name, marketplace) = plugin_name
    .split_once('@')
    .unwrap_or((plugin_name, "claude-plugins-official")); // ← 硬编码默认
```

**修复方向**: `install_plugin` 在无 `@marketplace` 时，应遍历所有已知 marketplace 查找插件。

---

### 分歧 2: `plugin marketplace add` 不立即 clone ⚠️ **严重**

| | Claude Code | Perihelion |
|--|-------------|------------|
| add 时 | `addMarketplaceSource()` 立即 `loadAndCacheMarketplace()` | 只写入 `known_marketplaces.json` |
| 实际 clone | **命令执行时立即发生** | **下次启动时**（`MarketplaceManager::init()` 后台任务） |

**位置**: `peri-tui/src/cli_plugin.rs:140-147`
```rust
// 只是把 KnownMarketplace 推入列表，没有触发 clone
marketplaces.push(KnownMarketplace {
    source: marketplace_source,
    install_location: String::new(),    // ← 空字符串，下次 init 才填充
    auto_update: false,
    last_updated: now,
});
save_known_marketplaces(&marketplaces, None)?;
```

**后果**: `plugin marketplace add vectorize-io/hindsight` 后立即 `plugin install hindsight-memory`，此时 hindsight marketplace 还没有被 clone，`marketplace.json` 不存在，所以即使搜索它也找不到插件。

**修复方向**: `run_marketplace_add` 应在 `save_known_marketplaces` 后立即触发 `fetch_github()` 或调用 `loadAndCacheMarketplace()`。

---

### 分歧 3: Marketplace 命名来源不同

| | Claude Code | Perihelion |
|--|-------------|------------|
| 名称来源 | manifest 内 `name` 字段 | `extract_name()` 从 source 提取 |

**Claude Code**: `marketplaceManager.ts:1914` → `config[marketplace.name] = ...`
**Perihelion**: `manager.rs:97-126` → `extract_name()` 从 `repo`/`url` 中提取最后一段

**影响**: 当 marketplace.json 声明的 `name` 与从 source 路径提取的名称不一致时，会产生 key 不匹配问题。不过大多数情况下两者一致（如 `hindsight` repo 的 manifest name 也是 `hindsight`）。

---

### 分歧 4: 缺少 Intent/State 双层分离

| | Claude Code | Perihelion |
|--|-------------|------------|
| settings.json 写入 | 有（`extraKnownMarketplaces`） | 无 |
| known_marketplaces.json | state 层 | 唯一的记录 |

Claude Code 在 `addMarketplaceSource` 后调用 `saveMarketplaceToSettings()` 写入 `settings.json` 的 `extraKnownMarketplaces` 字段。启动时 reconciler 对比两层的差异，自动补全缺失的 marketplace。这是独立于命令行操作的保护机制。

Perihelion 的 `init()` 函数虽然会从 `settings.json` 读 `extraKnownMarketplaces` 合并到 `known` 列表（`manager.rs:140-148`），但 `run_marketplace_add` 不写 settings。

---

### 分歧 5: 错误信息不可操作的

当前 Perihelion 错误:
```
Error: 安装失败: 插件未找到:  (marketplace: claude-plugins-official)
```

Claude Code 对应错误:
```
Plugin "hindsight-memory" not found in any configured marketplace
```

Claude Code 的消息告诉用户"在所有已配置的 marketplace 中都找不到"，而 Perihelion 只显示在默认 marketplace 中找不到，没有提示用户检查其他 marketplace。

---

## 7. 修复优先级建议

| 优先级 | 修改点 | 影响 |
|--------|--------|------|
| **P0** | `installPluginOp` 遍历所有 marketplace | 修复用户报告的 `plugin marketplace add` → `install` 失败 |
| **P1** | `run_marketplace_add` 立即 clone/mirror | 确保 add 后立即可用，不需要重启 |
| **P2** | `run_marketplace_add` 写 `settings.json` | 支持跨会话的 intent/state 同步 |
| **P3** | Marketplace 命名改为读 manifest | 处理 manifest name ≠ source name 的边缘情况 |

---

## 8. 参考文件索引

| 文件（Claude Code） | 行号 | 内容 |
|---------------------|------|------|
| `src/cli/handlers/plugins.ts` | 422-497 | `marketplaceAddHandler` |
| `src/cli/handlers/plugins.ts` | 630-663 | `pluginInstallHandler` |
| `src/services/plugins/pluginCliCommands.ts` | 102-142 | `installPlugin` CLI wrapper |
| `src/services/plugins/pluginOperations.ts` | 321-418 | `installPluginOp` 核心安装逻辑 ⭐ |
| `src/utils/plugins/parseMarketplaceInput.ts` | 23-169 | Marketplace source 解析 |
| `src/utils/plugins/pluginIdentifier.ts` | 51-57 | `parsePluginIdentifier` |
| `src/utils/plugins/officialMarketplace.ts` | 1-25 | 官方 marketplace 常量 |
| `src/utils/plugins/marketplaceManager.ts` | 161-192 | `getDeclaredMarketplaces` |
| `src/utils/plugins/marketplaceManager.ts` | 264-298 | `loadKnownMarketplacesConfig` |
| `src/utils/plugins/marketplaceManager.ts` | 1433-1650 | `loadAndCacheMarketplace` |
| `src/utils/plugins/marketplaceManager.ts` | 1782-1923 | `addMarketplaceSource` ⭐ |
| `src/utils/plugins/marketplaceManager.ts` | 2058-2107 | `readCachedMarketplace` / `getMarketplaceCacheOnly` |
| `src/utils/plugins/marketplaceManager.ts` | 2122-2177 | `getMarketplace` (memoized) |
| `src/utils/plugins/marketplaceManager.ts` | 2188-2280 | `getPluginByIdCacheOnly` / `getPluginById` |
| `src/utils/plugins/reconciler.ts` | 50-228 | `diffMarketplaces` / `reconcileMarketplaces` |
| `src/utils/plugins/officialMarketplaceStartupCheck.ts` | 134-200+ | 官方 marketplace 启动自检 |

| 文件（Perihelion） | 行号 | 内容 |
|---------------------|------|------|
| `peri-tui/src/cli_plugin.rs` | 82-108 | `run_plugin_install` ⚠️ |
| `peri-tui/src/cli_plugin.rs` | 124-151 | `run_marketplace_add` ⚠️ |
| `peri-middlewares/src/plugin/installer/install.rs` | 12-118 | `install_plugin` |
| `peri-middlewares/src/plugin/marketplace/manager.rs` | 97-126 | `extract_name` |
| `peri-middlewares/src/plugin/marketplace/manager.rs` | 129-197 | `init` (后台刷新 marketplace) |
