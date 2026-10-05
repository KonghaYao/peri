# peri-tui / peri-middlewares 解耦可行性设计（报告候选 C2/C3/C4）

状态：**设计草案；P0 与 P1 的 `McpPoolPort` 部分已实施（W2/W3 批，提交 `954231ca` / `e2a6b65a`，已并入 `perf/rust-build-speedup` 并与 `pre-release/main` 完成集成合并，实施注记见 §2.4.2 与 §4.2）**。本文原始内容只做可行性设计与数据核实：未改动任何源码（除本文件），未运行 cargo（构建测量由并行的另一 agent 负责），未执行 git add / commit。所有引用数据来自 2026-10-05 的代码快照（worktree `.worktrees/build-perf`，分支 `perf/rust-build-speedup`），统计命令见附录 A，可逐条复现。

日期：2026-10-05

关联材料：

- `spec/issues/2026-10-05-rust-build-performance-diagnosis.md`（下称**诊断报告**）：§一 基线、§二 归因 1、§四 候选 C2/C3/C4
- `spec/history/2026-08.md` 2026-08-05 两条（M-TUI、L5，均已压缩）
- `scripts/import-exemptions.conf` 头部「收紧任务对照」与豁免规则
- `docs/standards/architecture-contracts.md`：ARC-BOUNDARY-001、ARC-HOST-SHUTDOWN-001、ARC-MCP-ACP-001、ARC-FROZEN-001、ARC-MIDDLEWARE-CAPABILITY-001
- `peri-tui/CLAUDE.md`、`peri-middlewares/CLAUDE.md`

**编号约定**：C2/C3/C4 沿用诊断报告 §四编号。注意与代码内既有的 “C2/C3” **同名不同事**——后者指 MetaHarness 波 4 / v2 事件契约（见 `peri-acp-types/src/meta_harness.rs:51,158`、`peri-agent/src/agent/subagent_event_forwarder.rs:19`、`peri-acp/src/session/frozen.rs:161`），评审与提交信息中必须显式区分，避免两套语义串线。

---

## 一、结论摘要

1. **报告 §二 的引用统计逐行复核通过**：peri-tui → peri-middlewares 生产引用确为 **93 行、9 个文件**，且 `plugin` 70 行 / `mcp` 23 行**精确吻合**（§2.1）。

2. **关键修正（与报告结论不同，需评审确认）**：**C2 单独实施对编译关键路径收益约为 0**。peri-middlewares 经**两条彼此独立的边**进入 peri-tui 的构建闭包：
   - 边 a：`peri-tui` 直接依赖（93 行）；
   - 边 b：`peri-tui → peri-acp → peri-middlewares`（101 行，`peri-acp/Cargo.toml:39` 为硬依赖）。

   只要任一条保留，cargo 就必须在编译 peri-tui **之前**编完 peri-middlewares（Cargo 依赖是 **package 级**而非 target 级）。因此报告的「C2 关键路径 -10~20s」隐含前提是 **C3 同时完成**——报告 §四 C2 行只写了「需先解内部耦合（mcp↔assembly 互引）」，未点出这条跨 crate 的前提。

3. **端口化机制比报告描述的更成熟，但 2026-10-05 复核时 `McpPoolPort` 的能力面不足以覆盖 ACP 业务路径**：`peri-acp-types::ports` 已有 `McpPoolPort`（含 `snapshot()` 专供 `mcp/list`）、`McpTaskOwnerPort`、`ToolSearchPort`、`AgentCatalogPort`、`DynamicMcpDeploymentPort`、`WorkflowMiddlewarePort`、`PluginManagerPort`、`SettingsHooksPort`、`CronSchedulerPort`。**真正的阻碍不是"缺端口件"，而是端口能力缺口 + 装配面**：
   - `downcast_arc::<具体类型>()` 逃生口被当作**主路径**使用（peri-acp 内 10 处 downcast 回 `McpClientPool`）。**W3（2026-10-05）已按缺口补齐 `McpPoolPort` / `PluginManagerPort` 并替换全部逃生口（`downcast_arc` 10 处 + `downcast_ref` 3 处）**，详见 §2.4.2「W3 实施后」；`peri-acp` 生产引用 101 → 63 行（与 `pre-release/main` 集成后 62 行）；
   - **ACP Host 装配面**（`host/assemble.rs`，45 处 middlewares 引用）仍留在 peri-acp 内，连同 §2.4.2 列出的余项（冻结段落、prompt stage、静态装配工厂等）都尚未收口。

4. **mcp client 面不需要抽独立 crate**。M-TUI 已把 MCP 完整协议化：`mcp/list`、`mcp/oauth_start|callback|cancel`、`McpPoolPort::snapshot()`、`host/shutdown` 均已落地并被 TUI 消费（`peri-tui/src/kit/service_snapshot/session_services.rs:41-42`）。TUI 侧的 mcp 残留（23 行）是**遗留双轨**（自建第二个连接池 + 面板回退直读），不是能力缺口——可直接删除，不需要新 crate。

5. **推荐路线**：把 C2/C3 合并为 **M-TUI 收口批 + L5 收口批**，并补一个报告未列的**结构动作**：装配面独立 crate + `peri` bin 独立 package。三件齐备才有 **-5~-15s（未验证，推演见 §4.2）**；缺任一件收益归零。

6. **排序建议**：`C3 → C2 → 装配面/bin 拆分 → C4`（§四）。C3 与 C2 必须过架构评审：改动跨 `peri-acp` / `peri-tui` / 新增 crate 的依赖方向、§0 依赖门豁免清单与 ARC-BOUNDARY-001 / ARC-HOST-SHUTDOWN-001 契约。

7. **投入产出比提示（诚实项）**：同等人力投在第一梯队 A1（release LTO，**已实测 -97.7s**）或第二梯队 B1/B2（外部依赖，-30~50s CPU）的收益/风险比明显更高。结构改造的价值在于**降低此后每一次全量/CI 构建的固定成本**，并且是唯一能压缩 workspace 串行链（49.4s）的手段——但它不是一次性的"提速开关"。

---

## 二、事实核对（数据来自代码，命令见附录 A）

### 2.1 peri-tui → peri-middlewares 引用全清单（93 行 / 9 文件）

统计命令与结果：

```bash
cd peri-tui
grep -rnE "peri_middlewares" src --include="*.rs" | grep -vE "_test\.rs|_test/|/tests/" | wc -l
# → 93
grep -rnE "peri_middlewares" src --include="*.rs" | grep -vE "_test\.rs|_test/|/tests/" \
  | cut -d: -f1 | sort -u | wc -l
# → 9
grep -rnE "peri_middlewares" src --include="*.rs" | grep -vE "_test\.rs|_test/|/tests/" \
  | grep -oE "peri_middlewares::(plugin|mcp)" | sort | uniq -c
# → 70 peri_middlewares::plugin
# → 23 peri_middlewares::mcp
```

**结论：诊断报告 §二「93 行、9 个文件，plugin 70 + mcp 23」完全准确。**

用途归类口径：**装配点** = 构造/持有 middlewares 具体句柄并注入 ACP 端口，或为其生命周期服务；**面板数据源** = 为面板/快照派生数据的直读直写；**CLI** = `peri plugin …` 独立子命令路径（不经 ACP 会话）。

#### 2.1.1 `src/cli_plugin.rs` —— 36 行（`plugin` 面 36，CLI 子命令）

| 行 | 符号 | 用途 |
| --- | --- | --- |
| 23 | `plugin::config::load_installed_plugins` | `run_plugin_list` 读 installed_plugins.json |
| 84 | `plugin::config::marketplaces_cache_dir` | `run_plugin_install` 缓存目录 |
| 90 | `plugin::find_plugin_in_marketplaces` | install 前定位插件 |
| 95 | `plugin::install_plugin` | install 执行 |
| 118 | `plugin::uninstall_plugin` | uninstall 执行 |
| 127 | `plugin::parse_marketplace_input` | marketplace add 解析来源 |
| 130 | `plugin::MarketplaceManager::extract_name` | add：去重比对 |
| 132 | `plugin::load_known_marketplaces` | add：读已知 marketplace |
| 137 | `plugin::MarketplaceManager::extract_name` | add：同名判定 |
| 150 | `plugin::marketplace::refresh_marketplace` | add 后刷新缓存 |
| 159 | `plugin::KnownMarketplace` | add：构造条目 |
| 166 | `plugin::save_known_marketplaces` | add：落盘 |
| 174 | `plugin::load_known_marketplaces` | `run_marketplace_list` |
| 185 | `plugin::MarketplaceManager::extract_name` | list：展示名 |
| 187 | `plugin::MarketplaceSource::GitHub` | list：来源标签 |
| 190 | `plugin::MarketplaceSource::Git` | list：来源标签 |
| 191 | `plugin::MarketplaceSource::Url` | list：来源标签 |
| 192 | `plugin::MarketplaceSource::File` | list：来源标签 |
| 193 | `plugin::MarketplaceSource::Directory` | list：来源标签 |
| 196 | `plugin::MarketplaceSource::Npm` | list：来源标签 |
| 206 | `plugin::load_known_marketplaces` | `run_marketplace_remove` |
| 214 | `plugin::MarketplaceManager::extract_name` | remove：定位目标 |
| 217 | `plugin::KnownMarketplace` | remove：过滤集合类型 |
| 220 | `plugin::MarketplaceManager::extract_name` | remove：过滤谓词 |
| 228 | `plugin::save_known_marketplaces` | remove：落盘 |
| 247 | `plugin::load_known_marketplaces` | `run_marketplace_update` |
| 251 | `plugin::MarketplaceManager::extract_name` | update：定位 |
| 260 | `plugin::marketplace::refresh_marketplace` | update 执行 |
| 270 | `plugin::save_known_marketplaces` | update：回写 last_updated |
| 289 | `plugin::update_enabled_plugins` | enable |
| 307 | `plugin::remove_from_enabled_plugins` | disable |
| 329 | `plugin::config::marketplaces_cache_dir` | `run_plugin_update` 缓存目录 |
| 331 | `plugin::update_plugin` | update 执行 |
| 355 | `plugin::config::load_installed_plugins` | `run_plugin_info` |
| 422 | `plugin::cleanup_orphaned_plugins` | `run_plugin_cleanup` |
| 437 | `plugin::config::marketplaces_cache_dir` | `run_plugin_search` 缓存目录 |

