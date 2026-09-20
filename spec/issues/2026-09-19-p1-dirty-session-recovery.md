# P1：异常退出后 history 无法恢复，缺少安全恢复入口

**状态**：Implemented — 独立审查通过，待用户验收（未在真实残留进程现场实测；不标记 Fixed）
**优先级**：P1（用户指定）
**创建日期**：2026-09-19
**最后更新**：2026-09-20（记录只读准入策略变更：占用不再挡住进入，TUI 行为随之调整；见「策略变更」）

## 用户场景与预期

从 history 恢复已有会话时出现
`Session restore failed: ACP error [-32010]: previous session execution did not close cleanly; recovery is required`，
无法回到该会话继续工作。用户要求恢复**原会话**（原 ThreadId、原 frozen、原 binding/cwd），
不接受“新建派生会话”作为替代。

## 已确认事实

- `peri-resources/src/sessions/sqlite_store/execution.rs::acquire_execution_lease_impl` 先取得稳定 sidecar OS 锁，再读取 `execution_runs`；前代 `clean=false` 即拒绝。
- 持久化表只记录 thread、generation、clean；旧 owner 消失后，此前没有恢复入口。
- `mark_clean` 只属于活 owner，且 `mutation_uncertain` 时拒绝；不能借用它清 dirty。
- `peri-acp/src/host/shutdown.rs` 仅整体 shutdown Complete 时遍历 owner 标 clean；单个资源收尾失败可能连带保留其他会话 dirty。
- History 的 `v` 经 `peri/session_history` 只读预览，不需要执行 lease；它不是继续执行的替代品。

## 方案沿革（旧方案已撤回）

### 已撤回：boot 证据 + 新 ID 派生会话

第一轮方案要求新增 boot 身份、恢复检查点、原子代际转换与 ACP/TUI 恢复入口，
并在真实系统重启后核验证据才恢复；随后方案 subagent 又提出“从历史开始新会话”
（独立 root、新 ID、新 frozen、旧 dirty 不动）。

两个方向均**已被用户撤回**，不作为本 issue 的交付：

- 用户明确要求恢复原 ThreadId 与原 frozen，派生新会话不是可接受的替代品。
- 用户最终决定：不需要 boot 证据框架、不要求系统重启、不要求证明旧进程已终止。
- 现有仓库不引入 boot/检查点/准入凭据等框架。

### 现行方案：用户显式接受风险，精确解除指定代际

遇到 `RecoveryRequired` 时提供可达的确认交互，默认取消；确认文案明确风险
（旧子进程可能仍在运行、之前的副作用未知），确认后只清除**精确**
`(thread_id, generation)` 的 dirty 记录，再按正常路径恢复原会话：
原 ThreadId、原 binding/cwd、原 frozen。用户对后续结果担责。

这条路径**不是进程结束的证明**：OS 独占语义不变，稳定锁仍由持有者排他，
取不到锁是 `ExecutionBusy`（只报忙、不弹清除），不删锁、不批量清理、
不放宽普通 load、不静默 fallback。

## 实施范围

| 层 | 位置 | 变更 |
| --- | --- | --- |
| 契约类型 | `peri-acp-types/src/workspace.rs`、`store.rs` | `RecoveryRequiredDetails`（精确 thread/generation）、`WorkspaceErrorData`、`ResetDirtyRequest`（`target` + `accept_risk`，`deny_unknown_fields`）；`WorkspaceError::RecoveryRequired` 携带 details；新增 `ThreadStore::reset_dirty_execution`（默认 `Unsupported`） |
| 存储 | `peri-resources/src/sessions/sqlite_store/{execution.rs,sqlite_store.rs}` | 抽出 `lock_execution`（复用同一 stable lock 文件、绝不删除）；`reset_dirty_execution_impl` 在同一锁内以 `BEGIN IMMEDIATE` 事务 CAS `generation + clean=0`，错配返回 `RecoveryGenerationMismatch`；`acquire_execution_lease` 的 dirty 错误带上精确代次 |
| ACP | `peri-acp/src/host/workspace.rs`、`host/requests.rs`、`host/requests/session_lifecycle.rs` | `workspace_error` 保持 -32010 文本不变并附 typed `data`；新增 `peri/session_reset_dirty` handler：要求 `peri.sessionRecoveryV1` 已协商、`accept_risk == true`，直接返回存储错误（例如 `ExecutionBusy`）而不重试 |
| 能力协商 | `peri-acp-types/src/peri_caps.rs` | `session_recovery_v1`（`peri.sessionRecoveryV1`），默认 false，`all_enabled()` 为 true |
| TUI | `peri-tui/src/acp_client/client/{session.rs,requests.rs,client.rs}`、`kit/popups/confirm_popup.rs`、`kit/popup_overlay.rs`、`kit/atoms.rs`、`kit/status_bar.rs`、两份 `locales/*/main.ftl` | `load_session_under_gate` 从 typed details 与只读标记两处来源解析同一 target，在一次 load transition 内（operation gate 与 reservation 已持有、source/target 与有效 cwd 已固定）等待 `confirm_dirty_recovery`；接受才发送 `peri/session_reset_dirty` 并重新 load，取消按只读准入提交（不再返回错误，见「策略变更」）；`SESSION_READ_ONLY` + `read_only_label` 在状态栏说明本次准入只读的原因 |

