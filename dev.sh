#!/bin/bash
set -e

repo_root="$(cd "$(dirname "$0")" && pwd)"
cwd="$repo_root"
args=()

for arg in "$@"; do
    case "$arg" in
        --cwd=*)
            cwd="${arg#--cwd=}"
            ;;
        *)
            args+=("$arg")
            ;;
    esac
done

if [[ "$cwd" != /* ]]; then
    cwd="$repo_root/$cwd"
fi

if [[ ! -d "$cwd" ]]; then
    printf '工作目录不存在: %s\n' "$cwd" >&2
    exit 1
fi

# 加载仓库根目录的 .env
set -a; source "$repo_root/.env"; set +a

# 确保日志目录存在
mkdir -p "$(dirname "$RUST_LOG_FILE")"

# 在指定工作区启动 TUI；Cargo manifest 始终指向仓库根目录。
cd "$cwd"
cargo run --manifest-path "$repo_root/Cargo.toml" -p peri-tui -- "${args[@]}"