> 该文件是**整个 93 行里最大的一块（36/93 = 39%）**，且**只被 `main.rs` 的 `Commands::Plugin` 分支使用**（`peri-tui/src/main.rs:14` 声明 `mod cli_plugin;`，`:831-860` 分发）。它走的是**无 ACP 会话**的独立进程路径。

#### 2.1.2 `src/kit/panels/plugin/data.rs` —— 19 行（`plugin` 面 19，面板数据源）

| 行 | 符号 | 用途 |
| --- | --- | --- |
| 72 | `plugin::load_known_marketplaces` | marketplace 面板：已知源 |
| 73 | `plugin::marketplaces_cache_dir` | 缓存目录 |
| 76 | `plugin::load_installed_plugins` | 已装插件集 |
| 81 | `plugin::MarketplaceManager::extract_name` | 展示名 |
| 86 | `plugin::marketplace::find_marketplace_json` | 缓存 manifest 定位 |
| 150 | `plugin::MarketplaceSource::GitHub` | 来源标签分支 |
| 153 | `plugin::MarketplaceSource::Git` | 来源标签分支 |
| 156 | `plugin::MarketplaceSource::Url` | 来源标签分支 |
| 157 | `plugin::MarketplaceSource::Directory` | 来源标签分支 |
| 158 | `plugin::MarketplaceSource::File` | 来源标签分支 |
| 159 | `plugin::MarketplaceSource::Npm` | 来源标签分支 |
| 178 | `plugin::load_known_marketplaces` | discover 列表：已知源 |
| 179 | `plugin::marketplaces_cache_dir` | 缓存目录 |
| 184 | `plugin::MarketplaceSource::GitHub` | official marketplace 自动注入判定 |
| 190 | `plugin::KnownMarketplace` | 自动注入构造 |
| 191 | `plugin::MarketplaceSource::GitHub` | 自动注入来源 |
| 204 | `plugin::load_installed_plugins` | 已装集合 |
| 209 | `plugin::MarketplaceManager::extract_name` | marketplace 名 |
| 212 | `plugin::marketplace::find_marketplace_json` | manifest 定位 |

> 该文件**直接读盘**（marketplace 缓存目录 + `installed_plugins.json` + mtime 判定 Stale），是「面板数据全部经 ACP」缺口的核心：ACP 侧现有 `plugin/list`（会话投影，不含 marketplace）与 `plugin/search`（按 query 扫缓存），**没有 marketplace/list 面**。

#### 2.1.3 `src/kit/panels/plugin/panel_handler.rs` —— 8 行（`plugin` 面 8，面板数据源/写路径）

| 行 | 符号 | 用途 |
| --- | --- | --- |
| 75 | `plugin::load_known_marketplaces` | `delete_marketplace` 确认动作：读 |
| 80 | `plugin::MarketplaceManager::extract_name` | 过滤谓词 |
| 86 | `plugin::save_known_marketplaces` | 删除后落盘 |
| 230 | `plugin::save_claude_settings_enabled_plugins` | toggle 后**额外**直写 settings.json（见下注） |
| 485 | `plugin::load_known_marketplaces` | 同上删除路径（第二分支） |
| 490 | `plugin::MarketplaceManager::extract_name` | 过滤谓词 |
| 495 | `plugin::save_known_marketplaces` | 删除后落盘 |
| 608 | `plugin::save_claude_settings_enabled_plugins` | 同 230 |

> **疑点（需现场确认，不作结论）**：本文件 :217 / :595 已通过 ACP 发 `plugin/toggle`，而 ACP `handle_toggle` 会经 `cfg.plugin_manager.set_enabled` → `update_enabled_plugins` / `remove_from_enabled_plugins` 写 `settings.json` 的 `enabledPlugins`（`peri-middlewares/src/plugin/installer/mod.rs:171-199`）。TUI 侧 :230 / :608 的 `save_claude_settings_enabled_plugins` 写的是同一个 `claude_settings_path()`（`plugin/config.rs:445-451`）。两者是否存在重复写/竞态，应在 M-TUI 收口时一并裁定；本文不预设结论。

#### 2.1.4 `src/kit/panels/plugin/search_handler.rs` —— 5 行（`plugin` 面 5，面板数据源）

| 行 | 符号 | 用途 |
| --- | --- | --- |
| 106 | `plugin::parse_marketplace_input` | marketplace 添加输入解析 |
| 110 | `plugin::MarketplaceManager::extract_name` | 名称派生 |
| 118 | `plugin::load_known_marketplaces` | 去重读取 |
| 126 | `plugin::KnownMarketplace` | 新条目构造 |
| 132 | `plugin::save_known_marketplaces` | 落盘 |

#### 2.1.5 `src/launch.rs` —— 4 行（`plugin` 面 2 装配点 + `mcp` 面 2 装配点）

| 行 | 符号 | 用途 |
| --- | --- | --- |
| 113 | `plugin::load_enabled_plugins_aggregated` | 启动装配：`app.services.plugin_data`（面板派生输入） |
| 124 | `plugin::cleanup_orphaned_plugins` | 启动副作用：清理孤儿插件（`tokio::spawn`） |
| 271 | `mcp::McpClientPool` | `shutdown_mcp_pool(pool, owner)` 函数签名 |
| 272 | `mcp::McpTaskOwner` | 同上 |

#### 2.1.6 `src/app/mod.rs` —— 6 行（`mcp` 面 6，装配点）

| 行 | 符号 | 用途 |
| --- | --- | --- |
| 140 | `mcp::McpTaskOwner::new` | `spawn_mcp_init()` 构造 owner + spawner |
| 142 | `mcp::McpClientPool::new_pending_with_spawner` | 构造 TUI 侧连接池 |
| 145 | `mcp::OAuthCredentialClient::new` | 注入会话凭证 |
| 156 | `mcp::McpInitStatus::Pending` | 初始化状态 watch channel 初值 |
| 165 | `mcp::McpTaskKey::Initialize` | 后台任务登记 |
| 166 | `mcp::McpClientPool::run_initialize` | 后台初始化 |

> 代码内注释已自认这是遗留态：「MCP 资源句柄直读（C 类豁免至 M-TUI；「面板数据全部经 ACP」需 mcp/list 命令面，见批 3 tui-deps 未做项）」（`peri-tui/src/app/mod.rs:138-139`）。而 `mcp/list` 命令面**现已存在**（§2.1.8 注），注释已过期。

#### 2.1.7 `src/app/service_registry.rs` —— 3 行（`mcp` 面 3，装配点）

| 行 | 符号 | 用途 |
| --- | --- | --- |
| 85 | `mcp::McpClientPool` | `ServiceRegistry.mcp_pool: Option<Arc<…>>` 字段 |
| 86 | `mcp::McpTaskOwner` | `mcp_task_owner` 字段 |
| 87 | `mcp::McpInitStatus` | `mcp_init_rx: watch::Receiver<…>` 字段 |

#### 2.1.8 `src/kit/service_snapshot.rs` —— 11 行（`mcp` 面 11，面板数据源）

| 行 | 符号 | 用途 |
| --- | --- | --- |
| 55 | `mcp::McpClientPool` | `SnapshotSource.mcp_pool` 字段 |
| 59 | `mcp::McpInitStatus` | `SnapshotSource.mcp_init_rx` 字段 |
| 687 | `mcp::McpClientPool` | `derive_mcp_servers(pool)` 签名 |
| 703 | `mcp::OAuthStatus::NeedsAuthorization` | server 条目 needs_auth 投影 |
| 767 | `mcp::McpClientPool` | `derive_mcp_status(pool, rx)` 签名 |
| 768 | `mcp::McpInitStatus` | 同上 |
| 772 | `mcp::McpInitStatus::Pending` | init phase 映射 |
| 773 | `mcp::McpInitStatus::Initializing` | init phase 映射 |
| 774 | `mcp::McpInitStatus::Ready` | init phase 映射 |
| 775 | `mcp::McpInitStatus::Failed` | init phase 映射 |
| 787 | `mcp::ClientStatus::Connected` | connected 计数 |

> **双轨事实**：`service_snapshot.rs:279-288` 在有活跃 ACP 会话时用 ACP 结果**覆盖**直读结果（`mcp = services.mcp; mcp_servers = services.mcp_servers;`，来自 `mcp/list` + `plugin/list`），**只有 `session_id` 为空时才回退到直读派生**。即 11 行中的 9 行（687/703/767/768/772-775/787）只在无会话窗口生效。

#### 2.1.9 `src/kit/atoms.rs` —— 1 行（`mcp` 面 1，**死代码**）

| 行 | 符号 | 用途 |
| --- | --- | --- |
| 511 | `mcp::McpClientPool` | `pub static MCP_PANEL_POOL: OnceLock<Arc<…>>` |

> **已确认写后无读**：全仓 `MCP_PANEL_POOL` 仅两处出现——`:511` 定义 与 `app/mod.rs:153` 的 `.set(...)`，**没有任何读取方**。doc comment 所述「OAuth 授权完成后据此触发 reconnect」已不成立（`handle_oauth_completed` 现只在 `kit/acp_events/system.rs:603` 更新 BridgeState）。这一行可直接删除，并连带从豁免清单的 `kit/atoms.rs` 条目中移除该文件。

