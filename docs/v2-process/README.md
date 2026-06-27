# peri-agent v2 重做进度

> **[2026-06-26 归档]** v2 重做已**全部完成**：P1–P5 + P6.1-P6.6 物理清理全部落地。
> v1 `ReActAgent` / `executor/` 目录 / `State` trait / `CompactMiddleware` / v1 `MessageQueue`
> 已物理删除，所有执行路径统一通过 v2 `run_react_loop` 驱动。
>
> **当前权威文档**：
> - 架构状态：仓库根 `CLAUDE.md`「v2 架构状态」段落（活档）
> - **手动 Smoke test 清单：[`2026-06-26-p6-complete-smoke-test.md`](./2026-06-26-p6-complete-smoke-test.md)** ← 接手者从这里开始
>
> 本目录的其他快照、双轨/PERI_USE_V1/v1 残留描述均为 P5/P6 前的历史过程记录，**仅作归档参考**。

## 文件导航

| 文件 | 用途 |
|------|------|
| [`2026-06-26-p6-complete-smoke-test.md`](./2026-06-26-p6-complete-smoke-test.md) | **当前 smoke test 清单**：P6 完成后的手动验证矩阵 + 关键不变量 |
| `2026-06-25-stage4-complete.md` | 历史快照：Stage 4 完成（Langfuse Generation 端到端） |
| `2026-06-25-v2-default.md` | 历史快照：v2 切默认完成（双路径测试等价） |
| `2026-06-25-stage3-complete.md` | 历史快照：Stage 3 完成（Top 9/10）|
| `2026-06-25-stage2-complete.md` | 历史快照：Stage 2 完成（Top 2/3/6/7） |
| `2026-06-25-stage1-complete.md` | 历史快照：Stage 1 修复 |
| `roadmap.md` | 历史路线（P5.1–P5.5 已全部完成） |
| `files-index.md` | 历史文件索引（v1 残留 / 双轨边界，P6 后已失效） |
| `verification.md` | 历史验证步骤（双轨对比，P6 后已失效） |
| `p5-v1-removal-checklist.md` | P5 物理删除清单（已完成） |

## 一句话状态

**v2 单路径架构完全清理完成（P1-P5 + P6.1-P6.6）。2721 测试全过；所有 v1 执行路径物理删除；架构状态以仓库根 `CLAUDE.md` 为权威。**

## 上下文链接

- 起点设计：`docs/design/peri-agent-*.md`（10 份 v2 设计文档）
- 6-24 状态：`docs/superpowers/plans/2026-06-24-v2-architecture-status.md`
- 主计划：`~/.claude/plans/majestic-zooming-haven.md`
- 项目工作守则：仓库根 `CLAUDE.md`（「v2 架构状态」段落）

## 分支与基线

- 分支：`feature/v2-architecture`
- 测试基线：**2721 passed / 0 failed / 14 ignored** —— v2 单路径，无 v1 回退
- Workflow runs（历史）：`wb11noou1` / `w8tb17ryr` / `wktaez2zx` / `w0qofjdeh`

## 文件导航

| 文件 | 用途 |
|------|------|
| [`2026-06-25-stage4-complete.md`](./2026-06-25-stage4-complete.md) | **最新快照**：Stage 4 完成（Langfuse Generation 端到端，Top 11+12） |
| [`2026-06-25-v2-default.md`](./2026-06-25-v2-default.md) | 历史快照：v2 切默认完成（双路径测试等价） |
| [`2026-06-25-stage3-complete.md`](./2026-06-25-stage3-complete.md) | 历史快照：Stage 3 完成（Top 9/10，2 commits，+3 测试）|
| [`2026-06-25-stage2-complete.md`](./2026-06-25-stage2-complete.md) | 历史快照：Stage 2 完成（Top 2/3/6/7） |
| [`2026-06-25-stage1-complete.md`](./2026-06-25-stage1-complete.md) | 历史快照：Stage 1 修复（已合并入早期 commit） |
| [`roadmap.md`](./roadmap.md) | 剩余路线：手动 smoke test → 切默认 → P5 SubAgent/Hook 迁移里程碑 |
| [`files-index.md`](./files-index.md) | 相关文件索引（按 v2 主路径 / v1 残留 / 双轨边界 分类） |
| [`verification.md`](./verification.md) | 验证步骤（build / test / clippy / fmt / smoke） |

## 一句话状态

**v2 stages 默认路径 + Langfuse Generation 端到端完整（Stage 1/2/3/4 + 切默认全部落地）；2929 测试双路径等价；进入 dogfood 阶段；P5 物理删除 v1 需先迁移 SubAgent/Hook（独立里程碑，4–6 人周）。**

## 上下文链接

- 起点设计：`docs/design/peri-agent-*.md`（10 份 v2 设计文档）
- 6-24 状态：`docs/superpowers/plans/2026-06-24-v2-architecture-status.md`
- 主计划：`~/.claude/plans/majestic-zooming-haven.md`
- 项目工作守则：仓库根 `CLAUDE.md`（「v2 架构状态」段落）

## 分支与基线

- 分支：`feature/v2-architecture`
- 测试基线：**2929 passed / 0 failed / 4 ignored** —— 双路径指 v2 默认 + v1 回退（`PERI_USE_V1=1`），两路径数字完全一致
- Stage 4 落地后无回归（v2 默认 / v1 回退等价）
- Workflow runs：`wb11noou1`（Stage 2）+ `w8tb17ryr`（Stage 3）+ `wktaez2zx`（切默认）+ `w0qofjdeh`（Stage 4）
