# ACP v2 stdio Preview verification

状态：现行。S1–S4 的 wire 矩阵与进程内适配器测试可在同一 worktree 复跑；本页记录
如何运行、断言覆盖项与已知限制。历史段落只保留仍然有效的结论。

## 如何运行

前置：Python 3（`jsonschema`）、Node.js、已构建的 `peri` 二进制。矩阵使用独立的
SQLite 数据库和仅回环的确定性 OpenAI-compatible SSE 模型，不导入 Peri 服务端或 mock
JSON-RPC。

```bash
./scripts/cargo-rmcp-patched.sh build -p peri-tui --bin peri
python3 scripts/verification/acp_v2_stdio.py target/debug/peri \
  --official-client /tmp/acp-sdk-target/debug/examples/v2_one_shot_client
```

- 第二个位置参数（schema）可省略；默认读取仓库 fixture
  `scripts/verification/fixtures/acp-v2-schema.json`（git blob
  `301e906d8566c71f378e563905c3437d9edfac4c`，对应 pinned upstream schema commit
  `927e31867b31b1e3c961c7edee7e3680b90be718`）。也可以显式传入同一路径。
- `--official-client` 是 unmodified `agent-client-protocol` 3.5.0
  `v2_one_shot_client` 示例；它作为独立客户端 smoke，不替代 wire 矩阵。
- 矩阵只通过 stdio 驱动 `peri acp --cwd <root>`，不 import Peri 的实现。

进程内回归：

```bash
./scripts/cargo-rmcp-patched.sh test -p peri-acp --lib -- host::stdio::v2
```

## S4 运行记录（2026-11-24，macOS arm64，Rust 1.99.0）

工作树基线 `6866f4a9d`，外加本次整合修复提交
`fix(peri-acp): 收紧 v2 分页上限与错误码边界`（含页大小上限、宿主 `-32010`
归一化、preview-only 方法对 v1 拒绝、replay 状态机收口）；`peri` 由该工作树重建。
同一次运行 `EXIT=0`，官方 `v2_one_shot_client` 也通过：

1. `PASS v2 initialize repeat/version bounds, multi-session admission, setup/error-code shapes, prompt key bounds`（含不可用 cwd：`session/new` → `-32602`，message 保留 `workspace location is unavailable`）
2. `PASS v2 two sessions on one connection; concurrent turns; per-session close isolation`
3. `PASS v2 session/delete drops the session and its connection slot`
4. `PASS v2 negotiate/new/list; committed echo + early ACK; streamed answer, idle, keyed dedup/conflict`
5. `PASS v2 replay order replay_complete -> state -> reply; deferred live metadata flushed exactly once after the reply`
6. `PASS v2 active-turn replay refused deterministically: -32020 reason=session_busy (limited rejection, not a successful replay)`
7. `PASS v2 session/list limit=1 order/no duplicates/termination; limit=2 capped and resumed without gaps; exact cwd; invalid cursor -32602`
8. `PASS v2 session/list cursor remains valid in a new process (durable keyset position)`
9. `PASS v2 reopen same DB/root; replay and state ordering; fresh turn; both cancellation scopes; durable withdrawal; idempotent close`
10. `PASS v2 idempotency key survives a restart; same key retried once, different content -32602, keys scoped to the session`
11. `PASS v2 real permission request, requires_action, rejection, peer cancellation, idle, no tool side effect`
12. `PASS v2 session/new ConfigOptions + AvailableCommands v2 shapes`
13. `PASS v2 set_config_option projects configOptions and refuses unknown input`
14. `PASS v2 mode change governs the next turn; UsageUpdate carries used/size only`
15. `PASS v1 client with Preview enabled and disabled`（含 preview-only 内部方法在 v1 上 `-32601`）
16. `PASS upstream Rust ACP 3.5.0 v2_one_shot_client: accepted, streamed, idle, closed`

### 断言覆盖项

- **S1 方法面/错误码**：重复 `initialize`、`protocolVersion` 边界、首次 v2
  `initialize` 缺 `info` 的 `-32602`、多会话、setup 参数、不可用 cwd 的
  `session/new` → `-32602`（保留宿主 message，且不是 `-32010`）、`-32002` 的
  `data.sessionId`、`terminal/cancel`/`tool_call/cancel` 的 `-32602`、`auth/*`
  的 `-32601`、prompt key 1..=255。
