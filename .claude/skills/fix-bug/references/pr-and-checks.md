# PR 正文与 CI / CodeRabbit 等待

## PR 正文模板

```markdown
## 问题

<现象、触发条件、影响面；P 级别与定级依据>

## 根因

<根因链，含关键 文件:行号>

## 修复

<改动概述、范围、明确不做的事>

## 验证

- `<命令>` → <结果>
- 回归测试：<新增或修改的测试>

## 风险与遗留

<兼容性、性能、未覆盖项；无则写"无">

## 关联

- 本地问题记录：`spec/issues/YYYY-MM-DD-p1-<slug>.md`（如有）
```

## 等待 CI

```bash
# 当前状态；退出码 8 表示仍有 pending
gh pr checks <PR> --json name,state,bucket

# 失败日志
gh run view <run-id> --log-failed

# check 汇总
gh pr view <PR> --json statusCheckRollup
```

- `bucket` 为 `fail` 或 `cancel`：进入修复。
- `bucket` 为 `pending`：30 秒后重查。
- `bucket` 为 `skipping`：记录原因，确认不是必需的检查。

PR 刚创建时 checks 可能尚未注册。先等 60–120 秒再判断"无 pending"；结果为空时用 `gh run list --branch <head-branch>` 确认 CI 是否触发，不要把空 checks 当作 pass。

等待可以用：

```bash
gh pr checks <PR> --watch --interval 30
```

退出码 0 表示全部结束且通过，非 0 表示存在失败。Windows 构建可能超过 30 分钟；不要用一个长前台超时硬等，按 10 分钟一段后台 watch，超时后重新查询 `--json` 状态再续等。

## 等待 CodeRabbit

```bash
# review 活动
gh api repos/{owner}/{repo}/pulls/<PR>/reviews \
  --jq '.[] | select(.user.login | test("coderabbit"; "i")) | {state, submitted_at, body}'

# issue comment 活动
gh api repos/{owner}/{repo}/issues/<PR>/comments \
  --jq '.[] | select(.user.login | test("coderabbit"; "i")) | {created_at, body}'
```

`{owner}/{repo}` 由 `gh repo view --json nameWithOwner -q .nameWithOwner` 获取。

完成判据：

1. 存在时间晚于最后一次 push 的 CodeRabbit review 或 comment。
2. review 状态为 `CHANGES_REQUESTED` 时，逐条处理。
3. 评论中的 blocking 意见逐条对照代码判断；成立才修，不成立则在对应 thread 回复证据。

**绿色 `CodeRabbit` check 不等于评审完成。** 必须看 review/comment 内容与时间戳。

- 评论含 `Review limit reached`（或 `rate limited`）：本次没有实质评审。记录该限制并汇报，不要写成"CodeRabbit 已通过"；额度恢复后可再检查一次。
- 30 分钟内没有任何 CodeRabbit 活动时，检查 PR 是否安装或启用 CodeRabbit，然后汇报"未检测到"，不继续等待。

## 失败循环

1. 记录失败项：check 名、run id，或 CodeRabbit 评论摘要。
2. 本地复现失败原因，区分产品缺陷、测试缺陷、环境问题。
3. 修复并跑目标检查。
4. `git add` 本任务文件 → `git commit -m "fix(<scope>): ..."` → `git push`。
5. 重新等待 CI 与 CodeRabbit；轮次 +1。
6. 3 轮后仍失败：停止，汇报失败项、已尝试修复、PR 当前状态。

每轮保留：失败项、是否成立、修复内容、验证结果。禁止 force push、跳过 hook 或删除失败日志。
