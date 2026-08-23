# peri-js-runtime 代码索引

> 速查表：通用 JavaScript process/RPC host。细节以代码为准。更新：2026-08-22。
> 依据：`docs/design/programmatic-tool-calling.md`、`docs/standards/architecture-contracts.md`、源码（本 crate 无 CLAUDE.md/AGENTS.md）。

## 架构速览

- 定位：Workflow 与 PTC 共用的 Node lifecycle、NDJSON JSON-RPC 和 execution host；不解释 `workflow/*`、`agent/run` 或 `tool/call` 业务语义。
- 稳定不变量：pending request 在写帧前登记；每帧有字节上限、换行并 flush；execution 有 wall timeout、资源/并发预算与稳定错误分类；所有终态统一取消 router、回收进程树并 wait；stderr 正文不保留、不写 tracing。

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 改 Node 生命周期或 stderr | `peri-js-runtime/src/host.rs` | `JsExecutionHost::spawn`、`kill`、`wait` | host 持有 child/channel/incoming，并并行消费 stderr |
| 改 JSON-RPC framing/pending | `peri-js-runtime/src/rpc.rs` | `RpcChannel::send_request`、`parse_message`、`spawn_stdout_reader` | malformed frame 产生 protocol error 并 drain pending；EOF 同样 drain；通用 JSON-RPC error 保真传输，不承担 adapter 业务归类 |
| 改通用 JavaScript execution | `peri-js-runtime/src/executor.rs` | `JsExecutor::execute`、`JsExecutionLimits`、`JsRpcRouter` | 安全默认 wall timeout、source/input/frame/log/result 与并发预算；`execute` response 边界按 adapter error allowlist 归一化；所有终态统一 cleanup |
| 改错误类型 | `peri-js-runtime/src/error.rs` | `JsRuntimeError`、`JsExecutionFailure` | execute failure 提供固定安全 code/message 投影；通用 `RpcResponse` 保持独立 |

## 跨模块契约

- Workflow Adapter：`docs/code-index/peri-workflow.md`。
- PTC Adapter 与 Agent effective-tool dispatch：`docs/code-index/peri-middlewares.md`、`docs/code-index/peri-agent.md`。
- 工具可见性、事件与 Workflow RPC：ARC-TOOLS-001、ARC-EVENT-001、ARC-WORKFLOW-RPC-001。
