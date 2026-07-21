# WriteSandbox 工具仍容易被 subagent 误认为 Write——路径不在沙箱白名单内反复报错

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-20

## 问题描述

声明了 `allowedWriteDirs` 的 readonly subagent（如 explorer、plan）持有写工具后，需要向项目目录写入文件时选了它，路径不在白名单目录内被校验拒绝，反复重试同一路径后仍失败。

这不是"沙箱工具功能异常"——校验链正确地拒绝了非白名单路径。问题是：**工具名和描述未能让 LLM 区分"这是受限写，只能写这几个目录"**。

## 根因分析（systematic-debugging）

### 纠正：不是"选错工具"

explorer 的 `disallowedTools: [Write]` 已将通用 Write 移除——agent 的工具列表中**只有 WriteSandbox 一个写工具**。不是"在 Write vs WriteSandbox 之间选错"，而是 agent 需要用写工具时别无选择，只能拿唯一的写工具往非白名单路径上套。

### 因果链

```
agent 需要写文件
  → Write 被 disallowedTools 移除，工具列表中只有 WriteSandbox
    → WriteSandbox 描述："Write a file into your sandbox directories: .peri/plans/"
      → LLM 注意力落在 "Write a file"
      → "into your sandbox" 被理解为可选约束
    → 传入非沙箱路径
      → 失败在 check ③：报「已有祖先不在沙箱目录内」——不列出允许目录
      → agent 收不到 ".peri/plans/ 才是唯一合法目标" 这一信息
    → 重试同一路径（死循环）
```

### 四个修复点（按直接影响排序）

| # | 位置 | 问题 | 修复 |
|---|------|------|------|
| 1 | `write_sandbox.rs` 描述 | 用肯定句声明能力，没用否定句声明边界 | 描述开头加 "ONLY" + 否定句 |
| 2 | `write_sandbox.rs` check ③ | 错误不列允许目录，agent 不知道限制 | 所有错误路径统一追加允许目录 |
| 3 | `explorer.md` 系统提示 | "CRITICAL: don't create files" 和 "Use WriteSandbox" 矛盾 | 统一为"只能写入沙箱" |
| 4 | 工具名 `"WriteSandbox"` | "Write" 在前，LLM 按前缀匹配时激活写语义 | 改为 `"SandboxWrite"`（限制词在前） |

## 症状详情

| 维度 | 期望行为 | 实际行为 |
|------|----------|----------|
| agent 写入普通项目文件 | 调用 Write 工具 | 调用 WriteSandbox，路径不在白名单内 |
| 报错后 agent 反应 | 切换为 Write 工具 | 反复重试 WriteSandbox 同一路径（不改路径也不换工具） |
| 出现条件 | — | 仅限持有 WriteSandbox 的 subagent（声明了 `allowedWriteDirs`） |

**用户观察到的错误**（本次对话中 explorer agent 的表现）：

```
● WriteSandbox (path: "perihelion-architecture-report.md")
  ⎿ Tool execution failed: WriteSandbox - 已有祖先不在沙箱目录内
● WriteSandbox (path: "perihelion-architecture-report.md")
  ⎿ Tool execution failed: WriteSandbox - 已有祖先不在沙箱目录内
● WriteSandbox (path: "perihelion-architecture-report.md")
  ⎿ Tool execution failed: WriteSandbox - 已有祖先不在沙箱目录内
● WriteSandbox (path: "perihelion-architecture-report.md")
  ⎿ Tool execution failed: WriteSandbox - 已有祖先不在沙箱目录内
```

agent 连续 4 次调用 WriteSandbox 写同一个非白名单路径，报错后不切换工具、不调整路径，陷入死循环。

### 验证 #1（2026-07-21）—— 修复 #1 后错误率仍高

修复 #1（改名 SandboxWrite + 错误消息追加允许目录 + 描述加否定边界）已应用，但 agent 仍需 3 轮试错才能用对路径：

```
● SandboxWrite (path: "agent-capability-topics-report.md")
  ⎿ Tool execution failed: SandboxWrite - WriteSandbox: 路径 'agent-capability-topics-report.md'
     的已有祖先不在沙箱目录内。允许的目录: ["/Users/.../perihelion/.peri/plans"]
● Shell (ls -la /Users/.../perihelion/.peri/plans/ 2>/dev/null || mkdir -p ...)
  ⎿ Tool 'Bash' not found
● SandboxWrite (path: "agent-capability-topics-report.md")
  ⎿ [同上错误，盲重试]
● SandboxWrite (path: "/Users/.../perihelion/.peri/plans/agent-capability-topics-report.md")
  ⎿ Tool execution failed: 拒绝绝对路径 '...'。请使用基于项目根的相对路径。
     允许的目录: ["/Users/.../perihelion/.peri/plans"]
● SandboxWrite (path: ".peri/plans/agent-capability-topics-report.md")
  ⎿ Wrote 69 lines .peri/plans/agent-capability-topics-report.md
```

**残余问题分析**：

