# Goal 模式设计

**日期**: 2026-06-21
**状态**: Design（grill 完成，待实施）

## 概述

Goal 模式是长程目标跟踪机制。用户通过 `/goal` builtin skill 引导 agent 创建一个 goal，agent 在 goal 范围内自主决策、连续执行，直到声明完成或阻塞。

核心思路：agent 创建 goal 后，每轮 ReAct 循环结束时 `GoalMiddleware::after_agent` 检查 goal 状态——如果仍 Active，注入递增紧迫感的 steering 提示并设置 `block_continue`，executor 自动续跑。agent 必须调用 `goal(complete)` 或 `goal(block)` 才能真正结束循环。`complete` action 经过 LLM 二元验证（类似 compact 的辅助 LLM 调用），防止 agent 偷懒提前声明完成。

## 需求

- **单一 Goal 工具**：一个 deferred 工具，通过 `action` 参数区分 create/complete/block/get
- **会话级单例**：单个会话只有一个 goal，重复 create 报错
- **精简状态机**：None → Active → {Complete, Blocked}，终态不可逆
- **LLM 验证完成判定**：complete action 经辅助 LLM 二元判定（achieved true/false），防止偷懒
- **自驱循环**：goal active 时 after_agent 注入 steering + block_continue，executor 自动续跑
- **递增紧迫感**：连续未声明终态时，注入文本从温和提醒升级到警告
- **/goal builtin skill**：编译期嵌入的指导文档，引导 agent 何时、如何使用 goal 工具

## 架构

### 设计原则

| 原则 | 说明 |
|------|------|
| **单一工具** | 一个 `goal` 工具 + `action` 参数，而非 create/update/get 多个工具 |
| **信任 agent** | 不加连续 N 次强制放行保护，信任 agent 最终会调 complete/block |
| **验证仅 complete** | block 不验证（agent 求救信号应被信任），complete 验证（防偷懒） |
| **prompt cache 稳定** | 所有 steering 注入走 `add_message(Human, <system-reminder>)` 尾部追加 |
| **零侵入扩展** | ToolContext 扩展 BaseTool trait，为后续其他模式（如 autopilot）铺路 |

### 状态机

```
None ──create──→ Active ──complete(LLM验证通过)──→ Complete (终态)
                    └────block──────────────────→ Blocked  (终态)

终态不可逆。用户只能 /goal clear 后重建。
```

`GoalStatus` 枚举简化为 3 个值（砍掉现有的 Paused、BudgetLimited）：

```rust
pub enum GoalStatus {
    Active,    // 活跃中
    Complete,  // 终态：已完成
    Blocked,   // 终态：已阻塞
}
```

状态转换规则：
- `None → Active`：仅通过 `goal(create)` 触发
- `Active → Complete`：仅通过 `goal(complete)` + LLM 验证通过
- `Active → Blocked`：仅通过 `goal(block, reason)`
- `Complete/Blocked → 任意`：不可转换（终态）

### Goal 工具设计

单一 deferred 工具，通过 `action` 参数分发：

```json
{
  "name": "goal",
  "description": "长程目标跟踪工具。通过 action 参数区分操作：create 创建目标、complete 声明完成（需验证）、block 声明阻塞、get 查询当前目标状态。",
  "parameters": {
    "type": "object",
    "properties": {
      "action": {
        "type": "string",
        "enum": ["create", "complete", "block", "get"],
        "description": "操作类型"
      },
      "objective": {
        "type": "string",
        "description": "create 时必填。目标描述，需具体可验证。"
      },
      "reason": {
        "type": "string",
        "description": "block 时必填。阻塞原因。"
      }
    },
    "required": ["action"]
  }
}
```

| action | 状态转换 | 参数 | LLM 验证 | 重复调用 |
|--------|---------|------|---------|---------|
| `create` | None → Active | `objective`（必填） | 否 | goal 已存在时报错 |
| `complete` | Active → Complete | 无 | **是**（二元判定） | 终态不可逆 |
| `block` | Active → Blocked | `reason`（必填） | 否 | 终态不可逆 |
| `get` | 无转换 | 无 | 否 | 随时可查 |

