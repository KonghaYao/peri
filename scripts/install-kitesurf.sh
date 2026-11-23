#!/bin/bash
set -euo pipefail
export LC_ALL=C

# Kitesurf Install Script
# Usage: curl -fsSL https://raw.githubusercontent.com/konghayao/peri/main/scripts/install-kitesurf.sh | bash
#
# Options:
#   KITESURF_INSTALL_VERSION   Release version (e.g. 0.5.4 or v0.5.4); empty = latest.
#                              A pinned version skips the GitHub API entirely.
#   KITESURF_INSTALL_DIR       Install root (default: $HOME/.kitesurf)
#   GITHUB_PROXY               GitHub download proxy prefix (replaces https://github.com)
#   GITHUB_API_PROXY           GitHub API proxy prefix (replaces https://api.github.com)
#   GITHUB_TOKEN               GitHub token for the API request (rate limiting)
#   KITESURF_NO_PATH_HINT      Set to 1 to skip the PATH hint
#   KITESURF_INSTALL_PLATFORM  Override platform detection (macos-aarch64 | linux-x64)
#
# Layout:
#   $KITESURF_INSTALL_DIR/v<version>/        whole release archive (licenses included)
#   $KITESURF_INSTALL_DIR/bin/kitesurf  ->   ../v<version>/kitesurf
#   $KITESURF_INSTALL_DIR/current-version.txt
#
# The script never uses sudo, never writes outside the install root (apart from
# the PATH line in your own shell profile) and never touches running processes.
# After installing, start the engine with: kitesurf serve --port 18971

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

info()  { echo -e "${GREEN}[INFO]${NC}  $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC}  $*"; }
error() { echo -e "${RED}[ERROR]${NC} $*" >&2; }
step()  { echo -e "${CYAN}[STEP]${NC}  $*"; }

GITHUB_API="https://api.github.com/repos/KonghaYao/kitesurf"
RELEASES_DOWNLOAD_BASE="https://github.com/KonghaYao/kitesurf/releases/download"

unsupported_platform_hint() {
    error "Unsupported platform: $1"
    echo "  Supported platforms: macos-aarch64 (macOS arm64) and linux-x64 (Linux x86_64)." >&2
    echo "  Alternatives:" >&2
    echo "    mise use -g github:KonghaYao/kitesurf@latest" >&2
    echo "    or download a release asset manually from ${RELEASES_DOWNLOAD_BASE}" >&2
}

# --- Platform Detection ---
detect_platform() {
    local os arch platform

    if [[ -n "${KITESURF_INSTALL_PLATFORM:-}" ]]; then
        case "${KITESURF_INSTALL_PLATFORM}" in
            macos-aarch64|linux-x64)
                echo "${KITESURF_INSTALL_PLATFORM}"
                return
                ;;
            *)
                unsupported_platform_hint "${KITESURF_INSTALL_PLATFORM}"
                exit 1
                ;;
        esac
    fi

    case "$(uname -s)" in
        Darwin) os="macos" ;;
        Linux)  os="linux" ;;
        *)      unsupported_platform_hint "$(uname -s)-$(uname -m)"; exit 1 ;;
    esac

    case "$(uname -m)" in
        arm64|aarch64) arch="aarch64" ;;
        x86_64|amd64)  arch="x64" ;;
        *)             unsupported_platform_hint "${os}-$(uname -m)"; exit 1 ;;
    esac

    platform="${os}-${arch}"
    case "${platform}" in
        macos-aarch64|linux-x64) echo "${platform}" ;;
        *) unsupported_platform_hint "${platform}"; exit 1 ;;
    esac
}

# Release asset components for a supported platform.
asset_os() {
    case "$1" in
        macos-aarch64) echo "darwin" ;;
        linux-x64)     echo "linux" ;;
    esac
}

asset_arch() {
    case "$1" in
        macos-aarch64) echo "arm64" ;;
        linux-x64)     echo "x64" ;;
    esac
}

# --- Download with optional proxy ---
# The download host (github.com) and the API host (api.github.com) are separate:
# most mirrors proxy only one of them, so the prefixes are separate settings.
get_download_url() {
    local url="$1"
    local proxy="${GITHUB_PROXY:-}"
    if [[ -n "${proxy}" ]]; then
        echo "${url/https:\/\/github.com/${proxy}}"
    else
        echo "${url}"
    fi
}

get_api_url() {
    local url="$1"
    local proxy="${GITHUB_API_PROXY:-}"
    if [[ -n "${proxy}" ]]; then
        echo "${url/https:\/\/api.github.com/${proxy}}"
    else
        echo "${url}"
    fi
}

github_api() {
    local url
    url=$(get_api_url "$1")
    if [[ -n "${GITHUB_TOKEN:-}" ]]; then
        curl -fsSL -H "Authorization: Bearer ${GITHUB_TOKEN}" "${url}"
    else
        curl -fsSL "${url}"
    fi
}

