# peri-process 代码索引

进程退出等待的计时底层见 [peri-time](peri-time.md)；进程终止与退出证据仍由
`ProcessTree` 持有。

> OS 子进程资源生命周期基础能力；会话执行唯一性由 `peri-sdk` 协调，不持有会话执行租约。

| 我想做什么 | 主文件 | 稳定入口与契约 |
| --- | --- | --- |
| 改 Unix 终端隔离、进程组与通用生命周期 | `peri-process/src/{lib,unix,broker}.rs` | `ProcessTree::{new,prepare,prepare_std,attach,terminate,is_stopped,wait_for_exit}`；prepare 在 exec 与 broker 登记前建立无宿主控制终端的新 OS session，SID/PGID 由子进程身份确定；SDK supervisor 存在时 pre-exec anchor 保留 PGID 身份至 broker 确认释放；崩溃收敛不能仅凭裸 PID，发送信号不等于退出 |
| 改 Windows Job 与失败装配 | `peri-process/src/windows.rs` | `WindowsJob::{retain_process,attach_and_resume,is_stopped,terminate}`；挂起期间保存精确 process handle 并加入 Job，退出同时验证 leader 和 Job；空 Job 不能证明 attach 失败的 leader 已停止 |
| 改取消/Drop 语义 | `peri-process/src/lib.rs` | `Drop` 只请求终止；取消 `wait_for_exit` 不移走 owner，调用方超时须保留实际资源；`disarm` 仅供显式放弃管理的 standalone 路径 |

调用方：Agent `ShellExecutionGuard`（外部任务 token）、MCP transport owner、
dispatcher 和 `JsExecutionHost`。任何 session-owned 执行不得使用 `disarm`。
Unix 终端隔离遵循 `ARC-PROCESS-TERMINAL-001`；标准 IO 由调用方显式配置，
不能把仅重定向标准 IO 当作控制终端隔离。同一 Command 只准备一次，隔离失败使
spawn 失败，不回落共享终端。Unix 仅管理所属进程组，显式 setsid/setpgid 脱组
不在保证范围；该模块不是安全沙箱。

验证：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-process`，以及各 transport
的真实子进程关闭测试。`tests/pty_isolation.rs` 为同步/异步准备及 broker 路径的真实
PTY 回归入口，`src/unix_test.rs` 覆盖 session/group 身份、Command 配置与启动失败。
Windows tests 的交叉编译不等于 Windows 主机运行验收。
