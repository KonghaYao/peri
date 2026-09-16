# P0：升级 3.15.0 后旧 history 不可见、无法恢复

**状态**：Fixed — 兼容、分页与 UI 自动回归通过，待发布与受影响用户验收
**优先级**：P0（用户报告）
**创建日期**：2026-09-16
**检查提交**：`8b894bebdf723e896926d115cac3ae5cdf89d94b`

## 问题描述

用户报告其他用户升级到 3.15.0 后完全无法访问旧 history，要求检查上述提交并给出诊断。
本地 `agent-v3.15.0^{}` 精确指向上述提交。最初诊断在临时数据库中完成；随后用户明确要求开始修复，实施与验证见文末。修复与自动验证使用隔离库；追加分页排查对开发者本机库做过只读数量统计，未读取消息正文或改写实际数据库。

## 诊断结论

确定存在升级兼容性断裂：schema 升级保留历史但不回填 `SessionBinding`；TUI history 切换到只查询已绑定会话的列表；恢复入口又强制要求绑定。三者组合使升级前的无绑定会话全部退出正常的 history 浏览和恢复路径，与用户是否使用过 Git worktree 无关。

复现中旧消息完整保留，旧列表 API 和消息读取成功，新 scope 列表返回空，按 ID 的恢复前置校验返回 `BindingMissing`。数据保留结论仅针对本次 fixture；未据此断言每个受影响用户的库都没有其他故障。

## 因果链与代码证据

1. **不回填历史绑定。** [schema.rs](../../peri-resources/src/sessions/sqlite_store/schema.rs) 的 `init_schema` 补齐旧表、创建空的 `projects` / `workspaces` / `session_bindings`，提交 `user_version = 3`；旧 thread 不增加 binding。默认数据库仍为 `~/.peri/threads/threads.db`。
2. **列表无条件排除无绑定行。** [workspace.rs](../../peri-resources/src/sessions/sqlite_store/workspace.rs) 的 `list_scoped_threads_impl` 使用 `threads JOIN session_bindings JOIN workspaces`。这是内连接，Project / Workspace / ExactDirectory / All 均排除无绑定历史；`All` 也无法找回。
3. **TUI 使用了新列表。** [service_snapshot.rs](../../peri-tui/src/kit/service_snapshot.rs) 的旧 `list_thread_entries(cwd)` 被 `refresh_threads → AcpTuiClient::list_scoped_threads → session/list` scope 扩展替换。面板只能切 Project / Workspace，两者都会过滤旧历史。
4. **按 ID 也无法恢复。** [client/session.rs](../../peri-tui/src/acp_client/client/session.rs) 的 `load_session_under_gate` 先调用 `peri/session_context`；[session_lifecycle.rs](../../peri-acp/src/host/requests/session_lifecycle.rs) 的 `context_for_session` 首先 `validate_session_binding`。缺绑定返回 `session has no execution binding`。直接调用 ACP `session/load` / `resume` / `fork` 同样经 `prepare_existing → acquire_for_load → validate_expected` 被拒绝，错误码映射为 `-32010`。
5. **更老会话还有下一道兼容性障碍。** 同一提交删除了 `load_or_backfill_frozen_data` 中按 `ThreadMeta.cwd` 构建并 CAS 回填缺失 frozen snapshot 的旧分支；现在 snapshot 缺失直接报 `Bound session has no frozen snapshot`。所以补 binding 只能解除第一道阻塞，不能宣称所有旧会话已能完整恢复。此项来自提交差异和代码检查，本轮未跑完整 ACP 恢复实验。

## 复现与对照实验

使用仓库 `legacy_with_goals.sql` 创建 `user_version = 0` 的临时旧库，插入一条未隐藏、消息数为 1 的真实序列化 Human 消息。保存 cwd 是存在的临时目录。通过实际 `SqliteThreadStore::new` 执行升级，再调用实际 store API。

| 检查 | 结果 |
| --- | --- |
| 升级后读取旧消息 | 原消息仍在，内容一致 |
| 旧接口 `list_thread_entries(saved_cwd)` | 1 条 |
| 新 Project / Workspace / ExactDirectory / All 查询 | 全部 0 条 |
| `validate_session_binding(old_id)` | `BindingMissing` |
| 仅在临时库给原会话添加有效 binding | 列表恢复为 1 条，绑定校验通过，原消息不变 |

只改变 binding 的实验直接支持根因；相同数据库、cwd、消息内容、hidden 和 message_count 不变。控制实验没有调用完整 ACP load，因此“绑定校验通过”不等于“完整会话恢复通过”。

诊断命令：

```bash
cargo test -p peri-resources --test p0_315_history_diagnostic -- --nocapture
cargo test -p peri-resources --lib -- test_single_database_upgrade_preserves_history_and_binds_only_new_sessions --nocapture
```