### 确认交互的安全约束

- 专用 `DirtyRecoveryPopup`：默认选中「取消」，只有显式选择接受才返回 `true`；
  通用确认路径（`ConfirmAction::RecoverDirty` 落到 `execute_confirm_action`）按取消处理。
- 确认必须完整渲染（`RecoveryDisplay::record_area` 登记的矩形不小于内容）才可能接受；
  终端过小、等待方被丢弃、弹窗已被其他交互占用都 fail closed 且不写入。
- 未协商能力或非交互宿主不展示确认，直接保持原错误。
- 取消、Esc、`close_popup`、render drop 都不会产生任何 reset 写入。

## 不做的事

- 不删除或重建 lock 文件、不批量清 dirty、不按 TTL 抢占、不把用户确认当作执行收尾证据。
- 不修改 binding、cwd、frozen；不为恢复新建会话、不创建派生 ID。
- 不在普通 load 上放宽 lease 检查；`ExecutionBusy` 不提供清除入口。
- 不引入 boot 身份、检查点、准入凭据或新的恢复框架。

## 证据（2026-09-19 实跑）

**环境隔离范围**：新增用例使用临时 HOME 与临时 SQLite；但 `cargo test -p peri-tui --lib`
这一整条命令下，既有 `kit::input_history::tests::*` 会写真实 HOME：`input_history.rs:111-116`
在写入时读 `$HOME` 拼路径，`save_history()` 落盘，而 `input_history_test.rs:19/37` 等用例不隔离
HOME；实测在临时 HOME 下产生 `$HOME/.peri/input-history.json`（内容为测试生成的 `cmd-N` 条目）。
该缺陷为**既有测试缺陷、非本次改动引入**，「不访问真实 `~/.peri`」对本条命令不成立。

| 命令 | 结果 |
| --- | --- |
| `cargo test -p peri-resources --lib` | 122 passed 连续 10/10 全绿（含 `test_worktree_dirty_reset_held_stale_and_exact_generation` 的跨进程 reset 分支：held 拒绝、精确代次解除、过期代次拒绝、原 ID 可再取所有权） |
| `cargo test -p peri-tui --lib` | 并行 16 线程 8/8 全绿（含 `-- dirty_recovery` 7 例、`-- test_dirty_load` 11 例）；默认线程出现 1 次与本改动无关的墙钟敏感失败，见第 5 条 |
| `cargo test -p peri-tui --lib -- --test-threads=16 <bridge+recovery 组合>` | 6/6 全绿（组合含 reviewer 点名的 `acp_bridge_test` 两例） |
| `cargo test -p peri-tui --lib -- --test-threads=1` | 1634 passed, 7 ignored（单线程回归，含 18 个新增用例） |
| `cargo test -p peri-acp-types --lib` | 422 passed（含 `test_recovery_required_data_and_reset_request_wire_contract`） |
| `cargo test -p peri-acp --lib` | 678 passed（含 3 例 `host::requests::tests::recovery_tests::*`） |
| `target/debug/deps/peri_acp-*` 全量、4 路并发、共 18 轮 | 修复后 72 次 0 失败（每次 678 passed）；去掉重试的基线 8/32 失败（见第 7 条） |
| `cargo check --workspace`；四 crate `cargo fmt --check` | 通过；无 diff |

