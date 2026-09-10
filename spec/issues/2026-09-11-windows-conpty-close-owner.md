# Windows ConPTY 关闭缺少阻塞 reader 与 child 的完成所有权

**状态**：Open
**类型**：生命周期实现缺口 / 平台验收

## 当前证据

`peri-web-pty/src/ws_handler/windows.rs::run` 将阻塞 Read、DSR Write 和输出发送放入
`spawn_blocking`，退出时调用 `read_task.abort()`，未等待其完成。已开始的阻塞任务
不会因 abort 停止。`PtySession::finish` 仅在 Unix 编译；Windows Drop 的尽力 kill
没有显式 wait。源码证明关闭未持有完整 join/reap 保证，尚未证明每次断连都会泄漏。

本机 Darwin 的 Unix WebSocket/PTY 回归不能替代 ConPTY 运行证据。当前平台入口见
[WebPTY 索引](../../docs/code-index/peri-web-pty.md)；JS 的 Windows Job Object 已实现，
其运行覆盖缺口另见 [PTC 平台验收](2026-08-23-windows-ptc-production-e2e-disabled.md)。

## 实施边界

Windows connection 应持有可中断阻塞 I/O 的 reader owner、child 和原 join handle。
关闭须停止输入、解除满输出队列的发送等待、终止阻塞 I/O，并观察 child 与 reader
真实终态。取消等待后重试必须等待同一 owner；未完成须保留 owner 并明确报告。
保持现有 CRLF、UTF-8、DSR 与 WebSocket 协议，不以重新实现整个 PTY 后端替代此边界。

原生 I/O 取消与 ConPTY 句柄关闭顺序需要真实 Windows 验证；仅移动 JoinHandle、
跨编译或加入 ignored 测试不能作为修复完成证据。

## 验收

- [ ] Windows 实际运行：闲读断连、DSR/输入背压断连、child 自然结束、关闭等待取消后重试。
- [ ] 每个场景断言实际 reader join 与 child 终态，不只断言 socket Close。
- [ ] 隔离 HOME/cache/config，无外部网络及持久用户目录修改。
- [ ] Windows 严格 Clippy 与目标测试通过，Unix 既有协议/生命周期回归保持通过。
