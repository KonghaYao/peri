# TUI 普通对话发送先闪过待发送队列

**状态**：Fixed
**优先级**：中
**创建日期**：2026-09-16

## 问题描述

用户正常发送对话时，TUI 先展示 queue，然后消息才进入 chat。期望空闲提交直接显示在聊天区，不经过待发送区。

## 症状详情

- 用户报告普通发送存在 queue → chat 的中间显示。
- 忙碌时真正等待处理的输入仍应展示在队列。

## 复现条件

- **复现频率**：用户未指定；使用状态投影回归测试验证。
- **触发步骤**：空闲会话提交普通输入，观察请求发出至 Delivered 之间的队列投影。
- **环境**：启用 `peri.userInputQueue` 的 TUI。

## 涉及文件

- `peri-tui/src/kit/steer_state.rs` —— 请求和队列投影。
- `peri-tui/src/kit/steer_state_test.rs` —— 投影状态测试。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-09-16 | — | Open | agent | 根据用户反馈创建并开始修复 |
| 2026-09-16 | Open | Fixed | agent | 空闲提交跳过待发送投影，等待终端交互验收 |

## 修复记录

### 修复 #1（2026-09-16）

- **操作人**：agent
- **用户原意**：普通对话直接出现在聊天区，不先闪过 queue。
- **根因**：`SteerState::rows` 无差别展示本地 Enqueue 为 Submitting，并将直接投递的快照展示为 Dispatching；服务端空闲准入已直接启动，无需调整执行路径。
- **修复内容**：记录空闲直接提交的展示身份，在确认前跳过待发送行；首次会话绑定保持该投影。服务端 Queued、失败/未知回执、重载解除隐藏。正式气泡仍由 Delivered 生成，保持稳定 ID 去重。
- **验证证据**：原实现运行 `cargo test -p peri-tui --lib -- test_steer_idle_submission_skips_queue_until_delivery`，exit 101，1 项失败，断言为空闲提交不得出现在待发送区。修复后 `cargo test -p peri-tui --lib -- steer` exit 0，51 项通过；随后增加 bridge 链路测试，`cargo test -p peri-tui --lib` exit 0，1582 项通过、2 项忽略。`cargo build --workspace` exit 0（链接器有 compact unwind 大小警告）；`git diff --check` 通过。
- **文档同步**：更新队列现行设计和 TUI code-index，未改变 ACP 协议及执行生命周期。
- **验证状态**：自动验证通过；真实终端交互待验证。