- **S1 并发**：两个会话的两个 turn 必须都报告 `running` 后才出现任一 `idle`。探针以
  **首个 `idle`** 作为 turn 结束边界，不再把 prompt ACK 顺序当作 turn 结束证据（ACK 是
  交付确认，可以远早于 turn 结束）。临时把 per-session prompt lock 改成全局锁后跑同一
  矩阵，`EXIT=1`，失败点正是该断言：
  `turn ... was still missing running when the first session went idle; the turns did not overlap`。
- **S2 幂等**：跨三个进程的 key 重试返回同一 `messageId`、不重复插入、同 key 不同内容
  `-32602`、key 按会话隔离、重放只出现一次。
- **S3 配置面**：`session/new` 的 `configOptions`（`configId`、无 `id`）、
  `available_commands_update`（`type:"command"`/`input.type:"text"`）、
  `set_config_option(mode)` 往返、非法值/未知选项/未知会话拒绝、`bypass` 对下一 turn
  生效、`usage_update` 只有 `used`/`size`。
- **S4 分页**：三个有历史的会话；`limit=1` 逐页遍历的顺序与默认页一致、无重复、
  `nextCursor` 终止；`limit=2` 的响应也 `<= 2`，从它的 `nextCursor` 继续能覆盖
  余下全部条目且无重复/缺口；`cwd` 只匹配相同目录；非法 cursor `-32602`；旧进程
  取得的 cursor 在新进程中继续同一 keyset 遍历。遍历循环有页数上限并拒绝重复
  cursor，服务端若不推进 cursor 会失败而不是挂死。
- **S4 replay**：unsupported cursor `-32602`；回放后
  `replay_complete`（恰一次）→ state → 响应；host 在 load 响应前推的 config/commands
  被 deferred，响应后各恰好冲刷一次（以「响应后的后继请求」作为完成栅栏，再统计整个
  冲刷窗口，重复不会被漏数）；重启后回放与幂等重试只出现一次；空回放也发
  `replay_complete`（进程内测试）。活动 turn 内 replay 的 wire 断言是**受限拒绝的形状**：
  `-32020` + `data.reason == "session_busy"` + `data.sessionId`，不是成功回放。
- **S4 进程内**：`host::stdio::v2::replay` 覆盖活动 turn 中 resume 的 hold/flush 顺序、
  live chunk 不产生替换边界、1024 条缓冲上限与溢出、回放期间 `session/cancel` 收敛到
  `idle`、stale success 不冲刷 takeover 的 deferred、pending resume 期间的 done 仍结束
  turn、宿主 `-32010` 在 replay 与无 replay 的 resume 上都映射为 `session_busy`；
  失败 replay 在结束后补发被 hold 压掉的最终状态（含 `cancelled`/`error` stop reason）、
  close/delete 只有 close-incomplete 保留 `-32010`（其余 close 失败为 `-32603`）、
  被已提交 attach 取代的迟到失败不删除 slot、失败 takeover 保持 `Attaching` 并恢复
  owner 的 replay 状态、cursorless 等待中的 `session/cancel` 仍到达宿主。

## 复审收口（S4 之后的第二轮 CodeRabbit 复审）

本轮修复没有新增 wire 方法或 schema 字段，只在既有错误面、准入与回放合并语义上收口；
进程内回归 `./scripts/cargo-rmcp-patched.sh test -p peri-acp --lib` 全绿。

- **只读历史的 v1 准入门**：v1 连接的 `session/load` 若带分页 cursor 或 `history`
  窗口，不再走单会话 setup 门，也不记录 setup 绑定；已绑定 A 的连接可以读 B 的历史，
  读到成功也不会把 B 记成当前会话。初始 `session/load` 与畸形分页仍按 setup 准入。
  回归 `host::stdio::v2::state::tests::{read_only_history_loads_do_not_take_the_v1_setup_gate,
  an_initial_load_still_counts_as_a_v1_setup}` 与
  `boundary_tests::a_v1_history_page_for_another_session_does_not_bind_it`。