#### 2.1.10 归类汇总

| 归类 | 行数 | 分布 |
| --- | --- | --- |
| **MCP 池装配点** | 11 | `app/mod.rs`(6) + `service_registry.rs`(3) + `launch.rs`(2) |
| **MCP 面板数据源** | 12 | `service_snapshot.rs`(11) + `atoms.rs`(1，死代码) |
| **plugin 装配点** | 2 | `launch.rs`(2) |
| **plugin 面板数据源** | 32 | `data.rs`(19) + `panel_handler.rs`(8) + `search_handler.rs`(5) |
| **plugin CLI 子命令** | 36 | `cli_plugin.rs`(36) |
| **合计** | **93** | 9 个文件 |

报告 §二「均为装配点与面板数据源」的判断**基本成立**，但需补一条：其中 **36 行（39%）是 CLI 子命令路径**，与面板无关，且不经 ACP——这决定了「全量改经 ACP」需要**为 CLI 另设 ACP 会话/宿主**，不是删面板代码就能收口的。

### 2.2 peri-middlewares 结构画像

crate 合计 **98,872 行 / 302 个 `.rs` 文件**。

> **口径提示（重要）**：仓库惯用的"非测试"过滤是文件名 `! -name '*_test.rs'`（依赖门同款）。该口径**漏掉 4 个测试变体文件**——根级的 `assembly_test_baseline.rs` / `assembly_test_meta.rs` / `assembly_test_sections.rs` / `assembly_test_workflow.rs`（合计 **1,984 行**，文件名后缀是 `_meta.rs` / `_sections.rs` 等，不匹配 `*_test.rs`）。因此：
> - 按 `! -name '*_test.rs'` 口径：**42,704 行**
> - **剔除上述 4 个文件后的真实生产代码：40,720 行**
>
> 下文表内"非测试行"列按目录逐项给出（目录内无此类变体，数值不受影响），根级单独标注。行数按目录：

| 目录 | 总行 | 非测试行 | 文件数 | 职责（`peri-middlewares/CLAUDE.md` 任务路由） |
| --- | --- | --- | --- | --- |
| `mcp/` | **53,286** | 22,260 | 134 | MCP 合并、server/tool bridge、builtin 生命周期 |
| `subagent/` | 11,737 | 2,945 | 31 | SubAgent、后台任务、取消与事件 |
| `plugin/` | 7,294 | 3,515 | 20 | Plugin manifest、commands、agents、MCP 回退 |
| `hooks/` | 6,782 | 3,557 | 26 | Hook 事件与执行器 |
| `tool_search/` | 3,442 | 1,216 | 15 | 工具搜索索引与 deferred 执行 |
| `permission/` | 2,322 | 884 | 7 | 权限中间件 |
| `middleware/` | 1,578 | 648 | 11 | 通用 middleware（含 todo / image） |
| `skills/` | 1,299 | 755 | 6 | Skills 根解析、registry 投影、预载 |
| `goal/` | 1,239 | 450 | 5 | Goal 状态 |
| `tools/` | 1,181 | 546 | 5 | 宿主侧工具 |
| `assembly/` | 894 | 894 | 6 | 槽位构造 |
| `workflow/` | 823 | 491 | 3 | Workflow 中间件 |
| `attribution/` | 768 | 310 | 7 | 归因 |
| `at_mention/` | 539 | 272 | 4 | @mention |
| `default_system_prompt/` | 254 | 254 | 1 | 默认/语言段 |
| `hitl/` | 148 | 148 | 1 | 人在环 |
| `agents_md/` | 164 | 85 | 3 | AGENTS.md |
| `ask_user/` | 130 | 130 | 1 | ask_user 工具 |
| 根级生产 `.rs` 7 个（`assembly.rs` / `host_ports.rs` / `workspace_io.rs` / `lib.rs` / `completion_reminder.rs` / `settings.rs` / `platform.rs`） | 1,360 | 1,360 | — | 装配入口 / 端口实现 / 工作区 IO / 平台位 |
| 根级 `*_test.rs`（5 个） | 1,648 | 测试 | — | — |
| 根级 `*_test_*.rs` 变体（4 个，见上口径提示） | 1,984 | 测试 | — | — |

**生产代码合计 = 40,720 行**（= 目录 39,360 + 根级 1,360）。

要点：

- **`mcp/` 占全 crate 54%（总行）/ 52%（非测试行）**，与报告 §二「5.3 万行（54%）」一致。诊断报告 §1.3 给出的 **20.9s 编译耗时中约一半可归因到 `mcp/`**（按行数比例外推，未验证）。
- `mcp/` 内部子目录（总行）：`client/` 7,456、`builtin/` 5,081、`dynamic/` 4,936、`acp/` 1,471、`config/` 1,262、`skill_discovery/` 1,020；根级单文件最大为 `middleware.rs` 925、`client.rs` 902、`tool_bridge.rs` 875、`config.rs` 822、`agent_registry.rs` 809、`initialize.rs` 758。**测试文件占比极高**（非测试 22,260 / 总 53,286 = 42%），故按"总行数"估算拆分收益会显著高估，应按非测试行折算。
- **`plugin/`（3,515 非测试行）是本 crate 内最适合独立的一块**：外部依赖仅 `peri_acp_types`（command/plugin 类型）与 `peri_agent`（且后者只被 `plugin/middleware.rs:1,4` 的 `PluginMiddleware` 使用，TUI 与 CLI 都不用它）。

### 2.3 模块耦合图（核实诊断报告归因 1）

统计口径：非测试 `.rs` 文件中 `crate::<模块>` 的**行命中数**（与报告口径一致，已用 `assembly → mcp = 20` 校准；已剔除全部测试文件，**含 §2.2 提示的 4 个根级测试变体**；全 crate 命中合计 264 处）。完整矩阵：

| FROM \ TO | mcp | plugin | hooks | skills | subagent | tool_search | permission | assembly | workspace_io | platform | 其他 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| **assembly** | **20** | — | 1 | 2 | — | 2 | — | 6 | 4 | — | host_ports 1, tools 1 |
| **middleware** | 9 | — | — | — | — | — | — | — | — | — | completion_reminder 1, tools 1 |
| **platform** | 3 | — | — | — | — | — | — | — | — | — | — |
| **skills** | 2 | 1 | — | 1 | — | — | — | — | — | — | settings 2 |
| **subagent** | 5 | — | **11** | 8 | 3 | 5 | — | — | — | — | tools 2 |
| **workspace_io** | 5 | — | — | — | — | — | — | — | — | — | — |
| **host_ports** | 2 | **11** | 3 | — | — | — | — | — | — | — | — |
| **hooks** | — | — | 11 | — | — | — | **6** | — | — | — | — |
| **mcp** | 80 | **4** | — | 2 | **1** | — | — | **1** | — | 12 | — |
| **plugin** | — | 21 | 1 | — | — | — | — | — | — | — | — |
| **permission** | — | — | — | — | — | 3 | — | — | — | — | — |
| **hitl** | — | — | — | — | — | — | 1 | — | — | — | tools 1 |
| **ask_user** | — | — | — | — | — | 1 | — | — | — | — | — |
| **attribution** | — | — | — | — | — | 1 | — | — | — | — | workspace_io 2 |
| **goal** | — | — | — | — | — | — | — | — | — | — | completion_reminder 1 |
| **tools** | — | — | — | — | — | — | — | — | — | — | ask_user 1 |

**逐条核实报告归因 1**：

| 报告陈述 | 复核结果 | 说明 |
| --- | --- | --- |
| `assembly → mcp` 20 处 | ✅ **精确一致** | `assembly.rs`(6) + `assembly/mcp.rs`(9) + `assembly/preparation.rs`(2) + `assembly/workflow.rs`(3) |
| `mcp → assembly` 1 处 | ✅ 一致，**但为 doc comment** | `mcp/builtin/mod.rs:21` 是注释中提及 `crate::assembly`，**不是真实代码依赖**——该"互引"实际不构成编译期环 |
| `mcp → plugin` 4 处 | ✅ 一致 | 全在 `mcp/config.rs:58,399,447,476`：`plugin::loader::{LoaderError, load_enabled_plugins_for_mcp, LoadedPlugin}` |
| `mcp → subagent` 1 处 | ✅ 一致 | `mcp/agent_registry.rs:577` 调 `subagent::infer_agent_capability` |
| `subagent → hooks` 11 处 | ✅ 一致 | — |
| `host_ports → plugin` 11 处 | ✅ 一致 | 端口实现包装 plugin 业务函数 |
| `hooks → permission` 6 处 | ✅ 一致 | — |
| `mcp` 被 8 个模块引用 | ⚠️ **口径需澄清**：**非测试模块 7 个**（assembly / middleware / platform / skills / subagent / workspace_io / host_ports）+ `mcp` 自身 = 8 | 若含测试文件则来源为 13 个；`tool_search` 对 `crate::mcp` 的引用**只在 `tool_search/declaration_test.rs`**，不计入生产面 |

**会形成拆分环的边（C4 的真实阻碍）**：

