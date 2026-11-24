# Peri Swarm turn 终止与批量落库验收

代码基线 `cca9a2be6`（`origin/main`）。端到端观察在同一补丁的工作副本上完成（副本
基于 `fe665e60e`，与本 PR 的源码差异只有 `origin/main` 上的两个 `peri-acp` 宿主关闭
提交，未触及本次修改的文件）。端到端证据早于 PR 评审回合的补充修正，后者只收紧
失败路径与并发顺序（见文末「评审回合补充」），未改变被观测的 turn 终止与批量落库
路径。

环境：macOS 单机；`peri-swarm` 网关监听 `127.0.0.1:8080`；持久化使用单个 Peri Loom
v1.1.24 容器（hrana over HTTP，project 配额 `max_sessions=16`、
`max_sql_bytes=65536`）；模型 `deepseek-v4-flash`；工具面是一个 MCP 服务器
（`127.0.0.1:8093/mcp`）。客户端是官方 OpenAI Cookbook
`examples/agents_api/apps/data_analyst`，只把 function tools 换成该 MCP 工具。

## 修复前（同一环境，2026-11-24 09:37–10:10 UTC）

| 观察 | 事实 |
|---|---|
| turn 终止 | `session/prompt` 回包即写终态；真实 `peri/agent_event_done` 在队列中未被消费 |
| 时间尺度 | 模型调用约 23 秒（按 item `MessageId` 的 UUIDv7 时间戳），`session/update` 落库持续约 251 秒 |
| 会话状态 | 最后一条落库 item 把会话写回 `running`，`finish_turn` 已因 `active` 为空返回；会话永久停在 `in_progress` |
| 终态帧 | 仍在队列中的流被按 `complete:true` 冲刷，app 只拿到首条消息 |
| 后端 | 逐条 append 三次往返/item，吞吐约 10 item/s；超限写入得到 HTTP 413 |
| 后端配额 | Loom 审计 `open/quota_exceeded` 991 次、`open/ok` 17 次；project 转为 `quota_exceeded`，无流量时 3 分钟仍不恢复，两个部署（demo3、demo4）因此报废 |

## 修复后（2026-11-24 10:23 UTC）

| 观察 | 事实 |
|---|---|
| 会话 | `agent-01a20d14-c5ec-7d20-ab0e-62535861c3ff` |
| item | 4425 条（message 1183 类、thought 若干、tool_call 24、tool_result 16） |
| turn | `completed`；`completed_at` 与最后一条 item 的差为 **−0.8 秒** |
| 会话状态 | `idle` |
| 官方帧 | root assistant 消息全部 `complete`；恰好 1 条 `final_answer` |
| 答案 | 22.99% → 11.00%（绝对 −11.99 个百分点，相对 −52.15%），附 9 条 SQL |
| 后端错误 | 整轮 **0 次 413、0 次 429** |
| 多消息 turn | 另一次运行（`agent-01a20d13-99b0-79a3-99cf-f2e5eb5107eb`）产生 2 条 commentary + 1 条 final_answer，6 条 root assistant 消息全部 `complete` |

## 仓内回归

```
bash scripts/check-changed.sh                     → exit 0（-p peri-acp -p peri-swarm -p peri-tui -p peri-wasm）
./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm → exit 0
  lib 298 passed / 0 failed；actor_runtime 17、http_contract 25、items_pagination 13、
  mcp_injection 4、openai_hosted 7、session_resources 17、streaming 15，均 0 failed
  ignored 6 项为基线既有（需运行中的 Loom / Rivet / 本地 ACP 测量）
cargo fmt -p peri-swarm -p peri-acp -- --check     → exit 0
git diff --check                                   → exit 0
```

`-p peri-acp` 全量 lib 套件 1532 passed / 1 failed：失败项
`host::mcp_v4_wire_fixture::wire_fixture_script_logs_wire_and_answers_tools_call`
（`channel is empty and sending half is closed`）与本次改动无调用关系，单跑通过，
已在 `docs/verification/peri-swarm-requires-action-2026-11-21.md` 第 13 条记录为既有
负载 flake；本 PR 未修改该夹具。

## 边界与未覆盖

- 单机、单 Loom 实例、单一模型后端、单一 MCP 服务器；未做并发或吞吐验收，1000 Agent
  目标不在本轮证据范围内。
- 批量落库预算 8 KiB 是经验值：低于 Loom 声明的 64 KiB `max_sql_bytes`，并预留 SQL
  文本与转义膨胀。它不是从后端协商出的上限；触发 413 的批次是上一版 24 KiB 预算。
- Loom 的会话计数与会话不回收来自其审计日志与实测（无流量仍 `quota_exceeded`），
  服务端实现不在本仓库；「非 2xx 使 hrana stream 作废」依据 vendored 客户端
  `session.rs` 的注释与实现，服务端行为未独立验证。
- 修复前数值来自一次运行；demo3、demo4 的 project 配额耗尽后无法再查询，因此
  「修复前」状态不可重复观测。
- 官方示例 + 本地网关 + Loom 的端到端驱动未提交到仓库；复验需自行装配该环境，仓内
  可复现的是上面的 Rust 套件与 `scripts/check-changed.sh`。

## 评审回合补充（2026-11-24 12:00 UTC 前后）

CodeRabbit 对 PR head 的评审提出 6 条意见，其中 5 条成立并已修正（均为失败路径或
并发顺序，不改变端到端观测路径）：

