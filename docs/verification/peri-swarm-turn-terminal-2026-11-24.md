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

> 第 7 轮修订：本节的守卫 token（「generation 变化且 `steer_owns_turn()`」）与 `abandon_steer`
> 的 re-pin 分支已被替换，见[第 7 轮](#评审回合补充2026-11-24第-7-轮)；本节其余结论未变。

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

## 评审回合补充（2026-11-24，第 7 轮）

> 第 8 轮修订：本轮的 `max_outstanding()`、接管记「当前 `steer_generation`」、continuation 终态
> 「不通过即丢弃」、`suppress_terminal` 的 `get_or_insert` 与「集合非空即不重放」均已被替换，见
> [第 8 轮](#评审回合补充2026-11-24第-8-轮)；本节其余结论与真实输出未变，保留作历史记录。

基线 `10b77bd99` 加本轮未提交改动；以下是在该 worktree 运行的 Rust 回归，不是端到端复验。
CodeRabbit 提出两条 Major，先用源码推演确认成立，再重构 steer 归属判定。代码只改
`peri-swarm/src/actor/reader.rs`、`peri-swarm/src/actor/reader_test.rs`，另加本记录与 issue 的对应
小节；`service.rs`（`steer_pending()` 调用不变）、store、schema、`Cargo.toml` 未改，未提交。

### 两条 Major

| 问题 | 触发序列 | 第 6 轮行为 | 本轮修复 |
|---|---|---|---|
| A：回滚后的 continuation 终态被滞留 | continuation 终态以期望值 `1` 通过过滤进入 `settle`；`settle` 期间 `note_steer()`（→2）后 `abandon_steer(2)` | `abandon_steer` 命中 `Some(run) if run.generation >= generation` 直接返回；post-settle 守卫见 `steer_generation` 变化且 `steer_owns_turn()`（continuation 仍在）→ 保留终态并返回。此后没有路径重放，turn 滞留到服务层 300 秒 / 1800 秒兜底 | 让位判据改为「`max_outstanding() > 快照`」：note 后立即回滚把最大值带回快照值，closer 继续正常关闭，终态得以发布 |
| B：失败 steer 把新 steer 的 ownership 转给旧 run | A 的 run 在 gen 1 被接管；B、C 分别 note 到 2、3；B 失败触发 `abandon_steer(2)` | re-pin 分支把 A 的 generation 改成当时的 `steer_generation`（3，属 C）；A 的终态通过 `run.generation == steer_generation` 检查并关闭 turn，清掉 C 的保留 | 删除 re-pin；continuation 终态过滤改为 `run.generation >= max_outstanding()`。A（1）不满足 3 → 丢弃、turn 保持打开；C 的 run 接替后才关闭 |

### 归属判定（`peri-swarm/src/actor/reader.rs`）

- owner token：`LogState::max_outstanding()` 返回 `outstanding_steers` 的最大值（空集合为 `0`），
  取代「`steer_generation` 是否变化」作为让位判据。`steer_generation` 仍是全局单调的分配器，只用于
  接管时给 run 记录 generation；关闭 turn 与 `begin_turn` 都不重置它，只清空集合、`continuation`、
  `suppressed_terminal`。
- `finish_turn_observed(request_id, outcome, expected_max_outstanding, source)`：
  - pre-settle：`Some(expected)` 且 `max_outstanding() > expected` → 保留终态并返回；
  - 快照 `max_outstanding()`，`settle` 后：`active` 不再匹配 → 返回；`max_outstanding() > 快照`
    → 保留终态并返回；否则关闭。
  - 即只有「`settle` 期间出现且仍未被回滚的新 steer」才让位；note 后立即回滚不再让位，判据不再是
    generation 是否变化。
- `dispatch_event`：
  - queued `TurnStarted` 的接管条件为 `queued && active.is_some() && !outstanding_steers.is_empty()`，
    `run.generation` 记接管时的 `steer_generation`；已接管的 run 单独存在时不接管第二个。
  - prompt attempt 终态：`steer_owns_turn()` 为真 → 以 `source: None` 存入 suppressed；否则以
    `Some(0)` 关闭（此时最大未撤销值为 `0`）。
  - continuation 终态：`run.request_id == running && run.generation >= max_outstanding()`，通过时以
    `Some(max_outstanding())` 关闭并传 `source: Some(running)`；不通过时丢弃（不抑制）。
  - legacy `None` request id 分支保持原样。
- `SuppressedTerminal { request_id, outcome, source }`：`abandon_steer` 只在集合清空后尝试重放；
  `source: None` 要求没有 continuation，`source: Some(id)` 要求当前 continuation 正是该 run。重放走
  `finish_turn_observed(.., Some(0), source)`，`source` 原样带回，重放期间再来新 steer 时按同一来源
  重新保留。re-pin 分支已删除，adopted run 不再被改写 generation。
- `service.rs` 未改逻辑：`steer_pending()` 仍是 `steer_owns_turn()` 的别名（集合非空或 continuation
  存在），语义仍是「仍有 steer / continuation 占有 turn」。

### 定向回归

命令均为 `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm --lib -- <过滤词>`。
旧 17 + 新 4 共 21 条逐条运行，各 exit 0，各为
`1 passed; 0 failed; 0 ignored; 0 measured; 317 filtered out`。

| 测试 | 过滤词 | 结果 |
|---|---|---|
| `a_failed_steer_handoff_closes_the_turn_with_the_prompt_terminal` | 同名 | exit 0，1 passed |
| `a_steer_admitted_while_settling_keeps_the_turn_open` | 同名 | exit 0，1 passed |
| `a_terminal_kept_by_a_steer_guard_closes_the_turn_when_the_handoff_fails` | 同名 | exit 0，1 passed |
| `a_failed_timeout_close_kept_by_a_steer_guard_closes_when_the_handoff_fails` | 同名 | exit 0，1 passed |
| `a_terminal_decided_on_an_older_epoch_is_not_used_to_close` | 同名 | exit 0，1 passed |
| `a_steer_rolled_back_while_settling_lets_the_timeout_close_the_turn` | 同名 | exit 0，1 passed |
| `a_stale_decision_epoch_after_rollback_still_closes_the_turn` | 同名 | exit 0，1 passed |
| `a_deferred_lost_terminal_close_is_retried` | 同名 | exit 0，1 passed |
| `a_retry_does_not_close_after_the_continuation_terminal` | 同名 | exit 0，1 passed |
| `a_rollback_does_not_reuse_the_generation_a_later_steer_advances` | 同名 | exit 0，1 passed |
| `a_grace_close_waits_for_the_continuation_terminal` | 同名 | exit 0，1 passed |
| `a_grace_close_forces_the_turn_after_the_grace_without_terminal` | 同名 | exit 0，1 passed |
| `a_failed_steer_does_not_release_an_earlier_steers_turn` | 同名 | exit 0，1 passed |
| `out_of_order_steer_rollbacks_release_the_turn_only_at_the_last_one` | 同名 | exit 0，1 passed |
| `a_rollback_during_settle_does_not_defer_the_close` | 同名 | exit 0，1 passed |
| `a_stale_generation_without_an_owner_does_not_defer_the_close` | 同名 | exit 0，1 passed |
| `a_late_rollback_does_not_release_the_next_turns_steer` | 同名 | exit 0，1 passed |
| `a_rolled_back_steer_does_not_strand_the_continuation_terminal`（新增，Finding A） | 同名 | exit 0，1 passed |
| `a_failed_newer_steer_does_not_hand_the_turn_to_the_older_run`（新增，Finding B） | 同名 | exit 0，1 passed |
| `a_rolled_back_steer_still_lets_the_adopted_run_close_the_turn`（新增，B 的另一半） | 同名 | exit 0，1 passed |
| `a_rolled_back_steer_does_not_replay_the_prompts_terminal_for_a_live_run`（新增，prompt 终态） | 同名 | exit 0，1 passed |

旧用例中直接调用 `finish_turn_observed` 的 5 处补上 `source = None`（4 参数签名），断言未改。
新增测试说明：

- `a_rolled_back_steer_does_not_strand_the_continuation_terminal`（Finding A）：steer 1 的 queued run
  `req-2` 被接管后，其 `agent_event_done` 在暂停时钟下进入 `settle`；期间 `note_steer()`（→2）并
  `abandon_steer(2)`。断言 turn 已关闭、恰好一个 `Completed`（`end_turn`）终态、会话 `Idle`。
- `a_failed_newer_steer_does_not_hand_the_turn_to_the_older_run`（Finding B）：A 的 run `req-2` 在 gen 1
  接管；`note_steer()`→2、→3；`abandon_steer(2)`；A 的 `agent_event_done` 到达后断言 turn 仍打开、
  没有终态帧（C 仍 outstanding）；随后 `req-3` 的 `user_input_run_started` 被接管，`req-3` 的终态关闭
  turn，唯一终态为 `Completed`（`end_turn`），会话 `Idle`。
- `a_rolled_back_steer_still_lets_the_adopted_run_close_the_turn`（B 的另一半）：A 接管后
  `note_steer()`→2 并立即 `abandon_steer(2)`（无 C），A 的终态必须关闭 turn，唯一终态
  `Completed`（`end_turn`），会话 `Idle`。
- `a_rolled_back_steer_does_not_replay_the_prompts_terminal_for_a_live_run`：steer 1 的 prompt attempt
  `cancelled` 终态被保留、`req-2` 接管后，再 note→2 并回滚 2 和 1（集合清空）。断言 turn 仍打开、
  没有 `cancelled` 终态；`req-2` 的 `end_turn` 是唯一发布终态，会话 `Idle`。

### 反向验证（第 7 轮，真实执行）

先记录 `shasum -a 256 peri-swarm/src/actor/reader.rs`：

```text
fc6d38aeab183bb2070f705117c577311630ca3babb2c0e3e40f4f83a1b22af0  peri-swarm/src/actor/reader.rs
```

两次破坏都在 `reader.rs` 内做最小改动，跑完对应测试后立即恢复，再校验哈希一致。

破坏 1：把 post-settle 守卫改回第 6 轮形式
`if state.steer_generation != snapshot && state.steer_owns_turn()`（即恢复 Finding A），exit 101：

```text
test actor::reader::tests::a_rolled_back_steer_does_not_strand_the_continuation_terminal ... FAILED
thread 'actor::reader::tests::a_rolled_back_steer_does_not_strand_the_continuation_terminal' (39458506) panicked at peri-swarm/src/actor/reader_test.rs:1793:5:
assertion `left == right` failed: the continuation terminal must close the turn instead of waiting for the safety net
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 317 filtered out; finished in 0.01s
```

破坏 2：在 `abandon_steer` 恢复 re-pin（集合非空时把更早的 continuation 改绑到当前 generation，即恢复
Finding B），exit 101：

```text
test actor::reader::tests::a_failed_newer_steer_does_not_hand_the_turn_to_the_older_run ... FAILED
thread 'actor::reader::tests::a_failed_newer_steer_does_not_hand_the_turn_to_the_older_run' (39459720) panicked at peri-swarm/src/actor/reader_test.rs:1853:5:
assertion `left == right` failed: steer C still owns the turn
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 317 filtered out; finished in 0.03s
```

恢复后 `shasum -a 256 peri-swarm/src/actor/reader.rs` 仍为
`fc6d38aeab183bb2070f705117c577311630ca3babb2c0e3e40f4f83a1b22af0`。两处 panic 行号是当时（`cargo fmt`
重排测试调用点之前）的真实输出；当前文件对应断言在 `reader_test.rs:1825`、`:1885`（多行 `assert_eq!` 的
panic 位置即宏起始行，与断言同号）。

### 全量门禁

- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm --lib` → exit 0：

  ```text
  test result: ok. 318 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.25s
  ```

- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm` → exit 0，合计 416 passed、0 failed、
  6 ignored：lib 318、main 0、`actor_runtime` 17（1 ignored）、`http_contract` 25、`items_pagination` 13、
  `loom_store` 0（3 ignored）、`mcp_injection` 4、`openai_hosted` 7、`rivet_actor` 0（2 ignored）、
  `session_resources` 17、`streaming` 15、Doc-tests 0。
- `./scripts/cargo-rmcp-patched.sh clippy --locked -p peri-swarm --all-targets -- -D warnings` → exit 0，
  末行 `Finished \`dev\` profile [unoptimized + debuginfo] target(s) in 9.77s`。
- `./scripts/cargo-rmcp-patched.sh fmt -p peri-swarm -- --check` → exit 0（先对 `reader_test.rs` 的调用点
  执行过一次 `fmt -p peri-swarm`，未改动 `reader.rs`，其 SHA-256 保持 `fc6d38ae…`）。
- `git diff --check` → exit 0。
- `bash scripts/check-changed.sh` → exit 0，输出 `[check-changed] cargo check --locked -p peri-swarm`。

### 本次未覆盖与残留窗口

- 首次保留的终态不可替换：prompt attempt 的 `cancelled` 先占用 `suppressed_terminal`，之后
  continuation 自己的终态被 `get_or_insert` 忽略。若随后该 steer 回滚，保留项是 prompt 终态而
  continuation 仍存在 → 不重放，turn 仍需兜底关闭。本轮按设计保留「首次保留」，未替换为更权威的
  来源。
- Finding B 中被丢弃的旧 run 终态不会重新尝试：A 的终态（gen 1 < C 的 3）直接丢弃；若 C 的 run
  在 host 侧从未启动，turn 只能由 300 秒 / 1800 秒兜底关闭。丢弃是刻意的：无法证明 A 的终态属于
  C 之后的 turn。
- 迟到 queued run 在另一个 steer outstanding 时仍会被接管：接管只检查集合非空，不校验该 run 对应
  哪个 steer，因此一个迟到（甚至已回滚）的 run 可以替换 `continuation`，旧 run 的终态随后被丢弃。
  第 6 轮的 `a_rollback_does_not_reuse_the_generation_a_later_steer_advances` 正依赖这种接管。
- 持续 steer 流（每轮 `settle` 都有新 steer 接管）没有全局截止时间：closer 每轮让位，仍只有服务层
  300 秒 / 1800 秒兜底。未构造压力测试。
- 服务层取消 `run_turn`（300 秒 / 1800 秒等待期间）仍没有 closer；既有窗口，未新增测试。
- 只覆盖 `ItemLog`；未直接取消服务层 `run_turn`，未用真实宿主计时复现服务分支，没有端到端复验。

## 评审回合补充（2026-11-24，第 8 轮）

基线 `10b77bd99` 加第 7 轮与本轮未提交改动；以下是在该 worktree 运行的 Rust 回归，不是端到端复验。
独立 verification 在第 7 轮代码上确认两条阻塞项，本轮修正并补齐守卫。代码改动同样只落在
`peri-swarm/src/actor/reader.rs`、`peri-swarm/src/actor/reader_test.rs`，另加本记录与 issue；
`service.rs`、store、schema、`Cargo.toml` 未改，未提交。

### 两条阻塞项

| 问题 | 触发序列 | 第 7 轮行为 | 本轮修复 |
|---|---|---|---|
| 1：迟到接管继承当前 generation | A `note_steer()`→1 并派发；其 `user_input_run_started` 仍在 transport 的 60ms 合并窗口内时 C `note_steer()`→2；reader 才 ingest A 的 run_started | 接管把 `generation` 记为当前计数器 2；host 为 C 取消 req-2，req-2 的终态 `2 >= max(2)` 通过并关闭 C 仍拥有的 turn，C 的 run 与终态全部被忽略，session 变 `Idle` 而 host 仍在执行 | 接管认领**最老的未解决 generation**（`outstanding_steers.first()`）并从集合移除；依赖 transport 对同一 session 的 run_started 帧按派发顺序（FIFO）到达 |
| 2a：continuation 终态进入 `settle` 后被保留却无人重放 | continuation 终态通过过滤进入 `settle`；期间 `note_steer()`→2，post-check 保留（`source=req-2`）；随后 `abandon_steer(2)` | 第 7 轮「集合非空即返回」；即使不返回，continuation 自己的 gen 1 仍在集合里，保留终态没有重放路径 | `abandon_steer` 删除「集合非空即返回」，改按保留终态的来源是否仍是当前 owner 判定；`source: Some(run)` 要求当前 continuation 正是该 run 且 `run.generation >= max_unresolved()` |
| 2b：continuation 终态在新 steer 之后到达被丢弃 | 先 `note_steer()`→2，再交付 continuation（gen 1）终态 | continuation 终态过滤 `run.generation >= max_outstanding()` 不通过 → 直接丢弃，`abandon_steer(2)` 后无从重放 | `run.request_id == running` 且 `run.generation < max_unresolved()` 时以 `source: Some(run)` 保留终态；`>=` 时才关闭 |

### 归属判定（`peri-swarm/src/actor/reader.rs`）

- `outstanding_steers` 语义改为「已派发、尚未被 queued run 认领、也未被回滚的 steer」；
  `max_outstanding()` 改名 `max_unresolved()`，返回该集合最大值（空集为 `0`）。
- queued `TurnStarted`：`queued && active.is_some()` 时认领 `outstanding_steers.first()`（最小
  generation）并从集合 `remove`，`ContinuationRun { request_id, generation: 认领值 }`；集合为空则
  不接管（迟到 run 在回滚后仍不被接管）。注释写明 FIFO 假设。
- continuation 终态：`run.request_id == running` 且 `run.generation < max_unresolved()` →
  `suppress_terminal(turn_request_id, outcome, Some(run))` 保留；`>=` → 以
  `Some(max_unresolved())` 关闭。不是当前 continuation 的 run 终态仍被丢弃（无法证明属于当前 turn）。
- `finish_turn_observed(request_id, outcome, expected_max_unresolved, source)`：快照改为
  `(max_unresolved(), continuation.request_id)`；pre-settle 仍是 `max > expected` 让位；post-settle
  改为 `max > 快照 max || continuation.request_id != 快照 continuation` 让位。接管会把认领的
  generation 移出集合，`max` 可能回到快照值，因此必须同时比较 continuation 身份。
- `suppress_terminal`：不再用 `get_or_insert`。已有 `source: None` 且新终态 `source: Some(_)` 时替换；
  其余保持首次保留。有来源的 continuation 终态优先于无来源的 prompt 终态。
- `abandon_steer(g)`：`remove(&g)` 失败即返回（已回滚、已被认领、或来自已关闭 turn）；删除
  「集合非空即返回」；仅当保留终态来源仍是当前 owner 时重放：
  - `source == None`：要求 `continuation.is_none()` **且**没有更老的未解决 steer（否则该 steer 的
    handoff 仍可能产生终态，prompt 终态尚不属于 turn）；
  - `source == Some(run_id)`：要求当前 `continuation.request_id == run_id` 且
    `run.generation >= max_unresolved()`。
  重放走 `finish_turn_observed(.., Some(0), 原 source)`；重放 `settle` 期间出现新 steer 时守卫会
  再次保留。
- 关闭 turn 与 `begin_turn` 仍清空集合 / `continuation` / `suppressed_terminal`，不重置
  `steer_generation`；`steer_owns_turn()` 仍为「集合非空 || continuation 存在」，`steer_pending()`
  语义不变；`note_steer`、legacy `None` request-id 帧行为不变；prompt attempt 终态的门禁
  （`steer_owns_turn()` 时抑制）不变，但存储规则由「无条件覆盖」改为「首次保留，且有来源终态替换
  无来源终态」。

### 定向回归

命令均为 `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm --lib -- <过滤词>`。
旧 21 + 新 5 共 26 条逐条运行，各 exit 0，各为
`1 passed; 0 failed; 0 ignored; 0 measured; 322 filtered out`。

| 测试 | 结果 |
|---|---|
| `a_failed_steer_handoff_closes_the_turn_with_the_prompt_terminal` | exit 0，1 passed |
| `a_steer_admitted_while_settling_keeps_the_turn_open` | exit 0，1 passed |
| `a_terminal_kept_by_a_steer_guard_closes_the_turn_when_the_handoff_fails` | exit 0，1 passed |
| `a_failed_timeout_close_kept_by_a_steer_guard_closes_when_the_handoff_fails` | exit 0，1 passed |
| `a_terminal_decided_on_an_older_epoch_is_not_used_to_close` | exit 0，1 passed |
| `a_steer_rolled_back_while_settling_lets_the_timeout_close_the_turn` | exit 0，1 passed |
| `a_stale_decision_epoch_after_rollback_still_closes_the_turn` | exit 0，1 passed |
| `a_deferred_lost_terminal_close_is_retried` | exit 0，1 passed |
| `a_retry_does_not_close_after_the_continuation_terminal` | exit 0，1 passed |
| `a_rollback_does_not_reuse_the_generation_a_later_steer_advances` | exit 0，1 passed |
| `a_grace_close_waits_for_the_continuation_terminal` | exit 0，1 passed |
| `a_grace_close_forces_the_turn_after_the_grace_without_terminal` | exit 0，1 passed |
| `a_failed_steer_does_not_release_an_earlier_steers_turn` | exit 0，1 passed |
| `out_of_order_steer_rollbacks_release_the_turn_only_at_the_last_one` | exit 0，1 passed |
| `a_rollback_during_settle_does_not_defer_the_close` | exit 0，1 passed |
| `a_stale_generation_without_an_owner_does_not_defer_the_close` | exit 0，1 passed |
| `a_late_rollback_does_not_release_the_next_turns_steer` | exit 0，1 passed |
| `a_rolled_back_steer_does_not_strand_the_continuation_terminal` | exit 0，1 passed |
| `a_failed_newer_steer_does_not_hand_the_turn_to_the_older_run` | exit 0，1 passed |
| `a_rolled_back_steer_still_lets_the_adopted_run_close_the_turn` | exit 0，1 passed |
| `a_rolled_back_steer_does_not_replay_the_prompts_terminal_for_a_live_run` | exit 0，1 passed |
| `a_late_run_started_claims_the_oldest_steer_not_the_current_counter`（新增） | exit 0，1 passed |
| `a_retained_continuation_terminal_closes_the_turn_after_the_rollback`（新增） | exit 0，1 passed |
| `a_superseded_continuation_terminal_is_replayed_after_the_rollback`（新增） | exit 0，1 passed |
| `each_queued_run_claims_the_oldest_unclaimed_steer`（新增） | exit 0，1 passed |
| `a_continuation_terminal_replaces_the_retained_prompt_terminal`（新增） | exit 0，1 passed |

新增测试说明（paused clock 用于 2a）：

- `a_late_run_started_claims_the_oldest_steer_not_the_current_counter`（阻塞 1）：`note_steer()`→1、
  →2 后才 ingest A 的 `user_input_run_started`（`req-2`）；`req-2` 的 `cancelled` 终态到达后断言
  turn 仍打开且没有终态帧；`req-3` 的 run_started 认领 2，其 `end_turn` 关闭 turn，唯一终态为
  `Completed`（`end_turn`），会话 `Idle`。
- `a_retained_continuation_terminal_closes_the_turn_after_the_rollback`（阻塞 2a）：
  `req-2` 接管后其终态在暂停时钟下进入 `settle`；期间 `note_steer()`→2，closer 保留终态；断言 turn
  仍打开、无终态；`abandon_steer(2)` 后由保留的 continuation 终态关闭 turn（唯一 `end_turn` 终态、
  会话 `Idle`），不依赖 300 秒兜底。
- `a_superseded_continuation_terminal_is_replayed_after_the_rollback`（阻塞 2b）：`req-2` 接管后先
  `note_steer()`→2，再交付 `req-2` 的 `end_turn` 终态（被保留而非丢弃）；`abandon_steer(2)` 后由该
  终态关闭 turn。
- `each_queued_run_claims_the_oldest_unclaimed_steer`（认领顺序）：`note_steer()`→1、→2；
  `req-2`、`req-3` 依次 run_started；`req-2` 的终态在 C 已被 `req-3` 认领后不关闭 turn，只有 `req-3`
  的终态关闭。
- `a_continuation_terminal_replaces_the_retained_prompt_terminal`（来源优先）：prompt attempt 的
  `cancelled` 先占位；`req-2` 接管并 `note_steer()`→2 后，`req-2` 的 `end_turn` 终态替换占位项；
  `abandon_steer(2)` 后 turn 以 `end_turn` 关闭，`cancelled` 没有关闭过 live run。

### 全量门禁

- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm --lib` → exit 0：

  ```text
  test result: ok. 323 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.25s
  ```

- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm` → exit 0，合计 421 passed、0 failed、
  6 ignored：lib 323、main 0、`actor_runtime` 17（1 ignored）、`http_contract` 25、`items_pagination` 13、
  `loom_store` 0（3 ignored）、`mcp_injection` 4、`openai_hosted` 7、`rivet_actor` 0（2 ignored）、
  `session_resources` 17、`streaming` 15、Doc-tests 0。
- `./scripts/cargo-rmcp-patched.sh clippy --locked -p peri-swarm --all-targets -- -D warnings` → exit 0，
  末行 `Finished \`dev\` profile [unoptimized + debuginfo] target(s) in 1.96s`。
- `./scripts/cargo-rmcp-patched.sh fmt -p peri-swarm -- --check` → exit 0。
- `git diff --check` → exit 0。
- `bash scripts/check-changed.sh` → exit 0，输出 `[check-changed] cargo check --locked -p peri-swarm`。

### 本次未覆盖与残留窗口

- 持续 steer 流（每轮 `settle` 都有新 steer 接管）仍没有全局截止时间：closer 每轮让位，只有服务层
  300 秒 / 1800 秒兜底。未构造压力测试。
- 服务层取消 `run_turn`（两个长等待期间）仍没有 closer；既有窗口，未新增测试。
- 只覆盖 `ItemLog`；没有端到端复验，未用真实宿主计时复现 60ms 合并窗口。
- 认领依赖「同一 session 的 `user_input_run_started` 帧按派发顺序到达」这一 FIFO 假设（transport
  串行转发）；若未来出现重放或并发注入同一 session 的 run_started，认领顺序需要重新论证。
- `suppress_terminal` 的首次保留语义仍有残留：两个**有来源**终态之间首个保留，若其后 continuation 被
  更晚的 run 替换，较早保留的终态不会重放（其来源已不是 owner）。第 7 轮的「prompt 终态先占位导致
  continuation 终态被忽略」不再可达：有来源终态会替换无来源终态。
- 已被替换的 continuation run 的终态仍直接丢弃（`request_id` 不是当前 continuation）：若新 steer 的
  handoff 随后失败，turn 只能由兜底关闭。这是第 7 轮 Finding B 的既有取舍，本轮未改。
- 乱序回滚只移除自身；迟到旧 turn 的回滚不影响新 turn（第 6/7 轮契约保持），本轮未新增该场景测试。
- 矛盾状态：`dispatch_steer` 报错但宿主已启动该 run 时，`abandon_steer` 回滚的是未被认领的 generation；
  迟到的 `user_input_run_started` 会认领**另一个**等待中 steer 的 generation，该 run 的终态随后关闭那个
  steer 的 turn。仅在 `ItemLog` 级 probe 确认，未做服务层复现，本轮不修。
