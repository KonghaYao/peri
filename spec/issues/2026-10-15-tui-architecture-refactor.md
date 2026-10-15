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
| B 共享列表与正确交互 | 已完成 | Hooks、MCP 迁移并拆文件；MCP 详情稳定身份 + effect 收敛；共享视口已扩展到批次 C 的全部固定行高面板 |
| C 操作机制与简单面板 | 基本完成 | Cron、Memory、Tasks、Agent、Status、Goal 六个面板已迁移；`application/operations/` 原语落地并接入 Plugin 搜索与操作。剩余：Cron 会话级守卫与 Tasks 取消尚未接入原语（无显式代次/票据） |
| D 宿主配置与向导 | 基本完成 | Config、Model、Login、Theme 四个面板已迁移（命中同源、渲染边界、确认身份、保存失败如实展示）。剩余：「集中配置服务」抽象与向导字段未开始 |
| E 复杂面板与快照 | 未开始（已侦查） | Plugin、ThreadBrowser、Workflow 缺陷已逐行确认；快照拆分边界已确认（提交/失效门必须保持单点） |
| F 详情、弹窗和清理 | 未开始 | 详情迁移共享外壳；AskUserPopup 渲染体内写状态；`confirm_popup` 执行时重读全局 payload 的语义待核 |
| G 整体验收与文档固化 | 进行中 | 本 issue + design 随批次更新；体积/性能对照、Linux 口径与 e2e 未跑 |

## 基线（可复现）

