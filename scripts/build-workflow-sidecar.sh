#!/usr/bin/env bash
# Workflow sidecar 的生成前置：安装依赖 → 构建 → 身份校验 → 落盘标准布局。
#
# 正式发布（release-agent）与预发布（pre-release）共用本脚本，二者不再各自维护一套
# 生成规则；两边的 build 矩阵都依赖同一个前置任务，因此任何生成/身份问题都在 Rust
# 编译之前失败，而不是等六个平台编译完再在打包阶段炸掉。
#
# 产物不入库（`npm-packages/@peri-workflow/dist/` 由 `bun run build` 生成，仓库只保存
# 其 sha256 摘要 = `peri-workflow/src/runner/artifact.rs::WORKFLOW_ENTRY_SHA256`）。
# 因此本脚本必须自己完成构建，并且构建结果必须逐字节命中该摘要 —— 不匹配时不允许
# 「跳过校验」继续：要么修构建，要么同步更新 Rust 侧 pin 与其验证。
#
# 用法: build-workflow-sidecar.sh [--out-dir DIR]
#   --out-dir DIR   sidecar 布局根目录（默认 <repo>/dist/workflow-sidecar）
#                   产物写入 DIR/peri-resources/workflow/<内容身份>/
#
# 环境:
#   PERI_SIDECAR_FAULT   故障注入，仅用于验证「前置失败阻止矩阵」（默认 none）：
#                          none              正常构建
#                          build-failure     bun run build 之前失败
#                          missing-entry     删除构建产物，模拟 dist 缺失
#                          identity-mismatch 篡改构建产物，模拟修过的 JS 未同步 pin
#   GITHUB_OUTPUT        存在时追加写入 identity=<内容身份>

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/workflow-identity.sh
source "$script_dir/lib/workflow-identity.sh"

out_dir="$workflow_identity_root/dist/workflow-sidecar"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --out-dir)
            [[ $# -ge 2 ]] || { echo "usage: $0 [--out-dir DIR]" >&2; exit 2; }
            out_dir="$2"
            shift 2
            ;;
        -h|--help)
            sed -n '2,26p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "usage: $0 [--out-dir DIR]" >&2
            exit 2
            ;;
    esac
done

fault="${PERI_SIDECAR_FAULT:-none}"
case "$fault" in
    none|build-failure|missing-entry|identity-mismatch) ;;
    *)
        printf 'unknown PERI_SIDECAR_FAULT: %s\n' "$fault" >&2
        exit 2
        ;;
esac

workflow_identity_load

package_dir="$workflow_identity_root/npm-packages/@peri-workflow"
dist_dir="$package_dir/dist"
built_entry="$dist_dir/peri-workflow.js"

if ! command -v bun >/dev/null 2>&1; then
    echo "bun is required to build the workflow sidecar" >&2
    exit 1
fi
printf 'bun %s\n' "$(bun --version)"

# 依赖按锁文件安装：bundle 内联了 @claude-code-best/workflow-engine，依赖版本漂移会
# 直接改变产物字节，从而改变内容身份。
(cd "$package_dir" && bun install --frozen-lockfile)

if [[ "$fault" == build-failure ]]; then
    echo "injected sidecar build failure (PERI_SIDECAR_FAULT=build-failure)" >&2
    exit 1
fi

(cd "$package_dir" && bun run build)

if [[ "$fault" == missing-entry ]]; then
    rm -f "$built_entry"
fi

if [[ ! -s "$built_entry" ]]; then
    printf 'workflow sidecar entry is missing or empty: %s\n' "$built_entry" >&2
    printf '`bun run build` must emit a non-empty %s\n' "$workflow_entry_rel" >&2
    exit 1
fi

if [[ "$fault" == identity-mismatch ]]; then
    printf '\n' >> "$built_entry"
fi

actual_pin="$(workflow_sha256_file "$built_entry")"
if [[ "$actual_pin" != "$workflow_pin" ]]; then
    printf 'workflow sidecar identity mismatch\n' >&2
    printf '  built   %s\n' "$actual_pin" >&2
    printf '  pinned  %s  (%s::WORKFLOW_ENTRY_SHA256)\n' \
        "$workflow_pin" 'peri-workflow/src/runner/artifact.rs' >&2
    printf 'bundle 变化时必须重新发布并同步 pin；不得跳过该校验\n' >&2
    exit 1
fi

if ! meta_error="$(workflow_metadata_check < "$workflow_package_json" 2>&1)"; then
    printf 'workflow sidecar metadata does not match the Rust constants: %s\n' \
        "$(printf '%s' "$meta_error" | tail -1)" >&2
    exit 1
fi

# 落盘既定布局：消费端按 <可执行文件目录>/peri-resources/workflow/<内容身份>/ 读取
# （peri-workflow/src/runner/artifact.rs）。
sidecar_root="$out_dir/$workflow_sidecar_dir/$workflow_identity"
rm -rf "$sidecar_root"
mkdir -p "$sidecar_root/dist"
cp "$built_entry" "$sidecar_root/dist/peri-workflow.js"
cp "$workflow_package_json" "$sidecar_root/package.json"

if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
    printf 'identity=%s\n' "$workflow_identity" >> "$GITHUB_OUTPUT"
fi

printf 'sidecar %s @ %s (sha256 %s…)\n' \
    "$workflow_name" "$workflow_identity" "${workflow_pin:0:12}"
printf 'layout %s\n' "$sidecar_root"