1. `mcp ↔ subagent`：`mcp/agent_registry.rs:577` → `subagent::infer_agent_capability`（纯函数，输入 `ClaudeAgentFrontmatter`）；`subagent` 侧 5 处反向引用（`subagent/mod.rs:180,222`、`subagent/tool/mcp_activation.rs:4`、`subagent/skill_preload.rs:286,318`）。**真环**。
2. `mcp ↔ skills`：`mcp/skill_discovery.rs:428` → `skills::annotate_mcp_content`（纯字符串函数）；`skills` 侧 `skills/tools.rs:123`、`skills/mod.rs:61` 反向引用。**真环**。
3. `mcp ↔ plugin`：`mcp/config.rs` 用 `plugin::loader::{LoadedPlugin, LoaderError, load_enabled_plugins_for_mcp}`（真依赖）；`plugin/loader.rs:21` 反向用 `mcp::McpServerConfig`（**纯类型**）。**半真环**：反向边只是类型，下移类型即可解。
4. `mcp → platform`(12)：`platform.rs` 只有 33 行（`STDIO_UNAVAILABLE` / `builtin_policy` / 平台位），**下移即可**，不构成环。
5. `mcp → assembly`：**无真实边**（仅注释）。

结论：**C4 的前置面比报告描述的窄得多**——真正需要先解的是 1、2 两条（各 1 个纯函数）与 3 的类型下移，而不是"mcp↔assembly 互引"。

### 2.4 依赖图与关键路径复核

#### 2.4.1 workspace 依赖边（从各 `Cargo.toml` `[dependencies]` 实测）

```
peri-time ─┐
           ├─→ peri-model ─→ peri-acp-types ─┬─→ peri-resources ─┐
           │                                 ├─→ peri-agent ─────┤
           │                                 ├─→ peri-config ────┤
           │                                 └─→ peri-runtime ─→ peri-controller ─┐
           │                                                                      │
           └──────────────────────────────────────────────────────────────────────┴─→ peri-middlewares ─→ peri-acp ─→ peri-tui（lib + bin）
                                                                                          ↑
                                                              peri-tui（直连）────────────┘
```

关键边（逐行核实）：

- `peri-tui/Cargo.toml:33`：`peri-middlewares = { path = "../peri-middlewares" }`（**边 a**）
- `peri-acp/Cargo.toml:39`：`peri-middlewares = { path = "../peri-middlewares" }`，其上方注释自述「**保留（L5 复核时再收）**」（**边 b**）
- `peri-middlewares/Cargo.toml`：`peri-agent`（+ `peri-acp-types` / `peri-resources` / `peri-config` / `peri-model`）
- `peri-agent`：`peri-model` / `peri-acp-types` / `peri-time`（**不依赖 peri-resources**）
- `peri-resources`：`peri-acp-types` / `peri-time`

**`peri-middlewares` 只有两个下游消费者**（全仓 `grep -rn "peri-middlewares" --include=Cargo.toml`）：`peri-acp` 与 `peri-tui`。这正是「同时切断两条边，crate 就离开 `-p peri-tui` 闭包」的前提。

#### 2.4.2 peri-acp → peri-middlewares 引用画像（101 行 / 14 文件，C3 的改动面）

```bash
grep -rnE "peri_middlewares" peri-acp/src --include="*.rs" | grep -vE "_test\.rs|_test/|/tests/" | wc -l   # → 101
```

| 文件 | 行数 | 性质 | 端口化难度 |
| --- | --- | --- | --- |
| `host/assemble.rs` | 45 | **ACP Host 部署装配面**：构造 `McpClientPool` / `McpTaskOwner` / `ToolSearchIndex` / `ProductionChainAssembler` / `WorkflowAgentMiddlewareFactory` / `BuiltinInstanceContext` / plugin 聚合 / host_ports | 高——建议整体**外移为独立 crate**，非端口化 |
| `host/requests/mcp_oauth.rs` | 15 | `mcp/list` 等：`downcast_arc::<McpClientPool>()` 后读 `all_server_infos` / `ClientStatus` / `OAuthStatus` / `active_oauth_flow` | 中——补端口方法或改用 `McpPoolPort::snapshot()` |
| `session/frozen.rs` | 9 | 冻结段收集：6 个 middleware 的 `sections()` 静态声明 | 中——需「段落提供者」端口 |
| `host/workspace.rs` | 8 | 3 处 `downcast_ref::<McpClientPool>()` + `ClientStatus::Connected` + `builtin_closed_instances` | 中——同上 |
| `host/requests/plugin.rs` | 7 | `claude_home()` / `load_enabled_plugins` / `plugin_route_entries` / `find_marketplace_json` **绕过端口直调** | 低——`PluginManagerPort` 缺这几个方法 |
| `host/requests/session_lifecycle.rs` | 3 | `downcast_arc` + `mcp::middleware::{attach_connection_notifier, prewarm_discovery}` | 中 |
| `host/requests/session_close.rs` | 3 | `downcast_arc` + `McpClientPool::connect_trusted_workspace_for_close` | 中 |
| `host/requests.rs` | 3 | `downcast_arc`（2 处 helper 签名） | 中 |
| `host/prompt/stage.rs` | 3 | `ProductionChainAssembler` + `hooks::stage_firing::{fire_pre_compact, fire_post_compact}` | 中 |
| `host/requests/{session_restore,owner_catalog}.rs`、`host/{workspace_resources,prompt,mod}.rs` | 各 1 | 零散 downcast / `resolve_skill_roots` / `ExecuteExtraToolResolver` | 低 |

**主导模式 = `downcast_arc::<McpClientPool>()`（10 处，W3 前）**。W3 前端口 `McpPoolPort` 只暴露 `as_any` / `rewind_files` / `has_active_tasks` / `begin_shutdown` / `shutdown` / `snapshot`；服务器目录 / OAuth / 会话执行 owner / Workspace task scope / 发现接线均**不在端口内**，所以每个调用点都必须把具体类型拉回来。

**W3 实施后（2026-10-05，分支 `perf/w2-mw`，含 W2 批；已并入 `perf/rust-build-speedup` 并完成与 `pre-release/main` 的集成合并——main 侧 `9b5b257d` 移除了 execution-ownership 机制，端口化在同一批调用点重新施加）**：

- **全部 downcast 逃生口清零**（生产代码 0 处）：`downcast_arc::<McpClientPool>()` 10 处（`host/requests/mcp_oauth.rs` 4、`host/requests.rs` 2、`session_close.rs` 2、`session_restore.rs` 1、`session_lifecycle.rs` 1）+ `downcast_ref::<McpClientPool>()` 3 处（`host/workspace.rs` 技能/指令/meta 冻结读取）全部替换为端口调用；集成合并后复查仍为 0；
- `McpPoolPort` 扩展 22 个方法（契约 DTO `McpServerInfo` / `McpServerConnectionStatus` / `McpServerOAuthStatus` / `McpOAuthStartDisposition` / `McpBuiltinWorkspaceState`，默认实现与替换前类型还原失败分支同构）：`server_infos` / `active_oauth_flow` / `spawn_oauth_flow_with_id` / `deliver_oauth_callback` / `deliver_dynamic_oauth_callback` / `cancel_oauth_callback` / `cancel_dynamic_oauth_flow` / `has_unsupported_async_task_owner` / `bind_session_execution_owner` / `release_session_execution_owner` / `fence_workspace_task_scope` / `open_workspace_task_scope` / `close_workspace_task_scope` / `bind_session_task_manager` / `recover_workspace_tasks` / `watch_workspace_tasks` / `attach_connection_notifier` / `prewarm_discovery` / `builtin_workspace_state` / `read_builtin_workspace_skills` / `read_builtin_workspace_instructions` / `read_builtin_workspace_meta`；**与 main 集成后净 19 个**——`has_unsupported_async_task_owner` / `bind_session_execution_owner` / `release_session_execution_owner` / `fence_workspace_task_scope` 4 个随 ownership 移除而删除，新增 `reconcile_closing_workspace_scope`（关闭重核对，`host/requests/session_close.rs` 使用）；
- `PluginManagerPort` 扩展 4 个方法（`claude_home` / `enabled_plugin_commands` / `plugin_route_entries` / `find_marketplace_json`），替换 `host/requests/plugin.rs` 的 7 处静态直调（含 `search_marketplace_plugins` 改为经端口定位 manifest）；
- `peri-acp` 生产引用 **101 → 63 行**（W3）；**与 main 集成后 62 行**：`mcp_oauth.rs`(15) / `requests.rs`(3) / `session_restore.rs`(1) / `session_lifecycle.rs`(3) / `owner_catalog.rs`(1，文件随 main 删除) / `plugin.rs`(7) / `workspace.rs` 技能读取(6) / `session_close.rs`(1，静态构造) 清零；
- 余项（本批未授权 / 需架构评审，合计 62 行）：`host/assemble.rs` 45（C-2 装配面外移）、`session/frozen.rs` 9（段落端口）、`host/prompt/stage.rs` 3（hooks 端口）、`host/workspace.rs` 2（`assembly::builtin_closed_instances` 关闭集派生，随 C-2 收口；含 1 处注释命中）、`host/workspace_resources.rs` 1（`resolve_skill_roots` 适配器）、`host/prompt.rs` 1（`ExecuteExtraToolResolver`）、`host/mod.rs` 1（注释）；`session_close.rs` 原剩 1 处**静态构造** `McpClientPool::connect_trusted_workspace_for_close` 已随集成改经 `local.mcp_pool` 端口调用（0 处）。

#### 2.4.3 已经被 M-TUI 完成的协议面（避免重复设计）

