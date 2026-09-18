# 主 Agent 进入 idle 后没有自动发送队首输入

**状态**：Fixed
**优先级**：中
**创建日期**：2026-09-18

## 问题描述

用户在主 Agent loading 期间提交的 prompt 进入待发送区，但主 Agent 等待后台任务而进入 idle 后，原有输入仍然停留在队列中。期望 loading 期间只排队，每次进入 idle 后自动发送第一条可执行 prompt，再次驱动 loop。

## 症状详情

- 挂起前已排队的输入不会自动唤醒 loop，挂起后新提交的输入却可以直接唤醒。
- 普通输入应按 FIFO 每次发送一条；立即发送仍由用户指定单条或集合。
- Stop 和执行失败应保留待办，不能因为迟到的 idle 通知自动恢复。

## 复现条件

1. 主 Agent 运行期间有后台任务尚未完成。
2. loading 期间提交一个或多个普通 prompt。
3. 主 Agent 完成当前回复并进入 idle，检查待发输入是否开始执行。

## 涉及文件

- `peri-agent/src/session/user_input_mailbox.rs`：待发队列及执行准入。
- `peri-agent/src/agent/stages/mod.rs`：idle 边界与 loop 唤醒。
- 相邻测试及 `docs/design/user-input-queue.md`、`docs/code-index/peri-agent.md`。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-09-18 | — | Open | agent | 用户确认 idle 应自动发送第一条可执行输入 |
| 2026-09-18 | Open | Fixed | agent | idle 与自然成功边界按 FIFO 自动交接一条，保留取消及显式发送语义 |

## 修复记录

### 修复 #1（2026-09-18）

- **操作人**：agent
- **用户原意**：loading 时排队，主 Agent idle 时自动发送第一条可执行 prompt 并驱动 loop。
- **修复内容**：Mailbox 在 idle 边界与 enqueue 的同一锁内完成队首准入及 inbox 交接；首次交接占用本次 idle，后续输入继续排队。自然完成、空闲提交及明确恢复也逐条准入，显式发送集合保持原规则。loop 在队首可执行时直接回 Receive，不发布虚假的挂起事件。同步设计与 Agent 索引。
- **回归证据**：新增 idle 交接测试在旧实现失败，实际交接 0 条、期望 1 条。修复后覆盖入队早于/晚于 idle、连续提交、撤回跳过、Stop 与外部取消、正常完成逐条启动。真实 loop 在初始模型请求挂起时入队 A/B，随后分别收到初始、初始+A、初始+A+B 三次模型输入，Delivered 事件顺序正确且无重复。
- **验证**：`cargo test -p peri-agent --lib` 790 项通过；`cargo test -p peri-acp --lib -- user_input` 12 项通过；`cargo test -p peri-agent --doc` 8 项通过；`cargo clippy -p peri-agent --all-targets -- -D warnings`、`cargo build --workspace`、`git diff --check` 通过。构建保留已有 compact unwind 链接警告。真实终端队列文件 `npm run e2e -- --file tests/smoke/steer-queue-live.test.ts --serial --retry 0` 首轮通过，包含新增 FIFO 自动发送场景，未调用外部模型；本次设计与索引的本地链接检查通过。
- **验证状态**：自动验证通过，待用户实际使用验收。
