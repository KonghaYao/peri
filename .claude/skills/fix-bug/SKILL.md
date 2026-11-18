---
name: fix-bug
description: 在独立 worktree 中排查并修复 bug：按证据定 P 级（P0 停止汇报；P1 由 explorer 确认并写 spec/issues 后交给 coder 修复；P2/P3 主 agent 一次性修复），验证后提交 GitHub PR，并等待 CI 与 CodeRabbit，失败自动修复重推最多 3 轮。用户通过 /fix-bug 主动调用。
argumentHint: "<bug 描述 | issue 链接 | 报错/日志 | 复现步骤>"
disable-model-invocation: true
---

# Fix Bug

本 skill 由用户主动调用（`/fix-bug`）。主 agent 是唯一控制者与验收人。按 `WORKTREE → 检查 → 定级 → 修复 → 验证 → PR → 等待` 顺序执行；P0 在定级处终止。

## 0. 输入与基线

1. 从参数提取：现象、期望、复现步骤、报错/日志、相关 issue。只问阻塞复现的信息。
2. 读仓库 `AGENTS.md` / `CLAUDE.md`、`docs/standards/` 与模块指引，确认验证命令、提交规范和分支约定。
3. 运行 `git status --short --branch`，记录既有 dirty 文件。本流程不得 stash、reset、clean 或顺手提交这些改动。
4. 生成小写英文 slug（如 `session-admission-inode`），分支与 worktree 都使用它。

## 1. WORKTREE

所有读写与命令都在 worktree 内执行；主 checkout 只用于 `git worktree` 管理。

1. 默认先 `git fetch --prune origin`（清理远端已删除的跟踪分支，避免基于陈旧 ref），再把选定的 worktree 路径存入 `$WT`：默认 `.worktrees/<slug>`，`.worktrees/` 未被忽略时改用同级仓库外目录 `../<repo>-worktrees/<slug>`，避免污染 `git status`。从**远程最新目标分支**创建：通常为 `origin/main`，用户指定 base 时改用 `origin/<base>`。
   ```bash
   git fetch --prune origin
   WT=.worktrees/<slug>   # .worktrees/ 未被忽略时改为 ../<repo>-worktrees/<slug>
   git worktree add --no-track -b fix/<slug> "$WT" origin/main
   ```
   不要直接基于本地 `main` 或其它可能过期的本地分支创建：过期基线会带入已合并的旧改动，增加冲突和无关 diff。只有远程分支不存在或用户明确指定本地基线时才改用，并在汇报中说明原因。
2. 核对：`git -C "$WT" status --short --branch` 不得显示 `[origin/main]`；若显示，执行 `git -C "$WT" branch --unset-upstream`。
3. 在汇报中写明实际 worktree 路径、基线与 commit（`git -C "$WT" log -1 --oneline`）。

完成标准：worktree 存在、分支未跟踪 main、主 checkout 无新增改动。

## 2. 检查问题

1. 优先运行时复现。不能复现时用测试、日志或代码证据，并标注"未运行时复现"。
2. 记录：触发条件、实际/期望、影响面、首次出现版本（必要时 `git bisect`）、相关日志。
3. 写出根因链：`触发 → 机制 → 用户可见后果`，区分观察与推断。
4. 搜索同一根因或同一模式的其它调用点，按"修复一类问题"确定范围。
5. 涉及依赖行为的关键假设（hook 执行顺序、坐标变换、生命周期）必须读**实际版本**的依赖源码核对，不能只信本仓库注释；注释可能滞后于依赖升级。

完成标准：根因链每步有代码、日志或复现证据；范围清单完整。

## 3. 定级

| 级别 | 判据 | 动作 |
| --- | --- | --- |
| P0 | 数据丢失/损坏、安全漏洞或密钥泄露、崩溃/panic/无法启动、核心流程完全不可用且无规避 | 立即停止，向用户汇报 |
| P1 | 核心功能错误或明确回归、可稳定复现、影响主要路径但有临时规避 | explorer 确认 → 写本地 issue → coder 修复 |
| P2 | 非核心功能错误、边界条件、明显性能退化、错误提示/日志缺失 | 主 agent 一次性修复 |
| P3 | 轻微文案/格式、低影响边界、可维护性 | 与 P2 同批修复 |

判不准时取更高级别；P0/P1 有争议按 P0 汇报。定级依据写入最终汇报。

## 4. P0：汇报并停止

1. 汇报：现象、确认证据、影响面、可能原因、建议选项。
2. 保留 worktree 与证据，不建 issue、不修复、不 commit。
3. 等待用户指示；用户明确要求继续后才进入修复。