覆盖到的行为：dirty load 返回精确详情 → 取消不写库且不提交会话 → 确认后 CAS 清理
→ 原 ID/binding/frozen/cwd 正常 load；持有稳定锁时（另一进程持锁）拒绝解除；
stale generation 拒绝（并保持 dirty，不误放行）；非同会话/无 data 错误
（如 `ExecutionBusy`）不提示；确认等待期间不新建会话、不发送输入、第二个 load 等待 gate；
确认期间取消按取消收敛并释放 gate/reservation；无法展示确认（未协商能力、headless、
弹窗被占用、未完整渲染、popup 被替换）时 fail closed。

### 第二轮审查发现与修复（2026-09-19）

1. **P1 弹窗替换可挂死 load（已修）**：`confirm_dirty_recovery` 发布 payload 后若在首帧前被
   `open_popup` 覆盖再 `close_popup`，`RecoveryDisplay` 尚未建立、没有 Drop 兜底，残留
   payload 会一直持有响应通道，等待方永久占住 operation gate。现在 popup 替换/撤销边界
   （`open_popup`/`close_popup`）精确结清 dirty 确认载荷并保留新 popup，`PopupOverlay`
   的 effect 另对绕过 helper 的直接写入做一致性收敛；回归覆盖真实
   `open_popup(OAuth)` → `close_popup()` 下 load 收敛、gate 释放、reservation 释放与不写库。
2. **P2 未协商也放行 reset（已修）**：handler 原用 `effective_host_caps()`，MPSC 兜底
   `all_enabled` 会让未经 `initialize` 的连接解除 dirty。现改用 `negotiated_caps()`，
   其他旧 RPC 语义不变；新增“未 initialize 拒绝且不改动存储”回归（再次观测仍为原 dirty 代次）。
3. **并行失败根因纠正（已修）**：第二轮实测与 reviewer 复现一致——并行失败**不是**
   既有无关缺陷，而是本次新增测试只清理 popup 两个 atom，遗漏 interactive load 经
   `project_session_boundary` / `project_execution_cwd` 写入的 `ACTIVE_SESSION_ID`、
   `BRIDGE_RESET_COUNTER`、`VIEW_MODELS`、`ACP_STATE`、input/panel/rewind/todo 等全局
   状态。现在测试以局部 RAII 快照完整恢复上述 atom 并在结束前收束后台任务
   （abort 后 await），失败即恢复、不依赖全套串行掩盖。
4. **补充回归（已修）**：reset 成功后重新 load 失败时——存储保持 clean、代次不推进
   （同代次再 reset 报错配）、binding/frozen 不变、原 ID 之后仍可正常恢复；客户端恢复
   错误阻止无声新建会话，后续重试可用同一 ID；确认期间第二个 load 等待 gate 后按自身
   目标继续；确认期间取消（shutdown）按取消收敛。

5. **并行残余告警（非本次改动引入的断言；本轮写作时点未修复，HEAD 已修复，见本条末）**：
   `kit::acp_events::acp_events_test::group_incremental_test::test_incremental_group_tool_lifecycle`
   偶发失败，逐字段差异**只有** `TuiAssistantBubble.duration_ms` 的 `Some(0)` vs `Some(1)`；
   该字段来自 `Instant::elapsed().as_millis()`（`current_turn.rs` 的冻结路径），而该用例对
   增量快照与全量重建做严格相等比较，属真实墙钟敏感的既有断言。
   **与本改动无关的独立复现**：单独运行该用例（不运行任何新增用例）并施加外部 CPU 负载时
   2/20 失败，无额外负载 0/20。因此其根因不是新增测试写入的全局状态，而是调度压力放大了
   真实时钟取整差异。
   **仍需审查者裁量**：新增用例确实增加了并行负载（16 线程全套含新增用例曾 3/12 失败，
   跳过新增用例 0/18，其中 6 次带外部 CPU 负载）；本轮已把新增用例的等待窗口从 300ms 收到
   100ms 以降低负载，最近 16 线程 8/8、默认线程 2/3 全绿（1 次即上述断言）。是否要为该用例
   引入可控时钟属于该子系统自己的修复范围，本轮未改；**该裁量在 HEAD 已闭环（见下）**。

   **HEAD 复核（回填）**：上述「未修复、属该子系统修复范围」是本轮写作时点的事实陈述，在 HEAD
   已过时——`898da81b`（同日、实施之后）新增 `normalize_assembly_clock`，把装配时刻墙钟派生的
   `duration_ms` / `running_duration_ms` 归一到同一取值（保留 `Some`/`None` 存在性），其余结构字段
   仍逐值比较；实测该用例 16 线程 10/10 全绿，`peri-tui` 全套 1640 passed（16 线程与默认线程均绿）。
   不再需要审查者裁量。

