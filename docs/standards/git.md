# Git 分支与上游规则

### GIT-BRANCH-001

- **Scope**：从 remote-tracking branch 创建本地 feature、fix、fixture 或 refactor 分支。
- **Rule**：以 `origin/main` 等 remote-tracking branch 为基点创建非 main 分支时，必须使用 `git switch --no-track -c <branch> origin/main`（重建已有本地分支时用 `--no-track -C`）。禁止直接使用 `git switch -c/-C <branch> origin/main`，因为 `branch.autoSetupMerge` 可能把新分支 upstream 隐式绑定为 `origin/main`，导致普通 `git push` 或 `git pull` 面向 main。首次发布分支时使用 `git push -u origin HEAD`，让 upstream 绑定到远端同名分支。
- **Verify**：创建后运行 `git status --short --branch`、`git branch -vv` 和 `git config --get-regexp '^branch\.<branch>\.(remote|merge)$'`；feature 分支不得显示 `[origin/main]`，其 `branch.<branch>.merge` 不得为 `refs/heads/main`。若误绑，在尚未推送前执行 `git branch --unset-upstream`，再用 `git push -u origin HEAD` 建立同名 upstream。

### GIT-BRANCH-002

- **Scope**：切换、拆分、重建或压缩分支历史。
- **Rule**：`origin/main` 只作为 commit 基点，不等于目标 upstream。执行 `switch -C`、`checkout -B`、`rebase --onto` 或 squash 后，必须重新验证当前分支的 upstream；不得根据“分支名不是 main”推断 push 目标安全。发现 feature 分支跟踪 `origin/main` 时，在纠正前不得执行无显式 refspec 的 `git push` 或 `git pull`。
- **Verify**：运行 `git rev-parse --abbrev-ref HEAD`、`git rev-parse --abbrev-ref --symbolic-full-name '@{upstream}'` 和 `git branch -vv`；必要时使用 `git push --dry-run origin HEAD:<same-branch-name>` 核对目标，不执行 force push。
