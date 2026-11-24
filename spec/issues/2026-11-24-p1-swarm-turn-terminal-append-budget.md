# P1：swarm 网关在 prompt 回包时关闭 turn，逐条写入放大且迟到 item 把终态会话改回 running

**状态**：已修复。
**优先级**：P1。
**类型**：错误。
**创建日期**：2026-11-24。
**来源**：端到端运行。官方 OpenAI Cookbook `examples/agents_api/apps/data_analyst` 接入本地 `peri-swarm`，持久化用 Peri Loom v1.1.24（hrana / libSQL over HTTP）。
**确认方式**：explorer 只读确认 + 主 agent 复核（基线 `cca9a2be6`）。

## 确认证据

运行观察（同一轮真实 turn）：

- `session/prompt` 已返回 `stop_reason=Some("end_turn")`，但 turn 行在约 23 秒的模型调用后就写了终态，而 `session/update` 的落库持续约 251 秒；turn 关闭后队列中仍有数千条 item。
- 逐条写入的吞吐约 10 item/s（每条 3 个 HTTP 往返）。turn 关闭时未落库的流被按 `complete:true` 冲刷，真实的 `peri/agent_event_done` 没有被消费。
- 会话行最终停在 `in_progress`：最后一条落库 item 把 `state` 无条件写回 `running`，而 `finish_turn` 已因 `active` 为空直接返回。
- 后台存储为 Loom 时，超限写入返回 HTTP 413；之后每一条失败语句都新开一个服务端 session（见下），Loom 的 project 配额 `max_sessions=16` 被永久占满，project 转为 `quota_exceeded`，且无空闲 TTL 回收。

代码证据（基线 `cca9a2be6`）：

- `peri-swarm/src/actor/service.rs:550-570`：`session/prompt` 回包后直接 `finish_turn(&request_id, TurnOutcome::Completed(reply.stop_reason))`；只有 `steer_epoch() > 0` 分支会 `wait_turn_closed`（`service.rs:59`，300 秒）。`pending_tasks` 只写日志，不参与终止判断。
- `peri-swarm/src/actor/reader.rs:684`、`reader.rs:47-49`、`reader.rs:1990-1999`：`finish_turn` 开头的 `settle()` 每 15 毫秒比较 `ingested` 计数、上限 500 毫秒，只观察"通知是否到达"，不证明流已终止。
- `peri-swarm/src/actor/reader.rs:415-418`：投影出的每个 `Projected::Item` 各走一次 `push_item`；`reader.rs:1744-1774` 每次写都在 `seq_lock` 下先 `swarm_last_seq` 再单行 INSERT；`store/turso.rs:380-381` 即 `SELECT COALESCE(MAX(seq), 0) ...`。基线的 store trait 只有 `swarm_append_item`（`store/mod.rs:346`），全仓无批量写入路径。
- `peri-swarm/src/actor/reader.rs:1801-1806` 与 `store/turso.rs:232-233`：每次 append 都执行 `UPDATE swarm_sessions SET state = ?2 ...`，无终态 guard；`reader.rs:674-679` 中 `finish_turn` 在 `active` 不匹配时直接返回，因此迟到 item 无法再被拉回 `idle`。
- `peri-swarm/src/actor/runtime.rs:881-897`：读循环一次只 `client.recv()` 一条并串行 `ingest(...).await`；`peri-acp/src/transport/mpsc.rs:27` 通道容量 1024。
- `target/peri-turso-patches/<sha>/turso_serverless-0.1.3/session.rs:205-208` 与 `:216-224`：非 2xx 响应会 `reset_stream()`（`baton = None`），下一条语句以 `baton: null` 新开 stream；注释原文写明 "Any failure, including a non-200 response, is fatal for the stream (section 9.3)"。
- `peri-swarm/src/domain/item.rs:8`：`MAX_PAYLOAD_BYTES = 1 MiB`，远高于 Loom 的 `max_sql_bytes = 65536`。

## 根因链

触发：一轮包含大量流式 item 的真实 turn（模型 reasoning + 工具调用）。

