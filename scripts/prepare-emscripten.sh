#!/usr/bin/env bash
# Backport the Emscripten APIs required by the Cloudflare Tokio event loop.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# 补丁身份：四个补丁文件按固定顺序拼接后的 sha256。它同时是 stamp 文件的内容
# 与「补丁已应用」的期望值，是补丁集的唯一事实源。
# 该计算只依赖仓库内的补丁文件，故放在工具链解析之前 —— 预发布工作流需要
# 在 mise 安装 emsdk 之前（此时还没有 emcc）把它作为 mise 工具缓存 key 的一部分，
# 见 --print-patch-identity；补丁集一变，缓存 key 就变，不会复用一个按旧补丁集
# 打过补丁的工具链。
base_expected="$(cat "$repo_root/patches/emscripten/epoll-listeners.patch" "$repo_root/patches/emscripten/noderawsockets-dns.patch" | shasum -a 256 | cut -d ' ' -f 1)"
legacy_base_expected="$(shasum -a 256 "$repo_root/patches/emscripten/epoll-listeners.patch" "$repo_root/patches/emscripten/noderawsockets-dns.patch" | shasum -a 256 | cut -d ' ' -f 1)"
previous_expected="$(cat "$repo_root/patches/emscripten/epoll-listeners.patch" "$repo_root/patches/emscripten/noderawsockets-dns.patch" "$repo_root/patches/emscripten/noderawsockets-bun.patch" | shasum -a 256 | cut -d ' ' -f 1)"
expected="$(cat "$repo_root/patches/emscripten/epoll-listeners.patch" "$repo_root/patches/emscripten/noderawsockets-dns.patch" "$repo_root/patches/emscripten/noderawsockets-bun.patch" "$repo_root/patches/emscripten/workers-module-url.patch" | shasum -a 256 | cut -d ' ' -f 1)"

if [[ "${1:-}" == "--print-patch-identity" ]]; then
    printf '%s\n' "$expected"
    exit 0
fi

if [[ -n "${EMSCRIPTEN:-}" ]]; then
    frontend="$EMSCRIPTEN"
elif [[ -n "${EMSDK:-}" ]]; then
    frontend="$EMSDK/upstream/emscripten"
else
    frontend="$(cd "$(dirname "$(command -v emcc)")" && pwd)"
fi
frontend="$(cd "$frontend" && pwd)"

