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
| A 固定基线与契约 | 已完成 | 只读侦查：面板/弹窗清单、按类矩阵（只读/配置/资源/复合）、确认身份审计、hook/render 边界、快照审计、测试清单；基线测试与体积见下 |
| B 共享列表与正确交互 | 基本完成 | Hooks、MCP 已迁移并拆文件；MCP 详情稳定身份 + effect 收敛；剩余：把共享视口扩展到批次 C 面板 |
| C 操作机制与简单面板 | 进行中 | Cron、Memory 已迁移（稳定确认目标、视口/命中同源）；Tasks、Agent、Status、Goal 进行中；「作用域与完成门」原语落 `application/operations/` 并先迁移 Plugin 搜索/操作 |
| D 宿主配置与向导 | 进行中 | Theme、Login 正确性修复中（渲染体写状态、hook 顺序、命中差 1 行、Provider 删除身份）；Config、Model 迁移中（命中同源、保存失败如实展示） |
| E 复杂面板与快照 | 未开始（已侦查） | Plugin、ThreadBrowser、Workflow 缺陷已逐行确认；快照拆分边界已确认（提交/失效门保持单点） |
| F 详情、弹窗和清理 | 未开始 | 详情迁移共享外壳；AskUserPopup 渲染体内写状态；`confirm_popup` 执行时重读全局 payload 的语义待核 |
| G 整体验收与文档固化 | 进行中（文档随批次同步） | 本 issue + design 已建；体积/性能对照与 Linux 口径待补 |

## 基线（可复现）

- 测试：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib` 在 `56a8ccd8d` 为
  **1845 passed / 0 failed / 6 ignored**；本分支当前为 **1876 passed / 0 failed / 6 ignored**。
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
| `753ca0fda` | 目标设计 `docs/design/tui-panel-architecture.md`；本 issue；`peri-tui/CLAUDE.md` 不变量；代码索引与 README 路由同步 |
| `0f3015a18` | Memory 面板接入共享视口（删 `VISIBLE_ITEMS = 13`、具名 `handle_memory_event`、滚动区域与命中同源、编辑器打开加测试接缝），新增 7 个面板级用例 |
| `4c79289b2` | Cron 面板：确认删除改为持有 `job.id`（刷新/重排不删错任务）、确认态一次性清除的完成门、目标消失不删并留痕、事件入口具名化 + 动作出口可注入、区域同源，新增 9 个面板级用例 |

## 决策记录（本次授权内的裁决）

1. **依赖方向**：`application/operations/` 保持纯状态机，不读 kit atoms；`OperationScope` 由调用方构造，避免 `application → kit` 反向依赖（取值留在装配层/适配器）。
2. **超时**：不给 `send_raw_request` 增加超时——不在本批授权范围，保持既有语义；插件搜索仍是无时限路径，记为未决风险。
3. **代次过滤**：把只带会话身份的退化实现升级为带显式代次，属落实设计 §6.1「请求序号过滤被替代请求」，不视为产品行为变更。
4. **任务所有权**：本批只落「请求协调」的最小状态门；已发送变更的任务所有权与应用层结果跟踪留待后续，如实记为未完成。
5. **确认目标消失的提示**：Cron 目标消失当前只留 `tracing::warn` 并退出确认态，无 locale 文案；是否需要用户可见提示待定（需新增 locale key）。

## 已确认缺陷（批次 A 审计，尚未全部修复）

- 已修复 Cron：删除确认只存 `bool`、执行时按当前下标重取——`4c79289b2` 改为持有 `job.id`，并加「目标消失不删」「重复 Enter 不连发」完成门；toggle 连按的门仍待共享原语。
- 已修复 Memory：可见项写死 13、滚动区域与命中区域来源不一致——`0f3015a18` 统一为 `ListViewport` 与 `AreaTracker`。
- Login（批次 D）：`delete_provider(cursor)` 在确认执行时按 cursor 重新读 `PROVIDER_LIST` 取 ID（修复中）。
- Theme（批次 D）：渲染体内 `preview_cache.write().insert(...)` 两处；`use_state` 初始化器内切换主题 atoms；命中表头比绘制少算 1 行（修复中）。
- Tasks（批次 C）：取消任务在 RPC 前按当前下标取 `task_id`、无活动会话时 `unwrap_or_default()` 静默发送、选中越界时静默关面板（修复中）。Workflow kill 同型。
- Workflow（批次 E）：渲染体内写 `active_run`/`run_sync_missing`/`preselect_key`；`phase_sel`/`agent_sel` 亦在渲染体内 `.write()`；`kill_run` 无快照代次校验。
- Plugin（批次 E）：插件详情与市场详情按 index 定位，而列表每 2s 被重写。
- ThreadBrowser（批次 E）：归档回写无会话/代次/过滤复验；任意键清 `operation_loading` 并按会话无关方式刷盘缓存。
- AskUserPopup（批次 F）：payload 指纹变化分支在渲染体内写 `focused`/`answers`/`focused_option`/`session_fingerprint`。
- `confirm_popup`：确认执行时重读全局 `CONFIRM_PAYLOAD`（不是展示时的副本），未校验版本。

## 待完成 / 风险

1. 批次 C 剩余：Tasks、Agent、Status、Goal 迁移；`application/operations` 落地后迁移 Plugin 搜索/操作，随后接入 Cron 的 toggle 完成门与 Tasks 的取消票据。
2. 批次 D 剩余：Theme/Login/Config/Model 迁移收尾；「集中配置服务」抽象与向导字段未开始。
3. 批次 E/F 未开始；快照拆分必须保持「提交与失效门」单点，不得分散进各刷新单元。
4. 体积与性能对照未做（见基线）；Linux 口径缺失。
5. 迁移期间面板新旧实现并存，属批次内允许状态；每完成一个面板即删除其被替代逻辑，不留双写。