api_unreachable_hint() {
    warn "GitHub API (api.github.com) is unreachable."
    echo "    - Set GITHUB_API_PROXY if your network needs an API mirror"
    echo "    - Or pin KITESURF_INSTALL_VERSION=<version> to install without the API"
}

# --- SHA-256 verification (sha256sum or shasum) ---
# The checksum file is a "<64-hex>  <file name>" line for the asset.
verify_checksum() {
    local archive="$1"
    local checksum_file="$2"
    local expected_name="$3"
    local checksum_tool expected_digest actual_digest hash name extra

    if command -v sha256sum >/dev/null 2>&1; then
        checksum_tool=sha256sum
    elif command -v shasum >/dev/null 2>&1; then
        checksum_tool=shasum
    else
        error "Install sha256sum or shasum to verify downloads."
        return 1
    fi

    expected_digest=""
    while read -r hash name extra; do
        if [[ "${name}" == "${expected_name}" ]]; then
            expected_digest="${hash}"
            break
        fi
    done < <(tr -d '\r' < "${checksum_file}")

    if [[ ! "${expected_digest}" =~ ^[[:xdigit:]]{64}$ ]]; then
        error "No valid checksum entry for ${expected_name} in $(basename "${checksum_file}")."
        return 1
    fi

    if [[ "${checksum_tool}" == sha256sum ]]; then
        actual_digest=$(sha256sum "${archive}" | cut -d' ' -f1)
    else
        actual_digest=$(shasum -a 256 "${archive}" | cut -d' ' -f1)
    fi

    if [[ "${actual_digest}" != "${expected_digest}" ]]; then
        error "Checksum mismatch for ${expected_name}."
        error "  expected: ${expected_digest}"
        error "  actual:   ${actual_digest}"
        return 1
    fi
    info "Checksum verified: ${expected_name}"
}

# Remove every old v<version> directory. Non-interactive on purpose: agents run
# this script through a pipe where prompting would block on the tty.
cleanup_old_versions() {
    local install_dir="$1"
    local current="v$2"
    local dir base removed=0

    for dir in "${install_dir}"/v*; do
        [[ -d "${dir}" ]] || continue
        base=$(basename "${dir}")
        [[ "${base}" == "${current}" ]] && continue
        rm -rf "${dir}"
        info "Removed old version: ${base}"
        removed=$((removed + 1))
    done

    if [[ "${removed}" -eq 0 ]]; then
        info "No old versions to clean up."
    fi
}

# --- PATH hint ---
add_path_hint() {
    local bin_dir="$1"
    local shell_profile="" path_line=""

    case "${SHELL:-}" in
        */zsh)
            shell_profile="${HOME}/.zshrc"
            path_line="export PATH=\"${bin_dir}:\$PATH\""
            ;;
        */bash)
            shell_profile="${HOME}/.bashrc"
            path_line="export PATH=\"${bin_dir}:\$PATH\""
            ;;
        */fish)
            shell_profile="${HOME}/.config/fish/config.fish"
            path_line="set -gx PATH \"${bin_dir}\" \$PATH"
            ;;
    esac

    if [[ -z "${shell_profile}" ]]; then
        warn "Unknown shell. Add this directory to PATH manually:"
        echo "    export PATH=\"${bin_dir}:\$PATH\""
        return
    fi

    # Exact-entry check: avoid matching a sibling directory with the same prefix.
    local bin_dir_esc
    bin_dir_esc="${bin_dir//\./\\.}"
    if grep -qE "(^|[:\" ])${bin_dir_esc}([:\"\$ ]|$)" "${shell_profile}" 2>/dev/null; then
        return
    fi

    mkdir -p "$(dirname "${shell_profile}")"
    printf '%s\n' "${path_line}" >> "${shell_profile}"
    info "Added ${bin_dir} to PATH in ${shell_profile}"
    info "Open a new terminal (or source ${shell_profile}) to use kitesurf from PATH."
}

