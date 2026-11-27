#!/usr/bin/env bash
# Run Cargo against the pinned rmcp release plus the repository's patch.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
patch_file="$repo_root/patches/rmcp-3.9.0-task-subscriptions.patch"
crate_url="https://static.crates.io/crates/rmcp/rmcp-3.9.0.crate"
crate_sha256="d24a7e92f4bcaa36b97fa93c4fa7ca8f3e669eced949345b3462dba333c7f909"

if [[ $# -eq 0 ]]; then
    echo "usage: $0 <cargo subcommand> [arguments...]" >&2
    exit 2
fi

sha256_file() {
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        sha256sum "$1" | awk '{print $1}'
    fi
}

patch_sha256="$(sha256_file "$patch_file")"
cache_dir="$repo_root/target/peri-rmcp-patches/$patch_sha256"
crate_dir="$cache_dir/rmcp-3.9.0"
config_file="$cache_dir/cargo-config.toml"

if [[ ! -f "$crate_dir/.peri-patch-sha256" ]]; then
    mkdir -p "$cache_dir"
    staging_dir="$(mktemp -d "$cache_dir/staging.XXXXXXXX")"
    trap 'rm -rf "$staging_dir"' EXIT
    curl --fail --location --silent --show-error "$crate_url" -o "$staging_dir/rmcp.crate"
    actual_sha256="$(sha256_file "$staging_dir/rmcp.crate")"
    if [[ "$actual_sha256" != "$crate_sha256" ]]; then
        echo "rmcp 3.9.0 archive checksum mismatch" >&2
        exit 1
    fi
    tar -xzf "$staging_dir/rmcp.crate" -C "$staging_dir"
    if command -v patch >/dev/null 2>&1; then
        patch --dry-run --batch -p1 -d "$staging_dir/rmcp-3.9.0" < "$patch_file" >/dev/null
        patch --batch -p1 -d "$staging_dir/rmcp-3.9.0" < "$patch_file"
    else
        # Git Bash may not ship `patch`. A nested temporary repository makes
        # git apply target the extracted crate instead of the Peri repository.
        git init -q "$staging_dir/rmcp-3.9.0"
        git -C "$staging_dir/rmcp-3.9.0" apply --check "$patch_file"
        git -C "$staging_dir/rmcp-3.9.0" apply "$patch_file"
    fi
    printf '%s\n' "$patch_sha256" > "$staging_dir/rmcp-3.9.0/.peri-patch-sha256"
    if [[ -f "$crate_dir/.peri-patch-sha256" ]]; then
        : # Another invocation finished first; reuse its cache.
    else
        # Replace interrupted or partial caches instead of leaving them in place.
        rm -rf "$crate_dir"
        mv "$staging_dir/rmcp-3.9.0" "$crate_dir"
    fi
    rm -rf "$staging_dir"
    trap - EXIT
fi

if [[ "$(cat "$crate_dir/.peri-patch-sha256")" != "$patch_sha256" ]]; then
    echo "patched rmcp cache is incomplete; move $cache_dir aside and retry" >&2
    exit 1
fi
if command -v patch >/dev/null 2>&1; then
    patch_present=$(patch --dry-run --batch -R -p1 -d "$crate_dir" < "$patch_file" >/dev/null 2>&1 && echo yes || echo no)
else
    patch_present=$(git -C "$crate_dir" apply --reverse --check "$patch_file" >/dev/null 2>&1 && echo yes || echo no)
fi
if [[ "$patch_present" != yes ]]; then
    echo "patched rmcp cache does not contain the expected patch; move $cache_dir aside and retry" >&2
    exit 1
fi

python_bin=""
# `command -v` 可能命中目录，也可能命中未配置版本的 mise shim；两者都不可执行。
# 逐个探测，优先 python3，最后回退系统解释器。
for candidate in python3 python /usr/bin/python3; do
    path="$(command -v "$candidate" 2>/dev/null || true)"
    if [[ -n "$path" && -f "$path" && -x "$path" ]] &&
       "$candidate" -c 'import sys' >/dev/null 2>&1; then
        python_bin="$candidate"
        break
    fi
done
if [[ -z "$python_bin" ]]; then
    echo "python3 is required to generate the patched Cargo config" >&2
    exit 1
fi
cargo_crate_dir="$crate_dir"
cargo_config_file="$config_file"
if command -v cygpath >/dev/null 2>&1; then
    cargo_crate_dir="$(cygpath -w "$crate_dir")"
    # Git Bash launches a native cargo.exe, which needs the config path in the
    # same Windows form as the patched crate paths inside that config.
    cargo_config_file="$(cygpath -w "$config_file")"
fi
"$python_bin" - "$cargo_crate_dir" "$config_file" <<'PY'
import json
import pathlib
import sys

pathlib.Path(sys.argv[2]).write_text(
    '[patch.crates-io]\n'
    + 'rmcp = { path = ' + json.dumps(sys.argv[1]) + ' }\n'
)
PY

# Keep the caller's directory: dev.sh uses it as the TUI workspace.
if [[ "$1" == "--cross" ]]; then
    shift
    # cross 0.2.5 parses its own argv and takes the first non-flag token as the
    # subcommand. `--config <path>` is not a cross flag, so the path itself
    # became that token: the subcommand degraded to `Other`, cross silently
    # skipped the container, and the host fallback could not link non-host
    # targets. Pass the config as a single trailing `--config=<path>` token so
    # the subcommand stays the first positional argument, and mirror the patch
    # cache into the container at the same absolute paths so the paths inside
    # the config resolve there as well.
    # `--config=` only reaches the in-container cargo: cross also runs one
    # host-side `cargo metadata` before building, without it. Callers that
    # build `--locked` must therefore expose the same patch through cargo's
    # user-level config chain ($CARGO_HOME/config.toml); see the Linux release
    # job.
    export CROSS_CONTAINER_OPTS="-v \"${repo_root}/target:${repo_root}/target\" ${CROSS_CONTAINER_OPTS:-}"
    exec cross "$@" "--config=$cargo_config_file"
fi
if [[ "$1" == "clippy" ]]; then
    shift
    # cargo-clippy is an external subcommand: it does not inherit Cargo's
    # leading --config flag, so pass the patch config to clippy itself.
    exec cargo clippy --config "$cargo_config_file" "$@"
fi
if [[ "$1" == "nextest" ]]; then
    shift
    # `cargo nextest` is an external subcommand with the same problem as clippy:
    # the leading `cargo --config` never reaches the `cargo metadata` / build
    # invocations that cargo-nextest spawns internally. nextest 0.9.35+ documents
    # that its own `--config` is handed through to Cargo, so pass the patch
    # config explicitly after the nextest subcommand.
    #
    # Only `run` and `list` take `--config`; other subcommands (show-config,
    # archive, ...) are forwarded untouched so nextest keeps its own argv.
    subcommand="${1:-}"
    case "$subcommand" in
        run|list)
            shift
            exec cargo nextest "$subcommand" --config "$cargo_config_file" "$@"
            ;;
        "")
            exec cargo nextest
            ;;
        *)
            exec cargo nextest "$subcommand" "$@"
            ;;
    esac
fi
exec cargo --config "$cargo_config_file" "$@"
