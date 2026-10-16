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
| C 操作机制与简单面板 | 已完成 | Cron、Memory、Tasks、Agent、Status、Goal 六个面板已迁移；`application/operations/` 原语落地并接入 Plugin 搜索与操作；收口把 Cron 会话守卫、Tasks 取消与 Workflow kill 一并接入原语（作用域 = 会话 id + `BRIDGE_RESET_COUNTER`） |
| D 宿主配置与向导 | 已完成 | Config、Model、Login、Theme 四个面板已迁移（命中同源、渲染边界、确认身份、保存失败如实展示）；「集中配置服务」已落地（`kit/config_service.rs` 单一权威：候选落盘 → 成功才提交 → 失败可见），并据此修正 `delete_provider` 的失败发布。向导按同批缺陷类审计后**无适用改动**（证据见下「向导审计」），未改结构 |
| E 复杂面板与快照 | 已完成 | Workflow 渲染体副作用清零 + kill 固定 run id/完成门；ThreadBrowser 归档/删除逐项身份复验；Plugin 详情按稳定身份定位、缓存刷新归作用域；`service_snapshot.rs` 753→228 行按职责拆分，投影收敛为单一发布入口 |
| F 详情、弹窗和清理 | 已完成（范围调整） | ConfirmPopup 加版本守卫（所见动作锁定）；删除无消费者的 `popups/ask_user_popup.rs`（386 行）；「详情迁移共享外壳」经复核无第二真实用例，按设计 §5.1 不做投机抽象 |
| G 整体验收与文档固化 | 进行中 | 本 issue + design + code-index + 手册已更新；体积配对测量完成（**+0.230%，未通过不增长门槛**）；e2e L0 跑完 9/11（2 个失败经基线对照判定为预存，见下），**e2e 门禁未过**；Linux `.text` 口径与真实终端手测未做 |

## 基线（可复现）