| 能力 | 端口 / 路由 | 状态 |
| --- | --- | --- |
| MCP 池状态 + 服务器列表 | `McpPoolPort::snapshot()`（`peri-acp-types/src/ports.rs:108`）→ `mcp/list`（`peri-acp/src/host/requests.rs:132`） | ✅ 已落地，TUI 已消费（`session_services.rs:41-42`） |
| MCP OAuth 三件套 | `mcp/oauth_start|callback|cancel` | ✅ TUI 已消费（`kit/panels/mcp.rs:540`、`kit/popups/oauth_popup.rs:64,86`） |
| 池关闭 | `McpPoolPort::{begin_shutdown, shutdown}` + `McpTaskOwnerPort` | ✅ ARC-HOST-SHUTDOWN-001 已定义顺序 |
| Plugin 增删改查 | `plugin/install|uninstall|toggle|update|search|list` | ✅ 命令面已在，TUI 部分消费（`panel_handler.rs:54,217,254`、`discover_handler.rs:246`） |
| Plugin marketplace | 仅 `marketplace/refresh` | ⚠️ **缺 list / add / remove** |
| Cron | `CronSchedulerPort::list_tasks`（注释已写明「cron/list 命令面数据源」） | ⚠️ **端口就绪、命令面未接线** |
| 冻结段落 / 系统提示段 | 无 | ❌ 需新增端口（C3 项） |

### 2.5 对诊断报告的三处修正 / 待确认项

| # | 报告原文 | 本文复核 | 影响 |
| --- | --- | --- | --- |
| 1 | §四 C2「关键路径 **-10~20s**（peri-tui 方向）；与 M-TUI 同向」 | **前提缺失**：需 C3 同时完成，否则收益 ≈ 0（§4.2 推演）。且真正让收益兑现还需装配面外移 + bin 拆分 | 排序与评审范围（§五） |
| 2 | §1.2「peri-tui 自身 13.4s（lib 9.4 + bin 4.0）」与同节「peri-tui 9.4s (84.7→94.0) + 链接 4.0s」 | **两处口径不一致**：一处把 4.0s 记为 bin 编译、一处记为链接 | 收益推演的分母；建议基线复测时一并厘清 |
| 3 | §1.2 关键路径「peri-tui 方向 70.1s」与同节所列单元之和存在缺口：代码块 6 个单元合计 **59.0s**（+链接 4.0s = 63.0s）；若按结论行的 peri-tui「lib 9.4 + bin 4.0」口径则为 **63.0s**（+链接 = 67.0s） | 缺口 7~11s，疑为未列出的 `peri-controller` 分支（`peri-acp → peri-controller`）与排队损耗 | 同上的分母问题，**本文所有绝对值以报告为基准并标注不确定** |

---

## 三、方案设计

### 3.0 收益机制说明（后面四个方案共用）

先明确**什么才会产生编译收益**，避免把"重构"误当"提速"：

| 机制 | 说明 | 是否会缩短关键路径 |
| --- | --- | --- |
| **M1 切边** | 让某 crate 离开目标构建单元的闭包 | ✅ 唯一能"省掉"整块耗时的方式 |
| **M2 并行化** | 让原本串行的两块变为可同时编译（需要两者**互不依赖**） | ✅ 收益 ≈ `min(A, B)` |
| **M3 搬家** | 把模块从 crate X 移到 crate Y，依赖关系不变 | ❌ 总耗时不变（多一层边界，甚至略增） |

由此可得三条对所有方案的判据：

- **抽取不等于提速。** 从 A 抽出模块 B 且 A 仍依赖 B，则 `t(B) + t(A−B) ≈ t(A)`（M3）。
- **单向依赖的拆分也不提速。** 若拆出的两半仍是 `X → Y`，串行耗时不变；只有拆成"**互不依赖的兄弟**"才触发 M2。
- **只有切边（M1）能真正省时。** peri-middlewares 的两个下游是 `peri-acp` 与 `peri-tui`，因此全部收益都来自这两条边的切断。

耗时换算采用**线性外推**：middlewares 20.9s / 98,872 总行 ≈ **0.21 s/千行（总行）**，或 20.9s / 40,720 生产行 ≈ **0.51 s/千行（生产行）**（口径见 §2.2）。frontend（占 60~70%）含超线性成分，实际拆分后**大概率比线性外推更差**——所有推演均标注「未验证」。

### 3.1 方案 A：抽出 plugin + mcp client 面为独立轻量 crate（报告 C2 的「抽面」形态）

**内容**：新建 crate（下称 `peri-plugin`），承接现 `peri-middlewares/src/plugin/`（20 文件、总 7,294 行 / 非测试 3,515 行）；`peri-middlewares` 与 `peri-tui` 改为依赖它。

**改动面**

- 需先解 `plugin ↔ hooks` 文件级互引：`plugin/loader.rs:19-21,702` 用 `hooks::{types::RegisteredHook, loader::extract_hooks}`，`hooks/loader.rs:3-6` 用 `plugin::types::PluginManifest`。`hooks/types.rs`（400 行）**无任何 crate 内依赖**（只 `use serde`），可下移到契约层，互引即解。
- `plugin/middleware.rs`（117 行）依赖 `peri_agent`，是唯一拖入 agent 的文件；TUI 与 CLI **都不用**它 → 留在 `peri-middlewares`，只把纯数据/IO 面抽走。
- `plugin` 对 `mcp` 的依赖只有 1 处（`plugin/loader.rs:21` 的 `mcp::McpServerConfig`，纯类型）→ 类型下移即可。

**编译收益推演**：**≈ 0（未验证）**。按 §3.0 的 M3 判据：`peri-plugin`(~1.7s) + `middlewares−plugin`(~19.2s) ≈ 20.9s，无变化。若**不做**方案 B/C，本方案只有架构整洁收益。

**风险**：低。纯模块搬迁 + 类型下移，可用 `check-layer-imports.sh` 与既有 plugin 测试（`loader_test` / `marketplace_test` / `installer_test`）兜底。

**前置依赖**：`hooks/types.rs` 下移；`McpServerConfig` 下移（可入 `peri-mcp-core` 或 `peri-acp-types`）。

**与任务线关系**：**同向但非充分**。它是方案 B 的**落点**（让 CLI/面板改依赖 `peri-plugin` 而非 `peri-middlewares`），本身不产生编译收益。

> **mcp client 面不必抽**：TUI 侧 mcp 残留 23 行全部可用 ACP 面替代（§2.4.3），不构成"client 面缺失"。诊断报告 C2 的「plugin + mcp client 面（约 1.5 万行）」中的 mcp 部分，若按"TUI 实际使用面"精确裁剪，会拖出 `builtin/`(1,562 非测试行) / `apps` / `skill_discovery` / `resource_cache` / `middleware` 等一大串（`mcp/initialize.rs:10` 就依赖 `builtin::runtime`），**不划算**。

### 3.2 方案 B：peri-tui 全量改经 ACP（M-TUI 收口；C2 的「收敛」形态）

**内容**：把 TUI 的 93 行归零——面板数据全部经 ACP 取数、装配职责全部交给 ACP Host、CLI 子命令改走 ACP 宿主（或改依赖 `peri-plugin`）。

**缺口清单（按 §2.1 逐项映射）**

| 缺口 | 涉及行数 | 需要的动作 |
| --- | --- | --- |
| marketplace list/add/remove 无 ACP 面 | `data.rs` 19 + `panel_handler.rs` 8 + `search_handler.rs` 5 = 32 | 新增 `marketplace/list`、`marketplace/add`、`marketplace/remove` 路由（`marketplace/refresh` 已有，可作模板） |
| MCP 池与面板直读 | `service_snapshot.rs` 11 + `atoms.rs` 1 | 删除 `MCP_PANEL_POOL`（**死代码**，§2.1.9）；`service_snapshot` 的无会话回退分支改为空投影（或复用 `mcp/list`） |
| TUI 自建第二个 MCP 连接池 | `app/mod.rs` 6 + `service_registry.rs` 3 + `launch.rs` 2 | **整体删除** `spawn_mcp_init()` / `mcp_pool` / `mcp_task_owner` / `mcp_init_rx` / `shutdown_mcp_pool`；ACP Host 侧同名装配已存在（`peri-acp/src/host/assemble.rs:98,399,668,675`），属**遗留双轨** |
| plugin 聚合装配点 | `launch.rs` 2 | `plugin_data` 改由 ACP Host 构造（`HostAssemblyInput.prepared_plugins` 已有该输入位） |
| plugin CLI 子命令 | `cli_plugin.rs` 36 | 二选一：① CLI 内启动 ACP 宿主（`cli_print.rs:156-183` 已是现成范式：`assemble_server_config` + `spawn_acp_server` + mpsc transport）；② 改依赖 `peri-plugin`（方案 A） |

**编译收益推演**：**单独实施 ≈ 0（未验证）**。切断了边 a，但边 b（`peri-acp → peri-middlewares`）仍在，`peri-tui` 的闭包仍含 middlewares，串行前驱不变。

**改动面**：`peri-tui` 9 个文件 + ACP 新增 3 条路由 + 1 组端口的消费侧改造；测试面涉及 `peri-tui` 的 panel/service_snapshot 测试与 e2e（`e2e/tests/scenarios/` 的 TUI 场景）。

**风险**：中。① 无会话窗口的面板数据显示需要产品决策（空态 or 复用上次会话快照）；② CLI 启动 ACP 宿主会改变 `peri plugin …` 的启动耗时与失败模式；③ `panel_handler.rs:230/608` 的疑似重复写（§2.1.3）必须一并裁定，否则删掉直写可能改变落盘行为。

**前置依赖**：`marketplace/*` 路由；`PluginManagerPort` 补 `claude_home` / `plugin_route_entries`。

**与任务线关系**：**就是 M-TUI 的收口批**（`scripts/import-exemptions.conf:20` 已登记 M-TUI = 「TUI 全量改经 ACP」）。批 3（tui-deps）遗留的「需新增 cron/list、mcp/list 命令面」中，**mcp/list 已完成**，marketplace 面与 cron/list 是剩余项。

### 3.3 方案 C：ACP 协议面去 middlewares（C3 / L5 收口）+ 装配面外移 + bin 拆分

这是**唯一能产生实质收益**的方案，且必须三条一起做。

**C-1 端口补全（把 `downcast_arc` 从主路径降级为例外）**