- **cleanup-incomplete 的 v1 绑定**：v1 setup 失败但宿主保留 reservation 时
  （`data.reason = "session_cleanup_incomplete"`），连接仍绑定该会话直到显式 close；
  v1 错误响应形状不变。回归
  `boundary_tests::a_v1_setup_that_cannot_drain_a_close_stays_bound_until_that_close`。
- **`initialize` 版本校验**：字段存在但不是 `0..=65535` 的 JSON 整数
  （`-1`、`1.5`、`"2"`、`65536`）一律 `-32602`，不改变连接状态，随后仍可成功协商
  v2。回归 `boundary_tests::an_unusable_protocol_version_never_binds_the_connection_to_v1`。
- **`session/delete` 持久删除**：无本地 slot 的 delete 也转发宿主，宿主成功回复
  `{}`；失败（如 `-32002`）原样透出且不动 slot。回归
  `host::stdio::v2::lifecycle::tests::*`（无 slot 转发、拒绝保留 slot、成功 teardown、
  缺 `sessionId` 的 `-32602`）。
- **cleanup-incomplete 的 v2 setup 错误面**：setup 失败若是 cleanup-incomplete
  （宿主 `data.reason`，或 `Session close incomplete` 前缀），客户端收到 `-32603` +
  `data.reason/sessionId`，且适配器保留 `Attaching` slot 让 `session/close` 继续转发；
  该会话不可 prompt。普通 setup 失败仍移除 slot。回归
  `host::stdio::v2::errors_tests::{a_cleanup_incomplete_setup_keeps_a_close_retry_slot,
  a_prefix_only_cleanup_incomplete_setup_names_the_resume_target,
  a_cleanup_incomplete_setup_mapping_is_internal_and_keeps_the_host_data}`。
- **`-32002` 的 `data.sessionId`**：preview-only handler（prompt 的内部 input 端、
  resume 的配置快照）现在带请求里的会话 id；prompt 的错误结算原样返回该 error。
  配置快照的失败只在适配器内记 warn、resume 回复省略 `configOptions`，客户端看不到
  该 `-32002`。回归 `host::stdio::v2::errors_tests::preview_only_handlers_name_the_session_they_could_not_serve`。
- **回放 hold 的合并语义**：`session_info_update` 是 pinned v2 的补丁，hold/flush 不再
  按 kind 合并；`available_commands_update` / `config_option_update` /
  `current_mode_update` 仍只保留最新整表。回归
  `host::stdio::v2::replay::tests::session_info_patches_are_all_kept_in_arrival_order`。
- **handback 水位**：任何成功的 setup（含非 owning 的迟到成功）都推进
  `attached_setup`，失败 handback 不会复活被这次 attach 取代的更早 attempt（不再发
  `replay_complete`/flush）。回归
  `host::stdio::v2::replay::tests::a_stale_success_never_hands_the_slot_back_to_an_older_attempt`。
- **wire 矩阵断言窗口**：S2 的「重试不得回显 `user_message`」断言移到冲突请求与新会话
  `idle` 都被消费之后，`before` 仍作为窗口起点；此前在 ACK 处停止会漏掉随后补发的回显。

## 复审收口（`7a544cf29` 的 CodeRabbit 复审）

- **A（并发探针的 ACK 顺序）**：已修复。矩阵只以首个 `idle` 判定 turn 结束；prompt
  ACK 只消费、不参与顺序断言。串行变体实测 `EXIT=1`（见上）。
- **B（stale success 推进 attach 阶段）**：已修复。非 owner 成功只记录
  `slot.attached = true`，不把 takeover 的 `Attaching` 改成 `Attached`；回归
  `a_stale_success_keeps_the_metadata_parked_for_the_takeover` 在 stale 响应后再发一条
  `session_info_update`，断言它仍被 park，直到 owner 完成才按序冲刷。
- **C（Attaching 时丢弃 done）**：已修复。`peri/agent_event` 与
  `peri/agent_event_done` 的 guard 只在「slot 未 attach、未回放且没有 live turn」时丢弃
  事件；回归 `a_pending_resume_never_drops_the_turn_end` 在 resume pending 时发 done，
  断言 `idle` 仍发布，随后宿主 `-32010` 失败（v2 客户端收到 `-32020 session_busy`）不留下
  `running`，后续 replay 仍能转发。