6. **store 级测试根因（已修，属本次测试缺陷）**：第一版
   `test_worktree_dirty_reset_held_stale_and_exact_generation` 用「同进程第二个 store +
   drop lease 后立即再开锁」观察锁语义，在全套并行下约 40% 失败（`ExecutionBusy`）。
   诊断发现真正的缺陷在测试编排：跨进程子进程分支在 `reset_*` 之前先自行
   `acquire_execution_lease`，自己持锁后再 `reset` 必然自冲突（`lsof` 显示持锁 fd 属于该
   子进程自身）。现该用例改为**全部经独立子进程观测**（held 拒绝 / 精确解除 / 过期拒绝 /
   原 ID 再取得），不再依赖同进程锁释放时序；`peri-resources` 全套 10/10 稳定。

7. **`ExecutionBusy` 的另一条真实根因（已修，属生产缺陷）**：第 6 条只覆盖了那一个 store 用例的编排
   缺陷。`peri-acp` 全套在 4 路并发下仍可复现失败（去掉诊断探针干扰后基线 8/32，全部是
   `session is owned by another execution host`），失败点集中在「取得 lease → `mark_clean`
   关掉 fd → 立即再取」的路径（`create_bound_fixture` / `register_session_with_workflow`
   及其调用方）。用 `lsof` 定位持锁 fd 的进程后确认**没有外部持有者**：`flock` 的锁挂在 open
   file description 上，而 `CLOEXEC` 只在子进程 `exec` 时才关闭描述符——会话生命周期必然
   fork 子进程（Git 发现、`sw_vers`、LSP），它们在 exec 前共享父进程的锁描述符；父进程关掉自己
   的 fd 后立即重开同一 inode，内核看到的持有者是刚 fork 出的子进程，于是瞬时被拒。窗口在毫秒级，
   何时被调度取决于机器负载，故只在并行负载下偶发。修复：`lock_execution` 在有界预算内重试
   （10ms 间隔、最多 500ms），预算耗尽仍按原语义上报 `ExecutionBusy`；独占语义不变（跨进程
   busy 用例仍拒绝，dirty 代次语义不变）。证据（同机同条件）：去掉重试的基线 8/32 失败，
   加上重试后 72 次并发全量 0 失败（每次 678 passed）；新增回归用例
   `test_worktree_transient_holder_then_release_is_not_reported_busy` 在去掉重试时失败、保留时
   通过（子进程持锁 250ms 后释放，取得所有权必须发生在等待之后）。影响面不限于测试：生产准入
   同样可能在这个窗口里把一次正常取得所有权上报成「其他进程占用」。

## 策略变更（2026-09-20）：占用不再挡住进入

用户裁决：history 被占、执行所有权不可得或会话库被占时不再向用户报错，而是照常进入、
只记 warning；进入后的写入与执行仍按原独占语义拒绝。本 issue 的确认交互保留为「取回执行
所有权」的入口，不再是「能否进入」的闸门。

| 切片 | 位置 | 变更 |
| --- | --- | --- |
| A：ACP 准入 | `peri-acp/src/host/{workspace.rs,requests/session_lifecycle.rs}`、`peri-acp-types/src/workspace.rs` | `ReadOnlyAdmission`（他处持有 / 精确 dirty 代际 / 本节点不提供所有权）；`acquire_for_load` 返回 `LoadAdmission`，`session/load` 在协商了 `sessionWorkspaceV1` 时降级为只读准入并在 `_meta.peri.sessionWorkspaceV1.read_only` 下发，进程日志记 warning；未协商的客户端仍按原错误失败；`session/fork` 与同一次准入内的 `reacquire_for_load` 不接受降级 |
| B：TUI | `peri-tui/src/acp_client/client/session.rs`、`kit/{atoms,session_boundary,status_bar,steer_consumer}.rs`、两份 `locales/*/main.ftl` | 只读标记写入 `SESSION_READ_ONLY`（仅交互客户端写入，每次会话边界清空）；状态栏新增一段只读原因；dirty 只读准入照常弹确认——接受即取回所有权，取消也让会话按只读进入，取回失败仍保留首次只读准入（不再清空视图、置空会话并记恢复错误）；每个可能被宿主回放历史的 `session/load` 之前各投影一次回放边界；不新增客户端输入闸门（提交仍由 host 的 `require_owner` 拒绝），只把只读会话上的执行所有权拒绝（`-32010`）判为确定拒绝，原稿还回 composer |
| C：启动降级 | `peri-resources/src/context.rs`、`sessions/sqlite_store/{connection,workspace}.rs` | 会话库写打开失败（schema 锁被占、库文件/WAL 不可写）降级为只读打开并记 warning，进入与历史浏览不受影响；`WorkspaceError::ReadOnlyStore` 在进入 SQL 前拒绝新会话与新目录登记；写打开走到版本判定时不认识的 schema 不降级，在版本判定前失败（锁被占、不可写）时降级只按读取兼容的列形状把关、不复查 `user_version` |

