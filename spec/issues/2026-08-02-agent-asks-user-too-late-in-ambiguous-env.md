# agent 在症状不明/环境冲突时问用户太晚，静态深挖不收敛

**状态**：Fixed
**优先级**：中
**创建日期**：2026-08-02

## 问题描述

agent 把"自环境失败/症状不明"等同于"代码缺陷"，缺少向用户要环境状态/症状的升级意识；静态推理不收敛时无纪律切换到实证/提问。wander 报告（8-01 后样本）3/5 会话出现，单会话最高浪费估算 ~45-50%。

## 现状

- **019fc1b0（图片粘贴流程调查）**：#22 结论已成立（"代码看起来完整，链路应该是通的"）、#83 集成测试通过，仍继续 30 条静态追查（#84-#112），#113 才 AskUserQuestion，被用户当场中断——用户是唯一掌握运行时信息（剪贴板/权限）的人，静态分析永远无法回答
- **019fbdbe（workflow 测试等待修复）**：#65 已发现用户正在自己跑同 repo 测试（首要嫌疑），不升级为提问，继续 15 条深挖；40 分钟 tmux 死因取证与最终修复完全无关，#80 用户直接宣布"我修复了 e2e"
- **019fc204（plugin 卸载）**：功能代码层已实现、用户却说"少了卸载功能"，先猜 20 条（含 git log、snapshot 事件深挖）才问；#42 一条 AskUserQuestion 即拿到关键症状"Enter 卸载后卡死"

共性：推理文本连续出现"可能/也许"且无用户输入时仍继续深挖。

## 期望改进方向

prompt 硬规则（frozen prompt 层）：

1. 症状不明或存在不可静态验证的运行时环节（剪贴板/外部进程/并发用户）时，N 次工具调用内必须 AskUserQuestion
2. 检测到同 repo 存在用户测试进程/新 tmux session 时注入环境共享提示
3. 推理文本连续出现"可能/也许"且无用户输入时提示提问或切换实证

## 涉及文件

- frozen prompt / `run_react_loop` 行为约束所在处（修复时定位）
- `peri-agent/src/agent/` 相关 stage（reason/act 的提示词组装）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-02 | — | Open | agent | 创建 |
| 2026-08-02 | Open | Fixed | agent | P0 prompt 硬规则（Ask Before Diving + 05 工具规则）+ P1 speculation_guard 哨兵（分级提醒，SubAgent 排除） |

## 修复记录

### 修复 #1（2026-08-02）

- **操作人**：agent
- **用户原意**：agent 在症状不明/环境冲突时必须及时 AskUserQuestion，禁止无收敛的静态深挖（019fc1b0/019fbdbe/019fc204 三例）
- **修复内容**：
  - **P0（prompt 硬规则，frozen prompt 层）**
    - `peri-acp/prompts/sections/03_doing_tasks.md`：新增 `## Ask Before Diving` 小节——运行时环节（剪贴板/权限/外部进程/并发用户/tmux）6-8 次工具调用内必须提问；结论成立即停止追查；推测性语言连续出现且无新证据时停止深挖
    - `peri-acp/prompts/sections/05_using_tools.md`：AskUserQuestion 条目补充"推测而非证实时必须优先提问"；新增环境共享信号规则（ps/tmux 输出显示同 repo 测试进程/新 session → 先 AskUserQuestion 确认）
    - `peri-acp/src/prompt/prompt_test.rs`：新增 `Ask Before Diving` 与推测优先提问规则断言
  - **P1（speculation_guard 哨兵，代码层）**
    - 新文件 `peri-agent/src/agent/stages/speculation_guard.rs`：A 连续无输入工具轮 ≥N1（默认 6，最近 2 轮 thought 命中推测词降为 4）+ B 当前轮无用户输入 + C 最近 2 轮 thought 命中推测词或最近 2 轮工具错误 + D 本 turn 未 AskUserQuestion，全部满足才注入提醒；L1 温和（"已连续 N 轮推测深挖无新证据…立即 AskUserQuestion"）/ L2 强制（"必须 AskUserQuestion 或向用户汇报现状"）分级
    - `peri-agent/src/agent/stages/mod.rs`：`LoopState` 扩展（speculation_rounds / recent_speculation / recent_errors / warned_level / asked_user，生命周期=turn）；`run_react_loop` 接入（Receive 前查 `has_pending_prompt` → 用户 Prompt 到达时 reset；Reason 后提取 thought 副本与 AskUserQuestion 检测；Act 后对比 consecutive_failures 判断本轮工具错误，再调 `observe_tool_round`）；`StageContext`/`StageContextBuilder` 新增 `ask_discipline` 开关（默认 true，`with_ask_discipline(bool)` 可显式关闭）
    - `peri-agent/src/session/queue.rs`：`MessageSource` 新增 `SpeculationGuard` 变体；`MessageQueue` 新增 `has_pending_prompt()`
    - 提醒注入与 `handle_consecutive_failures` 同款：`QueuedMessage::info` push 到 v2 queue，下轮 Receive 消费（`<system-reminder>` 包裹）；Info 消息消费不触发 reset（否则哨兵自身提醒会把计数清零，L2 永远无法升级）
    - **SubAgent 排除**：SubAgent 构建点（`peri-middlewares/src/subagent/v2_bridge.rs`）与主 agent 构建点（`peri-acp/src/agent/builder.rs`）均在本 issue 修改范围之外且 agent_id 无法区分（随机 Uuid），故用运行时信号——主 agent 的 session_context 由 builder 注入 `session_id` 键，SubAgent 为空 HashMap；该键存在才启用
    - 测试：`speculation_guard_test.rs` 8 个用例（L1/L2 分级、SubAgent 无 session_id 不触发、ask_discipline(false) 不触发、AskUserQuestion 后不触发、用户中途插话 reset 后重新计数、工具错误路径 N1=6、推测词/window 纯函数），全部 stub LLM + stub 工具，无真实弹窗
  - **P2（after_tool 触发式同 repo 测试进程检测）**：**跳过**。`tool_dispatch.rs` 不在本 issue 允许修改范围（边界内仅 mod.rs / 新文件 / 测试）；且语义模糊——"疑似同 repo 测试进程"难以区分 agent 自己启动的测试与用户启动的测试，误报风险高于收益。P0 的 prompt 规则 2 已在提示层覆盖该场景
- **涉及 commit**：未提交（用户未要求，将统一提交）
- **验证状态**：已验证（peri-acp prompt 41/41、peri-agent stages 67/67 全过，workspace 构建成功，我的改动文件 fmt 干净 + clippy 零警告。注：验证期间另一 agent 正在改 `peri-agent/src/agent/events.rs` 及其连锁文件，workspace 级 `cargo fmt --check` 被其未格式化中间状态污染，与本次改动无关）

> **修订说明（2026-08-02，考究后）**：初版 prompt 含"6-8 次工具调用内必须提问"与推测词清单，经考究判定过火——工具调用次数非进度度量（打断正常深度工作流）、推测词表会被模型规避（Goodhart）。改为：prompt 原则化（去数字、去词表，保留运行时场景锚点与判断原则）；哨兵阈值统一 N1=6（删除推测词降阈值逻辑），推测词仅影响 L1 提醒措辞（推测措辞 vs 工具错误措辞），L2 措辞同步软化为"应停止静态追查"。`prompt_test.rs` 断言不变（标题保留）。