- 测试：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib` 在 `56a8ccd8d` 为
  **1845 passed / 0 failed / 6 ignored**；本分支当前为 **2002 passed / 0 failed / 6 ignored**（同命令、同机器）。
- 二进制（macOS ARM64，release，`strip = "symbols"`）：基线 worktree `/tmp/peri-tui-baseline`
  （detached 于 `56a8ccd8d`）与当前 worktree 用同一条命令配对构建后对比字节数，见下方测量记录。
  Linux `.text` 归属尚未测量；未测量的平台不得宣称通过。
- 性能口径（批次 G 执行，需配对测量中位数）：warm frame `scanned_slots=0/updated_slots=0`、
  缓存重绘 `full_parses=0`、同 key ≤1 在途查询已有确定性断言；30 轮配对中位数退化 ≤10% 未测。

## 体积配对测量记录（批次 G）

- 测量条件：macOS ARM64；同一命令
  `./scripts/cargo-rmcp-patched.sh build --locked --release -p peri-tui --bin peri`；两个 worktree
  各自独立 target 目录、同一 toolchain、同一 `Cargo.lock`；release profile 为
  `strip = "symbols"` / `opt-level = "z"` / `lto = "fat"` / `codegen-units = 1`，未改动。
- 基线：`/tmp/peri-tui-baseline`（detached `56a8ccd8d`，工作区干净，`git status` 无输出）→
  **21,745,968 B**；`size -m`：`__TEXT` 20,086,784、`__text` 14,076,828。
- 当前：本分支工作区 → **21,795,936 B**；`size -m`：`__TEXT` 20,119,552、`__text` 14,096,172。
- 差值：**+49,968 B（+0.230%）**；`__TEXT` +32,768 B；`__text` +19,344 B。
- **门槛判定：未通过。** 方案默认门槛是「stripped 文件不增长」，本次整包增长 0.23%，
  如实记录，不宣称体积收益。差值原因（按方案要求单列）：
  1. 新增正确性路径：稳定身份类型（`plugin/identity.rs`、`ThreadListViewKey`）、快照补丁结构与
     单点发布（`SnapshotPatch` / `commit_projection` / `publish_session_fields`）、完成门接入与
     结果可见通知（Cron/Tasks/Workflow）、新增 i18n key。
  2. 被替代实现的删除不产生 release 差异：删除 `ask_user_popup.rs`（386 行）与
     `close_ask_user_popup_*`（30 行）前后各构建一次，产物字节完全相同（21,795,936 B），
     即 LTO 早已消除这些未使用代码——源码行数下降不能替代字节差值。
- 口径限制：release 已 `strip = "symbols"`，无法做符号级 TUI / 公共依赖归属统计，只报整包与
  section 口径；方案 §3.3 的前轮 `.text` 归属来自 Linux 端专门统计，本机不可复现。
  **Linux x86_64 未测**，不用 macOS 比例换算。

## e2e 真实终端验收（批次 G）

- 命令：`cd e2e && npm run e2e:l0`（L0 PR 冒烟，11 个文件，串行、无 retry）。
- 结果：**9/11 通过 / 2 失败**（总耗时 7m28s；通过项含 plugin 卸载冻结回归、legacy history
  升级、workspace MCP resources、tool-cards、fresh-setup 等）。
- 失败项（隔离重跑稳定复现，非 flake）：
  1. `tests/scenarios/workspace-slow-git.test.ts` —— 「一次准入至多一轮发现」断言：
     窗口内发现 8 次（期望 ≤3）。
  2. `tests/smoke/viewport-40x8.test.ts` —— 40×8 下等待 turn footer 超时（180s）。
- **预存判定（非本分支引入）**：在干净基线 worktree `/tmp/peri-tui-baseline`（detached `56a8ccd8d`，
  `git status` 无输出）用同一命令跑同一对用例，**同样失败**：slow-git 发现 7 次（>3），
  viewport-40x8 同样 footer 超时 3m08s。本分支相对基线只改 `peri-tui/` 与文档（未改
  `e2e/`、`helpers/`、peri-sdk / peri-acp / peri-agent），失败数值亦接近，判定为预存失败。
- 处理：按方案「发现的现有失败单列，不被重构掩盖」，如实记录并单列跟进，本批不修
  （涉及后端 workspace 发现计数与 replay 驱动机制，超出本批授权范围）。
- 补充用例（覆盖本批改动的面板交互）：`npm run e2e -- --file tests/panels/model-switch.test.ts
  --serial --retry 0` → **1/1 通过**（14s；`/model` 面板左右分栏、字段切换与宿主 settings.json 持久化）。
  L0 内已通过的 plugin 卸载冻结回归与 legacy history 升级分别覆盖 Plugin 与 ThreadBrowser 面板。
- 未执行：`npm run e2e:l0` 之外的 L1 全量（`tests/panels/plugin.test.ts` 依赖 LLM Judge 与
  外部凭据，未跑）；真实终端手测未做。**e2e 门禁未通过**（首轮失败 2 > 预算 0），不作为
  本分支交付结论。

## 向导审计（批次 D 剩余项，结论：无适用改动）

按同批缺陷类逐项核查 `kit/setup_wizard.rs`、`kit/setup_wizard/{handler,render}.rs`、
`app/setup_wizard/mod.rs`：

1. **渲染体内写状态**：`kit/setup_wizard/render.rs` 全文 0 处 `.write()`；组件 `SetupWizard`
   的渲染段（`kit/setup_wizard.rs:139-180`）只读状态，写操作只在事件处理与 async effect 内。
2. **hook 顺序**：`use_atom`/`use_effect`/`use_async_effect`/`use_hook`/`use_event_handler_with_options`
   全部位于组件体前段（`:37-137`），其后无早退分支。
3. **保存/应用失败如实展示**：`finish_setup` 保存失败 → `report_setup_failure("setup-save-failed")`，
   激活/句柄缺失失败 → `"setup-activation-failed"`；两者只写 `save_in_progress=false` +
   `submit_error`，不置 `WIZARD_ACTIVE=false`、不置 `SETUP_COMPLETED`，向导保持打开
   （`:185-227`）；错误行参与渲染（`handler.rs:183` 的 `error_rows`）。「Enter 只请求保存、
   完成标记由成功路径发布」由既有用例 `kit/setup_wizard/handler_test.rs::
   test_save_enter_sets_busy_without_claiming_completion` 锁定。
4. **密钥**：三处展示点（`render.rs:338/574/673`）均经 `mask_api_key`（首尾各留 4 位，短串全掩），
   无明文回显。
5. **配置来源不变**：`save_setup`（`app/setup_wizard/mod.rs:543-563`）仍为
   `CONFIG_SOURCE_HANDLE.reload_merged()`（无 source 时 `config::load()`）→ `merge_setup`
   → `LlmProvider::from_config` 校验 → `save_effective`；未新增第二套持久化路径。

未做：向导面向同一结果的结构性迁移（例如把 730 行的 `render.rs` 按步骤拆分）不在方案口径内；
`finish_setup` 的失败分支需要注入保存接缝才能单测，本批不引入该接缝（E2E `fresh-setup.test.ts`
覆盖成功路径）。

## PR #231 评审处理（CodeRabbit，`c7e9b2a` 快照）

1. **login 动作目标与命中同源**（属实，已修 `1ab44ef81`）：`enter_edit_row` /
   `begin_confirm_delete` 原先按光标下标读实时 `PROVIDER_LIST`，而命中来自本帧快照
   `ctx.providers`，投影每 2s 重派生——渲染到事件之间列表一变就会作用到另一项。现两者同源；
   确认门的实时列表校验仍由 `confirm_delete` / `provider_list_contains` 承担。
2. **theme 失败后句柄保留未保存值**（属实，已修 `1ab44ef81`）：`persist_theme` /
   `toggle_daily_color` 原先先写 `TUI_CONFIG_HANDLE` / `PERI_CONFIG_HANDLE` 再落盘，失败后
   面板显示的仍是未保存值，违反本批写进设计文档的契约。现改为候选副本落盘成功才提交句柄；
   主题预览仍由 `switch_theme_atoms` 承担（不经句柄），预览行为不变。
3. **`panel-mcp-icon-unknown` 缺 key**（属实，已修 `1ab44ef81`）：`mcp/detail.rs` 对未知状态
   调该 key，两个 locale 都未定义，界面会显示原始 key。已补 en / zh-CN。
4. **flake 归属错误**（属实，本质是文档问题，已改）：见「交付证据 3」。新用例不能靠基线对照
   归为预先存在，已改记为「本批引入的隔离问题」并列入待完成项。
5. 复核后未采纳的部分：无。四条全部属实。

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
| `8b9da336c` | 文档同步：设计、本 issue、`docs/code-index/peri-tui.md` 与 `peri-tui/CLAUDE.md` 更新到批次 C/D 收口前状态 |
| `11898b7c7` | Workflow：渲染体 7 处 `.write()` 移入 effect 边界；选中身份 index→run id；`kill_run` 无会话不发空串、发送前校验目标存在、结果经通知可见；守卫选 `CompletionGate`（同时覆盖换会话与面板卸载）；判定抽 `handle_workflow_event` + `WorkflowEventCtx`；删 `index_for_run_id`/`clamp_run_selection`；workflow.rs 859 行 + 14 用例 |
| `207c14a33` | ThreadBrowser：归档/删除回写按捕获的身份（`ThreadListViewKey` = 会话 + 重置代次 + 作用域 + 归档视图）逐项复验，不符只告警不写列表；归档方向用捕获身份；删除失败给出可见通知；新增 9 用例 |
| `dad81ed6c` | `service_snapshot.rs` 753→228 行，拆 `commit`/`commit_test`/`file_scan`/`memory_scan`/`session_metadata`/`thread_list_refresh`；单点发布入口 `commit_projection`（短锁内逐字段 compare-and-set、保留 PartialEq 短路）；失效门 `identity_is_current` 单点；`SERVICE_SNAPSHOT` 写锁只留在 `commit.rs`；`model/edit.rs` 最后一处整快照回写改走窄入口 `publish_session_fields` |
| `d8f6f0eee` | Plugin：新增 `plugin/identity.rs`（已安装项/市场条目稳定身份，`plugin_id()` 与请求同源）；详情与市场详情按身份定位并逐项复验，身份消失即清理走不可用提示；删除 `operation_loading` 字符串标志与延迟写线程，缓存仅由成功且复验通过的市场请求触发；新增 `plugin/guard_test.rs` 10 用例 |
| `4cbe66ffa` | ConfirmPopup：`ConfirmPayload.version` + 渲染帧快照（所见动作），执行前比对版本，不一致不执行不关闭并提示；删除无消费者的 `popups/ask_user_popup.rs`（386 行）与其测试、模块声明，保留仍有消费者的 `PopupKind::AskUser`/`popup_text_shell!` |
| `7abae1806` | C 收口：Cron 会话守卫、Tasks 取消、Workflow kill 统一接入 `OperationScope`（会话 id + `BRIDGE_RESET_COUNTER`）与完成门；同作用域同目标在途重复触发判定为「拒绝」；新增 `tasks/tests/operations.rs` 与 cron/tasks 回归 |
| `dd4f907d5` | 删除无调用者的 legacy 清理路径 `popup_overlay::close_ask_user_popup_for_request/for_owner`（30 行，旧弹窗删除后已无调用者）；`docs/reference/tui-manual-verification.md` 的 popup 清单与 AskUser 检查项同步为面板路径 |
| `51ea726ab` | 文档固化：设计文档补作用域/已接入清单/快照发布单点/确认版本守卫/面板职责映射表（17 面板）；`docs/code-index/peri-tui.md`、`peri-tui/CLAUDE.md`、本 issue 同步批次 G 的测试/体积/e2e 证据 |
| `e65718878` | 批次 D 收口：新增 `kit/config_service.rs` 作为四个配置面板「候选落盘 → 成功才提交 → 失败可见」的单一权威，6 个落盘点收敛；`delete_provider` 失败不再改内存与推送 ACP；新增 `config_service_test.rs` 与删除候选构造回归 |
| `497f080c2` | 批次 D 文档：设计文档增「配置落盘边界」；code-index 增该规则路由；本 issue 更新批次表/用例数/证据/清理/决策 9-11 与三笔提交补记；`display-coverage.md` 同步 AskUser popup 删除 |
| `b68e1f033` | 向导审计结论文档化（批次 D 剩余项：无适用改动 + 逐条证据），批次 D 标记完成；记录密钥掩码两套样式待产品决策 |
| `66fc0fa24` | 配置落盘边界写入 `peri-tui/CLAUDE.md`（不变量 + 任务路由），补记 `b68e1f033` |
| `c7e9b2a14` | 如实记录全量测试抖动与基线对照（PR #231 评审指出其中 tasks 用例的归属有误，后由 `1ab44ef81` 的文档提交修正） |
| `1ab44ef81` | 处理 PR #231 评审：login 动作目标改取本帧快照（与命中同源）、theme 落盘成功才提交句柄、补 `panel-mcp-icon-unknown` 文案；flake 归属与新的待完成项随后记入本文档 |

## 交付证据（方案 §11）

1. **职责映射**：见设计文档「面板职责映射」（17 个面板 × 状态 owner / 数据来源 / 动作入口 / 完成门）。
2. **依赖检查**：每笔提交的 `check-layer-imports.sh`（22 条规则）0 违规；`application/operations/`
   为纯状态机、不读 kit atoms；`panels/shared/list.rs` 只做几何推导，无 RPC 与文件 I/O。
3. **行为记录**：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib` 由 1845 增至
   **2002 passed / 0 failed / 6 ignored**（净增 157 例，含 12 个新增测试文件）；新增覆盖确认期间
   列表刷新、切会话旧响应、关闭重开、结果未知三态、同 key 在途唯一、快照提交门与失效门、
   配置落盘边界（成功/失败提示与失败不发布）、删除候选取舍。
   已知 flake——**已修正归属**（PR #231 评审指出）：`kit::panels::tasks::tests::operations::
   reset_before_completion_discards_cancel_notification` 不属于「预先存在」：该用例由本批
   `7abae1806` 新增（`tasks/tests/operations.rs` 是新文件），基线 `56a8ccd8d` 不含它，基线
   对照无法证明它此前会抖。它的失败在收口后 15 轮里出现 1 轮（该用例已 `#[serial]`，读写的
   `BRIDGE_RESET_COUNTER` 是跨用例共享的全局 atom），**按本批引入的隔离问题记为待完成项**。
   其余三例（`kit::steer_consumer::tests` 两例、`kit::panels::plugin::guard_tests` 一例）仍是
   预先存在：这些模块本批 diff 为空，失败例隔离重跑 3/3 通过、`steer_consumer` 全模块隔离
   10/10 通过；收口后 15 轮全量 2 轮失败（1 轮 2 例 `steer_consumer`、1 轮 1 例 plugin guard），
   其余 13 轮 2002 全绿；基线 worktree `56a8ccd8d` 同命令 9 轮全绿（基线例数 1845、采样时负载
   更低），并行 worker 把改动 stash 后的基线 10 轮出现 2 例 plugin guard。抖动面是跨用例共享
   全局 atom + 超时驱动的用例，单列跟进，本批不修。
