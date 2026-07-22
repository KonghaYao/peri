> 归档于 2026-07-18，原路径 spec/issues/2026-07-18-subagent-write-sandbox-tool.md
# SubAgent 沙箱写工具（WriteSandbox）：让 planner 等 readonly agent 能输出交接文件

**状态**：Done
**优先级**：中
**创建日期**：2026-07-18

## 问题描述

`plan` 等 readonly subagent 的 frontmatter 通过 `disallowedTools` 禁用了 Write/Edit/Bash，导致它们无法把产出（计划、分析结果）落盘。交接文件是 subagent 之间最快的信息传递通道——主 agent 不需要把大段计划文本塞进下一个 subagent 的 prompt，只需给一个文件路径。当前 planner 的产出只能以回复文本形式返回，长计划在多跳传递中易丢失、难迭代。

## 设计目标

给 subagent 一个**能力最小化**的写入通道：只能写 frontmatter 声明的目录白名单（沙箱），不能碰项目代码。第一个使用方是 `plan` agent，沙箱目录为 `{cwd}/.peri/plans/`（项目级，可 git 追踪）。

## 核心设计决策（经 grilling 确认）

| # | 决策点 | 结论 | 否决方案及理由 |
|---|--------|------|----------------|
| 1 | 写入范围界定 | **通用沙箱写工具**，frontmatter 声明目录白名单，任何 readonly agent 可声明自己的沙箱 | 专用 WritePlan 硬编码目录（将来其他 agent 需要沙箱时要再造一个工具）；dispatch 层 hook 识别 caller（违反"工具自声明"惯例，安全逻辑不应塞进调度层） |
| 2 | 白名单声明位置 | **frontmatter 新增 `allowedWriteDirs` 字段**，构建时按 agent 实例化注入工具 | settings.json 按 agent id 配置（安全边界拆两处，配置漂移）；工具内硬编码 id→目录映射（集中式静态表反模式） |
| 3 | 写入语义 | **允许覆盖写**（plan 需迭代），限目录白名单 + 拒绝路径穿越，**不限后缀**（将来可能写 .json 结构化交接），不进 HITL 审批（沙箱本身即审批的替代） | create-only（逼出 plan-v2-final.md 垃圾命名）；完整 Write 等价（无必要） |
| 4 | 能力标签 | `can_mutate` 推断**忽略** `allowedWriteDirs`，plan 保持 `[readonly]`，可继续并行调度 | 标 `[writes]`（被踢出并行组，调度能力倒退）；第三档 `[sandboxed-writes]`（主 agent 认知负担，边际价值低） |
| 5 | 命名与可发现性 | 工具名 **`WriteSandbox`**（名字即边界）；frontmatter 字段 `allowedWriteDirs`（与 allowedTools 命名家族一致）；**description 动态注入**该实例的白名单目录 | WriteRestricted（描述限制而非能力，LLM 倾向不用）；WriteFile（与 Write 语义重叠） |
| 6 | 注入与过滤顺序 | `filter_tools(继承)` → **`allowedWriteDirs` 非空则追加 WriteSandbox 实例** → 应用 `disallowedTools` 黑名单。白名单未列出仍注入（声明即授权）；黑名单显式列出 `WriteSandbox` 可否决（逃生门） | 白名单优先（静默忽略是最差失败模式）；不可否决（违背最小权限可逆原则） |
| 7 | 路径安全 | 完整校验链：① 词法拒绝绝对路径和 `..`；② 构造时 canonicalize 沙箱根并缓存；③ 写入前确保父目录存在并 canonicalize，校验以沙箱根为前缀；④ 目标文件已存在时 canonicalize 目标再校验（拦 symlink 逃逸，写入会跟随 symlink） | 纯词法前缀匹配（symlink 逃逸防不住）；O_NOFOLLOW（平台相关，④ 已可移植地解决） |

威胁模型：攻击面主要是 **prompt injection**——planner 读到的恶意网页/issue 内容诱导它写 `../../.git/hooks/pre-commit` 或利用预置 symlink 逃逸沙箱。

## 方案

### 工具定义

```
name: WriteSandbox
description: 动态注入，示例：
  "Write a file into your sandbox directories: .peri/plans/.
   Paths are relative to the project root. Overwriting is allowed.
   Absolute paths and '..' are rejected."
params: { path: string, content: string }
```

### frontmatter 示例（built-in/plan.md）

```yaml
disallowedTools:
  - Agent
  - Write
  - Edit
  - Bash
allowedWriteDirs:
  - ".peri/plans/"
```

正文增加指引：将最终计划写入 `.peri/plans/<topic>.md`，并在回复中给出文件路径。

### 注入链路

```
SubAgentTool::invoke
  → load_agent_def（解析 frontmatter，新增 allowedWriteDirs 字段）
  → build_agent_from_def
      → filter_tools(parent_tools, tools, disallowed)   # 现有继承过滤
      → if !allowedWriteDirs.is_empty():
            push WriteSandbox::new(cwd, allowedWriteDirs)  # per-agent 实例
      → disallowedTools 再过滤一次（含 WriteSandbox 则移除）
  → build_v2_subagent_context（SharedToolMap）
```

注意：WriteSandbox **不走父工具继承**（共享 Arc 无法携带 per-agent 配置），不进 `CORE_TOOLS`，主 agent 不获得此工具（它有完整 Write）。

## 涉及文件

- `peri-middlewares/src/tools/filesystem/` —— 新增 `write_sandbox.rs`（复用 `WriteFileTool` 写入逻辑，外层包校验链）
- `peri-middlewares/src/subagent/` agent 定义解析处 —— `ClaudeAgent` frontmatter 新增 `allowedWriteDirs: Vec<String>`
- `peri-middlewares/src/subagent/build_agent.rs` —— `build_agent_from_def` 注入逻辑（决策 6 顺序）
- `peri-middlewares/src/subagent/built-in/plan.md` —— frontmatter + 正文指引
- `peri-middlewares/src/subagent/mod.rs:572-595` —— `infer_agent_capability` 注释：沙箱目录不计入 `can_mutate`
- `peri-acp/src/prompt/mod.rs:88` —— 注释同步（can_mutate = 是否修改项目代码）

## 测试清单

**WriteSandbox 错误路径（P0）**：
- `..` 穿越拒绝（错误消息含路径）
- 绝对路径拒绝
- 父目录 symlink 逃逸拒绝
- 目标文件为 symlink 指向沙箱外拒绝
- 沙箱外相对路径拒绝
- 正常创建 + 覆盖写成功
- description 包含注入的白名单目录

**frontmatter 解析（P0）**：`allowedWriteDirs` roundtrip + 缺失时默认空

**注入顺序（P1）**：
- 白名单 `tools` 未列 WriteSandbox 时仍注入（决策 6）
- `disallowedTools` 显式列出 WriteSandbox 时不注入
- `allowedWriteDirs` 为空时不注入

**能力推断（P1）**：声明 `allowedWriteDirs` 的 agent `can_mutate` 仍为 false

## 明确不做

- 不进 HITL 审批列表（沙箱即边界，审批是双重打断）
- `11_subagent.md` 调度规则不改、agent roster 标签保持 `[readonly]`
- TUI `tool_display` 暂不定制
- 不做全局 `~/.peri/plans/`（项目级已够用，YAGNI）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-18 | — | Open | agent | 创建（/grill-me 产出，7 项设计决策已确认） |

## 修复记录

（待修复后追加）