#### create 流程

```
goal(create, objective) 调用
  → GoalController.create_goal(objective)
    → 如果 goal 已存在：返回 Err("goal 已存在，请先 clear")
    → 否则：set_goal(objective) → None → Active
  → tool_result: "目标已创建: {objective}\n请围绕此目标持续推进。完成时调用 goal(complete)，阻塞时调用 goal(block, reason)。"
```

#### complete 验证流程（工具内部）

验证在 Goal 工具的 `invoke()` 内部完成，不走 middleware 协作：

```
goal(complete) 调用
  → 从 ToolContext 读 messages（完整对话历史）
  → 从 GoalController.snapshot() 读 objective
  → 调 auxiliary_model 验证：
      system: "你是目标完成度评估器。判断 agent 是否达成了用户设定的目标。
               严格评估——只有确凿证据表明目标已达成才判 true。"
      user:   "目标: {objective}\n
               对话历史:\n{messages formatted}\n
               请输出 JSON: {achieved, evidence, missing}"
      输出: { "achieved": bool, "evidence": String, "missing": String }

  → achieved == true:
      GoalController.complete_goal() → Active → Complete
      tool_result: "目标已完成。验证证据: {evidence}"

  → achieved == false:
      goal 保持 Active
      tool_result(is_error=true): "目标未达成: {missing}。请继续工作。"
```

验证 LLM 拿到完整对话历史（不截断），因为验证是低频操作（仅在 complete 时触发），且截断可能丢失关键上下文。

#### block 流程

```
goal(block, reason) 调用
  → GoalController.block_goal(reason) → Active → Blocked
  → tool_result: "目标已标记为阻塞: {reason}"
```

block 不验证——agent 的求救信号应被信任，外部验证者未必比 agent 更了解执行细节。

#### get 流程

```
goal(get) 调用
  → GoalController.snapshot()
  → tool_result (纯文本):
      无 goal 时: "当前无目标。"
      有 goal 时: "目标: {objective}\n状态: {status}\n已用: {tokens_used} tokens, {time_used}s"
```

### after_agent 注入（GoalMiddleware）

GoalMiddleware 放在中间件链**最后**（CompactMiddleware 之后），只实现 `after_agent` 钩子（砍掉现有的 `before_model`）。

```
GoalMiddleware::after_agent(state, output):
  // 1. 前面已有 block_continue（如 HookMiddleware stop block）→ 不干预
  if output.block_continue.is_some():
      return output

  // 2. 检查 goal 状态
  snap = GoalController.snapshot()
  if snap.goal is None or snap.status != Active:
      return output  // 终态或无 goal，放行

  // 3. 注入递增紧迫感 steering
  pending_rounds += 1
  template = match pending_rounds {
      1 => 温和提醒,
      2 => 强调,
      _ => 警告,
  }
  state.add_message(BaseMessage::human(template))

  // 4. 设 block_continue，executor 自动续跑
  output.block_continue = Some("goal_active")
  return output
```

**复用现有 `block_continue` 机制**（`peri-agent/src/agent/executor/mod.rs:395-411`）：executor 在 `handle_final_answer` 返回后检测 `output.block_continue`，如果 `Some` → emit 快照 → `continue`（重新进入 ReAct 循环）。无需新增字段、无需改 trait、无需加外层循环。

**递增紧迫感模板**（`pending_rounds` 由 GoalMiddleware 内部 `AtomicUsize` 维护）：

| 轮次 | 语气 | 文本示例 |
|------|------|---------|
| 第 1 次 | 温和提醒 | "你刚才给出了回答但未声明目标完成。请决策：继续/完成/阻塞" |
| 第 2 次 | 强调 | "目标尚未完成。你必须调用 goal(complete) 或 goal(block) 来结束，或继续执行下一步。" |
| 第 3+ 次 | 警告 | "注意：目标仍未完成。请立即决策——继续工作或声明终态。" |