4. **性能与体积记录**：见「基线」节。
5. **清理记录**：删除 `popups/ask_user_popup.rs`（386 行）及其测试与模块声明；删除
   `popup_overlay::close_ask_user_popup_for_request/for_owner`（旧弹窗删除后已无调用者的
   legacy 清理路径）；删除 `operation_loading` 字符串标志与延迟写线程；删除
   `index_for_run_id`/`clamp_run_selection`；批次 B/C 已删除被替代的 MCP/Cron/Memory 旧实现。
6. **重复实现收敛（批次 D 收口）**：四处同源提示构造（`config/actions.rs` 的
   `notify_saved`/`notify_save_failed`、`model/edit.rs` 的 `notify_save_result`、
   `login/config_store.rs` 的 `persist_and_notify` 与 `save_login_edit` 内联成功提示、
   `theme.rs` 两处内联提示，共 6 个落盘点）收敛到 `kit/config_service.rs`；
   `login/config_store.rs::delete_provider` 由「先改内存、无条件落盘并推送 ACP」改为
   「候选副本落盘成功才提交内存与推送」——该路径原有实现是本批契约（失败不发布）的
   唯一违反者。

## 决策记录（本次授权内的裁决）

1. **依赖方向**：`application/operations/` 保持纯状态机，不读 kit atoms；`OperationScope` 由调用方构造，避免 `application → kit` 反向依赖。
2. **超时**：不给 `send_raw_request` 增加超时——不在本批授权范围，保持既有语义；插件搜索仍是无时限路径。
3. **代次过滤**：把只带会话身份的退化实现升级为带显式代次，属落实设计 §6.1「请求序号过滤被替代请求」，不视为产品行为变更。
4. **任务所有权**：本批只落「请求协调」的最小状态门；已发送变更的任务所有权与应用层结果跟踪留待后续，如实记为未完成。
5. **确认目标消失的提示**：统一为可见提示并共用跨面板文案 `app-confirm-target-missing`（Cron 与 Login 同一语义）。
6. **滚轮语义**：视口接管后滚轮不再滚出被裁剪的行（可见范围即窗口、窗口跟随选中项），已在批次 B 接受，各面板一致，记于设计文档。
7. **Cron/子代理行的 Enter**：保持「关闭面板」的既有语义（与 `panel-tasks-nav-hint-close` 文案及既有用例一致）；只有可取消的后台任务行执行取消。
8. **行高可变面板**：Agent 不接入共享视口，复用既有逐行测量；分段列表（Tasks）自建行表。两者都在文件内写明理由与替代做法。
9. **集中配置服务的提取范围**：只收敛真实重复的「落盘 + 用户可见提示」边界（含失败日志），
   不统一候选构造与成功后的发布动作——四面板的发布语义不同（Config 提交 TUI+宿主句柄、
   Model 还推 ACP、Login 刷投影并推 ACP、Theme 只落盘）。`config-handle-missing` 仍由
   Config 面板自持（其 `notify` 未被替换），未把面板自有提示路径并入公共实现。
