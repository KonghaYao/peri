#!/usr/bin/env bash
# 机器人维护 KonghaYao/peri-e2e 的 SHA pin。
#
# 触发：.github/workflows/e2e-pin-bump.yml（schedule + workflow_dispatch）。
# 行为：解析 e2e main HEAD；与 base 分支上的 pin 不同时，在固定分支
# `chore/e2e-pin-bump` 上生成只改 pin 的提交，并创建/更新 PR。绝不直接 push base。
#
# `chore/e2e-pin-bump` 归机器人所有：每次运行都会把它重置到 base 当前提交，
# 不要在该分支上追加其他改动。
#
# 环境：
#   GITHUB_REPOSITORY   owner/repo（Actions 提供）
#   GH_TOKEN            具备 contents:write 与 pull-requests:write 的 token
#   E2E_REPOSITORY      默认 KonghaYao/peri-e2e
#   E2E_REF             默认 refs/heads/main
#   PIN_PATH            默认 .github/peri-e2e.rev
#   BASE_BRANCH         默认 main
#   BUMP_BRANCH         默认 chore/e2e-pin-bump
#   DRY_RUN=1           只读：解析并打印计划，不创建 ref/提交/PR
#   GITHUB_STEP_SUMMARY 存在时写入结论
#
# 退出码：0 = 已同步、已跳过或已创建/更新 PR；非 0 = 需要人工处理（网络、权限、
# pin 格式错误、PR 创建被仓库设置阻止等）。

set -euo pipefail

repo="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"
e2e_repo="${E2E_REPOSITORY:-KonghaYao/peri-e2e}"
e2e_ref="${E2E_REF:-refs/heads/main}"
pin_path="${PIN_PATH:-.github/peri-e2e.rev}"
base_branch="${BASE_BRANCH:-main}"
bump_branch="${BUMP_BRANCH:-chore/e2e-pin-bump}"
dry_run="${DRY_RUN:-0}"
api="repos/${repo}"

die() {
    printf 'e2e-pin-bump: %s\n' "$*" >&2
    exit 1
}

summary() {
    if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
        printf '%s\n' "$*" >> "$GITHUB_STEP_SUMMARY"
    fi
}

for command in gh git jq base64; do
    command -v "$command" >/dev/null 2>&1 || die "missing required command: $command"
done

# ── 1. 目标 SHA ───────────────────────────────────────────────────────────────
# pin 的语义是“跟随 e2e main”，因此这里不做只增不减判断：e2e main 回退时，
# pin 也会回到 main 当前值。
if ! remote_line="$(git ls-remote "https://github.com/${e2e_repo}.git" "$e2e_ref")"; then
    die "failed to read ${e2e_repo} ${e2e_ref} (network or repository visibility)"
fi
target_sha="$(printf '%s\n' "$remote_line" | awk 'NR == 1 {print $1}')"
[[ "$target_sha" =~ ^[0-9a-f]{40}$ ]] \
    || die "cannot resolve ${e2e_repo} ${e2e_ref} to a 40-char lowercase SHA"

# ── 2. base 上的当前 pin ──────────────────────────────────────────────────────
if ! base_sha="$(gh api "${api}/git/ref/heads/${base_branch}" --jq '.object.sha')"; then
    die "failed to resolve ${base_branch} HEAD"
fi
[[ "$base_sha" =~ ^[0-9a-f]{40}$ ]] || die "invalid ${base_branch} HEAD: ${base_sha}"

if ! pin_json="$(gh api "${api}/contents/${pin_path}?ref=${base_sha}")"; then
    die "failed to read ${pin_path} at ${base_sha}"
fi
[[ "$(jq -r '.encoding' <<<"$pin_json")" == "base64" ]] \
    || die "${pin_path} response is not base64 encoded"
file_sha="$(jq -r '.sha' <<<"$pin_json")"
[[ "$file_sha" =~ ^[0-9a-f]{40}$ ]] || die "invalid blob sha for ${pin_path}: ${file_sha}"
if ! current_pin="$(jq -r '.content' <<<"$pin_json" | tr -d '\n' | base64 -d)"; then
    die "failed to decode ${pin_path}"
fi
current_pin="${current_pin//[$'\r\n ']/}"
[[ "$current_pin" =~ ^[0-9a-f]{40}$ ]] \
    || die "${pin_path} on ${base_branch} must be a 40-char lowercase SHA"