- 第一次诊断：2 个升级可用性断言失败，exit 101，测试执行耗时 0.05 秒。
- 加入单变量控制后：同样 2 个断言失败，1 个控制通过，exit 101，测试执行耗时 0.07 秒。
- 现有升级测试：实际执行 1 个用例并通过，exit 0。它明确断言旧会话无 binding、取得执行 lease 失败、All 列表只包含新会话。
- 诊断源码及日志保存在本机 `/tmp/peri-p0-315-history-diagnostic/`；临时 Cargo 集成测试入口运行后移除，不将故意失败的诊断测试遗留在工作区。
- 测试运行在当前 HEAD；已检查相关 resources、TUI、ACP lifecycle/workspace 和 workspace 契约文件与发布提交完全一致。当前 HEAD 的其他改动不作为本次归因依据。

## 影响范围与仍可访问的路径

| 用户入口 | 无绑定旧会话的行为 |
| --- | --- |
| TUI history | 列表过滤，无法选中 |
| `-c` | 精确目录 scope 没有旧候选；只有旧历史时允许首次提交新建会话 |
| `-r <旧 ID>` | context 绑定校验失败 |
| ACP `session/load` / `resume` / `fork` | 绑定校验失败，历史回放前就返回 |
| 无 scope 扩展的标准 `session/list` | 仍调用旧 `list_threads` 路径，可列出历史，但不解除恢复限制 |
| `peri/session_history`、只读 metadata、底层消息读取 | 保留读取路径；TUI history 面板未接成可用的旧历史查看入口 |
| 3.15.0 新建且绑定有效的会话 | 不受“缺少旧绑定”这一根因影响 |

## 为什么现有验收没有拦住

[worktree 身份 issue](2026-09-12-worktree-session-identity.md) 的已确认范围和[身份设计](../../docs/design/session-workspace-identity.md) 将“不迁移旧历史”落实为“不回填 binding，旧会话拒绝 load/resume/fork”。现有测试保护了“原始数据保留”和“新会话可执行”，同时固化旧会话从新列表消失的行为；缺少从真实旧版数据库到 TUI 可见、可查看、可继续的升级验收。

因此这不是一次偶发 SQL 查询故障，而是发布所采用的兼容性目标没有满足存量用户的使用需求。

## 诊断时建议的修复边界

1. P0 止血应暂停继续推广有问题的升级，先保全数据库；如需降级，应在完整备份的隔离副本上验证，不让新旧 writer 混用正在使用的库。
2. 恢复旧历史的发现和只读查看，不把执行 binding 当作历史可见性的必要条件。目录失效也应能查看保存内容。
3. 为继续旧会话提供受控迁移/接纳流程：以保存的 `ThreadMeta.cwd` 为依据校验目录和工作区身份，再原子持久化绑定；无法确认的会话保留只读并明确说明。禁止默认绑定到当前终端 cwd 或关闭执行 lease 校验。
4. 同时处理旧 frozen snapshot 缺失的兼容路径，保留已有快照；未知/损坏版本保持错误，不覆盖。
5. 验收至少覆盖旧 schema、已升级到 schema 3 但仍无绑定的库、新旧会话共存、history 查看、`-c` / `-r`、完整 ACP 恢复，以及目录缺失/重建。只修改建表迁移不足以修复已经升级的用户。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
| --- | --- | --- | --- | --- |
| 2026-09-16 | — | Open | agent | 根据用户 P0 报告检查发布提交，完成临时旧库复现和单变量控制；仅诊断 |
| 2026-09-16 | Open | Fixed | agent | 用户授权后恢复旧历史发现、只读查看和显式恢复；存储/ACP/TUI 回归、真实终端与构建检查通过 |
| 2026-09-16 | Fixed | Reopen | 用户 | 反馈当前目录 history 面板仅显示 50 个会话，预期有大量旧记录 |
| 2026-09-16 | Reopen | Fixed | agent | 明确分页数量文案，键盘浏览末尾自动追加；106 条会话终端回归通过 |
| 2026-09-16 | Fixed | Reopen | 用户 | 指出面板布局与交互问题，要求先设计合理 UI，再由 Luna 实施 |
| 2026-09-16 | Reopen | Fixed | 主 agent / Luna | 主 agent 设计与验收、Luna 实施紧凑布局；8 个真实终端用例首轮通过，保留既有交互语义 |

## 修复记录

### 修复 #1（2026-09-16）