| 位置 | 修正 |
|---|---|
| `reader.rs::append_group` | `seq_lock` 覆盖下一次 `MAX(seq)` 缓存写入，避免并发 append 读到陈旧缓存后二次冲突 |
| `reader.rs::append_items` | 按组分批时记录已提交组的 seq；后续组失败不再丢弃前序组的 `first_seq` |
| `reader.rs::set_session_state` | 终态写与进度写共用 `seq_lock`，开放中的进度写不能在 `idle` 之后落库 |
| `service.rs::run_turn` | steer 续跑超时且无 `stop_reason` 时按失败关闭 turn，避免 `turn_in_progress` 永久拒绝后续输入 |
| `store/memory.rs::swarm_append_items` | 单锁内完成校验与写入，满足 trait 的全有或全无语义 |
| `tests/loom_store_test.rs` | 先等 reader 终态（覆盖合并窗口）再兜底 `finish_turn` |

`store/turso.rs` 提出的「progress session 缺失时在事务内回滚」需要触发器或外键等
schema 变更，不属于本次缺陷修复范围，已在 PR 对应 thread 回复说明并列为遗留项。
新增回归：`append_items_rejects_a_run_that_conflicts_without_writing_any_row`、
`append_items_with_a_missing_progress_session_writes_nothing`。

第二轮评审（head `9a9bd6fad`）又提出 2 条，均成立并已修正：

| 位置 | 修正 |
|---|---|
| `reader.rs::append_items_tracked` | 单个分组失败后继续尝试后续分组，最后上报首个错误；已提交分组按 item 下标记录 seq，超尺寸行不再连带丢弃整批 |
| `reader.rs` steer 交接 / `abandon_steer` | steer 期间被抑制的 prompt attempt 终态会保留；交接失败回滚到无 steer 状态时用它关闭 turn，不再等 1800 s 超时后被判失败 |

新增回归：`a_failed_group_does_not_discard_the_rest_of_the_run`、
`a_failed_steer_handoff_closes_the_turn_with_the_prompt_terminal`；lib 298 passed。

## 评审回合补充（2026-11-24，第 4 轮）

基线 `f5a69b1da`；以下是在该 worktree 运行的 Rust 回归，不是端到端复验。

| 位置 | 行为 |
|---|---|
| `reader.rs::finish_turn_observed` | 两处守卫均只在当前 `steer_epoch` 严格大于观察值（终态判定值或 `settle` 前快照）时拒绝关闭，因为有新 steer 接管。epoch 相等或因回滚而更低时，原关闭者继续。拒绝时不冲刷 streams 或发布完成帧；`suppressed_terminal` 仍用 `get_or_insert` 保留请求 ID 和完整 `TurnOutcome`，不覆盖已有终态。 |
| `reader.rs::dispatch_event` / `abandon_steer` | `dispatch_event` 将判定终态时的 epoch 传入 `finish_turn_observed`；交接失败回滚到无 steer 时，以 `Some(0)` 重放保留的终态。 |
| `reader.rs::close_lost_turn` | 先调用 `finish_turn`；同一 turn 若仍打开，最多等待 `within`，超时后用原 `TurnOutcome` 重试。等待中观察到 turn 关闭即返回。上限约束的是每轮等待，不是累计时长或尝试次数。 |
| `service.rs::run_turn` | steer 续跑超时、非 steer 终态超时、prompt 返回错误三个兜底关闭点先内联 `await finish_turn`；同一 turn 仍打开才由 `arm_deferred_close` 用 `tokio::spawn` 启动独立的 `close_lost_turn`。原 outcome、分支条件、日志和返回值不变；每轮等待仍为 `STEER_CONTINUATION_TIMEOUT`（300 秒）。 |

重放 `Failed` 时沿用既有契约：持久化 `TurnStatus::Failed`、将会话置为 `SessionState::Failed` 并发布 `session.failed`。
独立任务持有 runtime 和请求 ID，不依赖 `run_turn` future 的生命周期；启动后，调用返回或被取消都不会丢掉该关闭者。首次内联 `finish_turn` 的 `settle` 仍在交接之前，不受此保护。

### 定向回归

| 测试 | 命令 | 结果 |
|---|---|---|
| `a_steer_admitted_while_settling_keeps_the_turn_open` | `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm --lib a_steer_admitted_while_settling` | exit 0，1 passed，0 failed |
| `a_terminal_kept_by_a_steer_guard_closes_the_turn_when_the_handoff_fails` | `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm --lib a_terminal_kept_by_a_steer_guard` | exit 0，1 passed，0 failed |
| `a_failed_timeout_close_kept_by_a_steer_guard_closes_when_the_handoff_fails` | `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm --lib a_failed_timeout_close_kept_by_a_steer_guard` | exit 0，1 passed，0 failed |
| `a_terminal_decided_on_an_older_epoch_is_not_used_to_close` | `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm --lib a_terminal_decided_on_an_older_epoch` | exit 0，1 passed，0 failed |
| `a_steer_rolled_back_while_settling_lets_the_timeout_close_the_turn` | `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm --lib a_steer_rolled_back_while_settling` | exit 0，1 passed，0 failed |
| `a_stale_decision_epoch_after_rollback_still_closes_the_turn` | `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm --lib a_stale_decision_epoch_after_rollback` | exit 0，1 passed，0 failed |
| `a_deferred_lost_terminal_close_is_retried` | `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm --lib a_deferred_lost_terminal_close_is_retried` | exit 0，1 passed，0 failed |
| `a_retry_does_not_close_after_the_continuation_terminal` | `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm --lib a_retry_does_not_close_after_the_continuation_terminal` | exit 0，1 passed，0 failed |