echo "target ${e2e_repo}@${e2e_ref}: ${target_sha}"
echo "current ${pin_path} at ${base_sha}: ${current_pin}"

if [[ "$current_pin" == "$target_sha" ]]; then
    echo "pin already up to date"
    summary "peri-e2e pin 已是最新：\`${target_sha}\`。"
    exit 0
fi

# ── 3. 已有打开 PR 且分支 pin 已是目标值时跳过 ────────────────────────────────
pr_number="$(gh pr list --repo "$repo" --head "$bump_branch" --base "$base_branch" \
    --state open --json number --jq '.[0].number // empty')"
if [[ -n "$pr_number" ]]; then
    if branch_json="$(gh api "${api}/contents/${pin_path}?ref=${bump_branch}" 2>/dev/null)"; then
        if branch_pin="$(jq -r '.content' <<<"$branch_json" | tr -d '\n' | base64 -d)"; then
            branch_pin="${branch_pin//[$'\r\n ']/}"
            if [[ "$branch_pin" == "$target_sha" ]]; then
                echo "open PR #${pr_number} already pins ${target_sha}; nothing to do"
                summary "已有 PR #${pr_number} 指向 \`${target_sha}\`，跳过。"
                exit 0
            fi
        fi
    fi
fi

echo "plan: ${current_pin} -> ${target_sha} on ${bump_branch}"
summary "计划：\`${pin_path}\` \`${current_pin}\` -> \`${target_sha}\`。"
if [[ "$dry_run" == "1" ]]; then
    echo "dry-run: skip branch, commit and PR changes"
    exit 0
fi

# ── 4. 重置机器人分支并在其上生成单行提交 ────────────────────────────────────
# 分支基于刚读取的 base_sha，因此 file_sha 仍是该分支上 pin 文件的 blob sha；
# 带上它是 Contents API 的乐观并发检查。
if gh api "${api}/git/ref/heads/${bump_branch}" >/dev/null 2>&1; then
    gh api --method PATCH "${api}/git/refs/heads/${bump_branch}" \
        -f sha="$base_sha" -F force=true >/dev/null \
        || die "failed to reset ${bump_branch} to ${base_sha}"
else
    gh api --method POST "${api}/git/refs" \
        -f ref="refs/heads/${bump_branch}" -f sha="$base_sha" >/dev/null \
        || die "failed to create ${bump_branch} at ${base_sha}"
fi

new_content="$(printf '%s\n' "$target_sha" | base64 | tr -d '\n')"
gh api --method PUT "${api}/contents/${pin_path}" \
    -f message="chore(e2e): bump peri-e2e pin to ${target_sha:0:12}" \
    -f content="$new_content" \
    -f sha="$file_sha" \
    -f branch="$bump_branch" >/dev/null \
    || die "failed to commit pin update to ${bump_branch}"

# ── 5. 复用仍打开的 PR，否则新建 ─────────────────────────────────────────────
# PR 可能已被人工关闭或分支被删除，因此这里重新查询而不是复用步骤 3 的结果。
pr_number="$(gh pr list --repo "$repo" --head "$bump_branch" --base "$base_branch" \
    --state open --json number --jq '.[0].number // empty')"
title="chore(e2e): bump peri-e2e pin to ${target_sha:0:12}"
body="$(printf '%s\n' \
    "- \`${e2e_repo}@main\` 当前为 \`${target_sha}\`。" \
    "- \`${pin_path}\`: \`${current_pin}\` → \`${target_sha}\`。" \
    "- 合并后特性 PR rebase 到包含该 pin 的 \`${base_branch}\`；不要手改 pin。")"

if [[ -n "$pr_number" ]]; then
    gh pr edit "$pr_number" --repo "$repo" --title "$title" --body "$body" >/dev/null \
        || die "failed to update PR #${pr_number}"
    echo "updated PR #${pr_number} (${target_sha})"
    summary "已更新 PR #${pr_number}：\`${current_pin}\` -> \`${target_sha}\`。"
else
    if ! pr_url="$(gh pr create --repo "$repo" --base "$base_branch" --head "$bump_branch" \
        --title "$title" --body "$body")"; then
        die "failed to create PR; enable repository setting 'Allow GitHub Actions to create and approve pull requests'"
    fi
    echo "created PR: ${pr_url}"
    summary "已创建 PR：${pr_url}（\`${current_pin}\` -> \`${target_sha}\`）。"
fi