- **操作人**：agent
- **用户原意**：升级到 3.15.0 后仍能找到、查看、继续使用旧 history，包括已经完成 schema 3 升级的用户。
- **修复内容**：scope 查询改为包含未绑定历史，统一分页；Project/Workspace/ExactDirectory 按路径边界显示关联，兼容 Windows verbatim/UNC 与 macOS 系统路径别名，All 保留目录不可用记录。TUI 增加全部历史 scope 与 `v` 只读预览。
- **执行恢复**：仅显式 load/resume/fork 接纳未绑定根会话，以保存绝对 cwd 校验；同事务写 binding 和缺失 frozen。已有快照只验证不覆盖，配置、语言、MetaHarness 和插件来自保存目录。随后走原有 lease、dirty 与环境装配；native 缺快照、错误绑定、执行记录和损坏快照不走 legacy 重建。旧 child 写入仍要求根 owner。
- **评审取舍**：采纳 advisor 的按需接纳、只读发现与严格执行验证分离方案；采用 binding+frozen 同事务发布避免单独迁移标记。Standards/Spec 独立审查发现的路径规范化与目标配置问题已修复并复核，预览语言更新同时修正。
- **涉及 commit**：未提交；工作区补丁待审阅与发布。
- **验证状态**：本机自动验证通过；待受影响用户实际库验收。未发布版本，未在 Windows/Linux 实机运行终端生命周期验证。

验证证据（全部隔离临时 HOME / 数据库）：

| 检查 | 结果 |
| --- | --- |
| `cargo test -p peri-resources --lib` | 110 通过；含旧 schema、schema 3 重开、混合分页、路径边界、并发接纳、事务失败回滚与子会话 owner |
| `cargo test -p peri-acp --lib` | 668 通过；含 load/resume/fork、保存 cwd、目标配置与插件冻结、缺目录只读及 native 错误保留 |
| `cargo test -p peri-tui --lib -- workspace` | 17 通过；含 nullable legacy context、只读预览不切会话及恢复提交顺序 |
| `npm run e2e -- --file tests/scenarios/legacy-history-upgrade.test.ts --serial --retry 0` | 3 用例首轮通过；真实旧 schema → ACP → TUI，覆盖升级/重开列表、删除目录预览、`-c` 与跨目录 `-r`；无需模型或 judge 请求，加入 L0 门禁 |
| `cargo build --workspace` | 通过；macOS 调试链接器提示 unwind section 超过紧凑表容量，不影响构建完成 |
| `cargo clippy -p peri-resources -p peri-acp -p peri-acp-types -p peri-tui --all-targets -- -D warnings` | 通过 |
| `cargo fmt --check` / `git diff --check` / `bash scripts/check-layer-imports.sh` | 通过 |
| `cargo test -p peri-acp-types --doc` | 通过，2 个既有示例 ignored，无实际运行 doc test |

E2E 报告：`e2e/results/run-2026-09-16T03-49-40/report.md`；其他本机日志：
`/tmp/peri-p0-315-history-diagnostic/fix-*.log`。

旧数据没有历史目录对象证据，无法追溯证明当前同名目录仍是历史实例；按保存路径
解析当前身份，已登记冲突仍拒绝。任意自定义符号链接别名可能无法出现在当前 scope，
仍可经 All 查看或按 ID 恢复。Windows 路径 SQL 在本机测试，完整 Windows 执行待平台验收。

### 修复 #2（2026-09-16）—— history 分页不应表现成 50 条上限

- **用户反馈**：当前目录应有大量会话，history 面板却只显示 50 条。
- **证据**：只读统计本机库，保存 cwd 为本仓库的未隐藏且非空会话共 2,199 条。`refresh_threads` 每页 50 条、初始请求一页；此前仅按 `n` 追加。标题使用已加载数量却写成总会话数，普通向下浏览不会追加。
- **修复内容**：有后续页时标题明确“已加载 N 个会话 · 还有更多”；Down / PageDown / End 接近末尾时按需请求下一页，保留 `n`。重复输入合并到同一待取页，不一次性扫描全部历史；切 scope 重置分页和 has_more。PAGE_SIZE 由查询与请求方共享。
- **回归证据**：106 条真实旧会话经完整 TUI 路径浏览。修复前 End 后等待 100 条超时；修复后 50 → 100 → 106，并能看到最旧记录，原有三个升级/恢复用例一起通过。测试首轮 4/4 通过，报告 `e2e/results/run-2026-09-16T03-59-58/report.md`。
- **其他验证**：`cargo test -p peri-tui --lib` 1,571 通过、2 ignored；workspace build、TUI all-targets Clippy、fmt、依赖边界和 diff 检查通过。Standards / Spec 两轴增量复核均无阻塞。
- **验证状态**：本机自动验证通过，待使用新构建重启后的实际交互反馈。未提交或发布。

### UI 实施约定（2026-09-16，主 agent 设计，Luna 实施）

用户指出修复过程擅自扩展 UI 交互，截图中重复路径、每项三行、固定四条与冗长快捷提示影响浏览。
本轮只调整 history panel 的信息层级和布局，保留当前快捷键、分页、预览、恢复与删除语义。