10. **`delete_provider` 的失败路径行为变更**（本批唯一的用户可见行为改动）：落盘失败不再
    改内存、不再刷新列表、不再推送 ACP，只给失败提示——避免「界面显示已删除、重启后
    provider 复活」。该路径无单测覆盖（真实删除需进程级 `PERI_CONFIG_HANDLE`，测试进程
    不可安装），只覆盖候选取舍的纯函数判定；生产行为需人工验证，E2E 未覆盖。
11. **共享失败日志文案**：由 `"ConfigPanel: 配置保存失败"` 改为中立的 `"配置保存失败"`
    （函数现被四个面板调用，保留面板前缀会误标归属）；theme 的两处调用点各自补一条带
    上下文的 `tracing::error!`（主题名 / daily_color），失败时共两行日志。

## 已确认缺陷（批次 A 审计，按最新状态标注）

- 已修复（批次 A 审计清单）：Cron 删除确认按下标重取；Memory 可见项写死与区域来源不一致；Tasks 取消按下标取 `task_id`、无会话静默发送、越界静默关面板；Config/Model 保存失败静默或假装成功；Theme 渲染期写状态、hook 顺序、命中差 1 行、选择身份漂移；Login 删除确认按 cursor 重取、渲染期写状态；Agent/Goal hook 排在条件分支之后；Plugin 搜索/操作把各自的票据与代次收敛到共享原语。
- 已修复（批次 E）：Workflow 渲染体内写 `active_run`/`run_sync_missing`/`preselect_key`/`phase_sel`/`agent_sel` 已移入 effect；`kill_run` 改为固定 run id 并加完成门（无活动会话不再按空串发送，目标消失有可见提示）；选中身份由下标改为 run id。Plugin 详情/市场详情按稳定身份定位（`plugin/identity.rs`），删除 `operation_loading` 字符串标志与延迟写线程，缓存仅由成功且身份复验通过的市场请求触发。ThreadBrowser 归档/删除逐项身份复验，不符只告警不写列表，删除失败可见。`service_snapshot.rs` 拆分后最后一个整快照回写点（`model/edit.rs`）改走窄入口 `publish_session_fields`。
- 已修复（批次 F）：`confirm_popup` 改为执行前比对版本（`ConfirmPayload.version`），不一致则不执行、不关闭并给出 `popup-confirm-superseded` 提示；无消费者的 `popups/ask_user_popup.rs` 及测试整文件删除。
- 已修复（批次 D 收口）：四个配置面板的落盘与提示改由 `kit/config_service.rs` 单一权威维护（此前 6 处落盘点各写一遍同源文案与时长）；`login/config_store.rs::delete_provider` 不再在落盘失败时改内存与推送 ACP（见决策记录 10）。
- 已接入原语：`cron/actions.rs` 会话守卫、Tasks 取消、Workflow kill 均改用 `OperationScope`（会话 id + `BRIDGE_RESET_COUNTER`）+ 完成门；同作用域同目标的在途重复触发判定为「拒绝」（不发、不提示）。

