#!/bin/bash
set -euo pipefail
export LC_ALL=C

# Peri Install Script
# Usage: curl -fsSL https://raw.githubusercontent.com/konghayao/peri/main/scripts/install.sh | bash
#
# Options:
#   PERI_INSTALL_VERSION   Specific version tag (e.g. agent-v1.17), empty = latest.
#                          Pinned installs skip the GitHub API entirely.
#   PERI_INSTALL_DIR       Install directory (default: $HOME/.peri)
#   GITHUB_PROXY           GitHub download proxy prefix (replaces https://github.com in download URL)
#   GITHUB_API_PROXY       GitHub API proxy prefix (replaces https://api.github.com in API URL)
#   GITHUB_TOKEN           GitHub personal access token (bypasses API rate limiting)
#   PERI_NO_PATH_HINT      Set to 1 to skip PATH hint
#   PERI_INSTALL_PLATFORM  Override platform detection (e.g. linux-x86_64, macos-aarch64)
#
# Example:
#   PERI_INSTALL_VERSION=agent-v1.17 bash install.sh
#   GITHUB_PROXY=https://ghproxy.com/https://github.com curl ... | bash
#   GITHUB_API_PROXY=https://gh-proxy.com/https://api.github.com curl ... | bash
#   GITHUB_TOKEN=ghp_xxx curl ... | bash

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

    # Allow manual override
    if [[ -n "${PERI_INSTALL_PLATFORM:-}" ]]; then
        # Validate format: os-arch
        if [[ ! "${PERI_INSTALL_PLATFORM}" =~ ^(macos|linux|windows)-(x86_64|aarch64|riscv64)$ ]]; then
            error "Invalid PERI_INSTALL_PLATFORM: ${PERI_INSTALL_PLATFORM}"
            echo "  Expected: macos-x86_64 | macos-aarch64 | linux-x86_64 | linux-aarch64 | linux-riscv64 | windows-x86_64"
            exit 1
        fi
        info "Platform (manual): ${PERI_INSTALL_PLATFORM}" >&2
        echo "${PERI_INSTALL_PLATFORM}"
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
# 下载（github.com/releases/download/*）与查询（api.github.com）是两个域名，镜像对二者的
# 支持并不一致（多数只代理下载路径），因此代理变量分开配置。
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

# --- GitHub API request (with optional token) ---
github_api() {
    local url
    url=$(get_api_url "$1")
    local auth_header=""
    if [[ -n "${GITHUB_TOKEN:-}" ]]; then
        auth_header="-H Authorization: Bearer ${GITHUB_TOKEN}"
    fi
    curl -fsSL ${auth_header:-} "${url}" 2>/dev/null
}

# API 不可达时给出可行出路：代理需单独配置，或改用固定版本安装（该路径不查询 API）。
api_unreachable_hint() {
    warn "GitHub API (api.github.com) is unreachable."
    echo "    - Set GITHUB_API_PROXY if your network needs an API mirror"
    echo "    - Or pin PERI_INSTALL_VERSION=<tag> to install without the API"
}

# 下载失败时尽力区分“tag 不存在”与“该平台无制品”：固定版本路径没有预取资产清单，
# API 可达时补一次。查不到就只留通用提示，不掩盖原始失败。
report_release_assets() {
    local releases_json assets
    releases_json=$(github_api "${GITHUB_API}/releases/tags/$1") || return 0
    assets=$(echo "${releases_json}" | tr ',' '\n' | grep -F '"browser_download_url"' | cut -d'"' -f4 || true)
    [[ -z "${assets}" ]] && return 0
    echo ""
    warn "Assets published in $1:"
    echo "${assets}" | sed 's/^/  - /'
}

# --- SHA-256 verification (sha256sum or shasum) ---
# 校验和文件为 "<64-hex>  <文件名>" 行；逐资产 <asset>.sha256 与旧版 checksums.txt 清单通用。
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