不变式（未放宽）：`SessionExecutionLease` 的跨进程独占不变，只读准入不持有 lease；写入与
执行仍要求 owner；`ExecutionBusy` 仍不提供清除入口，只读进入不等于解除占用；未协商
`sessionWorkspaceV1` 的客户端契约不变。

本进程只尝试一次写打开（`Resources` 在启动时构造）：一旦降级即保持只读到进程退出，重启
才重新尝试。降级事件只进进程日志，界面以状态栏只读段说明原因。

证据：`cargo test -p peri-acp --lib` 679 passed、`cargo test -p peri-acp-types --lib` 424 passed、
`cargo test -p peri-tui --lib` 1658 passed（7 ignored）、`cargo test -p peri-resources --lib` 156
passed；定向用例 `cargo test -p peri-resources --lib -- open_with`（6 例，含
`test_open_with_busy_schema_lock_degrades_to_read_only`：持住 schema 锁后降级成功、列表可读、
写入失败）、`cargo test -p peri-tui --lib -- read_only`（10 例：只读准入进入、只读 dirty 准入
接受/取消/取回失败回退、非交互客户端不写交互投影、状态栏三条原因各有一份文案、只读会话上的
`-32010` 判为确定拒绝且不重投）、`cargo test -p peri-tui --lib -- recovery_tests`（25 例，含
每次回放各有边界：只读 dirty 接受后边界 +3、取消 +1）、`cargo test -p peri-acp --lib -- recovery`
（13 例，含未协商与已协商两条准入路径）；`cargo clippy --workspace --all-targets -- -D warnings`
无告警。一次未定因的 `peri-resources` 偶发失败（2026-09-20，批内 2 例失败、无法复现，见「未验证项」）。

## 未验证项

- 未在真实残留子进程现场复现用户路径（本机未复现用户现场），只用受控进程验证锁语义。
- 确认弹窗的实际终端渲染外观未做人工眼测（TUI 渲染按规范不入自动测试）。
- Windows Job 路径与本变更组合未实测。
- 独立 reviewer 已复核实现：此前发现的首帧前弹窗覆盖卡死、未协商能力放行、测试全局状态污染均已修复；确认没有新的真实阻塞。仍未完成真实残留进程现场与用户验收。
- **能力协商握手链路无端到端测试覆盖（用户可达路径的关键环节）**：TUI 测试直接注入私有原子
  （`recovery_test.rs:78/297/319`、`interactive_client():81-83`），ACP 测试直接 `set_pending_caps`
  （`requests_recovery_test.rs:142/170/185`），`unify_wire_baseline_test.rs:405-413` 的回显断言不含
  `peri.sessionRecoveryV1`，`peri_caps.rs` 无该 key 的往返测试；`entry.rs:401` → 回显 → TUI 读回
  这条链路目前只有静态阅读证据。若链路断开，用户会退回原始 bug 且无测试报警。
- 证据环境隔离声明不成立：见上文「环境隔离范围」——既有 `input_history` 用例会写真实 HOME。
- 「稳定锁文件从不删除」只有静态证据（存储层无 unlink）加跨进程 busy 测试；`workspace_test.rs:711-713`
  只覆盖只读路径不建目录，缺「删锁后仍互斥」的反向断言。