## 待完成 / 风险

1. 批次 D 已收口（含向导审计结论）；未做的向导工作是「面向同一结果的结构性迁移」与
   `finish_setup` 失败分支的单测（需注入保存接缝），不在本次授权口径内。`delete_provider`
   的落盘失败路径无单测覆盖（需进程级句柄），仅覆盖候选构造的纯函数判定，生产行为待人工验证。
2. 「详情迁移共享外壳」（方案原批次 F 项）经复核不做：`panel_shell!` 与 `panel_scroll` 已共享外壳，
   `ScrollbarGeometry` 全仓唯一使用者是 SubAgentDetail，无第二真实用例，按设计 §5.1 拒绝投机抽象。
3. 客户端内部超时目前仍映射为失败而非结果未知——区分它需要客户端返回类型化错误，字符串匹配不予采用；
   插件搜索取消仍为静默丢弃（无结果呈现）。Cron 操作 10s 超时同理未类型化。
4. 不接管已发送变更的任务所有权（方案 §6.2）：本批只落请求协调的最小状态门。
5. Status 面板在 60×18 下内容被裁切（页签栏与内容均分内区高度），本批明确不新增滚动能力，待产品决策。
6. 体积与性能对照：体积已配对测量（未达不增长门槛）；30 轮性能配对中位数、Linux `.text` 口径、
   真实终端手测未执行。e2e L0 有 2 个**预存失败**（`workspace-slow-git`、`viewport-40x8`，
   基线同样失败，见上），需单独立项跟进，本批未修。