剥离重试并加入测试 8 后，上表八条命令各跑一次，各命中一项测试，均为 `0 ignored; 305 filtered out`。测试 5、6 先断言回滚到 epoch `0` 后 turn 仍打开，再验证关闭者：测试 5 断言超时兜底关闭 turn、发出 `Failed`（错误码 `-32603`，消息 `agent turn reported no terminal`）并将会话置为 `Failed`；测试 6 断言用判定 epoch `1` 关闭 turn、发出 `Completed`（`end_turn`）并将会话置为 `Idle`。

测试 7 暂停 Tokio 时钟，在首次 `settle` 内注入一个 steer（epoch `1`），不回滚，也不输入 continuation 终态。等待 5 秒重试窗口加 1 秒余量并 `join` 后，断言 turn 已关闭、发出带原错误码 `-32603` 和消息 `agent turn reported no terminal` 的 `TurnEvent::Failed`，且持久化会话状态为 `SessionState::Failed`。

测试 8 暂停 Tokio 时钟，启动兜底任务后等 1 毫秒接纳 steer，再等 20 毫秒，确认 turn 仍打开、兜底任务尚未结束。用 `finish_turn` 模拟续跑合法终态并 `join` 后，断言 turn 已关闭，恰好发出一个终端 `TurnEvent`（`Completed`，`stop_reason = "end_turn"`），没有 `Failed`，持久化会话状态为 `SessionState::Idle`。

### 反向验证

以下是剥离独立任务前的记录，本次未重跑反向验证。测试 5、6 的反向验证及恢复重跑均使用上表对应命令；反向验证时仅将对应比较改回 `!=`，另一处仍为 `>`。

测试 5：仅回退 `settle` 后的比较，exit 101。实际失败输出节选：

```text
test actor::reader::tests::a_steer_rolled_back_while_settling_lets_the_timeout_close_the_turn ... FAILED
thread 'actor::reader::tests::a_steer_rolled_back_while_settling_lets_the_timeout_close_the_turn' (38942864) panicked at peri-swarm/src/actor/reader_test.rs:1323:5:
回滚后超时兜底应当关闭 turn
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 303 filtered out; finished in 0.01s
```

测试 6：仅回退首次加锁的 `expected_epoch` 比较，exit 101。实际失败输出节选：

```text
test actor::reader::tests::a_stale_decision_epoch_after_rollback_still_closes_the_turn ... FAILED
thread 'actor::reader::tests::a_stale_decision_epoch_after_rollback_still_closes_the_turn' (38945779) panicked at peri-swarm/src/actor/reader_test.rs:1358:5:
回滚后先前判定的终态应当关闭 turn
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 303 filtered out; finished in 0.01s
```

每次立即恢复为 `>` 后，重跑各为 exit 0，实际结果均为：

```text
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 303 filtered out; finished in 0.01s
```

两次恢复后 `reader.rs` 的 SHA-256 均与本次反向验证前一致：`b6e254d04effbb054337bf377fb96d2efa27b628357aa7b978a484d88c41c4dd`。

较早的第 4 轮反向验证（新增测试 5、6 前）：

- 测试 3：临时将 `finish_turn_observed` 两处保留逻辑限定为 `Completed` 后，exit 101、`0 passed; 1 failed`；`abandon_steer` 后触发「交接失败后 turn 应当关闭」断言，turn 仍打开。立即恢复后重跑，exit 0、`1 passed; 0 failed`。
- 测试 4：临时将 `abandon_steer` 重放传入的 `Some(0)` 换成 `None` 后，仍 exit 0、`1 passed; 0 failed`，未观察到 turn 提前关闭。立即恢复后重跑，exit 0、`1 passed; 0 failed`。该测试直接向 `finish_turn_observed` 传旧 epoch，未覆盖回滚完成与重放之间有新 steer 的时序，不能证明 `abandon_steer` 的参数变更会被它检测到。
- 两次临时修改前后，`git diff --no-ext-diff --no-color -- peri-swarm/src/actor/reader.rs` 的 SHA-256 一致：`9635b3fa9b2916cb37d11263a27aa6d92ba32d350be0cb60bf0fabdb1369a61f`。

测试 7：仅将 `close_lost_turn` 改为单次 `finish_turn`，用上表命令运行，exit 101。实际失败输出节选：

```text
test actor::reader::tests::a_deferred_lost_terminal_close_is_retried ... FAILED
thread 'actor::reader::tests::a_deferred_lost_terminal_close_is_retried' (39031692) panicked at peri-swarm/src/actor/reader_test.rs:1422:5:
重试应当关闭没有收到续跑终态的 turn
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 304 filtered out; finished in 0.00s
```

恢复重试后，`reader.rs` 的 SHA-256 与临时修改前一致（`095a972bac86a1d976c2852edfbd40a74eaee81345561b146cc96ed9aa9c48fd`），同一测试命令 exit 0：

```text
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 304 filtered out; finished in 0.00s
```

### 全量门禁

`./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm` → exit 0；合计 404 passed、0 failed、6 ignored。忽略项为本地 ACP 测量 1 项、Loom 3 项、Rivet 2 项。macOS 链接器报告 `__eh_frame` 超出 compact unwind 编码上限的警告，命令仍成功。剥离重试并加入测试 8 后，各目标实际输出的 `test result` 行：