```text
Threads
项目 · 已加载 50 · 还有更多
> 修复 Micro Compact 上下文污染          09-10   772 条
  优化系统提醒展示                       09-10   125 条
  审计并修正代码索引                     09-09   133 条
  ……按可用高度显示列表……

选中：修复 Micro Compact 上下文污染
路径：…/perihelion · ID：01a08429…
↑↓ 选择  Enter 继续  v 查看  Tab 范围  d 删除  Esc 关闭
```

- 保留现有面板容器、主题和全局快捷键，不调整其他面板或整个终端布局。
- 顶部只保留一行范围与数量状态。每个会话一行，标题靠左、日期/消息数靠右；无逐项 ID/绝对路径，无 ASCII 表格边框。
- 列表吃满容器中剩余高度；选中详情与操作提示固定在底部。可见条数、键盘翻页和鼠标命中由同一实际几何推导，不再硬编码四条。
- 下方最多两行选中详情：标题；路径与短 ID。路径按终端列宽保留尾部，长标题/宽字符不溢出。窄屏优先标题，逐步收起日期/消息数；提示可缩短或最多两行，Enter/Esc 必须可见。
- 保持 ↑↓ / PageUp / PageDown / Home / End、Enter 恢复、v 只读预览、Tab 范围、n 更多与 d 删除确认。点击会话继续沿用现有 click-as-enter。滚轮只滚动列表或预览正文，不卷走页头和底部；点击与键盘必须选中视觉对应项。
- 保留按需追加与同页请求合并，后台列表刷新保持选中 thread 身份；预览返回保持位置。不得改旧历史恢复/绑定/快照后端。
- 复用现有 ratatui-kit 组件与滚轮仲裁；若实现发现需要修改全局事件语义，先向主 agent 报告，不自行扩大范围。
- 验收：106 条会话跨页，140×45、80×24、60×18 与 resize；标题/路径不重复、正常高度超过四条、底部操作可见；键盘/滚轮/点击一致；空列表、加载错误、删除确认和只读预览仍可用。使用隔离 HOME / fixture，不调用真实模型、不修改真实数据库。

### 修复 #3（2026-09-16）——紧凑且保持既有操作的 history UI

- **分工**：主 agent 先给出上述布局和行为约定，`gpt-5.6-luna` 实施 history 面板与双语文案；主 agent 编写并运行真实终端回归、复核代码和维护文档。
- **实现**：会话改为单行，宽屏右侧显示日期/消息数，窄屏收起次要列。路径与短 ID 仅在固定选中详情展示。列表和预览各自保存滚动位置，预览页头及操作栏固定。列表刷新按 thread ID 保留选择；鼠标操作使用完成帧列表，删除确认保存独立目标 ID。键位、click-as-enter、按需分页、后端接纳历史的契约不变。
- **故障回归证据**：新紧凑布局测试在旧三行布局失败；初稿翻页选中错位被真实终端测试拦住。修复后覆盖翻页→长历史预览→返回原位置、后台新增时选择稳定、反复缩放、滚轮与点击对应同一会话、确认删除后目标被外部删除时不误删邻项。
- **终端结果**：最终 `npm run e2e -- --file tests/scenarios/legacy-history-upgrade.test.ts --serial --retry 0` 退出 0，8/8 用例通过，0 重试。包含原升级/恢复/106 条分页用例，以及中英文紧凑列表、空态与删除目标回归。报告：`e2e/results/run-2026-09-16T04-46-03/report.md`，逐用例结果：同目录 `worker-0.json`，画面：`recordings/worker-0/history-panel-ui/`。
- **实际布局**：中文 140×45 显示 22 条、80×24 显示 14 条、60×18 显示 8 条；路径和短 ID、Enter/Esc 均可见。放大后的点击等待新尺寸完整帧，避免 tmux 缓冲区先变而应用尚未重绘造成测试误判。
- **其他门禁**：`cargo test -p peri-tui --lib` 1,573 passed、0 failed、2 ignored；`cargo build --workspace`、`cargo clippy -p peri-tui --all-targets -- -D warnings`、`cargo fmt --all -- --check`、`bash scripts/check-layer-imports.sh`、`git diff --check` 均退出 0。构建保留已有 macOS linker unwind size 警告。Standards 审查的几何/Unicode/操作目标问题、Spec 审查的预览提示滚走问题均已关闭。
- **验证边界**：在本机 macOS + tmux、隔离 HOME/数据库执行，无模型请求，未改真实数据库；未跑发版全套或其他平台。列表/预览 RPC 错误分支保留并静态核对，本轮没有注入 RPC 失败。代码未提交或发布，运行中的旧进程需要重启到新构建。
