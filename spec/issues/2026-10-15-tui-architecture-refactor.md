# TUI 面板架构重构（active）

- 状态：进行中；批次 A→G 全部在本次授权范围内推进，按批次提交并如实标注未完成项。
- 设计：[docs/design/tui-panel-architecture.md](../../docs/design/tui-panel-architecture.md)
- 来源方案：用户批准的《peri-tui 面板架构重构方案》（A→G 批次；本次授权的唯一方案口径）。
- 分支：`refactor/tui-panel-architecture`（从 `56a8ccd8d` 用 `git switch --no-track` 建立，无 upstream）。

## 硬约束

1. 完整保留 TUI 能力，只整理 TUI 内部架构；不改 ACP 协议、后端实现与依赖版本。
2. 不升级依赖、不改 release profile / `Cargo.lock` 制造体积收益；新增生产代码只允许落在 `peri-tui`。
3. 不建立第二套全局状态；渲染体内不写状态（TUI-RENDER-001）；hooks 必须在条件分支之前（TUI-HOOK-001）。
4. 单文件 ≤1000 行（STD-SIZE-001）；只暂存本任务路径，不动 `peri-cool` 子模块既有改动（GIT-COMMIT-001）。

## 批次进度

| 批次 | 状态 | 证据 / 备注 |
| --- | --- | --- |
| A 固定基线与契约 | 已完成 | 9 份只读侦查报告（面板/弹窗清单、确认身份审计、hook/render 边界、快照、测试清单）；基线测试与体积见下 |
| B 共享列表与正确交互 | 基本完成 | Hooks、MCP 已迁移并拆文件；MCP 详情稳定身份 + effect 收敛；剩余：把共享视口扩展到批次 C 面板 |
| C 操作机制与简单面板 | 进行中 | Cron、Memory 迁移中（见“待完成”）；Tasks/Agent/Status/Goal 未开始；作用域与完成门共享原语待两类以上真实用例落地后提取 |
| D 宿主配置与向导 | 未开始 | Config、Model、Login、Theme；已确认缺陷：Provider 删除按 cursor 重新解析（`config_store`）、Theme 渲染体内写预览缓存 |
| E 复杂面板与快照 | 未开始 | Plugin、ThreadBrowser、Workflow；已确认缺陷：Workflow 渲染体内写状态；快照协调器按类拆分 |
| F 详情、弹窗和清理 | 未开始 | 详情迁移共享外壳；AskUserPopup 渲染体内写状态；`confirm_popup` 执行时重读全局 payload 的语义待核 |
| G 整体验收与文档固化 | 进行中（文档已在写） | 本 issue + design 已建；代码索引、`peri-tui/CLAUDE.md`、体积/性能对照待补 |

## 基线（可复现）

- 测试：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib` 在 `56a8ccd8d` 为
  **1845 passed / 0 failed / 6 ignored**；本分支当前为 **1860 passed**（新增面板级回归）。
- 二进制（macOS ARM64，release，`strip = "symbols"`）：干净基线构建 `/tmp/peri-tui-baseline`
  （detached 于 `56a8ccd8d`）= **21,745,968 bytes**。当前分支的对照构建与 Linux `.text`
  归属尚未测量，批次 G 补齐；未测量的平台不得宣称通过。
- 性能口径（批次 G 执行，需配对测量中位数）：warm frame `scanned_slots=0/updated_slots=0`、
  缓存重绘 `full_parses=0`、同 key ≤1 在途查询、整体中位数退化 ≤10%。

## 已完成（按提交）

| 提交 | 内容 |
| --- | --- |
| `6ce250fe5` | `panels/shared/list` 共享列表视口（`ListMetrics`/`ListViewport`/`content_rows`）；`panel_height`/`panel_content_rows` 抽出为唯一权威；Hooks 面板接入（删除写死可见项、click as enter、滚动条列排除、区域同源） |
| `af32b490d` | MCP 面板接入共享视口；详情改稳定身份（server 名）并由 `use_effect` 收敛；列表左键点击进详情；新增动态视口/稳定身份/目标消失/点击进详情回归；`mcp.rs` 拆分出 `mcp/detail.rs` 与 `mcp/tests.rs`（该提交消息只写了拆分，迁移内容同属本提交） |
| `c43d537d7` | `McpEventCtx` 收敛事件入口签名（消除 clippy `too_many_arguments`）；`hooks_test` 探针类型别名 |

## 已确认缺陷（批次 A 审计，尚未全部修复）

- Cron：删除确认只存 `bool`，确认执行时按当前下标重取 `CRON_JOBS.get(selected)`——列表 2s 刷新即可删除另一个 job；toggle 连按无完成门。
- Memory：可见项写死 13；事件判定匿名内联，滚动区域与命中区域来源不一致（`use_previous_size` vs `AreaTracker`）。
- Login（批次 D）：`delete_provider(cursor)` 在确认执行时按 cursor 重新读 `PROVIDER_LIST` 取 ID。
- Workflow（批次 E）：渲染体内写 `active_run`/`run_sync_missing`/`preselect_key`；`phase_sel`/`agent_sel` 亦在渲染体内 `.write()`。
- Theme（批次 D）：渲染体内 `preview_cache.write().insert(...)` 两处；`use_state` 初始化器里切换主题 atoms。
- AskUserPopup（批次 F）：payload 指纹变化分支在渲染体内写 `focused`/`answers`/`focused_option`/`session_fingerprint`。
- Tasks（批次 C）：取消任务在 RPC 前按当前下标取 `task_id`（无确认步骤）；Workflow kill 同型。
- `confirm_popup`：确认执行时重读全局 `CONFIRM_PAYLOAD`（不是展示时的副本），未校验版本。

## 待完成 / 风险

1. 批次 C 剩余：Tasks、Agent、Status、Goal 迁移；“作用域与完成门”共享原语在第三处真实用例出现后提取（当前重复实现位于插件搜索、插件操作、Cron 会话守卫）。
2. 批次 D/E 未开始；涉及配置写入目标与快照拆分，需要在实现前核对 `configuration-authority` 与快照协调器边界。
3. 体积与性能对照未做（见基线）；Linux 口径缺失。
4. 迁移期间面板新旧实现并存，属批次内允许状态；每完成一个面板即删除其被替代逻辑。
