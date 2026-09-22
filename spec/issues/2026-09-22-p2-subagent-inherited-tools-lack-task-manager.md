# P2：子 agent 继承的工具集缺失 task_manager，`Bash(run_in_background)` 恒失败

**状态**：Open
**优先级**：P2（失败是显式报错而非静默错误，不污染任务结果；真正代价是子 agent 的 prompt 与工具表矛盾，诱导模型发起必然失败的调用）
**类型**：装配漏注入 / 子 agent 能力缺口
**创建日期**：2026-09-22
**来源**：2026-09-22 用户报告「run_in_background 在 subagent 中不能触发，缺失 task manager」；两路 subagent 独立调查结论一致，主 agent 复核关键环节
**最后核查**：2026-09-22（macOS 26.5.1 arm64，本机；仅静态代码核查，未做运行时复现）

## 问题描述

模型在 subagent（Agent 工具派生的子 agent）内部调用 `Bash(run_in_background: true)` 时无法触发后台执行，返回硬错误：

```
run_in_background is not available: no background task manager configured
```

同一调用在主 session 内正常。前台执行的超时 promote 也依赖同一字段，因此在子 agent 内超时只会 kill 进程组、不会转为后台任务。

## 现象边界（两类 background 必须区分）

| 场景 | 现状 |
| --- | --- |
| ① 父 agent 用 `Agent(run_in_background: true)` 派后台 subagent | **正常**，有装配顺序契约与回归测试守护 |
| ②-b 子 agent 内部调 `Bash(run_in_background: true)` | **恒失败**，即本 issue |
| ②-a 子 agent 内部再派 subagent | 工具表中无 `Agent`（`fork.rs:46-48` 强制剔除），模型只能产生无效调用；但子 agent 的 system prompt 却宣称拥有该工具，见下「关联缺陷」 |

## 证据

### 失败路径（已复核）

`peri-middlewares/src/middleware/terminal.rs:309-315`：`run_in_background` 为真时对工具实例的 `task_manager` 字段 `ok_or`，字段为 `None` 直接返回上述硬错误，**不降级前台**。

### 实例来源（已复核）

- `peri-middlewares/src/assembly/preparation.rs:128-130`：子 agent 的继承源 `parent_tools` 用 `TerminalMiddleware::build_tools(cwd)` 构造 Bash；
- `terminal.rs:761-763`：`build_tools` → `BashTool::new(cwd)`，`task_manager: None`。

### 同一文件内的正确写法（已复核）

- `terminal.rs:765-774`：`build_tools_with_registry(cwd, task_manager)` 已存在，只是 `parent_tools` 未使用；
- `terminal.rs:789-795`：主链 `collect_tools` 走 `self.task_manager.clone()`，故主 session 可用；
- 对照路径：workflow agent 工厂用 `build_tools_with_registry`（`peri-middlewares/src/assembly/workflow.rs`，subagent 报告，未复核）。

### 子链无法补上（subagent 报告，未逐一复核）

- 子 agent 中间件链只有 AgentsMd/Skills/[SkillPreload]/Todo，**不含 TerminalMiddleware**（`peri-middlewares/src/subagent/tool/mod.rs:37-76`），无法覆盖继承来的 Bash；
- 工具集由 `parent_tools` 投影而来，`fork.rs:34-70` 只按名过滤、不重建实例；三条 spawn 路径（`execute_fork.rs:41`、`execute_bg.rs:51`、`execute_resume.rs:99`）都取 `parent_tools`；
- 子 agent 侧不做 `chain.collect_tools` 合并（`peri-agent/src/session/subagent/v2_bridge.rs:261-266`）。

### 可修性（已复核）

`peri-agent/src/session/factory.rs:315`：`AssemblyContext.task_manager: Arc<TaskManager>` 已在手边，`preparation.rs:115-120` 解构时用 `..` 忽略了它 —— 属遗漏，非设计约束。

## 关联缺陷：子 agent 的 prompt 与工具表矛盾（subagent 报告）

11_subagent 段被无条件收集进子 agent 自己的 system prompt（`peri-acp/src/session/frozen.rs:149-151`），该段开头却是「You have access to the `Agent` tool…」并含 Background Tasks 整节（`peri-acp/prompts/sections/11_subagent.md:3`），而子 agent 实际拿不到 `Agent`、也拿不到后台能力。模型据此发起的调用必然失败或落空。

可与主问题同批修，也可拆分。

## 测试空白

- 仓库**无任何**覆盖 ② 的单元/集成/e2e 用例；`bg_register_cancel_test.rs`、`e2e/tests/subagent/bg-task-area.test.ts` 覆盖的都是 ①（后者子 agent 内部用的是前台 `Bash sleep 12`，恰好绕过本缺陷）。
- `tool_test/fork_test.rs` 用手写 `parent_tools` 固化了生产路径不会出现的行为，易误导后续判断（subagent 报告）。

## 影响

- 子 agent 无法把长命令转后台，只能前台占用一轮；超时场景直接丢结果（kill 进程组、无 promote、无后台取回）。
- 报错本身清晰、不静默，任务结果不被污染，故定级 P2。
- 关联缺陷会让模型反复尝试无效调用，浪费轮次与 token。

## 建议方向

目标语义（2026-09-22 已定）：**支持子 agent 内后台工具**，采用最小注入方案。

1. **注入**：`preparation.rs:129` 改用 `build_tools_with_registry(cwd, Some(Arc::clone(&ctx.task_manager)))`（注意 `Arc<TaskManager>` → `Arc<dyn TaskManager>` 的 unsize coercion）。
2. **完成通知**：如需 `on_bg_complete`，当前 `build_tools_with_registry` 写死 `None`（`terminal.rs:772`），需扩展签名或复用 `AssemblyContext.on_bg_complete`（`factory.rs:319`）。
3. **必须接受的语义后果**（需写入文档）：子 agent 的后台 shell 注册进**父 session** 的 manager —— 父 session 的 idle 探针会等待该任务、父 session 关闭时 `cancel_all` 会清理它、与父共享 shell 并发上限（5）。
4. **实施前待验证**：
   - 任务完成通知的唤醒目标：父事件通道还是子 agent 对话；
   - 子 agent 已结束而任务未完成时的归属与可见性（是否会成为孤儿任务）；
   - `host.task_manager` 为空时 `Agent(run_in_background)` 静默降级同步的既有行为（`define.rs:209`）是否需一并明确。
5. **测试补充**：单元断言子 agent 继承到的 BashTool 具备 manager；e2e 增加「子 agent 内 Bash 后台 + 任务注册到父 manager + 可取回结果」阶段；现有 ① 类用例不得受影响。
6. **可选同批**：修正子 agent prompt，不再渲染 Agent 工具与 Background Tasks 段。
