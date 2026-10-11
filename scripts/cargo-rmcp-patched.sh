#!/usr/bin/env bash
# Run Cargo against the pinned rmcp release plus the repository's patch.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
patch_file="$repo_root/patches/rmcp-3.5.0-task-subscriptions.patch"
crate_url="https://static.crates.io/crates/rmcp/rmcp-3.5.0.crate"
crate_sha256="fae7019994ae0fe4ada40b732f798f3ff26f0f04facb1477f1bf37eb4f18a2d3"

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
crate_dir="$cache_dir/rmcp-3.5.0"
config_file="$cache_dir/cargo-config.toml"

if [[ ! -f "$crate_dir/.peri-patch-sha256" ]]; then
    mkdir -p "$cache_dir"
    staging_dir="$(mktemp -d "$cache_dir/staging.XXXXXXXX")"
    trap 'rm -rf "$staging_dir"' EXIT
    curl --fail --location --silent --show-error "$crate_url" -o "$staging_dir/rmcp.crate"
    actual_sha256="$(sha256_file "$staging_dir/rmcp.crate")"
    if [[ "$actual_sha256" != "$crate_sha256" ]]; then
        echo "rmcp 3.5.0 archive checksum mismatch" >&2
        exit 1
    fi
    tar -xzf "$staging_dir/rmcp.crate" -C "$staging_dir"
    if command -v patch >/dev/null 2>&1; then
        patch --dry-run --batch -p1 -d "$staging_dir/rmcp-3.5.0" < "$patch_file" >/dev/null
        patch --batch -p1 -d "$staging_dir/rmcp-3.5.0" < "$patch_file"
    else
        # Git Bash may not ship `patch`. A nested temporary repository makes
        # git apply target the extracted crate instead of the Peri repository.
        git init -q "$staging_dir/rmcp-3.5.0"
        git -C "$staging_dir/rmcp-3.5.0" apply --check "$patch_file"
        git -C "$staging_dir/rmcp-3.5.0" apply "$patch_file"
    fi
    printf '%s\n' "$patch_sha256" > "$staging_dir/rmcp-3.5.0/.peri-patch-sha256"
    if [[ -f "$crate_dir/.peri-patch-sha256" ]]; then
        : # Another invocation finished first; reuse its cache.
    else
        # Replace interrupted or partial caches instead of leaving them in place.
        rm -rf "$crate_dir"
        mv "$staging_dir/rmcp-3.5.0" "$crate_dir"
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

# Hyper's default DNS resolver uses spawn_blocking, unavailable on Emscripten.
# The target-specific patch routes hostname lookups through Tokio's async DNS fd.
hyper_patch_file="$repo_root/patches/hyper-util-0.1.21-emscripten-dns.patch"
hyper_patch_sha256="$(sha256_file "$hyper_patch_file")"
hyper_cache_dir="$repo_root/target/peri-hyper-util-patches/$hyper_patch_sha256"
hyper_crate_dir="$hyper_cache_dir/hyper-util-0.1.21"
if [[ ! -f "$hyper_crate_dir/.peri-patch-sha256" ]]; then
    mkdir -p "$hyper_cache_dir"
    staging_dir="$(mktemp -d "$hyper_cache_dir/staging.XXXXXXXX")"
    trap 'rm -rf "$staging_dir"' EXIT
    curl --fail --location --silent --show-error \
        "https://static.crates.io/crates/hyper-util/hyper-util-0.1.21.crate" \
        -o "$staging_dir/hyper-util.crate"
    actual_sha256="$(sha256_file "$staging_dir/hyper-util.crate")"
    if [[ "$actual_sha256" != "ddc03d96684f9226b8a787cdb71488417b53ab5ea8fdb1dac946cb9431cc8bff" ]]; then
        echo "hyper-util 0.1.21 archive checksum mismatch" >&2
        exit 1
    fi
    tar -xzf "$staging_dir/hyper-util.crate" -C "$staging_dir"
    if command -v patch >/dev/null 2>&1; then
        patch --dry-run --batch -p1 -d "$staging_dir/hyper-util-0.1.21" < "$hyper_patch_file" >/dev/null
        patch --batch -p1 -d "$staging_dir/hyper-util-0.1.21" < "$hyper_patch_file"
    else
        git init -q "$staging_dir/hyper-util-0.1.21"
        git -C "$staging_dir/hyper-util-0.1.21" apply --check "$hyper_patch_file"
        git -C "$staging_dir/hyper-util-0.1.21" apply "$hyper_patch_file"
    fi
    printf '%s\n' "$hyper_patch_sha256" > "$staging_dir/hyper-util-0.1.21/.peri-patch-sha256"
    if [[ -f "$hyper_crate_dir/.peri-patch-sha256" ]]; then
        : # Another invocation finished first; reuse its cache.
    else
        # Replace interrupted or partial caches instead of leaving them in place.
        rm -rf "$hyper_crate_dir"
        mv "$staging_dir/hyper-util-0.1.21" "$hyper_crate_dir"
    fi
    rm -rf "$staging_dir"
    trap - EXIT
