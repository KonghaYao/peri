# P1：异常退出后 history 无法恢复，缺少安全恢复入口

**状态**：Implemented — 独立审查通过，待用户验收（未在真实残留进程现场实测；不标记 Fixed）
**优先级**：P1（用户指定）
**创建日期**：2026-09-19
**最后更新**：2026-09-19（记录用户最终裁决与实施证据；同日回填独立核实结论；补记并行失败的第二条根因——执行锁的 fork 窗口误报 busy——及其修复）

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
| TUI | `peri-tui/src/acp_client/client/{session.rs,requests.rs,client.rs}`、`kit/popups/confirm_popup.rs`、`kit/popup_overlay.rs`、`kit/atoms.rs`、两份 `locales/*/main.ftl` | `load_session_under_gate` 解析 typed details 后，在一次 load transition 内（operation gate 与 reservation 已持有、source/target 与有效 cwd 已固定）等待 `confirm_dirty_recovery`；接受才发送 `peri/session_reset_dirty` 并重新 load，取消直接返回原错误 |

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

## 验收要求（状态）

- [x] dirty 会话存在用户可到达的继续工作路径（确认交互 + 显式 reset RPC）。
- [x] 活跃 owner（持锁）拒绝解除、stale generation 拒绝、不误放行。
- [x] 不删除稳定锁、不伪造正常 clean、不自动重放未知结果的工具调用。
  - 限定：「不伪造正常 clean」指不把未知终态当作已收尾。reset 写入的 `clean=1` 在存储中与真实收尾
    不可区分，该保证来自「用户显式授权 + `clean` 列的唯一消费者是准入判断」，不构成旧执行已正常
    结束的证明。
- [x] 取消/失败不提交半成品 session，不串用 cwd 或输入。
- [x] 隔离临时数据库与受控进程回归通过。
- [ ] 真实残留进程现场与用户验收（未完成，故不标记 Fixed）。