| 端口 | 需补的能力 | 覆盖的 downcast 点 |
| --- | --- | --- |
| `McpPoolPort` | ✅ **W3 已补齐 22 个方法**（`server_infos()`、`builtin_workspace_state()` + 3 个冻结读取等，见 §2.4.2）；⚠️ 未补 `connect_trusted_workspace_for_close`（静态构造，需装配工厂端口） | W3 前 10 处 `downcast_arc` + `host/workspace.rs` 3 处 `downcast_ref` 已全部替换 |
| `PluginManagerPort` | ✅ **W3 已补齐**：`claude_home()`、`enabled_plugin_commands()`、`plugin_route_entries()`、`find_marketplace_json()` | `host/requests/plugin.rs`(7) 已清零 |
| 新增「系统提示段提供者」端口（如 `PromptSectionPort`） | 6 个 middleware 的 `sections()` 静态声明 | `session/frozen.rs`(9) |
| 新增/复用 hooks 端口 | `stage_firing::{fire_pre_compact, fire_post_compact}` | `host/prompt/stage.rs`(3) |
| 复用 `ToolSearchPort` | `ExecuteExtraToolResolver` | `host/prompt.rs`(1) |
| 装配端口（已有先例） | `ProductionChainAssembler` / `BuiltinInstanceContext` / `builtin_closed_instances` / `resolve_skill_roots` | `host/assemble.rs`、`host/workspace_resources.rs`(1) |

> 先例可循：`WorkflowMiddlewareFactory` 已用这个模式收口（`peri-acp/Cargo.toml:20-23` 注释：「p1-wa 收口——ACP 不直接引用 middlewares」；`AcpServerConfig.workflow_middleware_factory: Arc<dyn peri_agent::agent::workflow::WorkflowMiddlewareFactory>`）。

**C-2 装配面外移为独立 crate（关键，但比看上去难）**

`host/assemble.rs`（45 处引用）整体移到新 crate（建议名 `peri-deployment`）。依赖方向：

```
peri-deployment ──→ peri-middlewares ──→ peri-agent ──→ peri-acp-types
        └────────→ peri-acp ──→ peri-acp-types        （协议面，不再依赖 middlewares）
```

**但装配面并不只在部署时被调用**，实测四处调用方：

| 调用方 | 位置 | 性质 |
| --- | --- | --- |
| TUI（MPSC 部署） | `peri-tui/src/launch.rs:176` | 部署装配点 |
| print / CLI | `peri-tui/src/cli_print.rs:156` | 部署装配点 |
| stdio 部署 | `peri-acp/src/host/stdio/mod.rs:190` | **在 peri-acp 内部** |
| **会话级环境装配** | `peri-acp/src/host/workspace.rs:325`（`assemble_server_config_with_mcp_profile`） | **每 session 各装配一次，在 peri-acp 内部** |

且 `WorkspaceAssembly`（`host/assemble.rs:39-45`）字段类型里就带着 middlewares 具体类型：`mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile`。

因此 C-2 的正确形态不是"搬文件"，而是：
1. 把 `McpCapabilityProfile` 等**仍是具体类型的字段**降为 `peri-acp-types` 契约类型；
2. 把装配入口以 **工厂端口**（如 `Arc<dyn SessionAssemblyFactory>`）注入 `AcpServerConfig`，具体实现留在部署 crate；
3. 这样 `peri-acp` 内部（stdio 与 per-session 两处）改为经端口调用。

**这一步的复杂度高于 M-TUI/L5 既有描述（"装配迁移落点"），是本设计中最需要独立评审的部分。**

**C-3 `peri` bin 拆为独立 package（关键，报告未列）**

Cargo 依赖是 **package 级**：只要 `peri-tui` 这个 package 依赖 `peri-deployment`/`peri-middlewares`，它的 **lib target** 也必须等这些依赖编完。因此要让 `peri-tui` lib 与 middlewares 并行，必须把 bin（`main.rs` / `cli_args.rs` / `cli_print.rs` / `cli_plugin.rs` / `cli_meta.rs`）拆到独立 package（如 `peri-cli`，`[[bin]] name = "peri"` 不变）。

- 影响面（实测）：`scripts/build-{i386,loongarch64}.sh`、`.github/workflows/pre-release.yml:55`、`npm-packages/@peri-sdk/README.md:95`、`e2e` 与多处 `docs/standards/architecture-contracts.md` 的 `-p peri-tui --bin peri` 验证命令需改为 `-p peri-cli --bin peri`。
- 若判定 bin 拆分代价过高，则本方案退化为「无收益」，应重新评估是否继续。

**编译收益推演（未验证）**

现状（依报告 §1.2，绝对值口径不一致，见 §2.5）：
```
acp-types(4.8) → resources(9.5) → middlewares(20.9) → acp(7.6) → tui-lib(9.4) → tui-bin(4.0) → link(4.0)
                                                     ≈ 67.0s（报告计 70.1s）
```
C-1+C-2+C-3 完成后：
```
分支 L：base(21.1) + middlewares(20.9) + deployment(~1.5) ─┐
分支 R：base(21.1) + acp(7.6) + tui-lib(9.4) ─────────────┴→ peri-cli(~3) → link(4.0)
T ≈ 21.1 + max(22.4, 17.0) + 3.0 + 4.0 ≈ 50.5s
```
**净收益 ≈ 16.5s（本推导）/ 保守区间 -5~-15s**，与报告 C2 的「-10~20s」量级一致。注意：**收益全部来自 M1+M2**，与"抽取 plugin"无关；`peri-plugin`（方案 A）在此路径上只是可选的落点，不是收益来源。

**风险**：高。
- 端口补全 = 把「具体类型逃生口」换成契约，需逐一核对语义等价（尤其 `connect_trusted_workspace_for_close` 这类**写**操作，端口化不能弱化 ARC-HOST-SHUTDOWN-001 的所有权顺序）。
- 装配面外移会改变 `AcpServerConfig` 的构造方与 `assemble_server_config` 的调用方（`peri-tui/src/launch.rs:176`、`cli_print.rs:156`、`peri-acp/src/host/stdio/mod.rs:190`、per-session 的 `host/workspace.rs:325`），是 deployment 生命周期 + **会话生命周期**两处的改动面。
- **会话级装配仍在 peri-acp 内部**（`host/workspace.rs:325`）——若只做"部署装配点外移"，边 b 不会断；这是本方案最容易被低估的一环。
- bin 拆分触及 CI / 打包脚本 / e2e / 契约文档的验证命令。
- `check-layer-imports.sh` 的 TUI / ACP-biz 两组豁免必须**同步删除**（脚本不检测僵尸豁免，需人工交叉验证）。

**前置依赖**：C-1 的端口设计需评审；`AcpServerConfig` 的所有权/构造位置需定案。

**与任务线关系**：**就是 L5 的收口批**（`peri-acp/Cargo.toml:20-23` 自述「L5 复核时再收」；`scripts/import-exemptions.conf` 边 2 的豁免归属全部标为 L5 宿主装配面）。

### 3.4 方案 D：拆 `peri-middlewares/src/mcp/`（报告 C4）

**内容**：把 `mcp/`（非测试 22,260 行，约占 crate 52%）拆为独立 crate。

**编译收益推演**：**取决于拆法，两种结果天差地别（未验证）**

| 拆法 | 结构 | 耗时 | 收益 |
| --- | --- | --- | --- |
| ❌ 整体切出（现状方向） | `middlewares-rest → peri-mcp` 单向（rest→mcp 43 处 ≫ mcp→rest 19 处） | 11 + 10 ≈ 21s，与现状 20.9s 持平 | **≈ 0（M3）** |
| ✅ 分层切出 | `peri-mcp-core`（pool/status/transport 等共用面）→ 兄弟层 `{mcp features, 其余中间件}` | 4 + max(7, 10) ≈ 14s | **≈ -7s（M2）** |

**要先解的环**（§2.3，比报告描述的更窄）：

1. `mcp/agent_registry.rs:577` → `subagent::infer_agent_capability`（纯函数 + `ClaudeAgentFrontmatter`）→ 下移到 `peri-mcp-core`（该 crate 已有 `agent_definition` 模块，天然落点）。
2. `mcp/skill_discovery.rs:428` → `skills::annotate_mcp_content`（纯字符串函数）→ 下移。
3. `plugin/loader.rs:21` ← `mcp::McpServerConfig`（纯类型）→ 下移。
4. `mcp → platform`（12 处）→ `platform.rs`（33 行）下移。
5. 报告所称的 `mcp ↔ assembly` 互引**不存在**（`mcp/builtin/mod.rs:21` 是注释）。

**风险**：中高。`mcp/` 是策略/生命周期密集区（`ARC-HOST-SHUTDOWN-001` / `ARC-MCP-ACP-001` 的验证入口都在 `mcp::client` / `mcp::task_scope` / `mcp::acp`），拆 crate 会移动这些测试的归属。

**前置依赖**：C-1/C-3 之后；且必须先定**分层方案**（否则收益为 0）。

**与任务线关系**：**不同向**——M-TUI/L5 是依赖方向收敛，C4 是 crate 粒度拆分；无既有任务线承接，需独立立项。

---

## 四、推荐与排序

### 4.1 推荐

**推荐路线：C（主干） → B（落点，切边 a） → A（可选落点） → D（独立评估）**

理由：

