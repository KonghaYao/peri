# peri-process 代码索引

> OS 子进程所有权基础能力；会话生命周期与 lease 仍由上层持有。

| 我想做什么 | 主文件 | 稳定入口与契约 |
| --- | --- | --- |
| 改 Unix 进程组与通用生命周期 | `peri-process/src/lib.rs` | `ProcessTree::{new,prepare,attach,terminate,is_stopped,wait_for_exit}`；prepare 在 spawn 前创建独立 group，观察空组后单调记录 settled；发送信号不等于退出 |
| 改 Windows Job 与失败装配 | `peri-process/src/windows.rs` | `WindowsJob::{retain_process,attach_and_resume,is_stopped,terminate}`；挂起期间保存精确 process handle 并加入 Job，退出同时验证 leader 和 Job；空 Job 不能证明 attach 失败的 leader 已停止 |
| 改取消/Drop 语义 | `peri-process/src/lib.rs` | `Drop` 只请求终止；取消 `wait_for_exit` 不移走 owner，调用方超时须保留实际资源；`disarm` 仅供显式放弃管理的 standalone 路径 |

调用方：Agent `ShellExecutionGuard`（外部任务 token）、MCP transport owner、LSP
dispatcher 和 `JsExecutionHost`。任何 session-owned 执行不得使用 `disarm`。
Unix 仅管理所属进程组，显式 setsid/setpgid 脱组不在保证范围；该模块不是安全沙箱。

验证：`cargo test -p peri-process --lib`，以及各 transport 的真实子进程关闭测试。
Windows tests 的交叉编译不等于 Windows 主机运行验收。
