> **状态**：Open
> **创建时间**：2026-07-24
> **类型**：技术债
> **优先级**：高
> **标签**：plugin, marketplace, TUI, CLI, UX, CRUD
> **创建人**：user

# Plugin 面板 & CLI 交互闭环不完整

## 问题描述

Plugin 子系统的用户交互（TUI 面板 + CLI 命令）存在多处 CRUD 闭环缺失。后端 API 已实现完善的安装/卸载/更新/启用/禁用机制，但大量功能未暴露给用户界面。用户操作路径不连贯，典型场景如：

- 安装了插件后**无法更新**
- 想搜索插件**没有 CLI 命令**
- marketplace 添加后 TUI **不立即拉取**
- 操作成功/失败**无反馈提示**
- 插件启用/禁用**CLI 无命令**

## 症状详情

### A. Plugin TUI 面板交互缺失

| # | 症状 | 关联代码 | 影响 |
|---|------|----------|------|
| A1 | **无插件更新操作** — `update_plugin()`（`installer/install.rs:168-217`）完整实现但 TUI 无入口 | `plugin.rs` | 用户无法在面板上升级插件 |
| A2 | **搜索 Loading/Error 不可见** — `SearchState::Loading` / `Error` 状态已写入（`plugin.rs:269,286`）但 UI 不渲染 | `plugin.rs:1286-1380` | 用户不知道搜索正在进行/失败 |
| A3 | **远程搜索结果未渲染** — `PLUGIN_SEARCH_RESULTS` atom（`atoms.rs:225`）已写入但 UI 从未展示 | `plugin.rs:1696` | Discover tab 只显示本地缓存 |
| A4 | **操作无反馈** — i18n key `panel-plugin-operation-complete/failed` 已定义但 UI 不调用 | `locales/zh-CN/main.ftl` | 用户不知道操作结果 |
| A5 | **Errors Tab 只读无交互** — 列出的加载错误无法点击跳转/重试 | `plugin.rs:1554-1594` | 只能看不能修 |

### B. Marketplace 管理 CRUD 缺失

| # | 症状 | 关联代码 | 影响 |
|---|------|----------|------|
| B1 | **TUI add 不立即 clone** — `plugin.rs:191-227` 仅写 `known_marketplaces.json`，不调 `refresh_marketplace()`。CLI 路径已修复 | `plugin.rs:600-614` | TUI 添加的市场等下次启动才可用 |
| B2 | **TUI 刷新只发搜索请求** — Enter 刷新调 ACP `plugin/search`，不实际拉取缓存 | `plugin.rs:600-614` | 刷新不会更新 marketplace 数据 |
| B3 | **状态系统虚设** — `MarketplaceStatus` 定义 6 种状态（Cached/Fetching/Fresh/Stale/NotFetched），实际只设过 Cached/NotFound | `plugin.rs:1612-1622` | 用户看不到真实的加载/错误状态 |
| B4 | **MarketplaceRefreshEvent 无人消费** — `manager.rs:199-255` 产生事件，TUI 纯同步读盘不订阅 | `manager.rs:199` | 后台刷新结果 TUI 永远看不到 |
| B5 | **manifest 损坏静默降级** — JSON 解析失败时 `plugin_count = 0`，状态仍为 Cached | `plugin.rs:1613-1641` | 用户看不到错误，以为该市场没有插件 |
| B6 | **无 CLI marketplace update/refresh** — 命令行只能 add/list/remove | `cli_plugin.rs:202-222` | 用户无法从 CLI 刷新 marketplace |

### C. CLI 命令缺失

| # | 症状 | 关联 API（已实现） | 影响 |
|---|------|-------------------|------|
| C1 | **无 `plugin enable <id>`** | `update_enabled_plugins()` | 无法从 CLI 启用插件（TUI 可通过 Space/Detail） |
| C2 | **无 `plugin disable <id>`** | `remove_from_enabled_plugins()` | 无法从 CLI 禁用插件 |
| C3 | **无 `plugin update <id>` / `--all`** | `update_plugin()`（`install.rs:168-217`） | 核心更新功能完全无入口（CLI/TUI 都没有） |
| C4 | **无 `plugin search <query>`** | `find_plugin_in_marketplaces()` / `MarketplaceManager::find_plugin()` | 无法从 CLI 搜索远程插件 |
| C5 | **无 `plugin info <id>`** | `load_plugin_manifest()` | 无法从 CLI 查看插件详情 |
| C6 | **无 `plugin cleanup`** | `cleanup_orphaned_plugins()` | 磁盘垃圾永远不清理 |