1. **只有 C 能真正省时**：它是唯一切断 `peri-acp → peri-middlewares` 的方案，而该边是当前关键路径上"middlewares 必须早于 acp、acp 必须早于 tui"的根本原因。
2. **B 必须与 C 同批**：任一条边保留收益即为 0（§3.0 M1 判据）。把 B 拆到 C 之后单独实施，等于做了一整轮重构却看不到任何编译改善——**难以向评审交代，也容易半途而废**。
3. **A 不是收益项而是备选项**：只有当 CLI/面板被判定"必须保留本地实现、不能走 ACP"时才需要 `peri-plugin`；否则 B 的 ② 路线（CLI 起 ACP 宿主）已足够。
4. **D 最后且需独立设计**：分层拆法才有效，整体切出无收益。

### 4.2 实施顺序

| 阶段 | 内容 | 编译收益 | 是否过架构评审 |
| --- | --- | --- | --- |
| **P0（前置，单独可交付）** | 删死代码：`MCP_PANEL_POOL`（atoms.rs:511 + app/mod.rs:153）→ 93 行变 92 行，豁免清单可少一个文件——**W2 已实施（commit `954231ca`，2026-10-05）** | 0 | 否 |
| **P1** | **C-1 端口补全**（`McpPoolPort` / `PluginManagerPort` / 段落端口）+ C-2 装配面外移（含 **per-session 装配端口化**，§3.3）→ 此时 `peri-acp` 去 middlewares——**W3 已完成 `McpPoolPort` + `PluginManagerPort` 两行（全部 downcast 逃生口清零，101 → 63 行；与 main 集成后 62 行，§2.4.2）**；段落端口 / hooks 端口 / C-2 装配面外移**未实施** | ≈ 0（`peri-tui` 直连边仍在，且 acp 与 middlewares 串行关系不变） | ✅ **必须** |
| **P2** | **B：TUI 侧收口**（面板/装配/CLI）+ C-3 bin 拆分 | **-5~-15s（未验证）** | ✅ **必须** |
| **P3** | A：`peri-plugin` 抽取（**仅当** P2 选择「CLI 保留本地实现」路线） | 0（架构整洁 + 为后续 crates 复用） | 建议 |
| **P4** | D：`mcp/` 分层拆分（需独立设计与立项） | -7s 量级（未验证） | ✅ 必须 |

**P1 与 P2 不可拆开验收**：P1 单独合并后，任何全量构建的对照测量都会显示"零收益"，容易被判为无效重构；应作为**同一条任务线的两个批次**，在 P2 完成后统一做前后对照。

### 4.3 排序理由与反例说明

- **不做"先易后难"**：P0 之后没有"容易且有效"的选项——B 单独收益为 0、A 收益为 0。因此排序依据不是难度，而是**收益依赖关系**。
- **不建议先做 C4**：在 peri-acp 仍持有 middlewares 时，`mcp/` 拆分只增加 crate 边界，无法让 `peri-tui` 受益（middlewares 仍在链上）。
- **不与 C4 抢同一段链**：C4 作用于 `peri-middlewares`（中段 20.9s），本方案作用于**该段的串行前驱关系**。两者可叠加，但 C4 必须先做分层（§3.4），否则收益为 0。
- **与 C1（`kit/` 拆分）存在收益重复计算风险**：并行产出的姊妹设计 `spec/issues/2026-10-05-peri-tui-kit-extraction-design.md` 把「kit 起跑点从 ≈88s 提前到 ≈53s」记为 C1 的 C-4 收益（该文自己也标注「这是 C2/C3 的收益，不应重复计入 C1」）。该效应与本设计「`peri-tui` lib 不再等 `peri-middlewares`」是**同一个并行化机制（M2）的两种切法**。评审时**这条收益只能计入一次**，并据实施顺序决定归属；两文均不得各自全额认领。
- **不建议只做 B**：见 4.2 的验收陷阱。

---

## 五、与 M-TUI / L5 任务线的合并评审结论

### 5.1 结论：**是同一任务线**，且应合并为一次评审

三条独立证据：

1. **豁免归属**：`scripts/import-exemptions.conf` 中，本设计要清掉的每一行都已被标为 M-TUI 或 L5 的待收项——
   - 边 1（TUI）全部豁免归属写作「全部 M-TUI（C 类装配/数据源直读）」，并注明「全部随 M-TUI 收紧后移除」；
   - 边 2（ACP 业务面）豁免归属写作「ACP Host 部署装配点（M-TUI 装配迁移落点）」「L5 宿主装配面」。
2. **代码自述**：`peri-acp/Cargo.toml:20-23` 写「peri-middlewares：L5 宿主装配面引用 …… 保留（L5 复核时再收）」；`peri-acp/src/host/assemble.rs` 注释写「M-TUI 收口：middlewares 具体实现由 ACP Host 装配面内部构造；TUI 只提供协议面输入」；`peri-acp-types/src/ports.rs:71-76` 写「`shutdown` / `snapshot` 为 M-TUI 收口新增数据端口（… TUI 不再直持具体句柄与 watch channel）」——**而 TUI 至今仍直持句柄**，即 M-TUI 的收口动作做了一半。
3. **契约**：`ARC-HOST-SHUTDOWN-001` 的 Scope 已把「`peri-tui` MCP panel / 内嵌 ACP deployment」与「`peri-middlewares` MCP pool」写进同一条契约，并明确「ACP 仅经 `peri-acp-types::ports::McpTaskOwnerPort` 持有」。

因此：**C2 ≡ M-TUI 的收口批；C3 ≡ L5 的收口批**；本设计不新开任务线，而是为这两条既有线提供**合并的设计输入与统一验收口径**。

### 5.2 但有 **2 项超出既有任务线**，需在评审中显式立项

| 新增项 | 为什么既有任务线不覆盖 | 归属建议 |
| --- | --- | --- |
| **装配面外移为独立 crate**（§3.3 C-2） | M-TUI/L5 的既有描述都是「迁移落点」= 把 middlewares 具体实现的构造**搬进** peri-acp 的 `host/assemble.rs`；本设计要求把它再**搬出** peri-acp。方向相反，属新决策 | 需要架构评审单独立项（可命名 `peri-deployment`） |
| **`peri` bin 拆为独立 package**（§3.3 C-3） | 既有任务线不涉及 package 结构；这是 Cargo 语义（package 级依赖）逼出来的 | 需评审 + 变更管理（CI / 打包 / e2e / 契约文档命令） |

### 5.3 协调方式（建议）

1. **一次评审、两个批次验收**：P1（C：端口 + 装配面）与 P2（B：TUI 收口 + bin 拆分）合并评审，分两批落地；不要拆成两个 issue 分别验收（§4.2 的"零收益陷阱"）。
2. **进度事实源用豁免清单**：`import-exemptions.conf` 的 TUI-use / TUI-fullpath / ACP-biz-use / ACP-biz-fullpath 四条规则的**豁免回收**即为进度看板——豁免清零 ⇔ 边切断。建议在 conf 头部「收紧任务对照」中为 M-TUI / L5 追加一行收口批注记，指向本文。
3. **不在本文改动任何任务线文件**：本文只作设计输入；`import-exemptions.conf`、`Cargo.toml`、契约文档的具体修改应在实施批次内进行。
4. **与第一梯队并行不冲突**：A1（release LTO）等配置层手段与本文无依赖关系，可先行落地。

### 5.4 豁免回收的具体清单（实施时使用，实测）

`bash scripts/check-layer-imports.sh` 当前 **✅ 22 条规则 / 0 违规**（本文实测）。P2 完成后：

| 豁免文件 | 现状 | P2 后 |
| --- | --- | --- |
| `app/cron_state.rs` | **已是僵尸豁免**（对该模式 0 命中；`peri_mcp_cron` 不在禁入列表内） | **现在即可移出**（与本设计无关的独立清理项） |
| `launch.rs` / `cli_plugin.rs` / `app/service_registry.rs` / `kit/atoms.rs` / `kit/panels/plugin/*` / `kit/service_snapshot.rs` | 仅 middlewares 命中 | **可移出** |
| `app/mod.rs` | 6 处 middlewares + **1 处 `peri_resources`（:94）** | 保留（待 TUI→Resources 收敛） |
| `cli_print.rs` / `cli_meta.rs` | 仅 `peri_resources` 命中 | 保留（M-res / TUI→Resources 线） |
| `kit/workflow_snapshot.rs` | 仅 doc comment 提及 `peri_workflow::` | 保留至注释改写 |
| `main.rs` | 已清零（2026-08-06） | — |

> ⚠️ `check-layer-imports.sh` **不检测僵尸豁免**（脚本只校验"命中是否都在豁免内"，不校验"豁免是否仍被命中"）。conf 头部约定的「验收时逐一移除交叉验证」必须人工执行。

---

## 六、验证方式

### 6.1 收益验证（编译时间）

**前置**：按诊断报告 §五 D2，构建密集测量必须使用独立 `CARGO_TARGET_DIR` 且避免多会话并发（本次诊断的时效数据已被并发污染）。建议在 P1 前、P2 后各做一次**无并发基线**。

```bash
# 基线 A：TUI 方向（关键路径主指标）
CARGO_TARGET_DIR=/tmp/peri-decouple-before ./scripts/cargo-rmcp-patched.sh \
  build --offline --locked -p peri-tui --bin peri --timings
# P2 后（bin 拆分后包名变化，按实施结果替换 -p 目标）
CARGO_TARGET_DIR=/tmp/peri-decouple-after ./scripts/cargo-rmcp-patched.sh \
  build --offline --locked -p <new-bin-package> --bin peri --timings

# 基线 B：CI 等价目标
./scripts/cargo-rmcp-patched.sh build --offline --locked --workspace --all-targets

# 关键路径与单元耗时：解析 target/cargo-timings/cargo-timing-*.html 内嵌 UNIT_DATA
```

**判读口径（重要）**：