1. **`allowed_dirs_display()` 显示绝对路径，但工具拒绝绝对路径**——矛盾信息导致 agent 在第 4 轮尝试绝对路径。`sandbox_roots` 是 canonicalized 的绝对路径，但 tool description 要求相对路径。错误消息中显示绝对路径目录 + 提示"请使用基于项目根的相对路径" = LLM 收到自相矛盾的信号。

2. **"已有祖先不在沙箱目录内"错误不够可操作**——当 agent 传入裸文件名 `agent-capability-topics-report.md`，错误只说"不在沙箱内"并列出允许目录（绝对路径版），但 agent 不清楚**具体应该用什么格式**。它需要自己推断 `裸文件名 → .peri/plans/裸文件名`。

**根因**：修复 #1 追加了允许目录到错误消息中，但用的是 `sandbox_roots`（canonicalized 绝对路径），与工具只接受相对路径的约束矛盾。应改为使用 `allowed_dirs`（原始相对路径）。

## 与历史修复的区别

| 历史 issue | 修了什么 | 本次的区别 |
|------------|----------|-----------|
| [plan-agent-writesandbox-not-found] | 沙箱目录不存在 → 工具构造失败，静默跳过 | 工具构造成功、校验链正确拒绝了非白名单路径——**agent 不该调用它** |
| [sandboxed-write-rejects-absolute-path] | 参数描述说绝对路径，校验拒绝绝对路径 → 描述矛盾 | 路径格式正确（相对路径），但**目标不在白名单目录下** |

前两个 issue 修的是工具本身的正确性问题。本 issue 是 **LLM 对工具的误解问题**：工具名 `WriteSandbox` 太像 `Write`，agent 选错工具。

## 涉及文件

- `peri-middlewares/src/tools/filesystem/write_sandbox.rs:11-15` —— 工具描述常量，`description` 由前缀 `"Write a file into your sandbox directories: "` + 白名单列表 + 后缀拼接
- `peri-middlewares/src/subagent/tool/build_agent.rs:92-127` —— WriteSandbox 注入逻辑，声明 `allowedWriteDirs` 的 agent 自动获得此工具

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-20 | — | Open | agent | 创建 |
| 2026-07-21 | Open | Fixed | agent | 修复 #2：allowed_dirs_display 改用相对路径 |

## 修复记录

### 修复 #1（2026-07-20）

- **操作人**：agent
- **用户原意**：WriteSandbox 工具名和描述容易被 LLM 误解，需系统修复
- **修复内容**：

  1. **工具描述加否定边界**（`write_sandbox.rs`）：前缀加 `ONLY`，后缀加 `Do NOT use this tool for files outside the sandbox directories listed above.`
  2. **所有错误路径追加允许目录**（`write_sandbox.rs`）：新增 `allowed_dirs_display()` 辅助方法，全部 8 条错误路径统一追加 `"{允许的目录: [...]}"`. 关键受益——check ③ 是 agent 最常遇到的失败点，现在会显示允许目录而非光秃秃的"祖先不在沙箱内"
  3. **消除系统提示词矛盾**（`explorer.md`, `plan.md`, `verification.md`）：CRITICAL 段落从"完全禁止创建文件"改为"NO PROJECT CODE MODIFICATIONS — Exception: you MAY use SandboxWrite for .peri/plans/". 同时追加否定句 `Do NOT attempt to use it for files outside .peri/plans/`
  4. **工具名改名**（`write_sandbox.rs`）：`name()` 从 `"WriteSandbox"` 改为 `"SandboxWrite"`（限制词在前，LLM 按前缀处理时优先激活沙箱语义）。`aliases()` 返回 `["WriteSandbox"]` 保持向后兼容。`build_agent.rs` disallowed 检查同时接受 `"sandboxwrite"` 和 `"writesandbox"`
- **涉及文件**：`write_sandbox.rs`, `write_sandbox_test.rs`, `build_agent.rs`, `explorer.md`, `plan.md`, `verification.md`
- **测试**：`cargo test -p peri-middlewares --lib` — 994 passed, 0 failed

### 修复 #2（2026-07-21）

- **操作人**：agent
- **用户原意**：修复 #1 后 SandboxWrite 错误率仍高，agent 需 3 轮试错才能用对路径
- **修复内容**：

  1. **`allowed_dirs_display()` 改用相对路径**（`write_sandbox.rs`）：原实现遍历 `sandbox_roots`（canonicalized 绝对路径），展示如 `/Users/.../.peri/plans`。但工具只接受相对路径，LLM 看到"允许绝对路径目录"后会尝试绝对路径然后被拒绝。改为直接使用 `allowed_dirs`（原始相对路径 `.peri/plans/`），消除矛盾信号。

  2. **`WriteSandboxTool` 新增 `allowed_dirs` 字段**：存储原始相对路径列表，供 `allowed_dirs_display()` 使用。`sandbox_roots` 保留用于路径校验。

  3. **新增回归测试** `test_write_sandbox_error_displays_relative_dirs`：验证错误消息展示相对路径而非绝对路径。

- **涉及文件**：`write_sandbox.rs`, `write_sandbox_test.rs`
- **测试**：`cargo test -p peri-middlewares --lib` — 995 passed, 0 failed
- **验证状态**：待验证