1. `session/prompt` 回包即触发 `finish_turn`，此时 agent 事件流尚未终止；终态事件被丢弃，仍在队列中的流被当作完成态冲刷。
2. 每条 item 需要 `MAX(seq)` 查询 + INSERT + 会话 UPDATE 三次往返，读循环又是串行单条读取，队列持续堆积，进一步拉大"turn 已关闭、item 仍在落库"的窗口。
3. 每次 append 都无条件把会话写成 `running`；turn 已关闭后没有代码再写 `idle`，会话永久停在 `in_progress`。
4. 任一条语句失败（例如超过后端 `max_sql_bytes` 的批量 append 得到 413）会让客户端丢弃 hrana baton，下一条语句新开服务端 session。后端按"打开过的 session"计费且不回收，最终耗尽 project 配额，后端对该 project 永久不可用。

用户可见后果：答案在 turn 关闭时被截断；会话列表显示 `in_progress`；后续 SSE 回放/终态帧缺失；后端配额耗尽后整个 project 不可用。

## 影响面

- 用户影响：任何使用真实模型与工具的 swarm 会话；Loom 作为首选持久化后端时影响为不可恢复。
- 受影响模块/调用点：`actor/reader.rs`（ingest/append/finish_turn/状态写）、`actor/service.rs`（run_turn 终止）、`actor/runtime.rs`（读循环）、`store/{mod,turso,memory}.rs`（批量写入、seq 缓存）、`peri-acp/src/transport/mpsc.rs`（通道容量与批量读取）。
- 是否可规避：重启网关可清掉卡住的 `in_progress`，但每次重启消耗后端 session；后端配额耗尽后只能换新部署。

## 修复方案

- 范围：
  - `run_turn` 在非 steer 路径等待 `wait_turn_closed`（1800 秒兜底），仅在超时时记录 `agent turn reported no terminal` 并以失败关闭；
  - 读循环批量取通知（`recv_batch`：先 `try_recv`，空队列时一次 `coalesce` 后再排空），通道容量 1024 → 16384；
  - 投影按"连续 item 段"分批落库：`append_items` → 单事务批量 INSERT + 会话 UPDATE，单批 payload 预算 8 KiB（后端 64 KiB `max_sql_bytes` 之下，留出 SQL 文本与转义膨胀，且一次 413 会永久消耗一个后端 session）；
  - `MAX(seq)` 结果缓存在 reader 状态里，冲突时清空并重试一次；
  - 区分进度写：用户输入与 turn 开始写 `running`，流式 item 只在 turn 仍打开时写 `running`，避免迟到 item 改写终态。
- 明确不做：不修改 `MAX_PAYLOAD_BYTES` 的领域上限；不引入后端配额配置项；不改 Loom。
- 回归测试：`peri-swarm/src/actor/reader_test.rs` 新增批量段保持 host 顺序与连续 seq；`store` 侧补 `swarm_append_items` 的冲突与全有或全无语义；既有 `http_contract`/`session_resources`/`loom_store` 测试随签名更新。

## 验收标准

1. 一轮真实 turn 结束后：turn 行 `completed`，`completed_at` 与最后一条 item 的时间差 ≤ 10 秒，会话 `idle`，root assistant 消息全部 `complete`，恰好 1 条 `final_answer`。
2. 整轮运行中后端零 413、零 429。
3. `scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm` 与 `-p peri-acp` 全绿。

## 验证记录