- 主指标是 **`-p peri-tui --bin peri`（或拆分后的等价包）的 clean 构建墙钟**，不是 `--workspace`（workspace 全量仍必须编 middlewares，收益不可见）。
- 必须同时记录 **关键路径**（而非只有总时长）：预期变化是「middlewares 从 peri-tui 的前驱变为与其并行的分支」，总时长可能只降 5~15s，但**并行度**与**关键路径构成**会明显改变。
- 增量场景（改 `peri-middlewares` 一个文件）也应复测：预期 `peri-tui` lib 不再重编（因为不再依赖它）——这是**日常开发体感**的直接改善，可能比全量数字更重要。
- 报告 §2.5 已指出两处口径不一致与 70.1s 的 7~11s 缺口，基线复测时一并厘清。

### 6.2 正确性验证

| 面 | 手段 |
| --- | --- |
| **依赖方向门** | `bash scripts/check-layer-imports.sh`（退出码 0 = 通过）；豁免按 §5.4 同步回收并逐条交叉验证 |
| **TUI / ACP 边界契约** | `ARC-BOUNDARY-001` Verify 列：TUI 的 prompt / cancel / session 请求经 ACP client；Agent 执行入口留在 ACP 会话路径 |
| **MCP 关闭所有权** | `ARC-HOST-SHUTDOWN-001` Verify 列：`cargo test -p peri-acp --lib -- host::task_scope`、`host::stdio::run_server_integration_tests`、`cargo test -p peri-middlewares --lib -- mcp::task_scope`、`mcp::client`、`cargo test -p peri-tui --lib -- app::mcp_lifecycle_tests`、`cargo test -p peri-tui --lib -- acp_client::deployment::tests`——**注意**：P2 会删除 TUI 侧 `app::mcp_lifecycle_tests` 覆盖的对象，测试需随之迁移到 ACP Host 侧而非删除 |
| **MCP over ACP** | `ARC-MCP-ACP-001` Verify 列：`cargo test -p peri-middlewares --lib -- mcp::acp`、`cargo test -p peri-acp --lib -- acp_mcp` |
| **冻结段一致性** | `ARC-FROZEN-001`；P1 新增段落端口后，必须验证「链装配」与「冻结收集」仍同源（`session/frozen.rs:150-162` 明确要求单一事实源、禁止双轨） |
| **面板行为** | `peri-tui` 的 panel / service_snapshot 测试；`marketplace/*` 新路由需补 ACP 侧契约测试 |
| **端到端** | `e2e/tests/scenarios/` 的 TUI 与 print 场景（tmux 驱动 + 假 model server），重点覆盖 plugin 面板与 MCP 面板 |
| **CLI** | `cargo test -p <bin-package> --bin peri -- cli_print`（原 `-p peri-tui --bin peri -- cli_print`）；`cargo test -p peri-tui --test print_exit` |
| **回归闸门** | `clippy` / `check`（pre-commit 全 workspace，见诊断报告 §五 A3 另有优化空间） |

### 6.3 分阶段验收标准

| 阶段 | 验收标准 |
| --- | --- |
| P0 | `check-layer-imports.sh` 通过；`app/cron_state.rs`、`kit/atoms.rs` 从豁免清单移出后门仍通过 |
| P1 | `peri-acp` 对 `peri_middlewares` 的 `grep` 命中归零（含测试外的全部路径）；ACP-biz 两条豁免删除；`ARC-HOST-SHUTDOWN-001` / `ARC-MCP-ACP-001` 验证命令全绿；**不宣称编译收益** |
| P2 | TUI 两条豁免删除；`-p <bin> --bin peri` clean 构建对照有可复现的下降；增量场景（改 middlewares）不再重编 TUI lib；e2e 全绿 |
| P3/P4 | 另行设计，本文不预设验收 |

---

## 七、未决事项与不确定项（诚实清单）

1. **所有编译收益数字均为推演，未实测**。本文未运行任何 cargo 命令。线性外推系数（0.21~0.49 s/千行）对 frontend（宏展开/类型检查，超线性）不可靠；拆分后多出的 crate 边界本身也有开销。**要求 P2 前后各做一次无并发基线**。
2. **报告 §1.2 的口径不一致未解**（peri-tui 的 4.0s 是 bin 还是 link；70.1s 与所列单元之和差 7~11s）。收益推演的分母因此不稳定。
3. **`panel_handler.rs:230/608` 的 `save_claude_settings_enabled_plugins` 是否与 ACP `plugin/toggle` 重复写**，未做运行时确认——可能影响 P2 的删除范围，需现场核对落盘行为。
4. **无活跃会话窗口的面板显示**：`service_snapshot.rs:279-288` 的直读回退分支被删后，会话建立前的 MCP/plugin 面板显示什么，属产品决策，未定。
5. **CLI 走 ACP 的可行性未验证**：`peri plugin …` 在无 provider 配置（首次运行）时能否装配 ACP Host（`assemble_server_config` 需要 `LlmProvider`，`cli_print.rs:156-183` 有前置条件）——需确认。
6. **`peri-deployment` / `peri-cli` 的命名、归属与打包影响**未评估（npm 包 `@peri-sdk`、跨平台构建脚本、发布流程）。
7. **C4 的分层方案未设计**：本文只给出"必须分层才有效"的判据与需先解的 4 条环，具体层次划分需独立设计。
8. **本设计与 `kit/` 拆分（报告 C1）的关系**：姊妹设计 `spec/issues/2026-10-05-peri-tui-kit-extraction-design.md` 得出「纯 C1 净收益接近零、应降级为 C2/C3 落地后的结构性收口」的结论，与本设计方向一致；但两文对「peri-tui/kit 起跑点提前」这同一条收益均有主张，**存在重复计算风险**（§4.3）。两者应放在同一份结构改造路线图里一起排序、统一认领收益。
9. **未评估 CI runner（2~4 核）下的收益**：本机 18 核的并行为主收益在低核机器上会被压缩，甚至可能因拆分多出的 crate 数而变差；诊断报告 §五待决 3 已提出需要真实 CI 基线。
10. **C-2 的工作量未估算**：per-session 装配仍在 peri-acp 内部（`host/workspace.rs:325`），且 `WorkspaceAssembly.mcp_profile` 携带 `peri_middlewares::mcp::apps::McpCapabilityProfile` 具体类型。C-2 需要「契约类型下移 + 装配工厂端口 + 两处内部调用改道」三件事，本文只给出形态，**未给工作量与排期**——这直接影响排序决策，应作为评审必答项。

---

## 附录 A：本文全部统计命令

```bash
# A.1 TUI → middlewares 引用（§2.1）
cd peri-tui
grep -rnE "peri_middlewares" src --include="*.rs" | grep -vE "_test\.rs|_test/|/tests/" | wc -l          # 93
grep -rnE "peri_middlewares" src --include="*.rs" | grep -vE "_test\.rs|_test/|/tests/" \
  | cut -d: -f1 | sort | uniq -c | sort -rn                                                              # 9 文件分布
grep -rhoE "peri_middlewares::(plugin|mcp)" src --include="*.rs" | sort | uniq -c                        # 70 / 23
grep -rn "MCP_PANEL_POOL" --include="*.rs" .. | grep -v "/target/"                                       # 死代码取证

# A.2 middlewares 结构画像（§2.2）
cd peri-middlewares/src
find . -name '*.rs' | wc -l                                                                              # 302
find . -name '*.rs' -exec cat {} + | wc -l                                                               # 98872
find . -name '*.rs' ! -name '*_test.rs' -exec cat {} + | wc -l                                            # 42704
for d in */; do echo "$(find $d -name '*.rs' -exec cat {} + | wc -l) $d"; done | sort -rn                 # 分目录总行
find mcp -name '*.rs' ! -name '*_test.rs' -exec cat {} + | wc -l                                          # 22260

# A.3 模块耦合矩阵（§2.3）
cd peri-middlewares/src
grep -rn "crate::mcp" assembly assembly.rs --include="*.rs" | grep -vE "_test\.rs:" | wc -l               # 20（校准点）
for a in <模块>; do for b in <模块>; do grep -rnE "crate::$b(::|\b)" $a --include="*.rs" | grep -vE "_test\.rs:" ; done; done
grep -rn "crate::assembly" mcp --include="*.rs" | grep -vE "_test\.rs:"                                   # 1（doc comment）

# A.4 peri-acp → middlewares（§2.4.2）
cd peri-acp
grep -rnE "peri_middlewares" src --include="*.rs" | grep -vE "_test\.rs|_test/|/tests/" | wc -l          # 101
grep -rnE "peri_middlewares" src --include="*.rs" | grep -vE "_test\.rs|_test/|/tests/" \
  | cut -d: -f1 | sort | uniq -c | sort -rn                                                               # 14 文件分布

# A.5 依赖图与端口（§2.4.1 / §2.4.3）
grep -rn "peri-middlewares" --include="Cargo.toml" . | grep -v "/target/"                                 # 2 个下游
grep -n "peri-middlewares" peri-acp/Cargo.toml                                                           # :39 硬依赖
grep -rn "McpPoolPort for\|McpTaskOwnerPort for" --include="*.rs" . | grep -v "/target/"
grep -n "^pub trait" peri-acp-types/src/ports.rs peri-acp-types/src/plugin.rs peri-acp-types/src/hooks.rs

# A.6 依赖门与豁免（§5.4 / §6.2）
bash scripts/check-layer-imports.sh                                                                       # ✅ 22 规则 / 0 违规
sed -n '1,60p' scripts/import-exemptions.conf                                                             # 收紧任务对照
```

---

## 附录 B：本文未做的事

- 未修改任何源码、`Cargo.toml`、`scripts/import-exemptions.conf` 或契约文档；
- 未运行 cargo（构建测量由并行的另一 agent 负责）；
- 未执行 `git add` / `git commit`；
- 未对 §七 列出的 9 项不确定内容做任何运行时验证。