```text
lib:
test result: ok. 306 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.26s
main:
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
actor_runtime:
test result: ok. 17 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 6.22s
http_contract:
test result: ok. 25 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
items_pagination:
test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
loom_store:
test result: ok. 0 passed; 0 failed; 3 ignored; 0 measured; 0 filtered out; finished in 0.00s
mcp_injection:
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.61s
openai_hosted:
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.39s
rivet_actor:
test result: ok. 0 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 0.00s
session_resources:
test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
streaming:
test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s
Doc-tests:
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

- `./scripts/cargo-rmcp-patched.sh clippy --locked --workspace --all-targets -- -D warnings` → exit 0。
- `./scripts/cargo-rmcp-patched.sh fmt -p peri-swarm -- --check` → exit 0。
- `git -C /Users/mino/code/peri/.worktrees/swarm-turn-append-budget diff --check` → exit 0。
- `bash scripts/check-changed.sh` → exit 0，检查 `-p peri-swarm`；脚本内部的 metadata 调用也转发至 `./scripts/cargo-rmcp-patched.sh`，未修改仓库脚本。

本次未覆盖的交错：

- 两个 steer 并发派发失败且乱序回滚，未验证 epoch 是否会残留或是否仍有关闭者。
- `abandon_steer` 回滚取出保留终态后、重放前又接纳 steer，未验证真实时序；回滚与再次接纳复用同一个 epoch 数字的情形也未覆盖。
- 旧 turn 的迟到回滚撞上新 turn，未验证是否会影响新 turn 的同值 epoch。
- 每轮 `settle` 都有后续 steer 接管，未验证最终能否关闭；每轮等待有上限不是总体截止时间。

测试 7、8 只覆盖 `ItemLog` 的单 steer 静默或正常结束；未在服务层直接取消 `run_turn`，未用真实宿主和 300 秒计时分别复现三个服务分支，也未做端到端复验。

按源码，若取消发生在首次内联 `finish_turn` 的 `settle` 内，独立任务尚未启动，`TurnLease::drop` 只清理 service 槽；若没有其他关闭者，`ItemLog.active` 仍可能保留。该窗口在剥离重试前已存在，本次未扩展或消除它。

## 评审回合补充（2026-11-24，第 5 轮）

基线 `767d012c4`；以下是在该 worktree 运行的 Rust 回归，不是端到端复验。第 5 轮评审提出
3 条意见，均成立并已修正，只涉及 steer 身份与关闭顺序，不改变端到端观测路径。

| 位置 | 修正 |
|---|---|
| `reader.rs::LogState` / `note_steer` / `abandon_steer` | steer 身份由可复用的 `steer_epoch` 改为单调递增、永不回退的 `steer_generation`，并新增 `steer_pending` 表示「有 steer 待接管 / 已接管」。`note_steer` 只递增 generation 并置 pending，无 active turn（`active.is_none()`）时返回 `0`；`abandon_steer` 回滚不递减 generation：没有已接管的 run 时只清 `steer_pending`，并用回滚后的当前 generation 重放 `suppressed_terminal`（不再传 `Some(0)`）；已有更早被接管的 run 时把该 run 重新绑定到当前 generation，让它继续拥有 turn。`begin_turn` 与关闭 turn 同时重置 generation 与 pending。 |
| `reader.rs::dispatch_event` | queued run 的接管条件、以及 prompt attempt 终态是否属于本 turn，改看 `steer_pending`；continuation 终态仍要求 `run.generation == 当前 generation`。 |
| `reader.rs::finish_turn_observed` | 两处守卫改为「快照 generation 与当前 generation 不相等即让位」的等值语义。settle 期间发生「回滚 + 新 steer」（例如 1 → 2）时，关闭者同样保留终态并返回，不再被复用后的相等值绕过。 |
| `reader.rs::close_lost_turn_after_grace` | 新增安全网入口：先用 `wait_turn_closed` 等待至多 `within`（`STEER_CONTINUATION_TIMEOUT`），turn 已被 continuation 关闭即返回；只有等待超时才执行强关，并保留 `close_lost_turn` 的重试循环作为兜底。 |
| `service.rs::arm_deferred_close` / `run_turn` | detached 任务改为调用 `close_lost_turn_after_grace`；`active_turn()` 早退检查保留。三处丢失终态路径把 `arm_deferred_close` 移到 `finish_turn(..).await` 之前，inline 关闭成功时 detached 任务经 `wait_turn_closed` 立即返回、不做多余强关。`steered` 判断改为 `steer_pending()`，失败回滚后不再误判为已 steer。 |

第 4 轮记录的取消窗口（首次内联 `finish_turn` 在 `settle` 内被取消时安全网尚未启动）只在
`finish_turn().await` 这一层关闭：`arm_deferred_close` 现在先于该调用执行，所以 `settle` 被取消时
已有关闭者。

**更正（第 6 轮）**：它并不先于 `run_turn` 的第一个可取消 await。两个 `wait_turn_closed`
（steer 续跑 300 秒、attempt 回包 1800 秒）都在挂安全网之前；请求 future 在这两个长等待中被取消
时，该 turn 仍没有 closer。这是既有窗口，本轮既未扩大也未消除。

### 定向回归

命令均为 `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm --lib <过滤词>`。

| 测试 | 过滤词 | 结果 |
|---|---|---|
| `a_steer_admitted_while_settling_keeps_the_turn_open` | `a_steer_admitted_while_settling` | exit 0，1 passed，0 failed，308 filtered out |
| `a_terminal_kept_by_a_steer_guard_closes_the_turn_when_the_handoff_fails` | `a_terminal_kept_by_a_steer_guard` | exit 0，1 passed，0 failed，308 filtered out |
| `a_failed_timeout_close_kept_by_a_steer_guard_closes_when_the_handoff_fails` | `a_failed_timeout_close_kept_by_a_steer_guard` | exit 0，1 passed，0 failed，308 filtered out |
| `a_terminal_decided_on_an_older_epoch_is_not_used_to_close` | `a_terminal_decided_on_an_older_epoch` | exit 0，1 passed，0 failed，308 filtered out |
| `a_steer_rolled_back_while_settling_lets_the_timeout_close_the_turn` | `a_steer_rolled_back_while_settling` | exit 0，1 passed，0 failed，308 filtered out |
| `a_stale_decision_epoch_after_rollback_still_closes_the_turn` | `a_stale_decision_epoch_after_rollback` | exit 0，1 passed，0 failed，308 filtered out |
| `a_deferred_lost_terminal_close_is_retried` | `a_deferred_lost_terminal_close_is_retried` | exit 0，1 passed，0 failed，308 filtered out |
| `a_retry_does_not_close_after_the_continuation_terminal` | `a_retry_does_not_close_after_the_continuation_terminal` | exit 0，1 passed，0 failed，308 filtered out |
| `a_rollback_does_not_reuse_the_generation_a_later_steer_advances`（新增） | `a_rollback_does_not_reuse` | exit 0，1 passed，0 failed，308 filtered out |
| `a_grace_close_waits_for_the_continuation_terminal`（新增） | `a_grace_close` | exit 0，2 passed，0 failed，307 filtered out |
| `a_grace_close_forces_the_turn_after_the_grace_without_terminal`（新增） | `a_grace_close` | exit 0，2 passed，0 failed，307 filtered out |

新增测试说明：

- `a_rollback_does_not_reuse_the_generation_a_later_steer_advances`：暂停时钟，关闭者在 handoff 终态判定
  generation `1` 后进入 `settle`；期间先 `abandon_steer(1)` 再 `note_steer()`，断言 generation 为
  `2`、pending 为真，并由 `user_input_run_started` 接管。关闭者返回后断言 turn 仍打开、没有终态帧；
  随后 `peri/agent_event_done`（`req-2`）由 continuation 关闭 turn，唯一终态是 `Completed`
  （`end_turn`），会话为 `Idle`。
- `a_grace_close_waits_for_the_continuation_terminal`：steer 被 queued run 接管后调用
  `close_lost_turn_after_grace(.., 5s)`；1 秒时断言 turn 仍打开且安全网任务未结束，随后 continuation
  终态关闭 turn，唯一终态为 continuation 的 `Completed`（`end_turn`），会话为 `Idle`。
- `a_grace_close_forces_the_turn_after_the_grace_without_terminal`：同样的接管场景，但 grace 内没有
  终态；等待 6 秒后断言 turn 已关闭，唯一终态保留原 `Failed`（错误码 `-32603`、消息
  `agent turn reported no terminal`），会话为 `Failed`。

原 8 条测试的断言改为 pending / generation 语义：仍打开时断言 `steer_generation()` 与
`steer_pending()`；回滚后断言 generation 保持 `1`、pending 为假，再验证保留终态或超时兜底关闭 turn。

### 反向验证

第 5 轮新增的两条测试各做一次临时破坏，运行同一命令后立即恢复。

测试 9：把 `abandon_steer` 的回滚改回递减 generation（`steer_generation = generation - 1`）后，
exit 101。先命中断言：

```text
test actor::reader::tests::a_rollback_does_not_reuse_the_generation_a_later_steer_advances ... FAILED
thread 'actor::reader::tests::a_rollback_does_not_reuse_the_generation_a_later_steer_advances' (39247257) panicked at peri-swarm/src/actor/reader_test.rs:1424:5:
assertion `left == right` failed
  left: 1
 right: 2
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 308 filtered out; finished in 0.01s
```

再临时放宽该数值断言（仅测试文件，随后恢复），同一次破坏在行为断言处失败：

```text
test actor::reader::tests::a_rollback_does_not_reuse_the_generation_a_later_steer_advances ... FAILED
thread 'actor::reader::tests::a_rollback_does_not_reuse_the_generation_a_later_steer_advances' (39248496) panicked at peri-swarm/src/actor/reader_test.rs:1439:5:
assertion `left == right` failed: the stale closer must not close a turn the second steer took over
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 308 filtered out; finished in 0.01s
```

测试 10：去掉 `close_lost_turn_after_grace` 的等待、直接调用 `close_lost_turn` 后，exit 101：

```text
test actor::reader::tests::a_grace_close_waits_for_the_continuation_terminal ... FAILED
thread 'actor::reader::tests::a_grace_close_waits_for_the_continuation_terminal' (39249710) panicked at peri-swarm/src/actor/reader_test.rs:1630:5:
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 308 filtered out; finished in 0.01s
```

第 1630 行是「grace 期间 turn 仍打开」的断言：立即强关会先发出 `Failed` 并关闭 turn。测试 9、10
恢复后重跑均 exit 0。

**更正（第 6 轮）**：第 5 轮回填时记录的 `426c55bb…` 与工作树不符，已撤回该证据；第 6 轮对当前代码
重新做了破坏性验证，重新记录 `ecab2f28…`，真实哈希与失败输出见下文第 6 轮「反向验证」。

### 全量门禁

- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm --lib` → exit 0：

  ```text
  test result: ok. 309 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.24s
  ```

