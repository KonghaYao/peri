#!/usr/bin/env bash
# Swatinem/rust-cache 用裸 `cargo metadata` 收集依赖集；本仓库 `wasm-fetch`
# 特性只存在于 patches/ 的本地补丁中，裸 cargo 解析失败会让保存阶段误删整个
# target（缓存退化成 ~11MB）。只把 metadata 转交带补丁配置的入口，其余
# rustc/rustup 命令原样执行。
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ "${1:-}" == "cargo" && "${2:-}" == "metadata" ]]; then
    shift
    exec "$repo_root/scripts/cargo-rmcp-patched.sh" "$@"
fi

exec "$@"
