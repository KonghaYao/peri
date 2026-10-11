#!/usr/bin/env bash
# 发行归档的 workflow sidecar 内容身份校验（release-agent / pre-release 的发布门）。
#
# 用法: verify-workflow-sidecar.sh <archive> <binary-member>
#   <archive>         peri-<platform>.tar.gz / .zip 或 peri-beta-<platform>.tar.gz
#   <binary-member>   归档根目录内的可执行文件名（peri / peri.exe / peri-beta）
#
# 校验四者一致（缺一即失败，exit 1）：
#   1. 归档内元数据（package.json 身份字段）== Rust 侧常量 + 仓库 package.json；
#   2. 归档内 `dist/peri-workflow.js` 的完整 sha256；
#   3. 目录身份（`peri-resources/workflow/<内容身份>/`，内容身份 = pin 前 12 位）；
#   4. Rust 侧编译期 pin（`peri-workflow/src/runner/artifact.rs::WORKFLOW_ENTRY_SHA256`）。
#
# 本脚本只读归档与仓库文件；不含网络访问。
set -euo pipefail

if [[ $# -ne 2 ]]; then
    printf 'usage: %s <archive> <binary-member>\n' "$0" >&2
    exit 2
fi

archive="$1"
binary_member="$2"
root="$(cd "$(dirname "$0")/.." && pwd)"
artifact_rs="${root}/peri-workflow/src/runner/artifact.rs"
package_json="${root}/npm-packages/@peri-workflow/package.json"
entry_rel="dist/peri-workflow.js"

fail() {
    printf 'FAIL [%s]: %s\n' "${archive##*/}" "$*" >&2
    exit 1
}

[[ -f "$archive" ]] || fail "archive not found"
[[ -f "$artifact_rs" ]] || fail "missing $artifact_rs"
# 仓库 manifest 仅用于核对身份字段口径（不参与字节比对；仓库不读工作树 dist）。
[[ -f "$package_json" ]] || fail "missing $package_json"

# sha256 工具（CI 为 GNU coreutils；macOS 本地为 perl shasum）
if command -v sha256sum >/dev/null 2>&1; then
    sha256_stdin() { sha256sum | cut -d' ' -f1; }
    sha256_file() { sha256sum "$1" | cut -d' ' -f1; }
else
    sha256_stdin() { shasum -a 256 | cut -d' ' -f1; }
    sha256_file() { shasum -a 256 "$1" | cut -d' ' -f1; }
fi

# ─── 从 Rust 侧读取编译期契约（容忍 rustfmt 折行）────────────────────────────
read_str_const() {
    grep -A 2 "^\(pub(super) \)\?const $1: &str =" "$artifact_rs" \
        | grep -oE '"[^"]+"' | head -1 | tr -d '"'
}
read_u32_const() {
    # 注意：类型名本身含数字（u32），必须取 `=` 之后的字面量。
    grep -A 2 "^\(pub(super) \)\?const $1: u32 =" "$artifact_rs" \
        | head -2 | tr '\n' ' ' \
        | sed -E 's/.*=[[:space:]]*([0-9]+).*/\1/'
}

pin="$(read_str_const WORKFLOW_ENTRY_SHA256)"
name="$(read_str_const WORKFLOW_PACKAGE_NAME)"
version="$(read_str_const WORKFLOW_NPM_VERSION)"
build_id="$(read_str_const WORKFLOW_BUILD_ID)"
protocol="$(read_u32_const WORKFLOW_PROTOCOL_VERSION)"
sidecar_dir="$(read_str_const SIDECAR_RELATIVE_DIR)"
[[ "$pin" =~ ^[0-9a-f]{64}$ ]] || fail "WORKFLOW_ENTRY_SHA256 is not a 64-hex digest: '$pin'"
[[ "$name" == @*/* ]] || fail "WORKFLOW_PACKAGE_NAME is not scoped: '$name'"
[[ "$sidecar_dir" == peri-resources/* ]] || fail "unexpected SIDECAR_RELATIVE_DIR: '$sidecar_dir'"
identity="${pin:0:12}"

# ─── 归档读取适配（tar.gz / zip）────────────────────────────────────────────
case "$archive" in
    *.tar.gz) listing="$(tar -tzf "$archive")"
              read_member() { tar -xzOf "$archive" "$1"; } ;;
    *.zip)    listing="$(unzip -Z1 "$archive")"
              read_member() { unzip -p "$archive" "$1"; } ;;
    *)        fail "unsupported archive type (expected .tar.gz or .zip)" ;;
esac

# ─── 布局：恰好一个二进制 + 仅 sidecar 前缀条目 ─────────────────────────────
# 严格计数（存在性检查会放过重复成员：两个 `peri` 的包会让安装端只解出一个二进制而
# 掩盖包体异常）。成员名按字面全行匹配（`peri.exe` 的 `.` 不作正则通配）。
test "$(printf '%s\n' "$listing" | grep -Fxc "$binary_member" || true)" = 1 \
    || fail "archive must contain exactly one '$binary_member' at its root"
unexpected="$(printf '%s\n' "$listing" \
    | grep -vxF "$binary_member" \
    | grep -v '^peri-resources/' || true)"
[[ -z "$unexpected" ]] || fail "unexpected archive entries: $(printf '%s' "$unexpected" | tr '\n' ' ')"

meta_member="${sidecar_dir}/${identity}/package.json"
entry_member="${sidecar_dir}/${identity}/${entry_rel}"
printf '%s\n' "$listing" | grep -qx "$meta_member" \
    || fail "missing sidecar metadata at '$meta_member' (dir identity must equal pin prefix)"
printf '%s\n' "$listing" | grep -qx "$entry_member" \
    || fail "missing sidecar entry at '$entry_member'"

# ─── 内容：归档内入口摘要 == Rust 侧 pin；元数据 == Rust 常量 == 仓库 manifest ──
# 只校验被校验的 artifact 本身与 pin 常量：不读取工作树 dist（dist 不入库，也不作为字节源）。
entry_digest="$(read_member "$entry_member" | sha256_stdin)"
[[ "$entry_digest" == "$pin" ]] \
    || fail "archive entry sha256 ${entry_digest:0:12}… != pinned ${pin:0:12}… (tampered or stale bundle)"

meta_check() {
    python3 -c '
import json, sys
meta = json.load(sys.stdin)
expected = json.loads(open(sys.argv[1]).read())
name, version, build, protocol = sys.argv[2:6]
assert meta["name"] == name == expected["name"], (meta["name"], name, expected["name"])
assert meta["version"] == version == expected["version"], (meta["version"], version)
assert meta["main"] == "dist/peri-workflow.js" == expected["main"], meta["main"]
assert meta["periProtocolVersion"] == int(protocol) == expected["periProtocolVersion"], meta["periProtocolVersion"]
assert meta["periBuildId"] == build == expected["periBuildId"], meta["periBuildId"]
' "$package_json" "$name" "$version" "$build_id" "$protocol"
}
if ! meta_error="$(read_member "$meta_member" | meta_check 2>&1)"; then
    fail "archive metadata does not match the Rust constants / repository manifest: $(printf '%s' "$meta_error" | tail -1)"
fi

printf 'OK [%s]: %s + %s/%s (sha256 %s…)\n' \
    "${archive##*/}" "$binary_member" "$sidecar_dir" "$identity" "${pin:0:12}"
