#!/bin/bash
set -e

repo_root="$(cd "$(dirname "$0")" && pwd)"

# 打开 TUI 组件 storybook；Cargo manifest 始终指向仓库根目录。
exec "$repo_root/scripts/cargo-rmcp-patched.sh" run --locked --manifest-path "$repo_root/Cargo.toml" -p peri-tui-components --bin storybook -- "$@"
