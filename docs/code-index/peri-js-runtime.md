# peri-js-runtime 代码索引

> 速查表：通用 JavaScript process/RPC host。细节以代码为准。更新：2026-09-12（共用 OS 进程树 owner、实际排空与取消重试）。
> 依据：`docs/design/programmatic-tool-calling.md`、`docs/standards/architecture-contracts.md`、源码（本 crate 无 CLAUDE.md/AGENTS.md）。

## 架构速览

- 定位：Workflow 与 PTC 共用的 Node lifecycle、NDJSON JSON-RPC 和 execution host；不解释 `workflow/*`、`agent/run` 或 `tool/call` 业务语义。
- 稳定不变量：pending request 在写帧前登记；每帧有字节上限、换行并 flush；execution 有 wall timeout、资源/并发预算与稳定错误分类；正常返回路径统一取消 router、回收进程树并 wait，收尾超时/失败如实保留错误；stderr 正文不写 tracing，仅在内存保留有界 tail 供上层安全摘要。

生命周期边界：上层直接 drop/abort `JsExecutor::execute` future 会跳过尚未执行的异步收尾；`Invocation::Drop` 只取消 token、abort request/router，host/process-tree Drop 只请求终止，不能证明 reader/router 已 join 或 child 已 reap。调用方应保留 execution future，触发其 CancellationToken 并等待返回；正常返回前统一等待清理，不再用内部清理超时丢弃未收尾 owner。单独取消 host 的 wait/kill 等待而仍保留 host 时，child/tree/reader JoinHandle 留在原 owner 中供重试；释放整个 owner 不提供同样的可重试保证。同步 spawn 在进程创建后失败返回 `CleanupFailed`，上层不得把该错误当作已证明回收。Windows Job Object 的真实运行验证不由 Unix 回归或跨编译代替。

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 改 Node 生命周期或 stderr | `peri-js-runtime/src/host.rs` + `peri-process/src/lib.rs` | `JsExecutionHost::spawn`、`terminate_and_wait`、`join_readers`、`kill`、`wait` | host 持有 child/channel/incoming 与共同 ProcessTree；Windows 挂起后入 Job 才执行；kill 请求整树终止，先 reap leader，再证明整树已退出，最后 join readers；自然 wait 也等待重定向输出的后代；等待取消保留原 owner 供重试 |
| 改 JSON-RPC framing/pending | `peri-js-runtime/src/rpc.rs` | `RpcChannel::send_request`、`parse_message`、`spawn_stdout_reader` | malformed frame 产生 protocol error 并 drain pending；EOF 同样 drain；通用 JSON-RPC error 保真传输，不承担 adapter 业务归类 |
| 改通用 JavaScript execution | `peri-js-runtime/src/executor.rs` + `peri-js-runtime/src/executor/invocation.rs` | `JsExecutor::execute`、`execute_in_directory`、`Invocation::shutdown`、`JsExecutionLimits`、`JsRpcRouter` | session 入口显式绑定 Node cwd，原生 fs 与工具回调使用相同目录；npx fallback 保持私有 npm 配置目录，无法提供会话 cwd 时返回 ArtifactUnavailable；准备阶段取消/deadline 先通知 provider 再等待收尾；握手与写帧服从同一预算；所有终态先取消派生 token，再 join request/router，最后回收 host |
| 改 PTC npm 安装/启动/handshake | `peri-js-runtime/src/artifact.rs` + `peri-js-runtime/src/executor.rs` + `npm-packages/@peri-ptc/` | `PtcArtifactProvider`、固定版本 npm install、package validation、`ptc/start` | 固定 `@peri-code/ptc@0.2.3`；安装使用私有 HOME/cache，adapter 运行仅保留 PATH 与 Windows OS 必需 allowlist；canonical path 只做 package containment 校验，Node argv 使用普通 absolute entry，避免 Windows `\\?\` CLI path；stdout 响应等待 drain，stdin 顶层异步失败仅输出脱敏诊断；跨进程 lockfile 以可取消 try-lock 轮询获取，TempDir 持有未发布 staging；损坏 target 锁内 quarantine；测试注入 fixture provider 但复用生产 `launch_in/ensure_install`；本地 cache 的启动/handshake 协议失败在 cleanup 后隔离，source 失败、用户取消、wall deadline 与 npx 不清缓存，独立握手协议超时仍隔离 |
| 改 npm 子进程回收 | `peri-js-runtime/src/artifact/install.rs` | `NpmInstaller::install`、`InstallProcess::finish` | 安装取消/90 秒超时先终止独立进程树并 wait，再 join 有界 stderr reader；installer 返回后才能删除 staging 或发布 target，取消不得降级 npx |
| 验证执行/安装取消契约 | `peri-js-runtime/src/executor_lifecycle_test.rs` + `peri-js-runtime/src/artifact_lifecycle_test.rs` + `peri-js-runtime/src/artifact/install_test.rs` | `test_prepare_*`、`test_handshake_*`、`test_*result_cancels_and_reaps_router`、`test_cancelled_*`、`test_install_cancel_*` | 协作准备、真实 Node 握手/畸形响应、持锁 waiter、staging 与 installer pid 回归；在退出边界检查 token、future drop、目录与进程，不只检查错误码 |
| 验证 host 后代管道与等待取消 | `peri-js-runtime/src/host_lifecycle_test.rs` | `test_exited_leader_cleanup_terminates_descendant`、`test_cancelled_reader_join_retains_owner`、`test_wait_retains_redirected_descendant_until_natural_exit`、`test_cancelled_kill_retries_same_owner_for_redirected_descendant` | Unix 真实 Node leader 退出后，持 stdout 或已重定向输出的后代都参与清理证明；wait 被取消后重试仍等受控 RELEASE，kill 被取消后可继续排空原树；不构成 Windows Job Object 运行证明 |
| 改错误类型 | `peri-js-runtime/src/error.rs` | `JsRuntimeError`、`JsExecutionFailure` | execute failure 提供固定安全 code/message 投影；通用 `RpcResponse` 保持独立 |

## 跨模块契约

- Workflow Adapter：`docs/code-index/peri-workflow.md`。
- PTC Adapter 与 Agent effective-tool dispatch：`docs/code-index/peri-middlewares.md`、`docs/code-index/peri-agent.md`。
- 工具可见性、事件、Workflow RPC 与 PTC artifact：ARC-TOOLS-001、ARC-EVENT-001、ARC-WORKFLOW-RPC-001、ARC-PTC-ARTIFACT-001。
