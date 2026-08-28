# Windows 上 ACP 发送提示词后静默卡死

**状态**：Fixed
**优先级**：高
**创建日期**：2026-08-29

## 问题描述

Windows 10 上运行 `peri acp` 时，ACP 客户端能够建立连接并显示权限模式、模型和推理等级，但发送提示词后会永久静默、没有任何响应。期望 ACP session 正常启动并返回 agent 输出。

## 症状详情

- ACP 握手和 session 配置展示正常。
- 任意 ACP 客户端发送提示词后均无输出，session 一直处于等待状态。
- 现场定位显示 peri 正在等待 `GitAttributionMiddleware::current_branch()` 启动的 `git rev-parse --abbrev-ref HEAD` 子进程结束。
- 同一环境中，peri TUI 和其他依赖 Git 的应用未出现 Git 卡死。
- 用户提供了卡死界面截图：<https://github.com/user-attachments/assets/4c45a803-baae-4441-91bb-3338f71d7929>

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 使用任意 ACP 客户端连接 `peri acp`。
  2. 向已建立的 session 发送提示词。
  3. 观察 session 静默等待且无 agent 输出。
- **环境**：Windows 10；Rust 1.95.0；Peri 3.9.6；Git 2.52.0.windows.1（Scoop 管理）

## 涉及文件

- `peri-middlewares/src/attribution/mod.rs` —— `GitAttributionMiddleware::current_branch()` 启动并等待 Git 子进程。
- `peri-middlewares/src/attribution/mod_test.rs` —— 覆盖正常分支输出与超时后直接子进程终止请求。
- `docs/code-index/peri-middlewares.md` —— 记录 attribution 的有界 best-effort Git 探测语义。

## 根因

已确认的缺陷是 `GitAttributionMiddleware::before_agent()` 在模型执行前同步等待一个可选的 Git 分支探测，且该等待没有 deadline；只要直接子进程不退出，整个 agent 阶段就会永久阻塞。原实现继承 stdin，且 future 被取消时没有请求终止直接子进程，这两点是进程隔离与生命周期上的薄弱处，但尚无证据证明它们是 Scoop Git 只在独立 ACP 模式卡住的内部原因。

本次修复将异步等待限制为一秒，并将该命令保持为失败不影响 turn 的 best-effort 观测。这个预算不包含同步 spawn/调度开销；`kill_on_drop` 请求终止直接子进程，但不承诺同步 reap 或清理任意后代进程。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-29 | — | Open | agent | 创建 |
| 2026-08-29 | Open | Fixed | agent | 修复：限制 Git 分支探测等待并隔离 stdin，超时时请求终止直接子进程；待 Windows/Scoop 实机验证 |

## 修复记录

### 修复 #1（2026-08-29）

- **操作人**：agent
- **用户原意**：`peri acp` 建立 session 后应能处理提示词并返回 agent 输出，不能被 attribution 的 Git 子进程静默卡死。
- **修复内容**：为 Git 分支探测增加私有的一秒异步等待预算；子进程使用 null stdin、piped stdout/stderr 与 `kill_on_drop(true)`；保留正常输出的 UTF-8 校验与 trim 语义；新增跨平台正常输出测试和 READY/RELEASE/SENTINEL 真实子进程生命周期回归；同步更新 middleware code index。
- **涉及 commit**：见包含本修复的提交（用户随后明确要求提交并推送）
- **验证状态**：待验证。两个精确回归各 1 条通过；attribution suite 27 passed / 1 intentional ignored；`cargo check`、`cargo build`、crate/workspace clippy、fmt 与 `git diff --check` 通过。完整 `peri-middlewares --lib` 在 sandbox 内外分别有 24/21 个非 attribution 既有或环境敏感失败，故该门禁记为 blocked。Windows 10 / Scoop Git 的真实 ACP prompt、`where git` shim 解析及残留后代进程检查尚未执行，不能标记 Verified。