7. 迁移期间面板新旧实现并存属批次内允许状态；每完成一个面板即删除其被替代逻辑，不留双写。
8. 预先存在、本批未改动：`peri-tui/src/kit/acp_events/system.rs` 为 1007 行（基线同为 1007 行，
   本批 diff 为空），超出 STD-SIZE-001 的 1000 行上限；不在本批授权范围，仅记录，未顺手重构。
9. 密钥掩码存在两套样式：向导 `mask_api_key`（`••••`，保留首尾各 4 位）、Login
   `mask_api_key_display`（`*`，保留末 4 位）。两者都是正确掩码、无明文泄漏，统一属展示一致性
   改动（会改变其中一处的可见输出），待产品决策，本批不改。
10. `kit::panels::tasks::tests::operations::reset_before_completion_discards_cancel_notification`
    是本批新增用例（`7abae1806`），其抖动属本批引入的测试隔离问题：用例已 `#[serial]`，但读取的
    `BRIDGE_RESET_COUNTER` 是跨用例共享的全局 atom，非 serial 的写入者仍能干扰。修法（把相关写入
    者一并纳入 serial，或让断言不依赖全局计数值）未在本批做，列为待完成。
11. 无测试校验「`i18n::tr` 的字面量 key 存在于 `locales/*/main.ftl`」——`panel-mcp-icon-unknown`
    缺失就是这样漏到评审阶段的（PR #231 第 3 条）。补一条扫描源码字面量 key 的回归测试属新工作，
    本批不引入，列为待完成。
