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

工作树基于 `7a544cf29` 的 S4 实现；`peri` 由当前工作树重建。同一次运行
`EXIT=0`，官方 `v2_one_shot_client` 也通过：

1. `PASS v2 initialize repeat/version bounds, multi-session admission, setup/error-code shapes, prompt key bounds`
2. `PASS v2 two sessions on one connection; concurrent turns; per-session close isolation`
3. `PASS v2 session/delete drops the session and its connection slot`
4. `PASS v2 negotiate/new/list; committed echo + early ACK; streamed answer, idle, keyed dedup/conflict`
5. `PASS v2 replay order replay_complete -> state -> reply; deferred live metadata flushed exactly once after the reply`
6. `KNOWN v2 active-turn replay: shared host prompt lock returns -32010 (adapter hold/order covered by host::stdio::v2::replay tests)`
7. `PASS v2 session/list limit=1 order/no duplicates/termination; exact cwd; invalid cursor -32602`
8. `PASS v2 session/list cursor remains valid in a new process (durable keyset position)`
9. `PASS v2 reopen same DB/root; replay and state ordering; fresh turn; both cancellation scopes; durable withdrawal; idempotent close`
10. `PASS v2 idempotency key survives a restart; same key retried once, different content -32602, keys scoped to the session`
11. `PASS v2 real permission request, requires_action, rejection, peer cancellation, idle, no tool side effect`
12. `PASS v2 session/new ConfigOptions + AvailableCommands v2 shapes`
13. `PASS v2 set_config_option projects configOptions and refuses unknown input`
14. `PASS v2 mode change governs the next turn; UsageUpdate carries used/size only`
15. `PASS v1 client with Preview enabled and disabled`
16. `PASS upstream Rust ACP 3.5.0 v2_one_shot_client: accepted, streamed, idle, closed`

### 断言覆盖项

- **S1 方法面/错误码**：重复 `initialize`、`protocolVersion` 边界、多会话、setup 参数、
  `-32002` 的 `data.sessionId`、`terminal/cancel`/`tool_call/cancel` 的 `-32602`、`auth/*`
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
  `nextCursor` 终止；`cwd` 只匹配相同目录；非法 cursor `-32602`；旧进程取得的 cursor
  在新进程中继续同一 keyset 遍历。
- **S4 replay**：unsupported cursor `-32602`；回放后
  `replay_complete`（恰一次）→ state → 响应；host 在 load 响应前推的 config/commands
  被 deferred，响应后各恰好冲刷一次；重启后回放与幂等重试只出现一次；空回放也发
  `replay_complete`（进程内测试）。
- **S4 进程内**：`host::stdio::v2::replay` 覆盖活动 turn 中 resume 的 hold/flush 顺序、
  live chunk 不产生替换边界、1024 条缓冲上限与溢出、回放期间 `session/cancel` 收敛到
  `idle`、stale success 不冲刷 takeover 的 deferred、pending resume 期间的 done 仍结束
  turn。

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
  断言 `idle` 仍发布，随后 host `-32010` 失败不留下 `running`，后续 replay 仍能转发。
- **D（设计文档仍称 `set_config_option` 不支持）**：已在设计文档重写中修正；配置面列为
  已实现，不再出现在 `-32601` 表。
- **E（held 元数据顺序）**：已修正。文档明确 plain resume 成功时 state 快照与 held
  元数据在响应**之前**；只有 `session/new` 的元数据在响应之后。

## 已知限制

- **活动 turn 内 replay**：适配器不再预拒绝，并具备 hold/flush 与状态机语义（进程内
  测试覆盖）；但共享宿主在 per-session prompt lock 被持有时对 `session/load` 返回
  `-32010 Session is executing`。wire 矩阵把该场景记为 `KNOWN` 而非 `PASS`。放开该 gate
  或增加 preview-only 只读 replay 路由属于宿主策略变更，本次未改共享层。
- `state_update` 在回放期间被合并：前台状态机继续推进，但逐条 parked `state_update`
  不重放，以 resume 完成时的权威快照为准（pinned 允许重复/幂等快照）。
- replay 期间 per-session live 缓冲上限 1024 条；溢出丢弃最新条目并记 warn，客户端可用
  另一次 `session/resume` 重新同步。
- `session/list` 的 `limit` 是 preview-only 顶层扩展：pinned `ListSessionsRequest` 没有
  页大小字段，标准客户端拿到默认 100 条；`1..=500` clamp。
- 同一进程的第二个连接 attach 同一 live 会话时，不会收到第一个连接的 committed-delivery
  事件；其同 key 重试会等待 `close`/`cancel` 而不是立即确认（不会重复插入）。这是 S2
  留下的边界。
- 未实现的可选面（`auth/*`、`session/fork`、`providers/*`、`mcp/message`、per-object
  cancel、自定义 replay cursor）见[设计文档](../design/acp-v2-stdio-preview.md)的
  「Deliberately not implemented」表；它们不是待补的回归缺口。
- 本轮矩阵在 macOS arm64 运行；Linux CI 未在本阶段重跑。官方 SDK smoke 使用 3.5.0
  未修改示例；不覆盖真实 provider 负载、跨平台终端与其它传输。