# --- Main ---
main() {
    INSTALL_DIR="${KITESURF_INSTALL_DIR:-${HOME}/.kitesurf}"

    echo ""
    info "Kitesurf Installer"
    info "-------------------------------"

    # Detect the platform before creating the install root: an unsupported
    # platform must fail without leaving an empty directory behind.
    PLATFORM=$(detect_platform)
    mkdir -p "${INSTALL_DIR}"
    INSTALL_DIR=$(cd "${INSTALL_DIR}" && pwd)
    OS_ASSET=$(asset_os "${PLATFORM}")
    ARCH_ASSET=$(asset_arch "${PLATFORM}")

    # Resolve the release tag. A pinned version needs no API call: the tag and
    # the asset naming contract determine both download URLs.
    if [[ -n "${KITESURF_INSTALL_VERSION:-}" ]]; then
        VERSION_TAG="${KITESURF_INSTALL_VERSION}"
        [[ "${VERSION_TAG}" == v* ]] || VERSION_TAG="v${VERSION_TAG}"
        VERSION="${VERSION_TAG#v}"
        if [[ ! "${VERSION}" =~ ^[0-9][0-9A-Za-z._-]*$ ]]; then
            error "Invalid KITESURF_INSTALL_VERSION: ${KITESURF_INSTALL_VERSION}"
            exit 1
        fi
        info "Release: ${VERSION_TAG} (pinned; GitHub API not queried)"
    else
        step "Fetching the latest kitesurf release..."
        RELEASE_JSON=$(github_api "${GITHUB_API}/releases/latest") || {
            error "Failed to fetch the latest release from GitHub."
            api_unreachable_hint
            exit 1
        }
        VERSION_TAG=$(echo "${RELEASE_JSON}" | tr ',' '\n' | grep -F '"tag_name"' | head -1 | cut -d'"' -f4 || true)
        if [[ -z "${VERSION_TAG}" ]]; then
            error "The latest release response has no tag_name."
            exit 1
        fi
        VERSION="${VERSION_TAG#v}"
        info "Release: ${VERSION_TAG}"
    fi

    ASSET_NAME="kitesurf-${VERSION}-${OS_ASSET}-${ARCH_ASSET}.tar.gz"
    ASSET_URL="${RELEASES_DOWNLOAD_BASE}/${VERSION_TAG}/${ASSET_NAME}"
    CHECKSUM_URL="${ASSET_URL}.sha256"

    info "Platform: ${PLATFORM}"
    info "Asset:    ${ASSET_NAME}"
    info "Install:  ${INSTALL_DIR}"

    TMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/kitesurf-install.XXXXXX")
    STAGING_DIR=""
    trap 'rm -rf "${TMP_DIR}" "${STAGING_DIR:-}"' EXIT

    TARBALL="${TMP_DIR}/${ASSET_NAME}"
    CHECKSUM_FILE="${TMP_DIR}/${ASSET_NAME}.sha256"

    DOWNLOAD_URL=$(get_download_url "${ASSET_URL}")
    if [[ "${DOWNLOAD_URL}" != "${ASSET_URL}" ]]; then
        info "Using proxy: ${DOWNLOAD_URL}"
    fi

    step "Downloading..."
    if ! curl -fSL "${DOWNLOAD_URL}" -o "${TARBALL}"; then
        error "Download failed: ${DOWNLOAD_URL}"
        exit 1
    fi

    step "Verifying checksum..."
    if ! curl -fsSL "$(get_download_url "${CHECKSUM_URL}")" -o "${CHECKSUM_FILE}"; then
        error "No checksum published for ${ASSET_NAME}; refusing to install."
        exit 1
    fi
    if ! verify_checksum "${TARBALL}" "${CHECKSUM_FILE}" "${ASSET_NAME}"; then
        exit 1
    fi

    step "Extracting..."
    VERSION_DIR="${INSTALL_DIR}/v${VERSION}"
    STAGING_DIR="${INSTALL_DIR}/.staging-v${VERSION}"
    mkdir -p "${INSTALL_DIR}"
    rm -rf "${STAGING_DIR}"
    mkdir -p "${STAGING_DIR}"
    if ! tar -xzf "${TARBALL}" -C "${STAGING_DIR}"; then
        error "Extraction failed."
        exit 1
    fi
    if [[ ! -f "${STAGING_DIR}/kitesurf" ]]; then
        error "The archive does not contain a kitesurf binary."
        ls -la "${STAGING_DIR}" || true
        exit 1
    fi
    chmod +x "${STAGING_DIR}/kitesurf"

    rm -rf "${VERSION_DIR}"
    mv "${STAGING_DIR}" "${VERSION_DIR}"
    STAGING_DIR=""

    # Atomic symlink swap: build the new link next to the old one, then rename.
    mkdir -p "${INSTALL_DIR}/bin"
    BIN_LINK="${INSTALL_DIR}/bin/kitesurf"
    ln -s "../v${VERSION}/kitesurf" "${BIN_LINK}.new.$$"
    mv -f "${BIN_LINK}.new.$$" "${BIN_LINK}"
    info "Linked: ${BIN_LINK} -> ../v${VERSION}/kitesurf"

    printf '%s\n' "${VERSION_TAG}" > "${INSTALL_DIR}/current-version.txt"

    cleanup_old_versions "${INSTALL_DIR}" "${VERSION}"

    if [[ "${KITESURF_NO_PATH_HINT:-}" != "1" ]]; then
        add_path_hint "${INSTALL_DIR}/bin"
    fi

    echo ""
    info "Kitesurf ${VERSION_TAG} installed."
    info "Binary: ${BIN_LINK}"
    info "Version file: ${INSTALL_DIR}/current-version.txt"
    echo ""
    step "Next step: start the engine, then call browser_connect"
    echo "    ${BIN_LINK} serve --port 18971"
    echo "    browser_connect(url: \"http://127.0.0.1:18971\")"
    echo ""
}

main