### D. Marketplace CRUD 操作完整性

| 操作 | CLI | TUI | 对比 |
|------|:---:|:---:|------|
| **Create（添加）** | ✅ `marketplace add` → 立即 `refresh_marketplace()` fetch | ⚠️ 仅写 `known_marketplaces.json`，`install_location` 为空，**不 clone** | ❌ 不一致 |
| **Read（查看）** | ✅ `marketplace list` → 名称+来源+时间（基本信息） | ✅ Marketplaces tab → 状态图标+插件数+已安装数（丰富） | ⚠️ CLI 信息少 |
| **Update（刷新）** | ❌ 完全缺失 `marketplace update` 命令 | ⚠️ Enter 触发 ACP search，**不是真正的 git pull/refetch** | ❌ 两路径都残缺 |
| **Delete（删除）** | ✅ 从 JSON 移除 | ✅ d→确认→从 JSON 移除 | ⚠️ 都**不清除磁盘缓存目录** |

补充发现：
- `MarketplaceAction` 枚举（`main.rs:220-233`）只有 `Add/List/Remove`，**没有 `Update` 变体**
- i18n key `command-plugin-update-failed` 和 `/plugin marketplace update` 帮助文本存在但路由未实现（幽灵功能）
- TUI add 调不到 `refresh_marketplace()` — 需要 ACP 协议桥接或本地调用

### E. 插件生命周期完整分析（subagent 补充）

#### E1. 安装流 11 步

```
parse → marketplace manifest 查找 → 匹配插件名 → 确定来源(git clone/本地路径)
→ 检测 plugin.json → 确定版本号(sha/version/timestamp) → 拷贝到版本缓存
→ 生成合成 manifest(如需要) → 写 installed_plugins.json → 启用(enabledPlugins) → ACP 事件推送
```

**状态跟踪**：无持久化 install 状态机。`InstalledPlugins` 结构体没有 `status` 字段。
**失败处理**：`installing: HashSet<String>`（`plugin.rs:164`）在失败后**不清理**（内存泄漏级 bug）。
**反馈**：CLI 有 `println`，TUI footer 显示"安装中..."但无完成通知。

#### E2. 卸载流 7 步

```
查找插件记录 → 从 installed_plugins.json 移除 → 从 enabledPlugins 移除
→ 删除 data 目录(条件) → 清理 plugin options → 标记 orphaned(.orphaned_at) → ACP 事件
```

**孤儿文件清理**：`cleanup_orphaned_plugins()`（`uninstall.rs:150-254`）实现完整（7 天延迟删除+级联空目录），但**从未被调用**。
**CLI 缺陷**：`run_plugin_uninstall` 接受 `_scope_str` 但标记为 `_`（忽略），无法按 scope 卸载。

#### E3. 更新流 — 基础设施完整但零入口

`update_plugin()`（`install.rs:168-217`）完整实现：
1. 版本比较（sha 前 7 位 / version 字段）
2. 版本相同 → 返回当前插件
3. 版本不同 → **先卸载再安装**（原子操作）

`check_updates()`（`uninstall.rs:109-147`）完整实现：
1. 遍历所有已安装插件
2. 懒加载 marketplace manifest
3. 返回 `Vec<PluginUpdateInfo>`

| 函数 | 代码存在 | CLI 绑定 | TUI 绑定 | ACP 绑定 | 启动钩子 |
|------|---------|:---:|:---:|:---:|:---:|
| `update_plugin()` | ✅ | ❌ | ❌ | ❌ | — |
| `check_updates()` | ✅ | ❌ | ❌ | ❌ | ❌ |
| `cleanup_orphaned_plugins()` | ✅ | ❌ | ❌ | ❌ | ❌ |

#### E4. installed vs enabled 是两个概念

| 概念 | 存储位置 | 含义 | 可单独操作 |
|------|---------|------|:---:|
| installed | `installed_plugins.json` | 文件已下载到缓存 | ❌ |
| enabled | `settings.json` → `enabledPlugins` | 启动时加载该插件 | ✅ Space/Detail |

安装自动启用，卸载自动禁用，TUI 可独立开关，但 CLI 无 enable/disable 命令。

### F. 流程闭环断裂点（更新）

