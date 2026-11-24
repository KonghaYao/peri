# Peri Swarm turn 终止与批量落库验收

代码基线 `cca9a2be6`（`origin/main`）。端到端观察在同一补丁的工作副本上完成（副本
基于 `fe665e60e`，与本 PR 的源码差异只有 `origin/main` 上的两个 `peri-acp` 宿主关闭
提交，未触及本次修改的文件）。

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
  lib 294 passed / 0 failed；actor_runtime 17、http_contract 25、items_pagination 13、
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
