#!/usr/bin/env bash
# CI 变更分类：判断一次运行是否只改了「已确认不影响构建与测试」的文档路径。
#
# 只有**全部**受影响路径都落在白名单内才判定 docs-only；任何一处落在白名单外、
# 状态码不认识、或 base 版本无法解析，都回退为完整验证。文档与代码混改因此天然
# 走完整验证，不需要额外规则。
#
# 白名单刻意不用 `**/*.md` 全局匹配：crate 内的 `descriptions/*.md`、`SKILL.md`
# 等是 `include_str!` 的构建输入，改它们必须触发验证。白名单只列经核实「CI 不读」
# 的路径（本仓库 CI 步骤只读 Cargo 工程、scripts/import-exemptions.conf 与
# patches/，没有 include_str!/build.rs 读 docs、spec 或根目录文档）。
#
# 用法: ci-classify-changes.sh [--full REASON]
#
# 环境（GitHub Actions 提供，缺一即回退完整验证）：
#   EVENT_NAME        github.event_name
#   PR_BASE_SHA       github.event.pull_request.base.sha   （pull_request）
#   PUSH_BEFORE_SHA   github.event.before                  （push）
#   CI_HEAD_SHA       github.sha
#   GITHUB_OUTPUT     存在时写入 docs_only / reason
#   GITHUB_STEP_SUMMARY  存在时写入可读结论（便于 review 区分「按策略跳过」与失败）
#   CI_DIFF_FILE      仅用于测试：直接读取预先算好的 `git diff --name-status -M -z`
#                     输出（NUL 分隔），跳过 base 解析与 git 调用
#
# 退出码：0 = 分类完成（docs_only 见输出），非 0 仅用于自身错误（调用方按完整验证处理）。

set -euo pipefail