## 5. P1：确认 → issue → coder 修复

1. 派 `explorer`（同步、只读）确认：复现路径、根因证据、影响面、修复边界、回归测试建议。要求返回文件位置与证据，不给结论性猜测。
2. 主 agent 逐条核对仓库事实。结论被否认或降级时，回到第 3 步重新定级并说明原因。
3. 写 `spec/issues/YYYY-MM-DD-p1-<slug>.md`，使用 [P1 issue 模板](references/p1-issue-template.md)，状态写"已确认，待修复"。
4. 派 `coder`（同步），给出：issue 路径、worktree 绝对路径、允许编辑的文件清单、验证命令、测试要求。要求不改无关文件、不 commit，返回改动/命令/失败/残留风险。
5. 主 agent 审 diff 并跑目标检查；不通过则 resume 同一 coder 修复。

Subagent 的 cwd 不能切到 worktree，必须在 prompt 中给出绝对路径，并明确"只能编辑该 worktree 内的文件"。

完成标准：issue 文件落盘、diff 只含允许文件、目标检查通过。

## 6. P2/P3：主 agent 一次性修复

1. 同一根因或同一模式的问题一次修完，补回归测试。
2. 不新建 issue；改动范围写入最终汇报。
3. 需要产品决策、数据库 schema 变更或跨模块重构时，先问用户。

## 7. 验证

3. 先验证用户可观察行为，再按风险补回归、失败路径与生命周期测试。TUI 坐标类问题按 [真实复现方法](references/real-repro.md) 在真实终端中对照修复前后。
2. 跑仓库约定的目标测试、lint、format、build；Peri 仓库优先 `./scripts/check-changed.sh`。
3. 运行 `git diff --check`；报告必须引用命令真实输出，不以"应该可以"代替。
4. 任何失败先修；环境/网络失败与产品失败分开记录。
5. P1 修复完成后，回填 `spec/issues/YYYY-MM-DD-p1-<slug>.md` 的"验证记录"，并把状态改为"已修复"。

完成标准：所有改动文件被目标检查覆盖，无未解释失败。

## 8. 提交与 PR

1. 只暂存本任务文件；审查 `git diff --cached`，不打包无关 WIP。
2. 提交信息用 `fix(<scope>): <summary>`；hook 正常运行，不跳过。
3. `git push -u origin HEAD`；禁止 force push。
4. 按 [PR 模板与等待命令](references/pr-and-checks.md) 写正文后：
   ```bash
   gh pr create --base main --title "fix(<scope>): <summary>" --body-file <file>
   ```
   目标分支默认 `main`；用户或仓库约定另有要求时按其执行。
5. 用 `gh pr view --json url,baseRefName,headRefName` 核对 PR。

完成标准：PR 已创建，URL、目标分支、head 分支正确。

## 9. 等待 CI 与 CodeRabbit

按 [等待与失败处理](references/pr-and-checks.md) 执行：

1. 轮询 CI 直到无 pending；失败时读取 `gh run view <id> --log-failed`。
2. 等待 `coderabbitai[bot]` 对当前 head 的 review 或 comment；review 的 `commit_id` 必须等于 PR head，不能只看时间。
3. 默认最长等待 30 分钟；超时汇报当前状态，不无限等待。
4. CI 失败或 CodeRabbit blocking 意见成立时：修复 → 本地验证 → commit → push，重新等待，最多 3 轮。
5. 意见不成立时在对应 thread 回复证据；不静默忽略。
6. 3 轮后仍失败：停止并汇报失败项、已尝试修复、PR 当前状态。

结束前必须确认 CI 与 CodeRabbit 处于终态；不得只输出"继续等待"而没有实际等待动作。

完成标准：CI 全部 pass（非阻塞 skip 需说明）；CodeRabbit 已产生实质 review（或明确记录限流/未启用），blocking 意见已处理或已回复。**CodeRabbit 的绿色 check 不等于评审完成**，必须核对 review 的 `commit_id` 与 PR head。

## 10. 最终汇报

按有序列表输出，少字：

1. P 级别与依据
2. worktree、分支、PR URL
3. 改动文件
4. 验证命令与结果
5. CI / CodeRabbit 状态
6. 遗留风险、未覆盖项、需要用户决定的事项

## 红线

- P0 必须停止汇报，不得自行修复或提交。
- 不修改数据库 schema、不 force push、不跳过 hook、不 stash/reset/clean 用户改动。
- 不把环境或网络故障当成产品 bug；无法复现必须标注证据等级。
- PR、commit、日志中不得出现密钥、token、连接串或完整环境变量。
- 同类修复只做一次；需要产品决策或扩大范围时先问用户。
