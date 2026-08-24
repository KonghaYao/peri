# Windows CI 暂停运行 PTC ACP production-path E2E

**状态**：待修复
**优先级**：高
**类型**：测试缺口 / Windows 兼容性
**创建日期**：2026-08-23
**来源**：Windows GitHub Actions 持续失败

## 问题

`host::executor_flow_tests::test_ptc_runs_through_acp_session_agent_production_path` 在 Unix CI 与本地环境通过，但在 Windows GitHub Actions 中持续失败。失败发生在完整 ACP session → agent → deferred `RunPtcCode` → Node adapter → effective tool dispatcher 链路；日志显示 scripted model 已进入最终 `EndTurn`，但测试的 production-path 断言未全部满足。

该测试会启动真实 Node 子进程、使用 Windows Job Object 管理进程树、临时替换 `HOME`、加载本地 PTC cache fixture，并断言 ACP event stream。Windows Runner 的父 Job Object、子进程创建 flags、路径语义或事件时序仍可能与本地 Unix 环境不同。

在根因和稳定复现手段明确前，测试通过 `#[cfg(not(windows))]` 暂停在 Windows 编译和运行。此绕过只适用于该 production-path E2E，不代表 Windows PTC runtime 已完成验证，也不得扩展为跳过 `peri-js-runtime` 的 Windows compile/clippy 门禁。

## 当前已完成

- Windows 使用 Job Object，并设置 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`；`peri-js-runtime` 中启动真实 Node/持有 pipe 的测试在 Windows 串行运行，避免 hard-timeout cleanup 与并行测试的进程生命周期互相干扰。
- Node 子进程不请求 `CREATE_BREAKAWAY_FROM_JOB`，以兼容禁止 breakaway 的 GitHub Actions 父 Job；进程树绑定到专用 nested Job Object。
- Windows target 的 `peri-js-runtime` `cargo check` 与 `cargo clippy -D warnings` 通过。
- E2E 中的 Windows 路径通过 JSON 编码嵌入 JavaScript。
- 测试脚本不再通过 `console.log` 输出额外 JSON。

## 风险

- Windows 上完整 PTC production path 暂无自动化 E2E 覆盖。
- compile/clippy 通过不能证明 Node 启动、Job Object containment、RPC handshake、tool dispatch、cancel 和 ACP event 映射在真实 Windows Runner 上均正确。
- 后续 runtime 或 event 变更可能只在 Windows 集成环境中暴露。

## 修复目标

1. 在 Windows GitHub Runner 上捕获失败断言及最小必要诊断，不打印完整 event JSON。
2. 将问题缩小到 process containment、artifact fixture、RPC、tool dispatch 或 ACP event assertion 中的单一边界。
3. 为 Windows 添加可重复的目标测试，覆盖 Node 启动、handshake、并发 `Read`、structured tool error 与 cancellation。
4. 恢复 `test_ptc_runs_through_acp_session_agent_production_path` 在 Windows 的执行，移除 `#[cfg(not(windows))]`。

## 验收标准

- [ ] Windows CI 连续运行该 production-path E2E 至少三次均通过。
- [ ] Windows timeout/cancel 后不存在残留 Node 子进程。
- [ ] Windows 路径、临时 `HOME` 和 PTC fixture 不访问网络或开发者目录。
- [ ] 失败输出只包含精简诊断，不输出完整 LLM/event JSON 流。
- [ ] 移除测试上的 `#[cfg(not(windows))]`。
- [ ] `cargo fmt --all -- --check`、Windows target clippy 与 workspace tests 通过。

## 相关代码

- `peri-acp/src/host/executor_flow_test.rs`
- `peri-js-runtime/src/host.rs`
- `peri-js-runtime/src/process_tree.rs`
- `.github/workflows/ci.yml`
