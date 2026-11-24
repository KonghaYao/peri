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
