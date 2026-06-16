#!/bin/bash
set -euo pipefail
export LC_ALL=C

# AGM Install Script
# Usage: curl -fsSL https://raw.githubusercontent.com/konghayao/peri/main/agm/install.sh | bash
#
# Options:
#   AGM_INSTALL_VERSION  Specific version tag (e.g. agm-v0.1.0), empty = latest
#   AGM_INSTALL_DIR      Install directory (default: $HOME/.agm)
#   GITHUB_PROXY         GitHub download proxy prefix (replaces https://github.com in download URL)
#   GITHUB_TOKEN         GitHub personal access token (bypasses API rate limiting)
#   AGM_NO_PATH_HINT     Set to 1 to skip PATH hint
#   AGM_INSTALL_PLATFORM Override platform detection (e.g. linux-x86_64, macos-aarch64)
#
# Example:
#   AGM_INSTALL_VERSION=agm-v0.1.0 bash install-agm.sh
#   GITHUB_PROXY=https://ghproxy.com/https://github.com curl ... | bash

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

info()    { echo -e "${GREEN}[INFO]${NC}  $*"; }
warn()    { echo -e "${YELLOW}[WARN]${NC}  $*"; }
error()   { echo -e "${RED}[ERROR]${NC} $*" >&2; }
step()    { echo -e "${CYAN}[STEP]${NC}  $*"; }

# --- Platform Detection ---
detect_platform() {
    local os arch platform

    if [[ -n "${AGM_INSTALL_PLATFORM:-}" ]]; then
        if [[ ! "${AGM_INSTALL_PLATFORM}" =~ ^(macos|linux|windows)-(x86_64|aarch64|riscv64)$ ]]; then
            error "Invalid AGM_INSTALL_PLATFORM: ${AGM_INSTALL_PLATFORM}"
            echo "  Expected: macos-x86_64 | macos-aarch64 | linux-x86_64 | linux-aarch64 | linux-riscv64 | windows-x86_64"
            exit 1
        fi
        info "Platform (manual): ${AGM_INSTALL_PLATFORM}" >&2
        echo "${AGM_INSTALL_PLATFORM}"
        return
    fi

    case "$(uname -s)" in
        Darwin)  os="macos" ;;
        Linux)   os="linux" ;;
        *)       error "Unsupported OS: $(uname -s)"; exit 1 ;;
    esac

    case "$(uname -m)" in
        x86_64|amd64)  arch="x86_64" ;;
        aarch64|arm64) arch="aarch64" ;;
        riscv64)       arch="riscv64" ;;
        *)             error "Unsupported arch: $(uname -m)"; exit 1 ;;
    esac

    platform="${os}-${arch}"
    info "Detected platform: ${platform}" >&2
    echo "${platform}"
}

# --- Download with optional proxy ---
get_download_url() {
    local url="$1"
    local proxy="${GITHUB_PROXY:-}"
    if [[ -n "${proxy}" ]]; then
        echo "${url/https:\/\/github.com/${proxy}}"
    else
        echo "${url}"
    fi
}

# --- GitHub API request (with optional token) ---
github_api() {
    local url="$1"
    local auth_header=""
    if [[ -n "${GITHUB_TOKEN:-}" ]]; then
        auth_header="-H Authorization: Bearer ${GITHUB_TOKEN}"
    fi
    curl -fsSL ${auth_header:-} "${url}" 2>/dev/null
}