- 只读准入的端到端用户现场未实测：未验证「只读准入后提交输入，host 的 `require_owner`
  按 `ExecutionLeaseRequired` 拒绝、且 TUI 按失败受理回执呈现」这条完整链路；TUI 侧的判定
  与呈现已由 `test_read_only_session_submission_is_a_determined_rejection`（判定 + 原稿还回）
  与 `test_steer_read_only_notice_is_translated_in_both_locales`（两份文案）覆盖，host 一段
  仍只有 `require_owner` 的静态阅读与 `-32010` 实测日志（`2026-09-17-p0-…-blocks-input.md`），
  缺「真实 host + 只读准入 + 提交」的往返用例。
- **全量并行跑 `peri-tui --lib` 存在与本次改动无关的偶发失败**（2026-09-20 复现）：5 次运行
  中 2 次失败，分别是 `kit::acp_bridge::tests::test_hitl_bridge_drops_unowned_events_before_all_side_effects`、
  `test_bridge_reset_rehydrates_pending_compact_note_for_same_session`、
  `kit::steer_consumer::tests::test_slow_session_preparation_is_not_capped_by_receipt_deadline`
  ——每次命中的用例都不同，单跑与定向重跑（`recovery_tests` 连跑 10 次、失败用例单独跑）全绿。
  这些用例都是「快照/恢复全局 atom + 断言某件事已经发生」，而 `VIEW_MODELS`/`ACP_STATE` 存在
  大量未标 `#[serial]` 的写入方（如 `acp_events_test/session_events_test.rs` 的
  `*VIEW_MODELS.state().write() = ViewModelsSnapshot::default()`），本次改动没有新增这类写入方，
  但新增用例延长了 serial 占用窗口。判为既有测试基建问题，已立
  `2026-09-20-p2-parallel-lib-test-atom-races.md` 专项收敛，不在本 issue 范围。
- 已知偏差（用户裁决：保持现状、记录在案）：`ReadOnlyAdmission::ExecutionLeaseRequired`
  不区分「本节点只读打开会话库」与「该线程是子线程、执行所有权归根 lease」
  （`acquire_execution_lease_impl` 对 `parent_thread_id.is_some()` 同样返回该错误），而 hidden
  子线程不进 `session/list`。用户显式 `-r <子线程 id>` 恢复时状态栏会显示「会话库不可写」
  （`statusbar-read-only-store`），归因不准确；只读准入本身正确（无执行所有权），不影响写入闸门。
- 启动降级后进程内不会重新尝试写打开：真正可写但瞬时不可用（SQLite busy、锁未释放）时，
  用户会在本次进程内保持只读直到重启。这是当前取舍下的已知后果，不是缺陷；若要改成
  「稍后重试取得可写」，需要新的重试与状态迁移契约，尚未设计。

## 验收要求（状态）

- [x] dirty 会话存在用户可到达的继续工作路径（确认交互 + 显式 reset RPC）。
- [x] 活跃 owner（持锁）拒绝解除、stale generation 拒绝、不误放行。
- [x] 不删除稳定锁、不伪造正常 clean、不自动重放未知结果的工具调用。
  - 限定：「不伪造正常 clean」指不把未知终态当作已收尾。reset 写入的 `clean=1` 在存储中与真实收尾
    不可区分，该保证来自「用户显式授权 + `clean` 列的唯一消费者是准入判断」，不构成旧执行已正常
    结束的证明。
- [x] 取消/失败不提交半成品 session，不串用 cwd 或输入。
- [x] 隔离临时数据库与受控进程回归通过。
- [x] 占用不再挡住进入：只读准入（ACP 标记 + TUI 状态）与启动时的只读降级均有定向用例。
- [ ] 一次未定因的偶发失败（2026-09-20，`cargo test -p peri-resources --lib`）：批内报 2 例失败，
  其中一例是自带过滤器的子进程（`0 passed; 1 failed; 155 filtered out`，与 `ADMISSION_CHILD` /
  `HISTORY_CHILD` / lease 子进程的形态一致）。当次日志未留存；此后 26 次串行 + 22 次并发（含 8 路
  并发，以及只跑 Git 重的 `-- worktree` 一轮）全绿，无法复现、无法定位用例名。同类负载敏感已在本
  目录的 P0 issue（2026-09-17）有前例：并行负载下 5 秒 Git 预算被击穿，出现 7 例
  `Git discovery timed out`。本变更不触及 Git 发现、`SqliteThreadStore::new`、lease 取得与子进程
  测试体（只改 `Resources::open_with` 的降级与只读 store 的写入闸门），但未能证明无关，按未定因
  保留。
- [ ] 真实残留进程现场与用户验收（未完成，故不标记 Fixed）。