- `./scripts/cargo-rmcp-patched.sh clippy --locked -p peri-swarm --all-targets -- -D warnings` → exit 0，
  末行 `Finished \`dev\` profile [unoptimized + debuginfo] target(s) in 1.95s`（最后一次增量检查；
  全量检查同轮为 `50.76s`）。
- `./scripts/cargo-rmcp-patched.sh fmt -p peri-swarm -- --check` → exit 0。
- `git diff --check` → exit 0。
- `bash scripts/check-changed.sh` → exit 0，输出 `[check-changed] cargo check --locked -p peri-swarm`。

### 本次未覆盖

- 第 3 条（安全网先于首个可取消 await 挂上）没有服务层取消测试：现有夹具无法在 `run_turn` 的
  `settle` 内直接 drop 请求 future。实现上保证 `arm_deferred_close` 在 `finish_turn(..).await` 之前
  执行，且 inline 关闭成功时 detached 任务经 `wait_turn_closed` 立即返回；该顺序由源码审查确认，
  无运行时测试。
- 回滚发生在「某个关闭者已经取完快照、但保留终态尚未写回」的精确交错：`abandon_steer` 的重放
  可能取不到该终态，turn 由 `close_lost_turn_after_grace` 的 grace 超时关闭。未构造该时序测试。