# --- Cleanup Old Versions ---
cleanup_old_versions() {
    local install_dir="$1"
    local current_version="$2"

    local old_dirs=()
    for d in "${install_dir}"/agm-v*; do
        [[ -d "$d" ]] || continue
        local base
        base=$(basename "$d")
        [[ "$base" == "$current_version" ]] && continue
        old_dirs+=("$d")
    done

    if [[ ${#old_dirs[@]} -eq 0 ]]; then
        info "No old versions to clean up."
        return
    fi

    echo ""
    warn "Found ${#old_dirs[@]} old version(s):"
    for d in "${old_dirs[@]}"; do
        local size
        size=$(du -sh "$d" 2>/dev/null | cut -f1)
        echo "  $(basename "$d")  (${size})"
    done
    local total_human
    total_human=$(du -sh "${old_dirs[@]}" 2>/dev/null | tail -1 | cut -f1)
    echo "  Total: ${total_human}"
    echo ""

    if ! [[ -t 0 ]] && [[ -e /dev/tty ]]; then
        exec 3< /dev/tty
    else
        exec 3<&0
    fi

    echo -e "${YELLOW}[WARN]${NC}  Delete old versions? [y/N] " >/dev/tty
    local answer
    read -r answer <&3
    exec 3<&-

    case "${answer}" in
        [yY]|[yY][eE][sS])
            for d in "${old_dirs[@]}"; do
                rm -rf "$d"
                info "Removed: $(basename "$d")"
            done
            info "Cleaned up ${#old_dirs[@]} old version(s)."
            ;;
        *)
            info "Skipped cleanup."
            ;;
    esac
}

# --- Main ---
main() {
    INSTALL_DIR="${AGM_INSTALL_DIR:-${HOME}/.agm}"
    GITHUB_API="https://api.github.com/repos/konghayao/peri"

    echo ""
    info "AGM Installer"
    info "-------------------------------"

    PLATFORM=$(detect_platform)
    ASSET_NAME="agm-${PLATFORM}.tar.gz"

    # Fetch release info
    if [[ -n "${AGM_INSTALL_VERSION:-}" ]]; then
        VERSION_TAG="${AGM_INSTALL_VERSION}"
        step "Fetching release: ${VERSION_TAG}..."
        RELEASE_JSON=$(github_api "${GITHUB_API}/releases/tags/${VERSION_TAG}") || {
            error "Failed to fetch release '${VERSION_TAG}'. Does this tag exist?"
            exit 1
        }
    else
        step "Fetching latest AGM release..."
        RELEASES_JSON=$(github_api "${GITHUB_API}/releases?per_page=30") || {
            error "Failed to fetch releases from GitHub."
            exit 1
        }
        VERSION_TAG=$(echo "${RELEASES_JSON}" | tr ',' '\n' | grep -F '"tag_name"' | grep -F '"agm-' | head -1 | cut -d'"' -f4)
        if [[ -z "${VERSION_TAG}" ]]; then
            error "No AGM release found."
            exit 1
        fi

        RELEASE_JSON=$(github_api "${GITHUB_API}/releases/tags/${VERSION_TAG}") || {
            error "Failed to fetch release '${VERSION_TAG}'."
            exit 1
        }
    fi

    info "Found release: ${VERSION_TAG}"

    # Create install directory
    VERSION_DIR="${INSTALL_DIR}/${VERSION_TAG}"
    TARGET="${VERSION_DIR}/agm"

    # Check if already installed
    if [[ -f "${TARGET}" ]]; then
        if [[ "${AGM_FORCE:-}" != "1" ]]; then
            info "Already up to date (${VERSION_TAG}). Use AGM_FORCE=1 to reinstall."
            cleanup_old_versions "${INSTALL_DIR}" "${VERSION_TAG}"
            exit 0
        fi
        info "Force reinstall, removing existing..."
        rm -rf "${VERSION_DIR}"
    fi

    # Find matching asset
    ASSET_DOWNLOAD_URL=$(echo "${RELEASE_JSON}" | tr ',' '\n' | grep -F '"browser_download_url"' | grep -F "${ASSET_NAME}" | head -1 | cut -d'"' -f4)

    if [[ -z "${ASSET_DOWNLOAD_URL}" ]]; then
        error "No binary found for platform '${PLATFORM}'."
        echo ""
        echo "Available assets:"
        echo "${RELEASE_JSON}" | tr ',' '\n' | grep -F '"browser_download_url"' | cut -d'"' -f4 | sed 's/^/  - /'
        exit 1
    fi

    info "Binary: ${ASSET_NAME}"

    mkdir -p "${VERSION_DIR}"

    TARBALL="${VERSION_DIR}/${ASSET_NAME}"

    # Download tarball
    FINAL_URL=$(get_download_url "${ASSET_DOWNLOAD_URL}")
    if [[ "${FINAL_URL}" != "${ASSET_DOWNLOAD_URL}" ]]; then
        info "Using proxy: ${FINAL_URL}"
    fi

    step "Downloading..."
    curl -fSL --progress-bar "${FINAL_URL}" -o "${TARBALL}" || {
        error "Download failed."
        exit 1
    }

    # Extract tarball
    step "Extracting..."
    tar -xzf "${TARBALL}" -C "${VERSION_DIR}" || {
        error "Extraction failed."
        exit 1
    }
    rm -f "${TARBALL}"

    # Tarball contains agm-<platform>, rename to agm
    if [[ ! -f "${TARGET}" ]]; then
        EXTRACTED=$(ls "${VERSION_DIR}"/agm-* 2>/dev/null | head -1)
        if [[ -f "${EXTRACTED}" ]]; then
            mv "${EXTRACTED}" "${TARGET}"
        else
            error "No binary found in extracted tarball."
            ls -la "${VERSION_DIR}" || true
            exit 1
        fi
    fi

    chmod +x "${TARGET}"
    info "Installed to: ${TARGET}"

    # Create symlink for convenience
    LINK="${INSTALL_DIR}/agm"
    rm -f "${LINK}"
    ln -sf "${TARGET}" "${LINK}"

    # Write current version
    echo "${VERSION_TAG}" > "${INSTALL_DIR}/current-version.txt"

    # --- PATH Setup ---
    if [[ "${AGM_NO_PATH_HINT:-}" != "1" ]]; then
        BIN_LINK="${INSTALL_DIR}/agm"
        SHELL_PROFILE=""
        case "${SHELL:-}" in
            */zsh)  SHELL_PROFILE="${HOME}/.zshrc" ;;
            */bash) SHELL_PROFILE="${HOME}/.bashrc" ;;
            */fish) SHELL_PROFILE="${HOME}/.config/fish/config.fish" ;;
        esac

        if [[ -n "${SHELL_PROFILE}" ]]; then
            INSTALL_DIR_ESC="${INSTALL_DIR//\./\\.}"
            if ! grep -qE "(^|[:\" ])${INSTALL_DIR_ESC}([:\"\$ ]|$)" "${SHELL_PROFILE}" 2>/dev/null; then
                if [[ "${SHELL}" == */fish ]]; then
                    echo "set -gx PATH ${INSTALL_DIR} \$PATH" >> "${SHELL_PROFILE}"
                else
                    echo "export PATH=\"${INSTALL_DIR}:\$PATH\"" >> "${SHELL_PROFILE}"
                fi
                info "Added ${INSTALL_DIR} to PATH in ${SHELL_PROFILE}"
            fi
        else
            echo ""
            warn "Unknown shell. Add this directory to your PATH manually:"
            echo "    export PATH=\"${INSTALL_DIR}:\$PATH\""
            echo ""
        fi
    fi

    # Offer to clean up old versions
    cleanup_old_versions "${INSTALL_DIR}" "${VERSION_TAG}"

    echo ""
    info "Installation complete! Version: ${VERSION_TAG}"
    echo ""

    if command -v "${BIN_LINK}" &>/dev/null || [[ -x "${BIN_LINK}" ]]; then
        info "Run 'agm' to start."
    else
        info "Run: ${BIN_LINK}"
    fi
    echo ""
}

main
