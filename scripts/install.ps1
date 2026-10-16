# Peri Agent Installer for Windows
# Usage: irm https://raw.githubusercontent.com/konghayao/peri/main/scripts/install.ps1 | iex
#
# Options:
#   $env:PERI_INSTALL_VERSION   Specific version tag (e.g. agent-v1.17), empty = latest.
#                               Pinned installs skip the GitHub API entirely.
#   $env:PERI_INSTALL_DIR       Install directory (default: $env:USERPROFILE\.peri)
#   $env:GITHUB_PROXY           GitHub download proxy prefix (replaces https://github.com)
#   $env:GITHUB_API_PROXY       GitHub API proxy prefix (replaces https://api.github.com)
#   $env:GITHUB_TOKEN           GitHub personal access token (bypasses API rate limiting)
#   $env:PERI_NO_PATH_HINT      Set to 1 to skip PATH hint
#   $env:PERI_INSTALL_PLATFORM  Override platform detection (e.g. windows-x86_64)
#
# Example:
#   $env:PERI_INSTALL_VERSION="agent-v1.17"; irm ... | iex
#   $env:GITHUB_PROXY="https://ghproxy.com/https://github.com"; irm ... | iex
#   $env:GITHUB_API_PROXY="https://gh-proxy.com/https://api.github.com"; irm ... | iex

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

# --- Logging ---
function info  { Write-Host "[INFO]  $args" -ForegroundColor Green }
function warn  { Write-Host "[WARN]  $args" -ForegroundColor Yellow }
function error { Write-Host "[ERROR] $args" -ForegroundColor Red }
function step  { Write-Host "[STEP]  $args" -ForegroundColor Cyan }

# --- Platform Detection ---
function Detect-Platform {
    if ($env:PERI_INSTALL_PLATFORM) {
        if ($env:PERI_INSTALL_PLATFORM -notmatch '^(macos|linux|windows)-(x86_64|aarch64|riscv64)$') {
            error "Invalid PERI_INSTALL_PLATFORM: $env:PERI_INSTALL_PLATFORM"
            Write-Host "  Expected: macos-x86_64 | macos-aarch64 | linux-x86_64 | linux-aarch64 | linux-riscv64 | windows-x86_64"
            exit 1
        }
        info "Platform (manual): $env:PERI_INSTALL_PLATFORM"
        return $env:PERI_INSTALL_PLATFORM
    }

    $arch = switch ($env:PROCESSOR_ARCHITECTURE) {
        "AMD64" { "x86_64" }
        "ARM64" { "aarch64" }
        default {
            error "Unsupported architecture: $env:PROCESSOR_ARCHITECTURE"
            exit 1
        }
    }

    $platform = "windows-${arch}"
    info "Detected platform: $platform"
    return $platform
}

# --- Download with optional proxy ---
# 下载（github.com/releases/download/*）与查询（api.github.com）是两个域名，镜像对二者的
# 支持并不一致（多数只代理下载路径），因此代理变量分开配置。
function Get-DownloadUrl {
    param([string]$Url)
    if ($env:GITHUB_PROXY) {
        return $Url -replace 'https://github\.com', $env:GITHUB_PROXY
    }
    return $Url
}

function Get-ApiUrl {
    param([string]$Url)
    if ($env:GITHUB_API_PROXY) {
        return $Url -replace 'https://api\.github\.com', $env:GITHUB_API_PROXY
    }
    return $Url
}

# --- GitHub API request (with optional token) ---
function Invoke-GitHubApi {
    param([string]$Url)
    $headers = @{}
    if ($env:GITHUB_TOKEN) {
        $headers["Authorization"] = "Bearer $env:GITHUB_TOKEN"
    }
    $response = Invoke-RestMethod -Uri (Get-ApiUrl $Url) -Headers $headers -ErrorAction SilentlyContinue
    return $response
}

# HTTP 错误被 -ErrorAction SilentlyContinue 吞成 $null，网络异常才抛异常；两者都归为不可用，
# 由调用方统一提示。
function Invoke-GitHubApiOrNull {
    param([string]$Url)
    try {
        return Invoke-GitHubApi $Url
    } catch {
        return $null
    }
}