full_reason=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --full)
            [[ $# -ge 2 ]] || { echo "usage: $0 [--full REASON]" >&2; exit 2; }
            full_reason="$2"
            shift 2
            ;;
        -h|--help)
            sed -n '2,28p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "usage: $0 [--full REASON]" >&2
            exit 2
            ;;
    esac
done

# ─── 白名单：改动这些路径不影响构建与测试 ────────────────────────────────────
# 新增条目必须能回答「CI 的哪一步读它」；答不上来才允许加入。
docs_only_patterns=(
    '^README\.md$'
    '^CHANGELOG\.md$'
    '^LICENSE$'
    '^\.github/README\.md$'
    '^docs/'
    '^spec/'
)

# ─── 解析 base 版本并取一份 `git diff --name-status -M -z` 输出 ──────────────
docs_only="false"
reason="$full_reason"
changed_count=0
offending=""
range=""

# ─── 取一份 `git diff --name-status -M -z` 输出 ──────────────────────────────
# 走临时文件而不是命令替换：`-z` 用 NUL 分隔记录，而 bash 的命令替换会丢掉
# NUL 字节，把整份 diff 粘成一行。
diff_file="$(mktemp)"
trap 'rm -f "$diff_file"' EXIT
have_diff=false

if [[ -n "${CI_DIFF_FILE:-}" ]]; then
    # 测试旁路：直接喂一份已算好的 diff，跳过 base 解析与 git 调用。
    cp "$CI_DIFF_FILE" "$diff_file"
    have_diff=true
elif [[ -z "$reason" ]]; then
    event_name="${EVENT_NAME:-}"
    head_sha="${CI_HEAD_SHA:-$(git rev-parse HEAD)}"
    base_sha=""
    range_style=""

    if [[ "$event_name" == "pull_request" || "$event_name" == "pull_request_target" ]]; then
        base_sha="${PR_BASE_SHA:-}"
        # 三点：只比较 PR 引入的改动，不把 base 分支上的其他提交算进来。
        range_style="three-dot"
    elif [[ "$event_name" == "push" ]]; then
        base_sha="${PUSH_BEFORE_SHA:-}"
        range_style="two-dot"
    fi

    if [[ -z "$base_sha" ]]; then
        reason="cannot resolve base revision for event '${event_name:-<unset>}'"
    elif [[ "$base_sha" =~ ^0+$ ]]; then
        # 新分支的首次 push 没有 before 提交。
        reason="push has no predecessor commit (before=${base_sha})"
    elif ! git cat-file -e "${base_sha}^{commit}" 2>/dev/null; then
        reason="base revision ${base_sha} is not available locally"
    elif ! git cat-file -e "${head_sha}^{commit}" 2>/dev/null; then
        reason="head revision ${head_sha} is not available locally"
    else
        if [[ "$range_style" == "three-dot" ]]; then
            range="${base_sha}...${head_sha}"
        else
            range="${base_sha}..${head_sha}"
        fi
        # --find-renames：重命名必须同时看到新旧两侧路径，只看新路径会把
        # 「docs/x.md → src/x.rs」误判成文档改动。
        if git diff --name-status --find-renames -z "$range" > "$diff_file" 2>/dev/null; then
            have_diff=true
        else
            reason="git diff failed for range ${range}"
        fi
    fi
fi

if [[ -z "$reason" && "$have_diff" == true ]]; then
    # -z 记录结构为 <status>\0<path>\0（重命名为 <status>\0<old>\0<new>\0）。
    # 状态码决定随后读入几条路径，因此文件名即使形如 `M` 或 `R100` 也不会错位。
    pending_paths=0
    is_docs_only=true
    while IFS= read -r -d '' token; do
        if [[ $pending_paths -eq 0 ]]; then
            case "$token" in
                A|D|M|T) pending_paths=1 ;;
                R[0-9]*|C[0-9]*) pending_paths=2 ;;
                *)
                    reason="unsupported diff status '${token}'"
                    break
                    ;;
            esac
            continue
        fi
        pending_paths=$((pending_paths - 1))
        changed_count=$((changed_count + 1))
        matched=false
        for pattern in "${docs_only_patterns[@]}"; do
            if [[ "$token" =~ $pattern ]]; then
                matched=true
                break
            fi
        done
        if [[ "$matched" != true ]]; then
            is_docs_only=false
            if [[ -z "$offending" ]]; then
                offending="$token"
            fi
        fi
    done < "$diff_file"

    if [[ -n "$reason" ]]; then
        :
    elif [[ "$changed_count" -eq 0 ]]; then
        # 分类没看到任何路径：可能是 range 退化（例如同一提交重跑）。
        # 宁可按完整验证处理，也不要把「没看到改动」当成「没有改动」。
        reason="diff reported no changed paths${range:+ (range ${range})}"
    elif [[ "$is_docs_only" == true ]]; then
        docs_only="true"
        reason="${changed_count} changed path(s), all in the docs-only whitelist"
    else
        reason="changed path outside the docs-only whitelist: ${offending}"
    fi
fi

summary_out="${GITHUB_STEP_SUMMARY:-/dev/null}"
if [[ "$docs_only" == "true" ]]; then
    {
        printf '### CI 变更分类：docs-only\n\n'
        printf '只改动了白名单内的文档路径（%s 个），按策略跳过 Rust 三平台任务。\n\n' "$changed_count"
        printf '该结论由 `CI result` 汇总任务记录为通过，不是失败或取消。\n'
    } >> "$summary_out"
else
    {
        printf '### CI 变更分类：完整验证\n\n'
        printf '原因：%s\n\n' "$reason"
        printf 'Rust 三平台任务将正常执行。\n'
    } >> "$summary_out"
fi

if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
    printf 'docs_only=%s\n' "$docs_only" >> "$GITHUB_OUTPUT"
    printf 'reason=%s\n' "$reason" >> "$GITHUB_OUTPUT"
fi

printf 'docs_only=%s reason=%s\n' "$docs_only" "$reason"
