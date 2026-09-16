# Git 分支与上游规则

### GIT-BRANCH-001

- **Scope**：从 remote-tracking branch 创建本地 feature、fix、fixture 或 refactor 分支。
- **Rule**：以 `origin/main` 等 remote-tracking branch 为基点创建非 main 分支时，必须使用 `git switch --no-track -c <branch> origin/main`（重建已有本地分支时用 `--no-track -C`）。禁止直接使用 `git switch -c/-C <branch> origin/main`，因为 `branch.autoSetupMerge` 可能把新分支 upstream 隐式绑定为 `origin/main`，导致普通 `git push` 或 `git pull` 面向 main。首次发布分支时使用 `git push -u origin HEAD`，让 upstream 绑定到远端同名分支。
- **Verify**：创建后运行 `git status --short --branch`、`git branch -vv` 和 `git config --get-regexp '^branch\.<branch>\.(remote|merge)$'`；feature 分支不得显示 `[origin/main]`，其 `branch.<branch>.merge` 不得为 `refs/heads/main`。若误绑，在尚未推送前执行 `git branch --unset-upstream`，再用 `git push -u origin HEAD` 建立同名 upstream。

### GIT-BRANCH-002

- **Scope**：切换、拆分、重建或压缩分支历史。
- **Rule**：`origin/main` 只作为 commit 基点，不等于目标 upstream。执行 `switch -C`、`checkout -B`、`rebase --onto` 或 squash 后，必须重新验证当前分支的 upstream；不得根据“分支名不是 main”推断 push 目标安全。发现 feature 分支跟踪 `origin/main` 时，在纠正前不得执行无显式 refspec 的 `git push` 或 `git pull`。
- **Verify**：运行 `git rev-parse --abbrev-ref HEAD`、`git rev-parse --abbrev-ref --symbolic-full-name '@{upstream}'` 和 `git branch -vv`；必要时使用 `git push --dry-run origin HEAD:<same-branch-name>` 核对目标，不执行 force push。

### GIT-WIP-001

- **Scope**：已有未提交改动或多人/多 agent 共用的工作树。
- **Rule**：开始前记录工作树与暂存区基线，区分本任务改动和既有改动；交接后重新核对。脏工作树无需清空，不得为通过测试、hook 或整理提交而 stash、checkout、reset、clean 本任务授权范围外的改动。需要干净基线时使用独立 worktree 验证；不能仅凭最终 dirty 文件集合归因失败。用户明确要求操作既有改动时按其范围执行，恢复和删除 stash 以固定身份及恢复核对为依据，不连续按会漂移的 `stash@{0}` 猜测归属。
- **Verify**：对比操作前后的 `git status --short`、`git diff` 与 `git diff --cached`；涉及 stash 时核对具体 OID、内容与恢复结果。结束时能说明每项遗留改动的归属，不以“工作树干净”替代保留证据。

### GIT-COMMIT-001

- **Scope**：暂存与提交，尤其同文件混合多个任务或只提交部分 hunk。
- **Rule**：提交授权按用户指定范围解释；默认只暂存本任务的路径或 hunk，不顺手打包其他 WIP，也不反复询问已明确授权的提交。提交前审查完整 staged diff，包括删除和未跟踪文件的纳入。局部暂存后验证实际提交快照的完整性；工作树测试通过不能证明抽走部分改动后的 staged tree 可编译。修改过暂存内容后，先前对该快照的验证不再适用。
- **Verify**：运行 `git diff --cached --check` 并核对 `git diff --cached`；局部暂存影响代码依赖时，在隔离目录验证 staged tree 的目标检查。从本次 commit 的成功输出核实实际生成的 OID，用 `git show --stat --oneline <commit-oid>` 与剩余 status 核对提交范围及无关改动仍被保留；并行工作时不得仅凭当前 HEAD 归属或宣称本任务已提交。
