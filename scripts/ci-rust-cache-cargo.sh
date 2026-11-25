#!/usr/bin/env bash
# Swatinem/rust-cache 用裸 `cargo metadata` 收集依赖集；本仓库 `wasm-fetch`
# 特性只存在于 patches/ 的本地补丁中，裸 cargo 解析失败会让保存阶段误删整个
# target（缓存退化成 ~11MB）。只把 metadata 转交带补丁配置的入口，其余
# rustc/rustup 命令原样执行。
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ "${1:-}" == "cargo" && "${2:-}" == "metadata" ]]; then
    # 首次调用会解包并打补丁，GNU patch 的 "patching file …" 会写到 stdout，
    # 污染 rust-cache 对 metadata JSON 的解析（成员清单为空，key 少算 manifest）。
    # 先静默预热一次，正式调用的 stdout 只剩 JSON。
    "$repo_root/scripts/cargo-rmcp-patched.sh" --version >/dev/null 2>&1 || true
    shift
    exec "$repo_root/scripts/cargo-rmcp-patched.sh" "$@"
fi

exec "$@"
