# Workflow 脚本加载阶段静默失败，工具应检测快速失败并同步报错

**状态**：Open
**优先级**：中
**类型**：Bug
**创建日期**：2026-06-23
**关联**：`spec/issues/2026-06-23-workflow-defects-consolidated.md` D1（Workflow 失败但无错误可追溯）

## 问题描述

当 Workflow 工具被调用后，若脚本在**加载/解析阶段**即失败（Node 进程启动后，脚本 ESM 解析或沙箱限制导致立即退出），工具仍返回 "Workflow 'xxx' started" 并给出 `run_id`。实际的失败信息后续通过 `<system-reminder>` 通知异步到达，但调用者（LLM）已经拿到了"已启动"的假成功回复，且通知中不包含任何错误细节。

触发的两条脚本均为合法 JavaScript ESM（含有效 `export const meta`），但均在 6ms 内失败，0 agents、0 tool calls，且 `state.json` 未生成。

## 症状详情

| 现象 | 实际表现 | 期望表现 |
|------|----------|----------|
| 工具返回值 | `"Workflow 'xxx' started. run_id: ..."` | 应返回错误信息 |
| 失败通知 | 异步到达，`status: Failed (6ms, 0 agents, 0 tool calls)` | 同步返回失败原因 |
| state.json | 未生成 | 应写入包含错误的 state.json |
| 错误细节 | 无 | 应包含脚本加载失败的具体原因（沙箱限制 / 语法错误 / 模块解析失败） |

## 复现条件

- **复现频率**：必现（两次不同脚本均一致）
- **涉及脚本**：
  1. 复杂脚本：含 `parallel` + `pipeline` + `phase` + `log`，7 个 agent
  2. 最简脚本：仅 `await agent('说一句 Hello World', ...)` + `return`
- **相同特征**：6ms 内失败，0 agents 执行，`state.json` 未生成

## 涉及文件

- `peri-workflow/src/runner.rs:162-355` —— `workflow/start` 发送后进入消息循环；脚本阶段失败通过 `workflow/done` 返回，但 runner 的 `Ok(())` 已在上层被当作启动成功
- `peri-workflow/src/tool.rs:96-269` —— `invoke()` 立即返回 "started"，然后 spawn notification task 等待 `done_rx`；快速失败信息无法同步返回给调用者
- `peri-workflow/src/error.rs:14` —— `ScriptParse(String)` 错误类型已定义，但当前路径中未被触发（Node 端脚本失败不走 Rust 端解析）

## 期望改进方向

### 核心：工具侧检测快速失败

`WorkflowTool::invoke()` 在返回 "started" 前，先竞速等待一小段时间（如 1 秒）看 `done_rx` 是否收到失败结果：

```
方案 A: tool.invoke() 中
  tokio::select! {
    result = done_rx => {
      // 快速失败：同步返回错误
      return Err(format!("Workflow failed: {}", result.error))
    }
    _ = tokio::time::sleep(Duration::from_secs(1)) => {
      // 未立即失败：正常返回 "started"
    }
  }
```

### 附属：错误信息透传

- Runner 的 `workflow/done` handler（runner.rs:293-313）已正确处理 error 字段，但若 Node 进程在 `workflow/start` 响应前就崩溃，错误信息通过 stderr 输出（runner.rs:153-159，仅 `tracing::debug` 记录），未被捕获到 `WorkflowResult.error` 中。需将 stderr 的最后 N 行纳入错误字段。

### 附属：state.json 写入

- Runner 的消息循环（runner.rs:325-337）仅在正常退出时写 state.json。若 Node 进程在发送 `workflow/done` 前崩溃，state.json 不生成。应在 `tokio::select!` 的 `_ = child.wait()` 路径也写 state.json。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-06-23 | — | Open | agent | 创建 |

## 修复记录

（待修复后追加）