- 端到端（官方 data_analyst 接本地网关 + Loom）：会话 `agent-01a20d14-c5ec-7d20-ab0e-62535861c3ff`，4425 条 item，turn `completed`，`completed_at` 与末条 item 差 −0.8 秒，会话 `idle`，1 条 `final_answer`，答案 22.99% → 11.00%（−11.99pp / −52.15%），9 条 SQL，整轮 0 次 413 / 0 次 429。详见 `docs/verification/peri-swarm-turn-terminal-2026-11-24.md`。
- `bash scripts/check-changed.sh` → exit 0（`-p peri-acp -p peri-swarm -p peri-tui -p peri-wasm`）。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm` → exit 0；lib 298 passed，`actor_runtime` 17、`http_contract` 25、`items_pagination` 13、`mcp_injection` 4、`openai_hosted` 7、`session_resources` 17、`streaming` 15，均 0 failed。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp` → 1532 passed / 1 failed；失败项为既有负载 flake（`host::mcp_v4_wire_fixture::wire_fixture_script_logs_wire_and_answers_tools_call`，单跑通过，见 `docs/verification/peri-swarm-requires-action-2026-11-21.md` 第 13 条），本 PR 未触碰该夹具。
- `cargo fmt -p peri-swarm -p peri-acp -- --check` 与 `git diff --check` → exit 0。
- 第 4 轮（基线 `f5a69b1da`）：`finish_turn_observed` 两处守卫只在当前 epoch 严格大于判定值或 `settle` 前快照时拒绝关闭（有新 steer 接管）；相等或因回滚而更低时，原关闭者继续。`suppressed_terminal` 仍保留完整 `TurnOutcome`；重放 `Failed` 时沿用 `SessionState::Failed` 与 `session.failed` 契约。
  - steer 续跑超时、非 steer 终态超时、prompt 返回错误三处先内联 `await finish_turn`；同一 turn 仍打开才由 `arm_deferred_close` 用 `tokio::spawn` 启动独立的 `close_lost_turn`。任务启动后，取消 `run_turn` future 不会丢掉该关闭者。原 outcome、守卫、分支条件、日志和返回值不变。`close_lost_turn` 仍先尝试关闭，同一 turn 若仍打开则每轮等待至多 300 秒，超时后用原 `TurnOutcome` 重试；这不是累计时长或次数上限。
  - 加入测试 7 前，原有四条定向测试各为 exit 0、`1 passed; 0 failed; 0 ignored; 303 filtered out`，命令见[验收记录第 4 轮](../../docs/verification/peri-swarm-turn-terminal-2026-11-24.md#评审回合补充2026-11-24第-4-轮)：
    - `a_steer_admitted_while_settling_keeps_the_turn_open`
    - `a_terminal_kept_by_a_steer_guard_closes_the_turn_when_the_handoff_fails`
    - `a_failed_timeout_close_kept_by_a_steer_guard_closes_when_the_handoff_fails`
    - `a_terminal_decided_on_an_older_epoch_is_not_used_to_close`
  - 新增两条定向测试，恢复 `>` 后各为 exit 0、`1 passed; 0 failed; 0 ignored; 303 filtered out`：
    - `a_steer_rolled_back_while_settling_lets_the_timeout_close_the_turn`：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm --lib a_steer_rolled_back_while_settling`。回滚发生在 `settle` 内，超时兜底关闭 turn、发出原错误码 `-32603` 和消息 `agent turn reported no terminal`，会话为 `Failed`。
    - `a_stale_decision_epoch_after_rollback_still_closes_the_turn`：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm --lib a_stale_decision_epoch_after_rollback`。回滚后判定 epoch `1` 仍能关闭 turn、发出 `Completed`（`end_turn`），会话为 `Idle`。
  - 剥离重试前的反向验证分别仅将第二、第一次加锁的比较改回 `!=`，测试 5、6 都在「turn 应当关闭」断言处失败（exit 101、`0 passed; 1 failed`）。各自恢复为 `>` 后重跑通过；两次恢复后 `reader.rs` 的 SHA-256 均与临时修改前一致。真实输出见验收记录。较早反向验证中，只保留 `Completed` 能让测试 3 失败；将回滚重放的 `Some(0)` 改为 `None` 未让测试 4 失败，两处当时均已恢复。
  - 测试 7 `a_deferred_lost_terminal_close_is_retried` 以暂停时钟复现单个 steer 在 `settle` 内接管、随后不回滚且没有 continuation 终态；5 秒等待超时后重试关闭，断言 turn 已关闭、`TurnEvent::Failed` 保留原错误码和消息、会话为 `SessionState::Failed`。剥离重试前，临时移除重试循环时在 turn 已关闭的断言失败（exit 101），恢复后 SHA-256 一致且测试通过；本次未重跑反向验证。
  - 测试 8 `a_retry_does_not_close_after_the_continuation_terminal` 以暂停时钟复现重试等待期间的合法关闭。等待任务退出后，断言 turn 已关闭，恰好一个终端 `TurnEvent::Completed`（`end_turn`），无 `Failed`，会话为 `SessionState::Idle`。本次测试 8、测试 7 和原六条定向测试各跑一次，均 exit 0，各 `1 passed; 0 failed; 0 ignored; 305 filtered out`。
  - `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm` → exit 0；合计 404 passed、0 failed、6 ignored。通过数：lib 306、main 0、`actor_runtime` 17、`http_contract` 25、`items_pagination` 13、`loom_store` 0、`mcp_injection` 4、`openai_hosted` 7、`rivet_actor` 0、`session_resources` 17、`streaming` 15、Doc-tests 0；忽略项为 `actor_runtime` 1、`loom_store` 3、`rivet_actor` 2。各目标 `test result` 行见验收记录。
  - `./scripts/cargo-rmcp-patched.sh clippy --locked --workspace --all-targets -- -D warnings`、`./scripts/cargo-rmcp-patched.sh fmt -p peri-swarm -- --check`、`git -C /Users/mino/code/peri/.worktrees/swarm-turn-append-budget diff --check` 均 exit 0。`bash scripts/check-changed.sh` 检查 `-p peri-swarm`，exit 0；内部 metadata 调用转发至同一 Cargo 包装脚本，仓库脚本未修改。
  - 取消边界（源码判断）：如果首次内联 `finish_turn` 在 `settle` 内被取消，独立任务尚未启动，`TurnLease::drop` 只清理 service 槽；没有其他关闭者时，turn 仍可能留在 `ItemLog.active`。这是已有窗口，不是本次剥离重试引入的。
  - 未覆盖：两个 steer 并发派发失败且乱序回滚；回滚取出终态后、重放前再次接纳 steer（含 epoch 数值复用）；旧 turn 的迟到回滚撞上新 turn；每轮 `settle` 都有后续 steer 接管。测试 7、8 只覆盖 `ItemLog`；未直接取消服务层 `run_turn`，未用真实宿主和 300 秒计时分别复现三个分支，本次没有端到端复验。
- 第 5 轮（基线 `767d012c4`）：评审提出 3 条意见，均已修正，状态仍为已修复。
  - steer 身份改为单调递增、永不回退的 `steer_generation`，并新增 `steer_pending` 取代所有 `steer_epoch == 0` / `> 0` 的有无判断：`note_steer` 只递增并置 pending（无 active turn 时返回 0），`abandon_steer` 不递减 generation、只清 pending 并以回滚后的当前 generation 重放保留终态；`finish_turn_observed` 两处守卫改为等值语义（快照值不再等于当前 generation 即让位）。`service.rs` 的 `steered` 判断改用 `steer_pending()`。
  - 新增 `ItemLog::close_lost_turn_after_grace`：detached 安全网先等待至多 `STEER_CONTINUATION_TIMEOUT` 的 continuation 终态，grace 内被 continuation 关闭即返回，超时才强关；`arm_deferred_close` 改用它。
  - 三处丢失终态路径把 `arm_deferred_close` 移到 `finish_turn(..).await` 之前，关闭第 4 轮记录的取消窗口；inline 关闭成功时 detached 任务经 `wait_turn_closed` 立即返回。
  - 新增测试 `a_rollback_does_not_reuse_the_generation_a_later_steer_advances`、`a_grace_close_waits_for_the_continuation_terminal`、`a_grace_close_forces_the_turn_after_the_grace_without_terminal`；原 8 条定向测试的断言改为 pending / generation 语义。两次临时破坏（回滚递减 generation、去掉 grace 等待）分别让测试 9、10 失败，恢复后重跑通过，`reader.rs` SHA-256 与修改前一致。
  - 验证（真实输出）：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm --lib` → exit 0，`test result: ok. 309 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out`；11 条定向测试各 exit 0（`1 passed; 0 failed; 308 filtered out`，两条 grace 测试合并运行时为 `2 passed; 0 failed; 307 filtered out`）；`clippy --locked -p peri-swarm --all-targets -- -D warnings`、`fmt -p peri-swarm -- --check`、`git diff --check`、`bash scripts/check-changed.sh`（检查 `-p peri-swarm`）均 exit 0。详见[验收记录第 5 轮](../../docs/verification/peri-swarm-turn-terminal-2026-11-24.md#评审回合补充2026-11-24第-5-轮)。
  - 未覆盖：服务层取消 `run_turn` 的时序测试（第 3 条只由实现顺序保证）；回滚发生在关闭者已取快照、保留终态尚未写回的交错；旧 turn 的迟到回滚撞上新 turn。本次没有端到端复验。
- 第 6 轮（基线 `767d012c4` 加第 5 轮未提交改动）：独立 verification 复现出第 5 轮引入的两项回归，均已修正，状态仍为已修复。
  - P0（阻塞）：`steer_pending: bool` 丢失「未撤销 steer 数量」。steer A 成功且其 prompt attempt 终态已进入 `suppressed_terminal` 时，若 A 的 `user_input_run_started` 尚未被 reader 处理，steer B 派发失败后的 `abandon_steer(2)` 会把 pending 置 false 并重放 A 的终态，turn 提前关闭；A 的 run 不再被接管、其终态被丢弃，host 仍在执行而 session 已 `idle`。改为 `LogState::outstanding_steers: BTreeSet<u64>`：`note_steer` 插入 generation，`abandon_steer` 只移除自己的值，集合非空时不重放，乱序回滚互不影响。
  - P2：等值守卫把「仅回滚」当成让位。closer 快照 generation `0` 后，`settle` 内先 `note_steer` 再 `abandon_steer` 会让 `1 != 0` 成立而保留终态，turn 只能等 300 秒 / 1800 秒兜底。两处守卫改为「generation 变化**且**仍有 steer 占有 turn（集合非空或 `continuation.is_some()`）才让位」；无 owner 时 closer 正常关闭。
  - 跨 turn ABA：`begin_turn` 与关闭 turn 不再把 `steer_generation` 归零，使其跨 turn 全局单调；`outstanding_steers`、`continuation`、`suppressed_terminal` 仍在两处清空。`note_steer` 无 active turn 仍返回 `0`，`service::steer` 的回退语义不变。`service.rs` 仅更正 `arm_deferred_close` 的顺序注释。
  - 新增测试：`a_failed_steer_does_not_release_an_earlier_steers_turn`、`out_of_order_steer_rollbacks_release_the_turn_only_at_the_last_one`、`a_rollback_during_settle_does_not_defer_the_close`、`a_late_rollback_does_not_release_the_next_turns_steer`。15 条定向测试（旧 11 + 新 4）各 exit 0；lib `313 passed; 0 failed`。
  - 收尾补齐 pre-settle 守卫回归：`a_stale_generation_without_an_owner_does_not_defer_the_close` 在 `begin_turn` 后 `note_steer()`（=`1`）并立即 `abandon_steer(1)`，再以过期期望值 `Some(0)` 直接调 `finish_turn_observed`；断言 turn 已关闭（`active_turn().is_none()`）、唯一 `Completed`（`end_turn`）终态、会话 `Idle`。单跑结果 `1 passed; 0 failed; 0 ignored; 0 measured; 313 filtered out`，lib 全量随之变为 `314 passed; 0 failed`；本轮未改实现代码，未对该用例做反向破坏验证。
  - 反向验证（真实执行）：分别临时改坏 `abandon_steer` 的集合判断、`settle` 后守卫的 owner 条件、`begin_turn` 的 generation 重置，对应测试在 `reader_test.rs:1508`、`:1581`、`:1638`、`:1677` 失败（exit 101，`0 passed; 1 failed`）；恢复后 `reader.rs` SHA-256 与修改前一致（`ecab2f281e4b9e417a86206ffc5da091fe7a6a8f6a212fb6eabf385c605d9b18`）。第 5 轮记录的该哈希不可复现，已在验收记录中更正。
  - 更正第 5 轮「关闭取消窗口」的表述：`arm_deferred_close` 只先于第一个 `finish_turn().await` 执行；`run_turn` 在两个 `wait_turn_closed` 长等待（300 秒 / 1800 秒）中被取消时仍没有 closer，这是既有窗口。
  - `./scripts/cargo-rmcp-patched.sh clippy --locked -p peri-swarm --all-targets -- -D warnings`、`./scripts/cargo-rmcp-patched.sh fmt -p peri-swarm -- --check`、`git diff --check`、`bash scripts/check-changed.sh`（检查 `-p peri-swarm`）均 exit 0。详见[验收记录第 6 轮](../../docs/verification/peri-swarm-turn-terminal-2026-11-24.md#评审回合补充2026-11-24第-6-轮)。
  - 未覆盖：持续 steer 流压力；并发 steer 更细的交错（一成功一失败的其它顺序）；re-pin 分支的直接测试；长等待期间取消的服务层时序测试。没有端到端复验。