fi
if [[ "$(cat "$hyper_crate_dir/.peri-patch-sha256")" != "$hyper_patch_sha256" ]]; then
    echo "patched hyper-util cache is incomplete; move $hyper_cache_dir aside and retry" >&2
    exit 1
fi
if command -v patch >/dev/null 2>&1; then
    patch_present=$(patch --dry-run --batch -R -p1 -d "$hyper_crate_dir" < "$hyper_patch_file" >/dev/null 2>&1 && echo yes || echo no)
else
    patch_present=$(git -C "$hyper_crate_dir" apply --reverse --check "$hyper_patch_file" >/dev/null 2>&1 && echo yes || echo no)
fi
if [[ "$patch_present" != yes ]]; then
    echo "patched hyper-util cache does not contain the expected patch; move $hyper_cache_dir aside and retry" >&2
    exit 1
fi

python_bin="python"
if ! command -v "$python_bin" >/dev/null 2>&1; then
    python_bin="python3"
fi
cargo_crate_dir="$crate_dir"
hyper_cargo_crate_dir="$hyper_crate_dir"
cargo_config_file="$config_file"
if command -v cygpath >/dev/null 2>&1; then
    cargo_crate_dir="$(cygpath -w "$crate_dir")"
    hyper_cargo_crate_dir="$(cygpath -w "$hyper_crate_dir")"
    # Git Bash launches a native cargo.exe, which needs the config path in the
    # same Windows form as the patched crate paths inside that config.
    cargo_config_file="$(cygpath -w "$config_file")"
fi
"$python_bin" - "$cargo_crate_dir" "$hyper_cargo_crate_dir" "$config_file" <<'PY'
import json
import pathlib
import sys

pathlib.Path(sys.argv[3]).write_text(
    '[patch.crates-io]\n'
    + 'rmcp = { path = ' + json.dumps(sys.argv[1]) + ' }\n'
    + 'hyper-util = { path = ' + json.dumps(sys.argv[2]) + ' }\n'
    + 'mio = { git = "https://github.com/guybedford/mio", tag = "1.2.3-cf.emscripten" }\n'
    + 'tokio = { git = "https://github.com/guybedford/tokio", tag = "1.53.1-cf.emscripten" }\n'
)
PY

# Keep the caller's directory: dev.sh uses it as the TUI workspace.
if [[ "$1" == "--cross" ]]; then
    shift
    exec cross --config "$cargo_config_file" "$@"
fi
if [[ "$1" == "clippy" ]]; then
    shift
    # cargo-clippy is an external subcommand: it does not inherit Cargo's
    # leading --config flag, so pass the patch config to clippy itself.
    exec cargo clippy --config "$cargo_config_file" "$@"
fi
exec cargo --config "$cargo_config_file" "$@"