# API 不可达时给出可行出路：代理需单独配置，或改用固定版本安装（该路径不查询 API）。
function Write-ApiHint {
    warn "GitHub API (api.github.com) is unreachable."
    Write-Host "    - Set GITHUB_API_PROXY if your network needs an API mirror"
    Write-Host "    - Or pin PERI_INSTALL_VERSION=<tag> to install without the API"
}

# 下载失败时尽力区分“tag 不存在”与“该平台无制品”：固定版本路径没有预取资产清单，
# API 可达时补一次。查不到就只留通用提示，不掩盖原始失败。
function Show-PublishedAssets {
    param([string]$ReleaseUrl, [string]$Tag)
    $release = Invoke-GitHubApiOrNull $ReleaseUrl
    if (-not $release) { return }
    $names = @($release.assets | ForEach-Object { $_.name })
    if ($names.Count -eq 0) { return }
    Write-Host ""
    warn "Assets published in ${Tag}:"
    $names | ForEach-Object { Write-Host "  - $_" }
}

# --- Cleanup Old Versions ---
function Clean-OldVersions {
    param([string]$InstallDir, [string]$CurrentVersion)

    # Collect agent-v* directories, excluding current version
    $oldDirs = @(Get-ChildItem -Path $InstallDir -Directory | Where-Object {
        $_.Name -match '^agent-v' -and $_.Name -ne $CurrentVersion
    })

    if ($oldDirs.Count -eq 0) {
        info "No old versions to clean up."
        return
    }

    Write-Host ""
    warn "Found $($oldDirs.Count) old version(s):"
    $totalSize = 0
    foreach ($d in $oldDirs) {
        $size = (Get-ChildItem -Path $d.FullName -Recurse -File -ErrorAction SilentlyContinue |
                 Measure-Object -Property Length -Sum).Sum
        if (-not $size) { $size = 0 }
        $totalSize += $size
        $sizeMB = [math]::Round($size / 1MB, 1)
        Write-Host "  $($d.Name)  ($sizeMB MB)"
    }
    $totalMB = [math]::Round($totalSize / 1MB, 1)
    Write-Host "  Total: $totalMB MB"
    Write-Host ""

    $answer = Read-Host "Delete old versions? [y/N]"
    switch ($answer) {
        { $_ -match '^[yY](es)?$' } {
            foreach ($d in $oldDirs) {
                Remove-Item -Recurse -Force $d.FullName
                info "Removed: $($d.Name)"
            }
            info "Cleaned up $($oldDirs.Count) old version(s)."
        }
        default {
            info "Skipped cleanup."
        }
    }
}

