#!/usr/bin/env bash
# Workflow sidecar 身份契约的单一事实源。
#
# 从 Rust 编译期常量（`peri-workflow/src/runner/artifact.rs`）读取内容摘要 pin、
# 包名、npm 版本、构建标识、协议版本与 sidecar 目录，供两个调用方共用：
#
#   - scripts/build-workflow-sidecar.sh  发布/预发布的生成前置（构建 + 校验）
#   - scripts/verify-workflow-sidecar.sh 发布门（校验已归档资产）
#
# 生成规则与校验规则必须读到同一份 pin 与同一套元数据口径；拆成两份实现就会
# 出现「生成端通过、校验端失败」且无人知道哪边是权威的窗口。
#
# 本文件只被 source，不直接执行。调用方须自行设置 `set -euo pipefail`。
# 校验函数失败时返回非零并把原因写到 stderr，由调用方决定措辞。

workflow_identity_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflow_artifact_rs="${workflow_identity_root}/peri-workflow/src/runner/artifact.rs"
workflow_package_json="${workflow_identity_root}/npm-packages/@peri-workflow/package.json"
workflow_entry_rel="dist/peri-workflow.js"

# sha256 工具（CI 为 GNU coreutils；macOS 本地为 perl shasum）
if command -v sha256sum >/dev/null 2>&1; then
    workflow_sha256_stdin() { sha256sum | cut -d' ' -f1; }
    workflow_sha256_file() { sha256sum "$1" | cut -d' ' -f1; }
else
    workflow_sha256_stdin() { shasum -a 256 | cut -d' ' -f1; }
    workflow_sha256_file() { shasum -a 256 "$1" | cut -d' ' -f1; }
fi

# ─── 从 Rust 侧读取编译期契约（容忍 rustfmt 折行）────────────────────────────
workflow_read_str_const() {
    grep -A 2 "^\(pub(super) \)\?const $1: &str =" "$workflow_artifact_rs" \
        | grep -oE '"[^"]+"' | head -1 | tr -d '"'
}

workflow_read_u32_const() {
    # 注意：类型名本身含数字（u32），必须取 `=` 之后的字面量。
    grep -A 2 "^\(pub(super) \)\?const $1: u32 =" "$workflow_artifact_rs" \
        | head -2 | tr '\n' ' ' \
        | sed -E 's/.*=[[:space:]]*([0-9]+).*/\1/'
}

# 读取并校验契约本身（pin 是 64 位十六进制、包名带 scope、sidecar 目录在
# `peri-resources/` 下）。成功后设置以下变量：
#   workflow_pin / workflow_name / workflow_version / workflow_build_id
#   workflow_protocol / workflow_sidecar_dir / workflow_identity
workflow_identity_load() {
    [[ -f "$workflow_artifact_rs" ]] \
        || { printf 'missing %s\n' "$workflow_artifact_rs" >&2; return 1; }
    [[ -f "$workflow_package_json" ]] \
        || { printf 'missing %s\n' "$workflow_package_json" >&2; return 1; }

    workflow_pin="$(workflow_read_str_const WORKFLOW_ENTRY_SHA256)"
    workflow_name="$(workflow_read_str_const WORKFLOW_PACKAGE_NAME)"
    workflow_version="$(workflow_read_str_const WORKFLOW_NPM_VERSION)"
    workflow_build_id="$(workflow_read_str_const WORKFLOW_BUILD_ID)"
    workflow_protocol="$(workflow_read_u32_const WORKFLOW_PROTOCOL_VERSION)"
    workflow_sidecar_dir="$(workflow_read_str_const SIDECAR_RELATIVE_DIR)"

    [[ "$workflow_pin" =~ ^[0-9a-f]{64}$ ]] \
        || { printf "WORKFLOW_ENTRY_SHA256 is not a 64-hex digest: '%s'\n" "$workflow_pin" >&2; return 1; }
    [[ "$workflow_name" == @*/* ]] \
        || { printf "WORKFLOW_PACKAGE_NAME is not scoped: '%s'\n" "$workflow_name" >&2; return 1; }
    [[ "$workflow_sidecar_dir" == peri-resources/* ]] \
        || { printf "unexpected SIDECAR_RELATIVE_DIR: '%s'\n" "$workflow_sidecar_dir" >&2; return 1; }

    # 内容身份 = pin 前 12 位；目录名与 Rust 消费端约定一致。
    workflow_identity="${workflow_pin:0:12}"
}

# 从 stdin 读取一个 package.json，核对身份字段 == Rust 常量 == 仓库 manifest。
# 与发布门使用同一份 python 断言，避免两处口径漂移。
workflow_metadata_check() {
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
' "$workflow_package_json" "$workflow_name" "$workflow_version" "$workflow_build_id" "$workflow_protocol"
}
