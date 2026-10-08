# 异步错误通知激活修复

- 状态：实现与轻量验证完成，待用户真实使用验收
- 基线：主树与独立 worktree 均为 `9df55f5c`，已合并 `a838cc15`。
- 工作树：`/Users/konghayao/code/ai/peri-async-chain-fixes-20261008`

## 证据与边界

后台任务失败已被 router 编码为 Error reminder，且显式采用 Required / EnsureProcessing。宿主却在模型失败和执行装配失败时调用全局 `activation.suppress()`，导致后续新到达的 Required 消息无法启动 loop。这是已确认的源码缺陷；不把所有 Error 或被动诊断提升为自动运行。

## 实施计划

1. 权威文档明确失败输入边界与主动 Stop 的区别。
2. 队列暴露原有接纳序号的快照及边界后 Required 激活查询，不增加持久化。
3. 每次执行开始捕获边界；失败只封锁该边界内的自动重试。统一 listener 与 continuation 准入判定，主动 Stop 仍关闭自动处理。
4. 少量定向回归覆盖失败后新任务错误、执行期间新消息、旧输入不自旋及主动 Stop；用户进行真实使用验证。
5. 在独立 worktree 提交，经轻量验证后整合回主树并同步两树。不涉及 SDK、执行恢复或数据库表。

## 交付与验证

失败记录绑定尝试开始时的原 runtime activation 身份和队列边界；模型、控制器和装配失败不再永久禁用后续新消息。listener 与 continuation scheduler 共用 `can_activate`。Cancelled 终态及显式 Stop 继续抑制自动处理；重排旧消息保持接纳序号。

使用 patched Cargo、`--locked --offline` 串行执行：

- `-p peri-acp --lib host::requests::tests::activation_tests`：11 passed。包含 HTTP 400 后子任务 Error 唤醒并写入 canonical history、装配失败后 Workflow Error 唤醒，以及晚到结果、被动 Info、空跑、取消、关闭和审批身份回归。
- `-p peri-acp --lib session::activation::tests`：4 passed。覆盖执行期间到达的新义务、旧输入及被动消息不自旋、主动 Stop、旧消息重排。
- `-p peri-acp-types --doc`：1 passed，2 ignored；ignored 不计入验证。
- 改动的 8 个 Rust 文件 rustfmt check 通过、均低于 1000 行；22 条依赖门通过、无越层；diff check 通过。

首轮宿主测试有一项 fixture 错误：裸 TaskManager 不允许缺少执行环境取消句柄的 Shell 注册。改为真实支持的子任务失败回调后重跑上述宿主测试，11 项全部通过；不通过放宽生产注册约束掩盖测试问题。

未运行真实模型/TUI E2E；Error 仅表示严重度，仅 UI 可见或显式 Passive 的通知仍不会自动启动模型。不会自动重试同一 HTTP 400 请求，需要新处理义务或用户输入。保留既有取消普通 prompt 的一次独立子任务结果续跑例外。
