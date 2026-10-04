# peri-js-runtime 代码索引

> 速查表：通用 JavaScript process/RPC host。细节以代码为准。更新：2026-09-12（共用 OS 进程树 owner、实际排空与取消重试）。
> 依据：`docs/standards/architecture-contracts.md`、源码（本 crate 无 CLAUDE.md/AGENTS.md）。

## 架构速览

- 定位：Workflow 使用的 Node lifecycle、NDJSON JSON-RPC 和 execution host；不解释 `workflow/*`、`agent/run` 或 `tool/call` 业务语义。
- 稳定不变量：pending request 在写帧前登记；每帧有字节上限、换行并 flush；host 结束时统一回收进程树并 wait，收尾超时/失败如实保留错误；stderr 正文不写 tracing，仅在内存保留有界 tail 供上层安全摘要。

生命周期边界：host/process-tree Drop 只请求终止，不能证明 reader 已 join 或 child 已 reap。调用方应显式等待清理。单独取消 host 的 wait/kill 等待而仍保留 host 时，child/tree/reader JoinHandle 留在原 owner 中供重试；释放整个 owner 不提供同样的可重试保证。同步 spawn 在进程创建后失败返回 `CleanupFailed`，上层不得把该错误当作已证明回收。Windows Job Object 的真实运行验证不由 Unix 回归或跨编译代替。

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 改 Node 生命周期或 stderr | `peri-js-runtime/src/host.rs` + `peri-process/src/lib.rs` | `JsExecutionHost::spawn`、`terminate_and_wait`、`join_readers`、`kill`、`wait` | host 持有 child/channel/incoming 与共同 ProcessTree；Windows 挂起后入 Job 才执行；kill 请求整树终止，先 reap leader，再证明整树已退出，最后 join readers；自然 wait 也等待重定向输出的后代；等待取消保留原 owner 供重试 |
| 改 JSON-RPC framing/pending | `peri-js-runtime/src/rpc.rs` | `RpcChannel::send_request`、`parse_message`、`spawn_stdout_reader` | malformed frame 产生 protocol error 并 drain pending；EOF 同样 drain；通用 JSON-RPC error 保真传输，不承担 adapter 业务归类 |
| 验证 host 后代管道与等待取消 | `peri-js-runtime/src/host_lifecycle_test.rs` | `test_exited_leader_cleanup_terminates_descendant`、`test_cancelled_reader_join_retains_owner`、`test_wait_retains_redirected_descendant_until_natural_exit`、`test_cancelled_kill_retries_same_owner_for_redirected_descendant` | Unix 真实 Node leader 退出后，持 stdout 或已重定向输出的后代都参与清理证明；wait 被取消后重试仍等受控 RELEASE，kill 被取消后可继续排空原树；不构成 Windows Job Object 运行证明 |
| 改错误类型 | `peri-js-runtime/src/error.rs` | `JsRuntimeError` | 通用 `RpcResponse` 保持独立 |

## 跨模块契约

- Workflow Adapter：`docs/code-index/peri-workflow.md`。
- 工具可见性、事件与 Workflow RPC：ARC-TOOLS-001、ARC-EVENT-001、ARC-WORKFLOW-RPC-001。