注入文本始终包含 goal objective，确保 agent 始终知晓当前目标。

`block_continue` 的 reason 字段固定为 `"goal_active"`（不带轮次信息，轮次由 middleware 内部计数器维护）。

**pending_rounds 重置时机**：goal 从 None → Active（create 成功）时重置为 0；goal 进入终态（Complete/Blocked）时重置为 0。确保每个新 goal 从第 1 轮开始计数。

### GoalController trait

新增读写 trait，定义在 `peri-agent` 层（解决 peri-middlewares → peri-acp 循环依赖）：

```rust
// peri-agent/src/goal/controller.rs

#[async_trait]
pub trait GoalController: Send + Sync {
    /// 创建 goal。如果 goal 已存在返回 Err。
    async fn create_goal(&self, objective: String) -> Result<(), String>;

    /// 声明完成。状态转换非法时返回 Err。
    async fn complete_goal(&self) -> Result<(), String>;

    /// 声明阻塞。reason 必填，状态转换非法时返回 Err。
    async fn block_goal(&self, reason: String) -> Result<(), String>;

    /// 只读快照（get action + after_agent 判断用）
    fn snapshot(&self) -> GoalViewSnapshot;
}
```

`peri-acp` 的 `GoalState` 同时实现 `GoalController`（读写）和现有的 `GoalStateView`（只读，T1 注入已砍但 trait 保留兼容）。

GoalMiddleware 和 Goal 工具持有同一个 `Arc<dyn GoalController>` 实例。

### BaseTool trait 扩展（ToolContext）

扩展 `BaseTool::invoke` 签名，加入只读上下文参数。这是基础设施投资，为后续其他模式（如 autopilot、agent 自省等）铺路：

```rust
// peri-agent/src/tools/mod.rs

/// 工具只读上下文（借用 state，零 clone）
pub struct ToolContext<'a> {
    pub messages: &'a [BaseMessage],
    pub cwd: &'a str,
}

#[async_trait::async_trait]
pub trait BaseTool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> serde_json::Value;

    async fn invoke(
        &self,
        input: serde_json::Value,
        ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>>;
}
```

**影响面**：仓库内 30+ 个工具实现需加 `ctx: ToolContext<'_>` 参数（不用则 `_ctx`）。这是一次性机械改动。

**调用点适配**：

1. `tool_dispatch.rs:344`：构造 ctx 并传入
   ```rust
   let ctx = ToolContext { messages: state.messages(), cwd: state.cwd() };
   t.invoke(input, ctx).await
   ```

2. `ExecuteExtraTool::invoke(input, ctx)`：转发 ctx 给 deferred tool
   ```rust
   tool.invoke(params, ctx).await  // 同一个 ToolContext 直接转发
   ```

**设计约束**：
- `ToolContext` 是**只读**引用——工具不能通过 ctx 修改 state，避免绕过 `dispatch_tools` 统一写入语义
- 字段最小集（仅 messages + cwd），后续按需扩展

### auxiliary_model 复用

`compact_model` 改名为 `auxiliary_model`，CompactMiddleware（摘要）和 Goal 工具（验证）共用同一个辅助 LLM 客户端。

涉及改名：
- `CachedLlmInstances.compact_model` → `CachedLlmInstances.auxiliary_model`
- `CompactSettings.model` → 保留或同步改名
- builder.rs 构造逻辑调整：auxiliary_model 同时注入 CompactMiddleware 和 GoalMiddleware/Goal 工具

### Builtin Skill

`/goal` skill 是编译期嵌入的指导文档：

**文件位置**：`peri-middlewares/src/skills/builtin/skills/goal/SKILL.md`

**注册**：`peri-middlewares/src/skills/builtin/mod.rs` 的 `BUILTIN_SKILLS` 常量数组追加 entry。

**内容大纲**：