# --- Cleanup Old Versions ---
cleanup_old_versions() {
    local install_dir="$1"
    local current_version="$2"

    # Collect agent-v* directories, excluding current version
    local old_dirs=()
    for d in "${install_dir}"/agent-v*; do
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

    # Read from /dev/tty to work with curl | bash pipe
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
    INSTALL_DIR="${PERI_INSTALL_DIR:-${HOME}/.peri}"
    GITHUB_API="https://api.github.com/repos/konghayao/peri"
    RELEASES_DOWNLOAD_BASE="https://github.com/konghayao/peri/releases/download"

    echo ""
    info "Peri Agent Installer"
    info "-------------------------------"

    PLATFORM=$(detect_platform)
    ASSET_NAME="peri-${PLATFORM}.tar.gz"

    # 解析版本与资产 URL。固定版本时不查询 API：下载地址由 tag 与发布命名契约
    # （<asset>、checksums.txt）唯一确定，受限网络中 api.github.com 不必可达。
    RELEASE_JSON=""
    CHECKSUMS_URL=""
    if [[ -n "${PERI_INSTALL_VERSION:-}" ]]; then
        VERSION_TAG="${PERI_INSTALL_VERSION}"
        ASSET_DOWNLOAD_URL="${RELEASES_DOWNLOAD_BASE}/${VERSION_TAG}/${ASSET_NAME}"
        CHECKSUMS_URL="${RELEASES_DOWNLOAD_BASE}/${VERSION_TAG}/checksums.txt"
        info "Found release: ${VERSION_TAG} (pinned; GitHub API not queried)"
    else
        step "Fetching latest agent release..."
        RELEASES_JSON=$(github_api "${GITHUB_API}/releases?per_page=30") || {
            error "Failed to fetch releases from GitHub."
            api_unreachable_hint
            exit 1
        }
        # Find latest agent-* tag
        VERSION_TAG=$(echo "${RELEASES_JSON}" | tr ',' '\n' | grep -F '"tag_name"' | grep -F '"agent-' | head -1 | cut -d'"' -f4)
        if [[ -z "${VERSION_TAG}" ]]; then
            error "No agent release found."
            exit 1
        fi

        # Fetch the specific release for asset list
        RELEASE_JSON=$(github_api "${GITHUB_API}/releases/tags/${VERSION_TAG}") || {
            error "Failed to fetch release '${VERSION_TAG}'."
            api_unreachable_hint
            exit 1
        }
        info "Found release: ${VERSION_TAG}"

        # Find matching asset（按精确文件名匹配，避免命中同前缀的 <asset>.sha256 校验和文件）
        ASSET_DOWNLOAD_URL=$(echo "${RELEASE_JSON}" | tr ',' '\n' | grep -F '"browser_download_url"' | grep -F "/${ASSET_NAME}\"" | head -1 | cut -d'"' -f4 || true)

        if [[ -z "${ASSET_DOWNLOAD_URL}" ]]; then
            error "No binary found for platform '${PLATFORM}'."
            echo ""
            echo "Available assets:"
            echo "${RELEASE_JSON}" | tr ',' '\n' | grep -F '"browser_download_url"' | cut -d'"' -f4 | sed 's/^/  - /'
            exit 1
        fi

        CHECKSUMS_URL=$(echo "${RELEASE_JSON}" | tr ',' '\n' | grep -F '"browser_download_url"' | grep -F '/checksums.txt"' | head -1 | cut -d'"' -f4 || true)
    fi

    info "Binary: ${ASSET_NAME}"

    # Create install directory
    VERSION_DIR="${INSTALL_DIR}/${VERSION_TAG}"
    mkdir -p "${VERSION_DIR}"

    TARGET="${VERSION_DIR}/peri"
    TARBALL="${VERSION_DIR}/${ASSET_NAME}"
    CHECKSUM_FILE="${VERSION_DIR}/${ASSET_NAME}.sha256"

    # Download tarball
    FINAL_URL=$(get_download_url "${ASSET_DOWNLOAD_URL}")
    if [[ "${FINAL_URL}" != "${ASSET_DOWNLOAD_URL}" ]]; then
        info "Using proxy: ${FINAL_URL}"
    fi

    step "Downloading..."
    if ! curl -fSL --progress-bar "${FINAL_URL}" -o "${TARBALL}"; then
        error "Download failed: ${FINAL_URL}"
        if [[ -z "${RELEASE_JSON}" ]]; then
            report_release_assets "${VERSION_TAG}"
            warn "Check that '${VERSION_TAG}' exists and publishes ${ASSET_NAME}."
        fi
        exit 1
    fi

    # 校验 SHA-256：逐资产 <asset>.sha256 自本版本起发布；旧 Release 只有 checksums.txt
    # 清单（待旧版本退场后可删除回退分支）。
    step "Verifying checksum..."
    if ! curl -fSL "$(get_download_url "${ASSET_DOWNLOAD_URL}.sha256")" -o "${CHECKSUM_FILE}" 2>/dev/null; then
        if [[ -z "${CHECKSUMS_URL}" ]]; then
            error "No checksum published for ${ASSET_NAME}."
            rm -f "${TARBALL}"
            exit 1
        fi
        warn "Per-asset checksum missing; falling back to checksums.txt"
        curl -fSL "$(get_download_url "${CHECKSUMS_URL}")" -o "${CHECKSUM_FILE}" || {
            error "Checksum download failed."
            rm -f "${TARBALL}"
            exit 1
        }
    fi

    if ! verify_checksum "${TARBALL}" "${CHECKSUM_FILE}" "${ASSET_NAME}"; then
        rm -f "${TARBALL}" "${CHECKSUM_FILE}"
        exit 1
    fi
    rm -f "${CHECKSUM_FILE}"

    # Extract tarball
    step "Extracting..."
    tar -xzf "${TARBALL}" -C "${VERSION_DIR}" || {
        error "Extraction failed."
        exit 1
    }
    rm -f "${TARBALL}"

    # 正式归档在根目录直接放 peri（mise 标准：安装器按归档内文件名暴露命令名，不剥离
    # 平台后缀）；兼容 ≤ agent-v3.19.x 的旧布局 peri-<platform>，待旧版本退场后可删除。
    if [[ ! -f "${TARGET}" ]]; then
        EXTRACTED=$(ls "${VERSION_DIR}"/peri-* 2>/dev/null | head -1 || true)
        if [[ -n "${EXTRACTED}" && -f "${EXTRACTED}" ]]; then
            mv "${EXTRACTED}" "${TARGET}"
        else
            error "No binary found in extracted tarball."
            ls -la "${VERSION_DIR}" || true
            exit 1
        fi
    fi

    # Make executable
    chmod +x "${TARGET}"
    info "Installed to: ${TARGET}"

    # Create symlink for convenience
    LINK="${INSTALL_DIR}/peri"
    BIN_LINK="${LINK}"
    rm -f "${LINK}"
    ln -sf "${TARGET}" "${LINK}"

    # Write current version
    echo "${VERSION_TAG}" > "${INSTALL_DIR}/current-version.txt"

    # --- PATH Setup ---
    if [[ "${PERI_NO_PATH_HINT:-}" != "1" ]]; then
        SHELL_PROFILE=""
        case "${SHELL:-}" in
            */zsh)  SHELL_PROFILE="${HOME}/.zshrc" ;;
            */bash) SHELL_PROFILE="${HOME}/.bashrc" ;;
            */fish) SHELL_PROFILE="${HOME}/.config/fish/config.fish" ;;
        esac

        if [[ -n "${SHELL_PROFILE}" ]]; then
            # Check for exact PATH entry (not substring: avoid .peri matching .perihelion)
            INSTALL_DIR_ESC="${INSTALL_DIR//\./\\.}"
            if ! grep -qE "(^|[:\" ])${INSTALL_DIR_ESC}([:\"\$ ]|$)" "${SHELL_PROFILE}" 2>/dev/null; then
                if [[ "${SHELL}" == */fish ]]; then
                    echo "set -gx PATH ${INSTALL_DIR} \$PATH" >> "${SHELL_PROFILE}"
                else
                    echo "export PATH=\"${INSTALL_DIR}:\$PATH\"" >> "${SHELL_PROFILE}"
                fi
                info "Added ${INSTALL_DIR} to PATH in ${SHELL_PROFILE}"
            fi
            # 立即生效：export 到当前进程。以 `source` 方式执行脚本时直接作用于
            # 当前窗口；管道/子进程执行时至少保证脚本内后续步骤与自检可用。
            export PATH="${INSTALL_DIR}:${PATH}"
            SHELL_PROFILE_SET=1
        else
            echo ""
            warn "Unknown shell. Add this directory to your PATH manually:"
            echo "    export PATH=\"${INSTALL_DIR}:\$PATH\""
            echo ""
        fi
    fi

    # Offer to clean up old versions
    cleanup_old_versions "${INSTALL_DIR}" "${VERSION_TAG}"

    # --- Workflow dependency check ---
    echo ""
    step "Checking workflow runner..."
    if command -v node &>/dev/null; then
        info "node found — bundled workflow runner is ready"
    else
        warn "node not found. Install Node.js for workflow support:"
        echo "    https://nodejs.org/"
        echo ""
    fi

    echo ""
    info "Installation complete! Version: ${VERSION_TAG}"
    echo ""

    if command -v "${BIN_LINK}" &>/dev/null || [[ -x "${BIN_LINK}" ]]; then
        info "Run 'peri' to start."
    else
        info "Run: ${BIN_LINK}"
    fi
    # 管道（curl | bash）或子进程（bash install.sh）执行时，脚本内 export
    # 无法穿透到父 shell——给出当前窗口立即生效的一行命令（source 方式执行
    # 则已生效，无需提示）。
    # 注意：curl | bash（stdin）执行时 BASH_SOURCE 未定义，set -u 下必须给默认值
    if [[ "${BASH_SOURCE[0]:-}" != "$0" && "${SHELL_PROFILE_SET:-}" == "1" ]]; then
        echo ""
        info "To use 'peri' in the current window immediately, run:"
        if [[ "${SHELL}" == */fish ]]; then
            echo "    set -gx PATH \"${INSTALL_DIR}\" \$PATH"
        else
            echo "    export PATH=\"${INSTALL_DIR}:\$PATH\""
        fi
        echo "    (or: source ${SHELL_PROFILE}; new terminal windows pick it up automatically)"
    fi
    echo ""
}

main