```
安装 → ✅ CLI / TUI / ACP
卸载 → ✅ CLI / TUI / ACP  
启用/禁用 → ⚠️ 仅 TUI（Space/Detail），CLI 无
更新 → ❌ CLI/TUI/ACP 全无入口（后端完整实现）
检查更新 → ❌ 完全无入口
搜索 → ⚠️ TUI 有本地过滤，CLI 无，远程搜索未渲染
marketplace add → ⚠️ CLI ✅（立即 fetch），TUI ❌（不 fetch）
marketplace refresh → ❌ CLI 无命令，TUI 有但不实际 refresh
marketplace remove → ✅ 两者都有，但都不清磁盘缓存
插件清理 → ❌ cleanup 逻辑完整但从不触发
```

## 复现条件

1. `peri plugin install some-plugin` → 安装成功
2. 打开 TUI `/plugin` → Installed tab 可见已安装插件
3. 按 Space 可启用/禁用 → ✅
4. 想更新插件 → ❌ 没入口
5. Discovery tab 搜索 → 只显示本地缓存，看不到 Loading 状态
6. 添加 marketplace → TUI 添加后列表不出现新市场（需重启）
7. 删除 marketplace → 有确认，但删完后无反馈

## 涉及文件

| 文件 | 相关症状 |
|------|----------|
| `peri-tui/src/kit/panels/plugin.rs` | A1-A5, B1-B5 |
| `peri-tui/src/cli_plugin.rs` | B6, C1-C6, D（marketplace CLI） |
| `peri-tui/src/main.rs:220-233` | D（MarketplaceAction 枚举缺 Update） |
| `peri-tui/src/kit/atoms.rs` | A3 |
| `peri-tui/src/acp_server/requests.rs` | E1（install via ACP）、E2（uninstall via ACP） |
| `peri-middlewares/src/plugin/installer/install.rs` | C3、E3（update_plugin 完整实现但无入口） |
| `peri-middlewares/src/plugin/installer/uninstall.rs` | C6、E2（cleanup 完整实现但不触发） |
| `peri-middlewares/src/plugin/marketplace/manager.rs` | B4（refresh event） |
| `peri-middlewares/src/plugin/marketplace/mod.rs` | B3（MarketplaceStatus） |
| `peri-middlewares/src/plugin/config.rs:428-463` | C1-C2（enabledPlugins 读写） |
| `peri-middlewares/src/plugin/types.rs:277-282` | E1（InstalledPlugins 无 status 字段） |

## 验证记录（2026-07-24，ultra-batch）

派出 3 个并行 explorer subagent 对 25 项修复进行可行性/风险/阻塞点验证。

### 验证结论矩阵

| # | 修复项 | 基础 | 风险 | 关键发现 |
|---|--------|:---:|:---:|------|
| **B6** | CLI marketplace update | ✅ | 🟢 | 零阻塞，参考 add 命令模板即可 |
| **B5** | manifest 损坏显示错误 | ✅ | 🟢 | 2 行改动 |
| **A2** | SearchState Loading/Error 渲染 | ✅ | 🟢 | 纯 UI 分支，状态机已就绪 |
| **A3** | 远程搜索结果展示 | ⚠️ | 🟢 | atom 已写入，只需加判断分支 |
| **C1** | CLI plugin enable | ✅ | 🟢 | 同步函数，直接调用 |
| **C2** | CLI plugin disable | ✅ | 🟢 | 同 C1 |
| **C5** | CLI plugin info | ✅ | 🟢 | 读 installed_plugins.json 即可 |
| **C6** | CLI plugin cleanup | ✅ | 🟢 | async 函数，已完整实现 |
| **D** | MarketplaceAction::Update | ✅ | 🟢 | 枚举+路由各 1 行 |
| **A4** | 操作反馈通知 | ✅ | — | **已就绪**（NOTIFICATION atom 已覆盖） |
| **E1a** | installing HashSet 泄漏 | ❌ | 🟢 | 只增不减+无人读取→建议直接删除 |
| **B3** | MarketplaceStatus 六态完善 | ⚠️ | 🟡 | 枚举+渲染已备，改状态分配逻辑 |
| **A5** | Errors tab 点击重试 | ⚠️ | 🟡 | 需先定义"重试"语义 |
| **D-del** | marketplace remove 清磁盘 | ⚠️ | 🟡 | install_location 已有，需确认默认行为 |
| **C3** | CLI plugin update | ⚠️ | 🟡 | async，需 --all 迭代；无事务保护 |
| **C4** | CLI plugin search | ⚠️ | 🟡 | 现有 API 只返回 market 名，需自写遍历 |
| **E2** | cleanup 触发入口 | ✅ | 🟡 | 函数完整，需 CLI/启动/TUI 三个入口 |
| **A1** | Installed tab 更新按钮 | ⚠️ | 🔴 | 缺 ACP `plugin/update` handler |
| **B1** | TUI add 立即 clone | ⚠️ | 🔴 | event handler 不能 await async |
| **B2** | Enter 实际 git pull | ⚠️ | 🔴 | 同 B1，缺 ACP `marketplace/refresh` |
| **B4** | 订阅 MarketplaceRefreshEvent | ⚠️ | 🔴 | TUI 启动无 MarketManager 实例 |
| **E3** | 更新流全路径接线 | ✅ | 🔴 | 后端完整但全入口缺失；缺事务保护 |