- 旧 turn 的迟到回滚撞上新 turn：`finish_turn_observed` 的 `active` 检查会拒绝，但 generation 在
  `begin_turn` 时归零，新 turn 可能再次使用相同数值；本次未覆盖该交错。
- 测试 9、10 只覆盖 `ItemLog`；未用真实宿主与 300 秒计时复现服务分支，本次没有端到端复验。

## 评审回合补充（2026-11-24，第 6 轮）

基线 `767d012c4` 加第 5 轮未提交改动；以下是在该 worktree 运行的 Rust 回归，不是端到端复验。
独立 verification 推导并复现出第 5 轮引入的两项回归（P0 阻塞、P2 行为差异）；本轮修正它们，
并补上跨 turn 的 generation 身份。代码只改 `reader.rs`、`service.rs`（一处注释）、
`reader_test.rs`，另加本记录与 issue 的对应小节。

### 回归与修复

| 问题 | 触发 | 第 5 轮行为 | 本轮修复 |
|---|---|---|---|
| P0：`bool` 丢失「未撤销 steer 数量」 | steer A 派发成功，prompt attempt 的 `cancelled` 终态进入 `suppressed_terminal`；A 的 `user_input_run_started` 尚未被 reader 处理时，steer B 派发失败 | `abandon_steer(2)` 把 `steer_pending` 置 false 并重放 A 的终态，turn 提前关闭；A 的 run_started 不再被接管、其终态被丢弃，host 仍在跑 A 而 session 已 `idle` | `LogState::outstanding_steers: BTreeSet<u64>` 取代 `steer_pending: bool`；`abandon_steer` 只移除自己的 generation，集合非空时不重放 |
| P2：仅回滚被当成让位 | closer 在 `settle` 前快照 generation `0`；`settle` 期间 `note_steer()`（→1）后立即 `abandon_steer(1)` | 等值守卫 `1 != 0` 让位并保留终态，turn 只能等 300 秒 / 1800 秒兜底 | 两处守卫改为「generation 变化**且**仍有 steer 占有 turn 才让位」；仅回滚时 closer 正常关闭 |
| 跨 turn ABA | 已关闭 turn 的迟到 `abandon_steer(g)` 与新 turn 的第一个 steer 同值 | `begin_turn` 把 generation 归零，迟到回滚会清掉新 turn 的 steer | `steer_generation` 跨 turn 全局单调；`begin_turn` 与关闭 turn 都不再归零；`outstanding_steers`、`continuation`、`suppressed_terminal` 仍在两处清空 |

新语义（`peri-swarm/src/actor/reader.rs`）：

- `LogState::steer_owns_turn()`：`!outstanding_steers.is_empty() || continuation.is_some()`，即
  「有 steer 占有 turn」。`steer_pending()`、`dispatch_event` 的接管/终态归属、两处守卫统一用它。
- `note_steer`：无 active turn（`active.is_none()`）返回 `0`（`service::steer` 的「按普通 turn 运行」回退语义不变），
  否则递增 generation 并插入集合。
- `abandon_steer(g)`：`remove(&g)` 失败说明已回滚或来自已关闭的 turn，直接返回，不影响其它 steer；
  否则按序判断——已接管的 run 且 `run.generation >= g`：run 拥有 turn，不重放；更早的 continuation：
  把该 run 重绑到当前 generation（不清集合）；无 continuation：只有集合已空才取出
  `suppressed_terminal` 重放。重放仍走 `finish_turn_observed(.., Some(g))`；重放前来了新 steer 时，
  守卫按新语义让位并保留终态。
- 关闭 turn 与 `begin_turn` 不再重置 `steer_generation`；因此 `service::steer` 的 `epoch == 0`
  仍只表示「没有 active turn」。