```markdown
---
name: goal
description: 长程目标跟踪。当用户给出需要多步骤完成的复杂任务时使用。
  触发词："goal"、"目标"、"持续执行直到完成"、"直到 X 为止"。
---

# Goal 模式

## 何时使用
- 用户给出复杂任务，需要多轮执行才能完成
- 用户说"持续执行直到完成"、"不要中途停下"

## 如何使用
1. 调用 goal(create, objective) 创建目标
2. 持续工作，直到目标达成
3. 达成 → goal(complete)
4. 遇到无法解决的阻塞 → goal(block, reason)

## 注意
- 创建 goal 后，每轮结束时会收到提醒，要求你决策：继续/完成/阻塞
- 目标必须具体可验证（"优化代码"不好，"将测试覆盖率提到 80%"好）
- complete 会经过 LLM 验证，只有真正达成才会通过
- 可随时调用 goal(get) 查询当前目标状态
```

## 数据流

### 创建 goal → 自驱循环

```
用户: "持续重构直到所有测试通过"
  → /goal skill 触发
  → agent 调用 goal(create, "所有测试通过")
    → GoalController.create_goal() → None → Active
    → tool_result: "目标已创建: 所有测试通过"
  → agent 开始工作（调工具、写代码、跑测试）
  → ReAct 循环：LLM 给出最终答案（不再调工具）
  → handle_final_answer → chain.run_after_agent
    → GoalMiddleware::after_agent:
      - output.block_continue is None ✓
      - snap.status == Active ✓
      - pending_rounds = 1
      - 注入温和提醒 steering
      - output.block_continue = Some("goal_active")
    → 其他 middleware（链最后）无操作
  → executor 检测 block_continue == Some → continue
  → 下一轮 ReAct 循环
  → ... 重复直到 agent 调 goal(complete) 或 goal(block)
```

### complete 验证通过

```
agent 调用 goal(complete)
  → Goal 工具 invoke:
    - 从 ToolContext 读 messages
    - 从 GoalController 读 objective
    - 调 auxiliary_model 验证
    - LLM 返回 { achieved: true, evidence: "所有测试通过，共 42 个" }
  → GoalController.complete_goal() → Active → Complete
  → tool_result: "目标已完成。验证证据: 所有测试通过，共 42 个"
  → agent 看到成功，给出最终答案
  → after_agent: snap.status == Complete → 放行（不注入 block_continue）
  → executor 正常结束
```

### complete 验证失败

```
agent 调用 goal(complete)
  → Goal 工具 invoke:
    - 调 auxiliary_model 验证
    - LLM 返回 { achieved: false, missing: "仍有 3 个测试失败" }
  → goal 保持 Active
  → tool_result(is_error=true): "目标未达成: 仍有 3 个测试失败。请继续工作。"
  → agent 看到 error，继续工作（不需要 after_agent 干预）
```

### block 终止

```
agent 调用 goal(block, reason="缺少数据库访问权限，无法继续")
  → GoalController.block_goal(reason) → Active → Blocked
  → tool_result: "目标已标记为阻塞: 缺少数据库访问权限，无法继续"
  → agent 给出最终答案（说明阻塞情况）
  → after_agent: snap.status == Blocked → 放行
  → executor 正常结束
```

## 组件变更清单

| 变更 | 文件 | 说明 |
|------|------|------|
| **ToolContext 定义** | `peri-agent/src/tools/mod.rs` | 新增 `ToolContext<'a>` 结构体 |
| **BaseTool trait 扩展** | `peri-agent/src/tools/mod.rs` | invoke 加 `ctx: ToolContext<'_>` 参数 |
| **30+ 工具改签名** | 各工具实现文件 | 加 `ctx` / `_ctx` 参数 |
| **tool_dispatch 调用点** | `peri-agent/src/agent/executor/tool_dispatch.rs:344` | 构造 ctx 并传入 |
| **ExecuteExtraTool** | `peri-middlewares/src/tool_search/execute_tool.rs` | 转发 ctx |
| **GoalController trait** | `peri-agent/src/goal/controller.rs`（新文件） | 读写接口 |
| **GoalStatus 简化** | `peri-agent/src/goal/model.rs` | 砍 Paused/BudgetLimited |
| **GoalState 实现 GoalController** | `peri-acp/src/session/goal_state/mod.rs` | 新增 create/complete/block 方法 |
| **Goal 工具实现** | `peri-middlewares/src/goal/tool.rs`（新文件） | 单一工具，action 分发 |
| **GoalMiddleware 重写** | `peri-middlewares/src/goal_middleware.rs` | 砍 before_model，只保留 after_agent |
| **GoalMiddleware 注册** | `peri-acp/src/agent/builder.rs` | 链最后（CompactMiddleware 之后） |
| **auxiliary_model 改名** | `peri-acp/src/agent/builder.rs` 等 | compact_model → auxiliary_model |
| **/goal builtin skill** | `peri-middlewares/src/skills/builtin/skills/goal/SKILL.md` | 指导文档 |
| **BUILTIN_SKILLS 注册** | `peri-middlewares/src/skills/builtin/mod.rs` | 追加 goal entry |

