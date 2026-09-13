# Workflow 脚本描述与实际 grammar 不一致

**状态**：Open  
**优先级**：高  
**类型**：缺陷 / 工具契约 / 文档  
**创建日期**：2026-09-13  
**来源**：本轮用户授权记录；观察队列 `PERI-20260913-WORKFLOW-GRAMMAR`

## 问题描述

Workflow 工具参数把 `script` 描述为“JavaScript ESM”并列出可用 primitives，但实际 parser/engine 要求唯一的 `export const meta` 形状，并依靠顶层 `return` 返回结果；额外 export、import 和旧式 `workflow.*` 调用还会被拒绝。模型依据宽泛描述生成脚本时，可能在副作用前往返失败，错误修复又可能破坏必需结构。期望工具描述和示例直接表达现行 grammar，并与 parser 的拒绝边界一致。

## 症状详情

- 观察队列记录 6 个显式错误、3 个受影响 root thread，分母为按 cwd/创建窗口选出的 48 个可见 root；这些数字只描述审计样本中的错误结果，不是任务失败率。
- 研究证据定位：thread `01a08017-0fc9-72a3-aeda-576b5c9e500e` 的 messages `01a0802c-9dbf-7370-9fd2-000bad7fe2c6`、`01a0802d-5f17-7472-afea-4fb1d73c202a`；thread `01a08429-fe60-7b72-8f2a-6dcf13d6728d` 的 message `01a08539-b971-7c72-ab66-77589cb8abda`；thread `01a0906e-2489-7702-8198-87eee6bcce6e` 的 message `01a09078-5827-7881-9cc9-054828d0ffc7`。
- `peri-workflow/src/tool.rs:179-184` 将脚本描述为 JavaScript ESM 与 primitives；`:228-234` 暴露 strictPreflight/writeIntent，`:275-291` 读取 script/scriptPath，`:327-331` 在启动前捕获 Git baseline。
- `npm-packages/@peri-workflow/src/validate.ts:4-10`、`:40-83` 记录并实现 meta、parseScript、旧式调用和顶层 return 的检查；注释明确 engine 只允许 `export const meta`。
- `peri-workflow/src/journal/git.rs:21-42`、`:77-121` 对 canonical repo/cwd 和 path allowlist 做前置约束。

## 现行观察与待验证假设

### 现行观察

- 工具描述、静态 validate 文案和 engine/parser 的约束来自不同位置，当前表述粒度不一致。
- source audit 已确认 meta 与 canonical repository guards 存在；对最小 meta、顶层 return/agent 形状做过 parser-only 验证，但没有进行真实 Agent/provider 运行回放。
- 错误文本本身不能证明模型错误是由描述造成；参数损坏、任务上下文和模型差异均仍是可行解释。

### 待验证假设

- 将脚本描述成一般 ESM，未同时给出唯一 meta export 和顶层 return 约束，可能提高生成不符合 parser 的脚本概率。
- 错误修复轮次中，模型可能根据错误提示重新生成仍不满足 grammar 的形状；需在固定模型和上下文配对实验中检验。

这里不把错误与描述的相关性写成根因结论。

## 验收场景

1. 工具描述和最小示例明确：唯一允许的 meta export、顶层 return、可用顶层 primitives，以及 static import/额外 export/旧式 `workflow.*` 的拒绝边界；示例均通过现有 parser。
2. 固定上下文下运行合法只读脚本，只调用 preflight，不执行 Agent；覆盖额外 export、缺 meta、import、缺 return、旧式调用、cwd 不符和合法示例。非法输入均在副作用前拒绝。
3. `writeIntent` 的只读/写入含义、canonical repo/cwd、path allowlist 和现有 postcondition 约束保持不放宽。
4. 记录固定任务首次有效调用及修复轮次；重复、盲化后再讨论生成行为是否改善，不能用一次成功运行替代 grammar 验证。

## 范围边界

- 范围限于 Workflow tool description、validate/parser-facing 示例、启动前 preflight 说明和相应契约测试。
- 不放宽 canonical repository、cwd 或 path allowlist，不把 strictPreflight 的不可用状态改成虚假的静态证明。
- 不修复本 issue 之外的 Workflow engine 启动、网络安装、Agent 执行或 provider 错误；相邻的 Workflow 握手问题已有独立 issue。

## 复现条件

- **复现频率**：待固定模型/上下文实验；当前为审计样本观察。
- **触发步骤**：
  1. 仅依据当前 Workflow tool description 生成脚本。
  2. 通过现有 validate/parser 和 Workflow preflight。
  3. 记录首次失败、错误修复轮次及副作用是否发生。
- **环境**：Peri Workflow tool、npm parser/engine；模型和 provider 在验收 fixture 中冻结。

## 涉及文件

- `peri-workflow/src/tool.rs` —— Workflow 参数描述、script/scriptPath 读取和 preflight。
- `npm-packages/@peri-workflow/src/validate.ts` —— grammar 辅助验证和错误文案。
- `peri-workflow/src/journal/git.rs` —— canonical cwd/repository 与写入意图保护。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-09-13 | — | Open | agent | 依据本轮用户授权和观察队列创建；待实际执行验证 |

## 修复记录

（由 auto-issue-fixer 修复阶段追加，创建时留空）
