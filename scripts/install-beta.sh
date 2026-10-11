#!/bin/bash
set -euo pipefail
export LC_ALL=C

temporary_dir=""
staged_binary=""

cleanup() {
    if [[ -n "$temporary_dir" ]]; then
        rm -rf "$temporary_dir"
    fi
    if [[ -n "$staged_binary" ]]; then
        rm -f "$staged_binary"
    fi
}

fail() {
    printf '[ERROR] %s\n' "$*" >&2
    exit 1
}

detect_platform() {
    local system architecture
    if [[ -n "${PERI_BETA_INSTALL_PLATFORM:-}" ]]; then
        printf '%s\n' "$PERI_BETA_INSTALL_PLATFORM"
        return
    fi
    case "$(uname -s)" in
        Darwin) system=macos ;;
        Linux) system=linux ;;
        *) fail "Only macOS and Linux are supported." ;;
    esac
    case "$(uname -m)" in
        x86_64|amd64) architecture=x86_64 ;;
        aarch64|arm64) architecture=aarch64 ;;
        *) fail "Only x86_64 and ARM64 are supported." ;;
    esac
    printf '%s-%s\n' "$system" "$architecture"
}

download() {
    local url="$1" destination="$2"
    curl --fail --silent --show-error --location --retry 3 \
        --connect-timeout 30 --max-time 300 --proto '=https' --proto-redir '=https' \
        "$url" --output "$destination"
}

main() {
    local platform archive base_url install_dir expected_digest expected_name extra_field actual_digest
    local checksum_tool
    for dependency in curl tar mktemp install; do
        command -v "$dependency" >/dev/null 2>&1 || fail "Missing dependency: $dependency"
    done
    if command -v sha256sum >/dev/null 2>&1; then
        checksum_tool=sha256sum
    elif command -v shasum >/dev/null 2>&1; then
        checksum_tool=shasum
    else
        fail "Install sha256sum or shasum to verify downloads."
    fi

    platform=$(detect_platform)
    case "$platform" in
        macos-x86_64|macos-aarch64|linux-x86_64|linux-aarch64) ;;
        *) fail "Unsupported beta platform: $platform" ;;
    esac
    install_dir="${PERI_BETA_INSTALL_DIR:-$HOME/.local/bin}"
    archive="peri-beta-${platform}.tar.gz"
    base_url="https://github.com/konghayao/peri/releases/download/peri-beta"
    temporary_dir=$(mktemp -d)
    printf '[INFO] Downloading peri-beta for %s...\n' "$platform"
    download "$base_url/$archive" "$temporary_dir/$archive" || fail "Beta download failed. Check that the peri-beta prerelease has been published."
    download "$base_url/$archive.sha256" "$temporary_dir/$archive.sha256" || fail "Checksum download failed."
    read -r expected_digest expected_name extra_field < "$temporary_dir/$archive.sha256" || fail "Invalid checksum file."
    [[ "$expected_digest" =~ ^[[:xdigit:]]{64}$ && "$expected_name" == "$archive" && -z "$extra_field" ]] || fail "Invalid checksum entry."
    if [[ "$checksum_tool" == sha256sum ]]; then
        actual_digest=$(sha256sum "$temporary_dir/$archive")
    else
        actual_digest=$(shasum -a 256 "$temporary_dir/$archive")
    fi
    [[ "${actual_digest%% *}" == "$expected_digest" ]] || fail "Checksum mismatch; retry after the beta publication finishes."
    [[ "$(tar -tzf "$temporary_dir/$archive")" == peri-beta ]] || fail "Archive must contain only peri-beta."
    tar -xzf "$temporary_dir/$archive" -C "$temporary_dir"
    [[ -f "$temporary_dir/peri-beta" && ! -L "$temporary_dir/peri-beta" ]] || fail "Invalid peri-beta binary."
    chmod 755 "$temporary_dir/peri-beta"
    "$temporary_dir/peri-beta" --version || fail "The beta binary cannot run on this system."
    mkdir -p "$install_dir"
    [[ ! -d "$install_dir/peri-beta" ]] || fail "Installation target is a directory: $install_dir/peri-beta"
    staged_binary=$(mktemp "$install_dir/.peri-beta.XXXXXX")
    install -m 755 "$temporary_dir/peri-beta" "$staged_binary"
    mv -f "$staged_binary" "$install_dir/peri-beta"
    staged_binary=""
    printf '[INFO] Installed: %s/peri-beta\n' "$install_dir"
    printf '[INFO] Update beta by running this installer again; do not use peri-beta update.\n'
    case ":${PATH:-}:" in
        *":$install_dir:"*) ;;
        *) printf '[INFO] Add this directory to your shell PATH: %s\n' "$install_dir" ;;
    esac
}

trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
main "$@"