## 边界情况

| 场景 | 处理 |
|------|------|
| goal active 时用户 Ctrl+C | goal 保持 Active（保留优于清除），continuation 停止。下次用户发消息时 after_agent 继续注入 |
| compact 后 agent 失忆 | agent 可调 goal(get) 查询当前目标；after_agent 注入文本始终含 objective |
| agent 反复 complete 但验证不通过 | 信任 agent——每次验证失败返回 missing feedback，agent 自行调整 |
| HookMiddleware 和 GoalMiddleware 同时 block_continue | GoalMiddleware 放链最后，检查 output.block_continue.is_some() → 如果前面 hook 已设则 skip |
| auxiliary_model 为 None（未配置辅助 LLM） | Goal 工具的 complete 跳过验证，直接变更状态 Active → Complete（降级为信任模式，与 block 一致） |
| goal active 时 LLM error | goal 保持 Active，history 保留。不主动 clear |
| SubAgent 调用 goal 工具 | SubAgent 中间件链不注册 GoalMiddleware、不暴露 goal 工具（goal 是 main agent 专属） |

## 不做什么

- **不加 ForceFinish 保护**：信任 agent 最终会调 complete/block，max_iterations(500) 兜底
- **不保留 before_model 注入**：create 的 tool_result + after_agent 注入已覆盖信息传递
- **不保留 Paused 状态**：用户中断用 Ctrl+C，不需要语义化的暂停状态
- **不保留 BudgetLimited 状态**：token 预算自动停止不在本期范围
- **不保留 token_budget 参数**：create 只有 objective，保持最小设计
- **不做 goal 持久化恢复（resume）**：session 结束后 goal 不保留，新 session 无 goal
- **不做 TUI goal panel**：goal 状态通过 after_agent 注入的 `<system-reminder>` 在对话流可见，本期不做独立 panel
- **不验证 block**：agent 的阻塞声明被信任
- **不改 GoalStateView trait**：保留兼容（虽然 T1 注入已砍，trait 不删，后续可能复用）
- **不做 SubAgent goal 隔离**：SubAgent 不感知 goal，不暴露 goal 工具

## 实施阶段

| Phase | 内容 | 依赖 | 验证 |
|-------|------|------|------|
| **1. ToolContext 基础设施** | BaseTool trait 扩展 + 30+ 工具改签名 + 调用点适配 | 无 | `cargo build` 全过 + 全量测试通过 |
| **2. Goal 数据层** | GoalStatus 简化 + GoalController trait + GoalState 实现 | Phase 1 | 单元测试（状态转换、重复 create 报错） |
| **3. Goal 工具** | 工具实现（create/complete/block/get）+ auxiliary_model 改名 | Phase 1, 2 | 工具单元测试（含 LLM 验证 mock） |
| **4. GoalMiddleware** | 砍 before_model + after_agent 重写 + 递增紧迫感 + 链注册 | Phase 2, 3 | 集成测试（block_continue 续跑、终态放行） |
| **5. Builtin Skill** | SKILL.md 编写 + BUILTIN_SKILLS 注册 | 无（可与 Phase 1-4 并行） | skill 加载测试 |
