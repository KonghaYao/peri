#!/usr/bin/env bash
# 物化补丁 crate 并把 [patch.crates-io] 安装到 $CARGO_HOME/config.toml。
#
# 幂等：补丁目录命中时只校验并重写 config，未命中时才下载解压。
# rust-cache 恢复 target/ 时会丢掉 target/peri-*-patches，而 config.toml 指向这些
# 目录，缺失时 cargo metadata 直接报 "patch location ... does not contain packages"；
# 因此缓存恢复前后各执行一次。
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

./scripts/cargo-rmcp-patched.sh --version >/dev/null

# 统一将补丁 crate 内所有文件的 mtime 固化为 2020-01-01T00:00:00Z，
# 避免任何情况下 mtime 新于 rust-cache 恢复的 .fingerprint 触发级联重编。
python3 - <<'PY'
import os, pathlib
ts = 1577836800  # 2020-01-01T00:00:00Z
for root in (
    "target/peri-rmcp-patches",
):
    p = pathlib.Path(root)
    if not p.exists():
        continue
    for dirpath, dirnames, filenames in os.walk(p):
        for name in filenames:
            os.utime(os.path.join(dirpath, name), (ts, ts))
        for name in dirnames:
            os.utime(os.path.join(dirpath, name), (ts, ts))
PY

# 将 [patch.crates-io] 一次性安装到 $CARGO_HOME/config.toml（兼容 Windows Git Bash 路径）
cargo_home="$(cygpath -u "${CARGO_HOME:-$HOME/.cargo}" 2>/dev/null || printf '%s' "${CARGO_HOME:-$HOME/.cargo}")"
mkdir -p "$cargo_home"
# 历史缓存里可能残留多个 patch-sha 目录；glob 保序取第一个，且不用 ls|head 以免
# pipefail 把 SIGPIPE 当成失败。
patch_cfg=""
for candidate in target/peri-rmcp-patches/*/cargo-config.toml; do
    if [[ -f "$candidate" ]]; then
        patch_cfg="$candidate"
        break
    fi
done
if [[ -z "$patch_cfg" ]]; then
    echo "no cargo-config.toml under target/peri-rmcp-patches" >&2
    exit 1
fi
cp "$patch_cfg" "$cargo_home/config.toml"
