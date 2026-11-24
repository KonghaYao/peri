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
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-swarm` → exit 0；lib 296 passed，`actor_runtime` 17、`http_contract` 25、`items_pagination` 13、`mcp_injection` 4、`openai_hosted` 7、`session_resources` 17、`streaming` 15，均 0 failed。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp` → 1532 passed / 1 failed；失败项为既有负载 flake（`host::mcp_v4_wire_fixture::wire_fixture_script_logs_wire_and_answers_tools_call`，单跑通过，见 `docs/verification/peri-swarm-requires-action-2026-11-21.md` 第 13 条），本 PR 未触碰该夹具。
- `cargo fmt -p peri-swarm -p peri-acp -- --check` 与 `git diff --check` → exit 0。