### 共同阻塞点：ACP 桥接层

A1/B1/B2/B4 共享同一根因——TUI 和 plugin 后端之间缺少 ACP handler。当前 `requests.rs` 只有 `install/uninstall/toggle/search` 四个 handler，需新增 `plugin/update` 和 `marketplace/refresh`（预计 ~120 行模式化代码）。`PluginActionResult` 事件类型足够通用无需新增。

### 实施批次

| 阶段 | 修复项 | 预估改动 | 解锁能力 |
|------|--------|:---:|------|
| **P0 快赢** | B6, B5, A2, A3, C1, C2, C5, C6, D | ~80 行 | CLI 8 个命令 + TUI 渲染补齐 |
| **P1 清理** | B3, D-del, E1a(删除 HashSet) | ~40 行 | 消除技术债 |
| **P2 ACP 桥接** | 新增 `plugin/update` + `marketplace/refresh` handler | ~120 行 | 解锁所有高优先级 |
| **P3 功能补齐** | A1, B1, B2, C3 | ~200 行 | TUI/CLI 更新完整闭环 |
| **P4 架构加固** | B4, E3(事务保护), C4(搜索) | ~200 行 | 后台刷新+安全更新+远程搜索 |
| **P5 收尾** | A5(重试语义), E2(cleanup 启动钩子) | ~60 行 | 完整闭环 |

### 待确认项

1. **A5 Errors 重试**：语义未定义——进入详情页 vs 卸载重装 vs 仅重读 manifest？建议从"点击→进入详情"起步
2. **D-delete 磁盘清除**：默认清缓存 vs `--clean` flag 可选？需确认用户预期
3. **E1a installing HashSet**：三 agent 一致确认只增不减且无人读取，应直接删除

## 实施记录（2026-07-24，auto-devflow 5 阶段串行）

| 阶段 | 修复项 | 结果 | 文件 | 改动 |
|------|--------|:---:|------|:---:|
| **P0 快赢** | B6,B5,A2,A3,C1,C2,C5,C6,D | ✅ | main.rs, cli_plugin.rs, plugin.rs | +280/-10 |
| **P1 清理** | B3,D-del,E1a | ✅ | plugin.rs, cli_plugin.rs | +30/-10 |
| **P2 桥接** | plugin/update + marketplace/refresh handlers | ✅ | requests.rs | +76 |
| **P3 功能** | A1,B1,B2,C3 | ✅ | main.rs, cli_plugin.rs, plugin.rs, locales | +80 |
| **P4 加固** | C4(搜索), B4-light(缓存刷新) | ✅ | main.rs, cli_plugin.rs, plugin.rs | +60 |
| **P5 收尾** | A5(Errors跳转), E2(启动清理) | ✅ | plugin.rs, launch.rs | +14 |

**总计**: 8 文件, +538/-20 行, 21/25 修复项完成, `cargo test --lib` 587 passed。

### 延期/未实施项

| 项目 | 原因 |
|------|------|
| **B4-full** (实时订阅 RefreshEvent) | 需 entry.rs 架构变更（event channel + MarketManager 生命周期），工作量大且跨层 |
| **E3 事务保护** (update_plugin 原子性) | 需改 middleware 核心逻辑（install.rs），属于安全加固而非 CRUD 闭环 |
| **E3 启动检查** (启动时 check_updates) | 性能影响需独立评估（每个 marketplace 都发起 git fetch） |
| **E2-TUI** (TUI 面板内 cleanup 入口) | CLI 已覆盖（C6），TUI 入口可后续通过 Errors tab 交互补充 |

## 状态变更记录

| 日期 | 旧状态 | 新状态 | 操作人 | 备注 |
|------|--------|--------|--------|------|
| 2026-07-24 | — | Open | user | 初始创建 |
| 2026-07-24 | Open | Verified | agent | ultra-batch 3 并行验证 |
| 2026-07-24 | Verified | Closed | agent | auto-devflow 5 阶段实施完成，21/25 项修复 (8 files, +538/-20) |
