#!/bin/bash
set -euo pipefail
export LC_ALL=C

# Peri Browser Install Script
# Usage: curl -fsSL https://raw.githubusercontent.com/konghayao/peri/main/scripts/install-browser.sh | bash
#
# Installs the peri-browser binary (package mcp-packages/peri-browser-gateway) and
# then, in the same run, the Kitesurf engine it drives.
#
# Options:
#   PERI_BROWSER_VERSION           Release version (e.g. 0.3.0 or browser-v0.3.0);
#                                  empty = newest browser-v* release.
#                                  A pinned version skips the GitHub API entirely.
#   PERI_BROWSER_INSTALL_DIR       Install root (default: $HOME/.peri/browser)
#   PERI_BROWSER_SKIP_KITESURF     Set to 1 to install only the gateway binary and
#                                  skip `peri-browser install kitesurf`
#   PERI_BROWSER_NO_PATH_HINT      Set to 1 to skip the PATH hint
#   PERI_BROWSER_INSTALL_PLATFORM  Override platform detection
#                                  (macos-aarch64 | linux-x86_64)
#   GITHUB_PROXY                   GitHub download proxy prefix (replaces https://github.com)
#   GITHUB_API_PROXY               GitHub API proxy prefix (replaces https://api.github.com)
#   GITHUB_TOKEN                   GitHub token for API requests (rate limiting)
#
# Layout:
#   $PERI_BROWSER_INSTALL_DIR/v<version>/peri-browser        extracted from the release
#   $PERI_BROWSER_INSTALL_DIR/bin/peri-browser   ->          ../v<version>/peri-browser
#   $PERI_BROWSER_INSTALL_DIR/current-version.txt            e.g. browser-v0.3.0
#
# Every download is checked against the published <asset>.sha256; a missing
# checksum or a mismatch aborts the install. There is no skip-checksum switch.
#
# Supported platforms are the same two the Kitesurf installer covers: macOS arm64
# and Linux x86_64. Anything else fails before a single byte is downloaded.
#
# The script is non-interactive (safe through a pipe), never uses sudo, never
# writes outside the install root and does not edit shell profiles: it prints the
# PATH line instead. After installing:
#   peri-browser start kitesurf        # engine on 18971, gateway on 127.0.0.1:7391

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

info()  { echo -e "${GREEN}[INFO]${NC}  $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC}  $*"; }
error() { echo -e "${RED}[ERROR]${NC} $*" >&2; }
step()  { echo -e "${CYAN}[STEP]${NC}  $*"; }

GITHUB_API="https://api.github.com/repos/konghayao/peri"
RELEASES_DOWNLOAD_BASE="https://github.com/konghayao/peri/releases/download"

unsupported_platform_hint() {
    error "Unsupported platform: $1"
    echo "  Supported platforms: macos-aarch64 (macOS arm64) and linux-x86_64 (Linux x86_64)." >&2
    echo "  Alternatives:" >&2
    echo "    PERI_BROWSER_INSTALL_PLATFORM=<platform> to override detection" >&2
    echo "    or download an asset manually from ${RELEASES_DOWNLOAD_BASE}" >&2
}

# --- Platform Detection ---
# Tags are the same strings the release workflow publishes in the asset name.
detect_platform() {
    local os arch platform

    if [[ -n "${PERI_BROWSER_INSTALL_PLATFORM:-}" ]]; then
        case "${PERI_BROWSER_INSTALL_PLATFORM}" in
            macos-aarch64|linux-x86_64)
                info "Platform (manual): ${PERI_BROWSER_INSTALL_PLATFORM}" >&2
                echo "${PERI_BROWSER_INSTALL_PLATFORM}"
                return
                ;;
            *)
                unsupported_platform_hint "${PERI_BROWSER_INSTALL_PLATFORM}"
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
        x86_64|amd64)  arch="x86_64" ;;
        *)             unsupported_platform_hint "${os}-$(uname -m)"; exit 1 ;;
    esac

    platform="${os}-${arch}"
    case "${platform}" in
        macos-aarch64|linux-x86_64) echo "${platform}" ;;
        *) unsupported_platform_hint "${platform}"; exit 1 ;;
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
    echo "    - Or pin PERI_BROWSER_VERSION=<version> to install without the API"
}