### 定向回归

命令均为 `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm --lib -- <过滤词>`。

| 测试 | 过滤词 | 结果 |
|---|---|---|
| `a_failed_steer_handoff_closes_the_turn_with_the_prompt_terminal` | `a_failed_steer_handoff_closes_the_turn_with_the_prompt_terminal` | exit 0，1 passed，0 failed，312 filtered out |
| `a_steer_admitted_while_settling_keeps_the_turn_open` | `a_steer_admitted_while_settling` | exit 0，1 passed，0 failed，312 filtered out |
| `a_terminal_kept_by_a_steer_guard_closes_the_turn_when_the_handoff_fails` | `a_terminal_kept_by_a_steer_guard` | exit 0，1 passed，0 failed，312 filtered out |
| `a_failed_timeout_close_kept_by_a_steer_guard_closes_when_the_handoff_fails` | `a_failed_timeout_close_kept_by_a_steer_guard` | exit 0，1 passed，0 failed，312 filtered out |
| `a_terminal_decided_on_an_older_epoch_is_not_used_to_close` | `a_terminal_decided_on_an_older_epoch` | exit 0，1 passed，0 failed，312 filtered out |
| `a_steer_rolled_back_while_settling_lets_the_timeout_close_the_turn` | `a_steer_rolled_back_while_settling` | exit 0，1 passed，0 failed，312 filtered out |
| `a_stale_decision_epoch_after_rollback_still_closes_the_turn` | `a_stale_decision_epoch_after_rollback` | exit 0，1 passed，0 failed，312 filtered out |
| `a_deferred_lost_terminal_close_is_retried` | `a_deferred_lost_terminal_close_is_retried` | exit 0，1 passed，0 failed，312 filtered out |
| `a_retry_does_not_close_after_the_continuation_terminal` | `a_retry_does_not_close_after_the_continuation_terminal` | exit 0，1 passed，0 failed，312 filtered out |
| `a_rollback_does_not_reuse_the_generation_a_later_steer_advances` | `a_rollback_does_not_reuse` | exit 0，1 passed，0 failed，312 filtered out |
| `a_grace_close_waits_for_the_continuation_terminal` / `a_grace_close_forces_the_turn_after_the_grace_without_terminal` | `a_grace_close` | exit 0，2 passed，0 failed，311 filtered out |
| `a_failed_steer_does_not_release_an_earlier_steers_turn`（新增） | `a_failed_steer_does_not_release_an_earlier_steers_turn` | exit 0，1 passed，0 failed，312 filtered out |
| `out_of_order_steer_rollbacks_release_the_turn_only_at_the_last_one`（新增） | `out_of_order_steer_rollbacks_release_the_turn_only_at_the_last_one` | exit 0，1 passed，0 failed，312 filtered out |
| `a_rollback_during_settle_does_not_defer_the_close`（新增） | `a_rollback_during_settle_does_not_defer_the_close` | exit 0，1 passed，0 failed，312 filtered out |
| `a_stale_generation_without_an_owner_does_not_defer_the_close`（新增，收尾） | `a_stale_generation_without_an_owner_does_not_defer_the_close` | exit 0，1 passed，0 failed，0 ignored，313 filtered out |
| `a_late_rollback_does_not_release_the_next_turns_steer`（新增） | `a_late_rollback_does_not_release_the_next_turns_steer` | exit 0，1 passed，0 failed，312 filtered out |

新增测试说明：

- `a_failed_steer_does_not_release_an_earlier_steers_turn`（P0 回归）：steer A 成功后其 prompt
  attempt 的 `cancelled` 终态进入 `suppressed_terminal`、turn 仍打开；A 的 `user_input_run_started`
  尚未处理时 `note_steer()` 得到 `2` 并 `abandon_steer(2)`。断言 `steer_pending()` 仍为真、turn 仍
  打开、没有终态帧；随后 A 的 run 被接管，其 `agent_event_done` 关闭 turn，唯一终态是
  `Completed`（`end_turn`），会话为 `Idle`。
- `out_of_order_steer_rollbacks_release_the_turn_only_at_the_last_one`：两个 steer 都失败，先
  `abandon_steer(2)`，断言 turn 仍打开、没有终态帧；再 `abandon_steer(1)`，集合为空后才重放保留的
  `cancelled` 终态并关闭 turn。
- `a_rollback_during_settle_does_not_defer_the_close`（P2 回归）：closer 以
  `finish_turn_observed(.., Some(0))` 进入 `settle` 后，`note_steer()` 与 `abandon_steer(1)` 均在
  `settle` 内完成。closer 返回后断言 turn 已关闭、发出 `Completed`（`end_turn`）、会话为 `Idle`，
  而不是保留终态等兜底。
- `a_stale_generation_without_an_owner_does_not_defer_the_close`（pre-settle 分支）：`begin_turn` 后
  `note_steer()` 得到 `1` 并立即 `abandon_steer(1)`，此时无 outstanding steer、无 continuation、无
  `suppressed_terminal`；随后直接以过期的期望值 `Some(0)` 调用 `finish_turn_observed`。断言 turn 已关闭
  （`active_turn().is_none()`）、恰好一个终态 `Completed`（`end_turn`）、会话为 `Idle`。与
  `a_rollback_during_settle_does_not_defer_the_close`（post-settle）互补：本用例在进入 `settle` 前就让
  generation 变化且无 owner。断言的是关闭结果、终态帧与会话状态，不是内部 generation 字段。本轮不改
  实现代码，未对它做反向破坏验证。