if [[ ! -f "$frontend/emscripten-version.txt" ]] ||
   [[ "$(tr -d '"[:space:]' < "$frontend/emscripten-version.txt")" != 6.0.10 ]]; then
    echo "Cloudflare Tokio patches require Emscripten 6.0.10: $frontend" >&2
    exit 1
fi

# emsdk's Git checkout may be rooted above `upstream/emscripten`; git apply
# otherwise silently sees zero matching paths and reports success.
git_root="$(git -C "$frontend" rev-parse --show-toplevel 2>/dev/null || printf '%s' "$frontend")"
apply_directory=()
if [[ "$git_root" != "$frontend" ]]; then
    apply_directory=("--directory=${frontend#"$git_root"/}")
fi
# Bash 3.2 with `set -u` exits on "${empty[@]}", so the directory
# argument is passed only when emsdk's Git root is above the frontend.
apply_patch() {
    if [[ ${#apply_directory[@]} -gt 0 ]]; then
        git -C "$git_root" apply "${apply_directory[@]}" "$@"
    else
        git -C "$git_root" apply "$@"
    fi
}

lock="$frontend/.peri-patch-lock"
locked=false
for ((attempt = 0; attempt < 100; attempt++)); do
    if mkdir "$lock" 2>/dev/null; then
        locked=true
        break
    fi
    sleep 0.1
done
if [[ "$locked" != true ]]; then
    echo "Could not lock Emscripten frontend: $frontend" >&2
    exit 1
fi
trap 'rmdir "$lock"' EXIT

stamp="$frontend/.peri-cloudflare-emscripten-patches"
verify_epoll() {
    grep -q 'emscripten_epoll_add_listener:' "$frontend/src/lib/libepoll.js" &&
    grep -q 'emscripten_epoll_remove_listener:' "$frontend/src/lib/libepoll.js" &&
    grep -q 'emscripten_epoll_add_listener__sig:' "$frontend/src/lib/libsigs.js" &&
    grep -q 'emscripten_epoll_add_listener' "$frontend/system/include/emscripten/epoll.h" &&
    grep -q 'sock.stream.node.notifyListeners' "$frontend/src/lib/libsockfs_node.js"
}
verify_dns() {
    grep -q 'emscripten_dns_lookup_async:' "$frontend/src/lib/libsockfs.js" &&
    grep -q 'emscripten_dns_lookup_result:' "$frontend/src/lib/libsockfs.js" &&
    grep -q 'emscripten_dns_lookup_async__sig:' "$frontend/src/lib/libsigs.js" &&
    grep -q 'emscripten_dns_lookup_async' "$frontend/system/include/emscripten/emscripten.h"
}
verify_workers_module_url() {
    grep -Fq "createRequire(import.meta.url ?? Module['mainScriptUrlOrBlob'])" "$frontend/src/shell.js" &&
    grep -Fq "var _scriptName = import.meta.url ?? Module['mainScriptUrlOrBlob'];" "$frontend/src/shell.js"
}
if [[ -f "$stamp" ]]; then
    installed="$(cat "$stamp")"
    if [[ "$installed" != "$expected" && "$installed" != "$previous_expected" && "$installed" != "$base_expected" && "$installed" != "$legacy_base_expected" ]]; then
        echo "Emscripten patch files changed; reinstall the unpatched 6.0.10 frontend" >&2
        exit 1
    fi
    if ! verify_epoll || ! verify_dns; then
        echo "Emscripten patch stamp exists but backports are missing or incomplete: $frontend" >&2
        exit 1
    fi
    if [[ "$installed" == "$expected" ]]; then
        if ! grep -q "typeof Bun === 'undefined'" "$frontend/src/lib/libsockfs_node.js" ||
           ! apply_patch --reverse --check "$repo_root/patches/emscripten/noderawsockets-bun.patch" >/dev/null 2>&1 ||
           ! verify_workers_module_url ||
           ! apply_patch --reverse --check "$repo_root/patches/emscripten/workers-module-url.patch" >/dev/null 2>&1; then
            echo "Emscripten patch stamp exists but a patch is missing or incomplete: $frontend" >&2
            exit 1
        fi
        exit 0
    fi
fi

for name in epoll-listeners noderawsockets-dns; do
    patch="$repo_root/patches/emscripten/$name.patch"
    case "$name" in
        epoll-listeners) marker='emscripten_epoll_add_listener:'; source="$frontend/src/lib/libepoll.js"; verify=verify_epoll ;;
        noderawsockets-dns) marker='emscripten_dns_lookup_async:'; source="$frontend/src/lib/libsockfs.js"; verify=verify_dns ;;
    esac
    if grep -q "$marker" "$source"; then
        if ! "$verify"; then
            echo "Emscripten patch $name is partially applied: $frontend" >&2
            exit 1
        fi
        continue
    fi
    if ! apply_patch --check "$patch"; then
        echo "Emscripten patch $name does not match $frontend; reinstall 6.0.10" >&2
        exit 1
    fi
    apply_patch "$patch"
    if ! "$verify"; then
        echo "Emscripten patch $name did not install all expected symbols: $frontend" >&2
        exit 1
    fi
    # The patch adds a public header. Force emcc to reinstall the sysroot headers.
    rm -f "$frontend/cache/sysroot_install.stamp"
    echo "Applied Cloudflare Emscripten patch: $name" >&2
done
patch="$repo_root/patches/emscripten/noderawsockets-bun.patch"
if apply_patch --reverse --check "$patch" >/dev/null 2>&1; then
    : # Already installed by a previous interrupted run.
elif apply_patch --check "$patch" >/dev/null 2>&1; then
    apply_patch "$patch"
    echo "Applied Bun NODERAWSOCKETS patch" >&2
else
    echo "Emscripten Bun patch does not match $frontend; reinstall 6.0.10" >&2
    exit 1
fi
patch="$repo_root/patches/emscripten/workers-module-url.patch"
if apply_patch --reverse --check "$patch" >/dev/null 2>&1; then
    : # Already installed by a previous interrupted run.
elif apply_patch --check "$patch" >/dev/null 2>&1; then
    apply_patch "$patch"
    echo "Applied Workers module URL patch" >&2
else
    echo "Emscripten Workers module URL patch does not match $frontend; reinstall 6.0.10" >&2
    exit 1
fi
if ! verify_workers_module_url; then
    echo "Emscripten Workers module URL patch is incomplete: $frontend" >&2
    exit 1
fi
printf '%s\n' "$expected" > "$stamp"