- **D（设计文档仍称 `set_config_option` 不支持）**：已在设计文档重写中修正；配置面列为
  已实现，不再出现在 `-32601` 表。
- **E（held 元数据顺序）**：已修正。文档明确 plain resume 成功时 state 快照与 held
  元数据在响应**之前**；只有 `session/new` 的元数据在响应之后。

## 收口复跑：v2 setup 的 busy 错误码与宿主 `-32010`

活动 turn 内 resume 的 busy 收口之后，v2 setup 回复不再透传宿主的 `-32010`：
`session/resume` 失败于宿主的执行中/关闭中拒绝时，客户端收到 `-32020` +
`data.reason = "session_busy"` + `data.sessionId`。

本次整合修复进一步收口其余 `-32010` 路径（映射集中在
`host/stdio/v2/errors.rs::host_failure`）：

- `session/new` 的 cwd 绝对但不可用 → `-32602`，message 保留宿主原文
  `workspace location is unavailable`；
- `session/list` 存储错误、`session/prompt` 的 workspace 资源错误 → `-32603`，
  message 保留；
- 只有 `session/close`（及其共享 close/delete 闸门）的 close-incomplete 仍透传
  `-32010`；v1 连接上的错误形状不变。

同一矩阵命令复跑 `EXIT=0`，断言条目见上（第 1 条新增不可用 cwd 断言，第 7 条新增
`limit=2` 续页断言，第 15 条新增 v1 对 preview-only 方法的 `-32601` 断言）。

## 已知限制

- **活动 turn 内 replay（不支持，拒绝形状已确定）**：适配器具备 hold/flush 与状态机
  语义（进程内测试覆盖），但共享宿主在 per-session prompt lock 被持有时对
  `session/load` 与 `session/resume` 返回 `-32010 Session is executing`。适配器把它收口为
  `-32020` + `data.reason = "session_busy"` + `data.sessionId`，客户端应等 `idle`/turn
  结束后重试；wire 矩阵断言这个受限拒绝的形状，而不是成功回放。放开 gate 或增加
  preview-only 只读 replay 路由属于宿主策略变更，本次未改共享层。
- `state_update` 在回放期间被合并：前台状态机继续推进，但逐条 parked `state_update`
  不重放，以 resume 完成时的权威快照为准（pinned 允许重复/幂等快照）。
- replay 期间 per-session live 缓冲上限 1024 条；溢出丢弃最新条目并记 warn，客户端可用
  另一次 `session/resume` 重新同步。
- `session/list` 的 `limit` 是 preview-only 顶层扩展：pinned `ListSessionsRequest` 没有
  页大小字段，标准客户端拿到默认 100 条；`1..=500` clamp。`limit` 是**响应条目上限**：
  `cwd` 后过滤会连续取页（每次只取仍缺的条数），`nextCursor` 指向最后消费的原始页，
  一次响应不会超过 `limit`。
- preview-only 内部方法（`_peri/stdio-v2/*`）对 v1 连接一律 `-32601`，即使
  `PERI_ACP_V2_PREVIEW=1` 已开启。
- 首次 v2 `initialize` 缺 `info` 是 `-32602`（pinned v2 `InitializeRequest` 的
  required 字段）；`protocolVersion` 存在但不是 `0..=65535` 的 JSON 整数
  （负数、小数、字符串、超上限）同样是 `-32602`，且不改变连接状态；版本号低于 2
  （含 0）仍走 v1 协商，行为未改。
- 同一进程的第二个连接 attach 同一 live 会话时，不会收到第一个连接的 committed-delivery
  事件；其同 key 重试会等待 `close`/`cancel` 而不是立即确认（不会重复插入）。这是 S2
  留下的边界。
- 未实现的可选面（`auth/*`、`session/fork`、`providers/*`、`mcp/message`、per-object
  cancel、自定义 replay cursor）见[设计文档](../design/acp-v2-stdio-preview.md)的
  「Deliberately not implemented」表；它们不是待补的回归缺口。
- 本轮矩阵在 macOS arm64 运行；Linux CI 未在本阶段重跑。官方 SDK smoke 使用 3.5.0
  未修改示例；不覆盖真实 provider 负载、跨平台终端与其它传输。