- `a_late_rollback_does_not_release_the_next_turns_steer`：turn 1 `note_steer() == 1` 后关闭；新 turn
  `begin_turn` 后 `note_steer()` 大于 1，`abandon_steer(1)` 不得清掉新 turn 的 steer，turn 仍打开、
  会话为 `Running`。

### 反向验证（第 6 轮，真实执行）

先记录 `shasum -a 256 peri-swarm/src/actor/reader.rs`：

```text
ecab2f281e4b9e417a86206ffc5da091fe7a6a8f6a212fb6eabf385c605d9b18  peri-swarm/src/actor/reader.rs
```

三次破坏均在 `reader.rs` 内做最小改动，跑完对应测试后立即恢复，再校验哈希一致。

破坏 1：把 `abandon_steer` 的 `let unowned = state.outstanding_steers.is_empty();` 改成
`let unowned = true;`（等同第 5 轮的 bool 语义）。两条 P0 测试均 exit 101：

```text
test actor::reader::tests::a_failed_steer_does_not_release_an_earlier_steers_turn ... FAILED
thread 'actor::reader::tests::a_failed_steer_does_not_release_an_earlier_steers_turn' (39338539) panicked at peri-swarm/src/actor/reader_test.rs:1508:5:
steer A still owns the turn
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 312 filtered out; finished in 0.01s
```

```text
test actor::reader::tests::out_of_order_steer_rollbacks_release_the_turn_only_at_the_last_one ... FAILED
thread 'actor::reader::tests::out_of_order_steer_rollbacks_release_the_turn_only_at_the_last_one' (39339145) panicked at peri-swarm/src/actor/reader_test.rs:1581:5:
steer 1 is still outstanding
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 312 filtered out; finished in 0.02s
```

破坏 2：去掉 `settle` 后守卫的 `&& state.steer_owns_turn()`（等同第 5 轮的等值语义），exit 101：

```text
test actor::reader::tests::a_rollback_during_settle_does_not_defer_the_close ... FAILED
thread 'actor::reader::tests::a_rollback_during_settle_does_not_defer_the_close' (39340264) panicked at peri-swarm/src/actor/reader_test.rs:1638:5:
a rolled-back steer must not defer the close
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 312 filtered out; finished in 0.01s
```

破坏 3：在 `begin_turn` 恢复 `state.steer_generation = 0;`（等同第 5 轮的跨 turn 归零），exit 101：

```text
test actor::reader::tests::a_late_rollback_does_not_release_the_next_turns_steer ... FAILED
thread 'actor::reader::tests::a_late_rollback_does_not_release_the_next_turns_steer' (39341381) panicked at peri-swarm/src/actor/reader_test.rs:1677:5:
the generation must not be reused across turns: 1
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 312 filtered out; finished in 0.02s
```

恢复后 `shasum -a 256 peri-swarm/src/actor/reader.rs` 仍为
`ecab2f281e4b9e417a86206ffc5da091fe7a6a8f6a212fb6eabf385c605d9b18`，与破坏前一致；四条新增测试
重跑均 exit 0。

### 全量门禁

- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm --lib` → exit 0：

  ```text
  test result: ok. 313 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.24s
  ```

- `./scripts/cargo-rmcp-patched.sh clippy --locked -p peri-swarm --all-targets -- -D warnings` →
  exit 0，末行 `Finished \`dev\` profile [unoptimized + debuginfo] target(s) in 1.86s`（最后一次增量检查）。
- `./scripts/cargo-rmcp-patched.sh fmt -p peri-swarm -- --check` → exit 0。
- `git diff --check` → exit 0。
- `bash scripts/check-changed.sh` → exit 0，输出 `[check-changed] cargo check --locked -p peri-swarm`。

收尾补充：新增 pre-settle 用例后重跑 `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm --lib`
→ exit 0，`test result: ok. 314 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.24s`；
上面 313 的计数是加入该用例前的真实输出（既有行的 `filtered out` 计数同理，加入该用例后会多 1）。同轮 `fmt -p peri-swarm -- --check`、`git diff --check`、
`bash scripts/check-changed.sh` 均 exit 0。

### 本次未覆盖

- 持续 steer 流（每次 `settle` 都有新 steer 接管）：turn 会一直保留终态，直到某个 closer 观察到
  无 owner 或安全网强关；每轮等待有上限，不构成总体截止时间。未构造该压力测试。
- 两个并发 steer 的「一成功一失败」乱序组合只覆盖了「都失败且逆序回滚」与「A 成功后 B 回滚且 A
  的 run_started 尚未到达」两种顺序；更细的交错（B 回滚与 A 的 run_started 同时进行）由 `Mutex`
  串行化，但未设计确定性测试。
- `abandon_steer` 的 re-pin 分支（更早 continuation 存在时把 run 重绑到当前 generation）没有测试：
  现有用例的回滚都发生在尚未接管时，没有一条在 continuation 已存在后回滚另一个 steer。该行为从
  第 5 轮保留，只经源码审查确认，未做运行时验证。
- `run_turn` 在两个长等待（300 秒 / 1800 秒）中被取消时仍没有 closer；这是既有窗口，见上文更正。
- 仍只覆盖 `ItemLog`；未直接取消服务层 `run_turn`，未用真实宿主计时复现服务分支，没有端到端复验。