- 测试：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib` 在 `56a8ccd8d` 为
  **1845 passed / 0 failed / 6 ignored**；本分支当前为 **1956 passed / 0 failed / 6 ignored**。
- 二进制（macOS ARM64，release，`strip = "symbols"`）：干净基线构建 `/tmp/peri-tui-baseline`
  （detached 于 `56a8ccd8d`）= **21,745,968 bytes**。当前分支的对照构建与 Linux `.text`
  归属尚未测量，批次 G 补齐；未测量的平台不得宣称通过。
- 性能口径（批次 G 执行，需配对测量中位数）：warm frame `scanned_slots=0/updated_slots=0`、
  缓存重绘 `full_parses=0`、同 key ≤1 在途查询、整体中位数退化 ≤10%。

## 已完成（按提交）

| 提交 | 内容 |
| --- | --- |
| `6ce250fe5` | `panels/shared/list` 共享列表视口（`ListMetrics`/`ListViewport`/`content_rows`）；`panel_height`/`panel_content_rows` 抽出为唯一权威；Hooks 面板接入 |
| `af32b490d` | MCP 面板接入共享视口；详情改稳定身份（server 名）并由 `use_effect` 收敛；点击进详情；`mcp.rs` 拆出 `mcp/detail.rs`、`mcp/tests.rs`（提交消息只写了拆分，迁移内容同属该提交） |
| `c43d537d7` | `McpEventCtx` 收敛事件入口签名；`hooks_test` 探针类型别名 |
| `753ca0fda` | 目标设计 `docs/design/tui-panel-architecture.md`、本 issue、`peri-tui/CLAUDE.md` 不变量与路由同步 |
| `0f3015a18` | Memory 面板接入共享视口（删写死可见项、具名 `handle_memory_event`、滚动区域与命中同源、编辑器打开测试接缝） |
| `4c79289b2` | Cron 面板：确认删除改为持有 `job.id`、确认态一次性清除的完成门、目标消失不删、事件入口具名化 + 动作出口可注入、区域同源 |
| `51f4b01b2` | issue 文档同步批次 C/D 进度与决策记录（上一轮） |
| `928837700` | i18n：确认目标守卫文案与 Tasks 取消三态文案（不再借用 Cron 与空态文案） |
| `bff52f09c` | Cron 确认目标消失时给出可见提示 |
| `5b85c4832` | `application/operations/` 操作作用域与完成门原语（纯状态机、Drop 取消、结果未知三态）；Plugin 搜索与操作接入并删除被替代实现 |
| `60bf42af3` | Config：命中/绘制同源、保存失败如实展示（候选副本落盘成功才提交）、按职责拆 `config/rows.rs` 与 `config/actions.rs` |
| `6828a7eba` | Model：命中/绘制同源、保存失败不再假装成功（不写内存、不投影、不推送） |
| `0f1d6b351` | Theme：hooks 前置、三处渲染期副作用移到 effect、命中表头补回预览标题行、选择身份改为主题名 |
| `bc6140eac` | Login：删除确认冻结 provider id、目标消失不请求并提示、编辑态回退改由 effect 承担、Edit 视图拆分 |
| `02f36814d` | Tasks：三段行表统一绘制/命中/取消解析、选中收敛、取消身份与结果可见、鼠标 click as enter、测试拆文件 |
| `1f9aa8c71` | Agent/Status/Goal：hook 顺序修正、Agent 命中改用实测视口（修 1 列偏差）、点击选择、cursor 收敛、Status 首次有测试 |

## 决策记录（本次授权内的裁决）

1. **依赖方向**：`application/operations/` 保持纯状态机，不读 kit atoms；`OperationScope` 由调用方构造，避免 `application → kit` 反向依赖。
2. **超时**：不给 `send_raw_request` 增加超时——不在本批授权范围，保持既有语义；插件搜索仍是无时限路径。
3. **代次过滤**：把只带会话身份的退化实现升级为带显式代次，属落实设计 §6.1「请求序号过滤被替代请求」，不视为产品行为变更。
4. **任务所有权**：本批只落「请求协调」的最小状态门；已发送变更的任务所有权与应用层结果跟踪留待后续，如实记为未完成。
5. **确认目标消失的提示**：统一为可见提示并共用跨面板文案 `app-confirm-target-missing`（Cron 与 Login 同一语义）。
6. **滚轮语义**：视口接管后滚轮不再滚出被裁剪的行（可见范围即窗口、窗口跟随选中项），已在批次 B 接受，各面板一致，记于设计文档。
7. **Cron/子代理行的 Enter**：保持「关闭面板」的既有语义（与 `panel-tasks-nav-hint-close` 文案及既有用例一致）；只有可取消的后台任务行执行取消。
8. **行高可变面板**：Agent 不接入共享视口，复用既有逐行测量；分段列表（Tasks）自建行表。两者都在文件内写明理由与替代做法。

## 已确认缺陷（批次 A 审计，按最新状态标注）

- 已修复 Cron：删除确认按下标重取；已修复 Memory：可见项写死与区域来源不一致；已修复 Tasks：取消按下标取 `task_id`、无会话静默发送、越界静默关面板；已修复 Config/Model：保存失败静默或假装成功；已修复 Theme：渲染期写状态、hook 顺序、命中差 1 行、选择身份漂移；已修复 Login：删除确认按 cursor 重取、渲染期写状态；已修复 Agent/Goal：hook 排在条件分支之后；已修复 Plugin 搜索/操作：把各自的票据与代次实现收敛到共享原语。
- 待修复 Workflow（批次 E）：渲染体内写 `active_run`/`run_sync_missing`/`preselect_key` 与 `phase_sel`/`agent_sel`；`kill_run` 无快照代次校验、无活动会话时按空串发送。
- 待修复 Plugin（批次 E）：插件详情与市场详情按 index 定位，而列表每 2s 被重写；任意键清 `operation_loading` 并按会话无关方式刷盘缓存。
- 待修复 ThreadBrowser（批次 E）：归档回写无会话/代次/过滤复验。
- 待修复 AskUserPopup（批次 F）：payload 指纹变化分支在渲染体内写状态。
- 待核 `confirm_popup`（批次 F）：确认执行时重读全局 `CONFIRM_PAYLOAD`（非展示时副本），未校验版本。
- 未接入原语：`cron/actions.rs` 会话级守卫仍无显式代次；Tasks 取消仍无票据/完成门（连按 Enter 会重复发送，依赖服务端幂等）。

## 待完成 / 风险

1. 批次 E：Plugin、ThreadBrowser、Workflow 迁移与 `service_snapshot.rs`（753 行）按职责拆分；拆分必须保持「提交与失效门」单点，不得分散进各刷新单元。
2. 批次 F：详情外壳统一、弹窗生命周期、已确认无消费者路径的清理。
3. 批次 C/D 收口：Cron 会话守卫与 Tasks 取消接入原语；「集中配置服务」抽象与向导字段。
4. 客户端内部超时目前仍映射为失败而非结果未知——区分它需要客户端返回类型化错误，字符串匹配不予采用。
5. Status 面板在 60×18 下内容被裁切（页签栏与内容均分内区高度），本批明确不新增滚动能力，待产品决策。
6. 体积与性能对照未做（见基线）；Linux 口径、e2e 与真实终端手测未执行。
7. 迁移期间面板新旧实现并存属批次内允许状态；每完成一个面板即删除其被替代逻辑，不留双写。