# --- Main ---
function Main {
    $InstallDir = if ($env:PERI_INSTALL_DIR) { $env:PERI_INSTALL_DIR } else { Join-Path $env:USERPROFILE ".peri" }
    $GitHubApi = "https://api.github.com/repos/konghayao/peri"
    $ReleasesDownloadBase = "https://github.com/konghayao/peri/releases/download"

    Write-Host ""
    info "Peri Agent Installer (Windows)"
    info "-------------------------------"

    $Platform = Detect-Platform
    $AssetName = "peri-${Platform}.zip"
    $ExeName = "peri.exe"

    # 解析版本与资产 URL。固定版本时不查询 API：下载地址由 tag 与发布命名契约
    # （<asset>、checksums.txt）唯一确定，受限网络中 api.github.com 不必可达。
    $Release = $null
    $ChecksumsUrl = $null
    if ($env:PERI_INSTALL_VERSION) {
        $VersionTag = $env:PERI_INSTALL_VERSION
        $DownloadUrl = "$ReleasesDownloadBase/$VersionTag/$AssetName"
        $ChecksumsUrl = "$ReleasesDownloadBase/$VersionTag/checksums.txt"
        info "Found release: $VersionTag (pinned; GitHub API not queried)"
    } else {
        step "Fetching latest agent release..."
        $Releases = Invoke-GitHubApiOrNull "$GitHubApi/releases?per_page=30"
        if (-not $Releases) {
            error "Failed to fetch releases from GitHub."
            Write-ApiHint
            exit 1
        }

        # Find latest agent-* tag
        $VersionTag = ($Releases | Where-Object { $_.tag_name -like "agent-*" } | Select-Object -First 1).tag_name
        if (-not $VersionTag) {
            error "No agent release found."
            exit 1
        }

        $Release = Invoke-GitHubApiOrNull "$GitHubApi/releases/tags/$VersionTag"
        if (-not $Release) {
            error "Failed to fetch release '$VersionTag'."
            Write-ApiHint
            exit 1
        }
        info "Found release: $VersionTag"

        # Find matching asset
        $Asset = $Release.assets | Where-Object { $_.name -eq $AssetName }
        if (-not $Asset) {
            error "No binary found for platform '$Platform'."
            Write-Host ""
            Write-Host "Available assets:"
            $Release.assets | ForEach-Object { Write-Host "  - $($_.name)" }
            exit 1
        }

        $DownloadUrl = $Asset.browser_download_url
        $ChecksumsAsset = $Release.assets | Where-Object { $_.name -eq "checksums.txt" }
        if ($ChecksumsAsset) { $ChecksumsUrl = $ChecksumsAsset.browser_download_url }
    }

    info "Binary: $AssetName"

    # Create install directory
    $VersionDir = Join-Path $InstallDir $VersionTag
    New-Item -ItemType Directory -Force -Path $VersionDir | Out-Null

    $ZipPath = Join-Path $VersionDir $AssetName

    # Download
    $FinalUrl = Get-DownloadUrl $DownloadUrl
    if ($FinalUrl -ne $DownloadUrl) {
        info "Using proxy: $FinalUrl"
    }

    step "Downloading..."
    try {
        Invoke-WebRequest -Uri $FinalUrl -OutFile $ZipPath
    } catch {
        error "Download failed: $FinalUrl"
        if (-not $Release) {
            Show-PublishedAssets -ReleaseUrl "$GitHubApi/releases/tags/$VersionTag" -Tag $VersionTag
            warn "Check that '$VersionTag' exists and publishes $AssetName."
        }
        exit 1
    }

    # Verify SHA-256. Per-asset <asset>.sha256 is published from this release on;
    # older releases only ship the checksums.txt manifest (drop the fallback once
    # those releases are gone).
    step "Verifying checksum..."
    $ChecksumPath = "$ZipPath.sha256"
    $PerAssetChecksum = $true
    try {
        Invoke-WebRequest -Uri (Get-DownloadUrl "$DownloadUrl.sha256") -OutFile $ChecksumPath
    } catch {
        $PerAssetChecksum = $false
    }
    if (-not $PerAssetChecksum) {
        if (-not $ChecksumsUrl) {
            error "No checksum published for $AssetName."
            Remove-Item -Force $ZipPath -ErrorAction SilentlyContinue
            exit 1
        }
        warn "Per-asset checksum missing; falling back to checksums.txt"
        try {
            Invoke-WebRequest -Uri (Get-DownloadUrl $ChecksumsUrl) -OutFile $ChecksumPath
        } catch {
            error "Checksum download failed: $_"
            Remove-Item -Force $ZipPath -ErrorAction SilentlyContinue
            exit 1
        }
    }

    $Expected = $null
    foreach ($line in (Get-Content $ChecksumPath)) {
        $parts = $line.Trim() -split '\s+'
        if ($parts.Count -ge 2 -and $parts[1] -eq $AssetName) {
            $Expected = $parts[0]
            break
        }
    }
    Remove-Item -Force $ChecksumPath -ErrorAction SilentlyContinue

    if (-not $Expected -or $Expected -notmatch '^[0-9a-fA-F]{64}$') {
        error "No valid checksum entry for $AssetName."
        Remove-Item -Force $ZipPath -ErrorAction SilentlyContinue
        exit 1
    }

    # PowerShell string comparison is case-insensitive; Get-FileHash returns uppercase hex.
    $Actual = (Get-FileHash -Algorithm SHA256 -Path $ZipPath).Hash
    if ($Actual -ne $Expected) {
        error "Checksum mismatch for $AssetName."
        error "  expected: $Expected"
        error "  actual:   $Actual"
        Remove-Item -Force $ZipPath -ErrorAction SilentlyContinue
        exit 1
    }
    info "Checksum verified: $AssetName"

    # Extract
    step "Extracting..."
    try {
        Expand-Archive -Path $ZipPath -DestinationPath $VersionDir -Force
    } catch {
        error "Extraction failed: $_"
        exit 1
    }
    Remove-Item -Force $ZipPath -ErrorAction SilentlyContinue

    # Zip contains peri.exe; older releases shipped peri-<platform>.exe.
    # Take the single .exe present and place it at peri.exe.
    $SourceExe = Get-ChildItem -Path $VersionDir -Recurse -Filter "*.exe" | Where-Object { $_.Name -notlike "unins*" } | Select-Object -First 1
    if (-not $SourceExe) {
        error "No .exe found in extracted archive."
        Get-ChildItem -Path $VersionDir -Recurse | ForEach-Object { Write-Host "  $($_.FullName)" }
        exit 1
    }

    $TargetExe = Join-Path $VersionDir $ExeName
    if ($SourceExe.FullName -ne $TargetExe) {
        Move-Item -Force $SourceExe.FullName $TargetExe
    }

    info "Installed to: $TargetExe"

    # Create convenience copy (Windows doesn't support symlinks without admin)
    $LinkPath = Join-Path $InstallDir $ExeName
    Copy-Item -Force $TargetExe $LinkPath

    # 随包 workflow sidecar 必须与用户实际调用的可执行文件同目录：上面的便捷副本位于
    # $InstallDir，sidecar 源就在 $VersionDir（解压产物）。只复制到两者不同的位置：
    # 目标与源同一路径时不复制（幂等、杜绝自复制）。
    $SidecarSource = Join-Path $VersionDir "peri-resources"
    $SidecarTarget = Join-Path $InstallDir "peri-resources"
    if ((Test-Path $SidecarSource) -and ($SidecarTarget -ne $SidecarSource)) {
        New-Item -ItemType Directory -Force -Path $SidecarTarget | Out-Null
        Copy-Item -Recurse -Force (Join-Path $SidecarSource "*") $SidecarTarget
    }

    # Write current version
    $VersionFile = Join-Path $InstallDir "current-version.txt"
    $VersionTag | Out-File -FilePath $VersionFile -Encoding ascii -NoNewline

    # --- PATH Setup ---
    if ($env:PERI_NO_PATH_HINT -ne "1") {
        $currentPath = [Environment]::GetEnvironmentVariable("Path", "User") -split ";"
        $installPathNormalized = (Resolve-Path $InstallDir).Path.TrimEnd("\")

        # Check if install dir is already in PATH (case-insensitive)
        $alreadyInPath = $false
        foreach ($p in $currentPath) {
            if ($p.TrimEnd("\") -eq $installPathNormalized) {
                $alreadyInPath = $true
                break
            }
        }

        if (-not $alreadyInPath) {
            [Environment]::SetEnvironmentVariable("Path", "$InstallDir;$([Environment]::GetEnvironmentVariable('Path', 'User'))", "User")
            info "Added $InstallDir to user PATH"

            # Refresh current session's PATH
            $env:Path = "$InstallDir;$env:Path"
        }
    }

    # Offer to clean up old versions
    Clean-OldVersions -InstallDir $InstallDir -CurrentVersion $VersionTag

    # --- Workflow dependency check ---
    Write-Host ""
    step "Checking workflow runner..."
    $nodeCmd = Get-Command node -ErrorAction SilentlyContinue
    if ($nodeCmd) {
        info "node found - bundled workflow runner is ready"
    } else {
        Write-Host ""
        warn "node not found. Install Node.js for workflow support:"
        Write-Host "    https://nodejs.org/"
        Write-Host ""
    }

    Write-Host ""
    info "Installation complete! Version: $VersionTag"
    Write-Host ""
    info "Open a new terminal and run 'peri' to start."
    Write-Host ""
}

Main