# --- Version Resolution ---
# Prints the release tag (browser-v<version>) on stdout; progress goes to stderr.
resolve_tag() {
    if [[ -n "${PERI_BROWSER_VERSION:-}" ]]; then
        local version="${PERI_BROWSER_VERSION#browser-}"
        version="${version#v}"
        if [[ ! "${version}" =~ ^[0-9][0-9A-Za-z._-]*$ ]]; then
            error "Invalid PERI_BROWSER_VERSION: ${PERI_BROWSER_VERSION}"
            echo "  Expected a release version such as 0.3.0 or browser-v0.3.0." >&2
            exit 1
        fi
        info "Release: browser-v${version} (pinned; GitHub API not queried)" >&2
        echo "browser-v${version}"
        return
    fi

    step "Fetching the latest browser release..." >&2
    local releases_json tag
    releases_json=$(github_api "${GITHUB_API}/releases?per_page=100") || {
        error "Failed to fetch releases from GitHub."
        api_unreachable_hint
        exit 1
    }
    # Agent releases share the same repository, so /releases/latest is the newest
    # release overall and may be an agent-* one. Filter by the browser-v prefix
    # instead (the API lists releases newest first). per_page=100 is the maximum
    # the endpoint accepts; a browser release outside that window is not found,
    # hence the pin-it hint below.
    tag=$(echo "${releases_json}" | tr ',' '\n' | grep -F '"tag_name"' | grep -F '"browser-v' | head -1 | cut -d'"' -f4 || true)
    if [[ -z "${tag}" ]]; then
        error "No browser-v* release found in the newest releases returned by the GitHub API."
        api_unreachable_hint
        exit 1
    fi
    info "Release: ${tag}" >&2
    echo "${tag}"
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

# Remove every other v<version> directory. Non-interactive on purpose: agents run
# this script through a pipe where prompting would block on the tty.
cleanup_old_versions() {
    local install_root="$1"
    local current="v$2"
    local dir base removed=0

    for dir in "${install_root}"/v*; do
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

# Print (never append to) the PATH line: installing a binary should not edit the
# user's shell profile behind their back.
print_path_hint() {
    local bin_dir="$1"

    [[ "${PERI_BROWSER_NO_PATH_HINT:-}" == "1" ]] && return
    case ":${PATH:-}:" in
        *":${bin_dir}:"*) return ;;
    esac

    warn "${bin_dir} is not in PATH. Add it with:"
    echo "    export PATH=\"${bin_dir}:\$PATH\""
}

# --- Main ---
main() {
    INSTALL_ROOT="${PERI_BROWSER_INSTALL_DIR:-${HOME}/.peri/browser}"

    echo ""
    info "Peri Browser Installer"
    info "-------------------------------"

    # Detect the platform before creating anything: an unsupported platform must
    # fail without leaving an empty install root behind.
    PLATFORM=$(detect_platform)
    TAG=$(resolve_tag)
    VERSION="${TAG#browser-v}"
    ASSET_NAME="peri-browser-${PLATFORM}.tar.gz"
    ASSET_URL="${RELEASES_DOWNLOAD_BASE}/${TAG}/${ASSET_NAME}"
    CHECKSUM_URL="${ASSET_URL}.sha256"

    info "Platform: ${PLATFORM}"
    info "Asset:    ${ASSET_NAME}"
    info "Install:  ${INSTALL_ROOT}"

    TMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/peri-browser-install.XXXXXX")
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
        error "Check that '${TAG}' exists and publishes ${ASSET_NAME}."
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
    VERSION_DIR="${INSTALL_ROOT}/v${VERSION}"
    STAGING_DIR="${INSTALL_ROOT}/.staging-v${VERSION}"
    mkdir -p "${INSTALL_ROOT}"
    rm -rf "${STAGING_DIR}"
    mkdir -p "${STAGING_DIR}"
    if ! tar -xzf "${TARBALL}" -C "${STAGING_DIR}"; then
        error "Extraction failed."
        exit 1
    fi
    if [[ ! -f "${STAGING_DIR}/peri-browser" ]]; then
        error "The archive does not contain a peri-browser binary."
        ls -la "${STAGING_DIR}" || true
        exit 1
    fi
    chmod +x "${STAGING_DIR}/peri-browser"

    # Reinstalling the same version replaces the directory; the move is atomic so
    # the previous install keeps working until the new one is complete.
    rm -rf "${VERSION_DIR}"
    mv "${STAGING_DIR}" "${VERSION_DIR}"
    STAGING_DIR=""

    # Atomic symlink swap: build the new link next to the old one, then rename.
    mkdir -p "${INSTALL_ROOT}/bin"
    BIN_LINK="${INSTALL_ROOT}/bin/peri-browser"
    ln -s "../v${VERSION}/peri-browser" "${BIN_LINK}.new.$$"
    mv -f "${BIN_LINK}.new.$$" "${BIN_LINK}"
    info "Linked: ${BIN_LINK} -> ../v${VERSION}/peri-browser"

    printf '%s\n' "${TAG}" > "${INSTALL_ROOT}/current-version.txt"

    cleanup_old_versions "${INSTALL_ROOT}" "${VERSION}"

    step "Checking the installed binary..."
    if ! REPORTED_VERSION=$("${BIN_LINK}" --version); then
        error "The installed binary does not run on this machine (${PLATFORM})."
        exit 1
    fi
    info "Binary reports: ${REPORTED_VERSION}"

    # Engine install goes through the binary's own installer path, which downloads
    # and runs the official Kitesurf install script.
    if [[ "${PERI_BROWSER_SKIP_KITESURF:-}" == "1" ]]; then
        warn "Skipped 'peri-browser install kitesurf' (PERI_BROWSER_SKIP_KITESURF=1)."
    else
        step "Installing the Kitesurf engine..."
        if ! "${BIN_LINK}" install kitesurf; then
            error "Kitesurf engine install failed."
            error "The gateway binary is installed; rerun the engine step with:"
            error "  ${BIN_LINK} install kitesurf"
            exit 1
        fi
    fi

    print_path_hint "${INSTALL_ROOT}/bin"

    echo ""
    info "peri-browser ${TAG} installed."
    info "Binary: ${BIN_LINK}"
    info "Version file: ${INSTALL_ROOT}/current-version.txt"
    echo ""
    step "Next step: start the engine and the gateway"
    echo "    ${BIN_LINK} start kitesurf"
    echo "    # MCP:     http://127.0.0.1:7391/mcp"
    echo "    # Monitor: http://127.0.0.1:7391/"
    echo ""
}

main "$@"
