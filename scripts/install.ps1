# Peri Agent Installer for Windows
# Usage: irm https://raw.githubusercontent.com/konghayao/peri/main/scripts/install.ps1 | iex
#
# Options:
#   $env:PERI_INSTALL_VERSION   Specific version tag (e.g. agent-v1.17), empty = latest
#   $env:PERI_INSTALL_DIR       Install directory (default: $env:USERPROFILE\.peri)
#   $env:GITHUB_PROXY           GitHub download proxy prefix (replaces https://github.com)
#   $env:GITHUB_TOKEN           GitHub personal access token (bypasses API rate limiting)
#   $env:PERI_NO_PATH_HINT      Set to 1 to skip PATH hint
#   $env:PERI_INSTALL_PLATFORM  Override platform detection (e.g. windows-x86_64)
#
# Example:
#   $env:PERI_INSTALL_VERSION="agent-v1.17"; irm ... | iex
#   $env:GITHUB_PROXY="https://ghproxy.com/https://github.com"; irm ... | iex

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
function Get-DownloadUrl {
    param([string]$Url)
    if ($env:GITHUB_PROXY) {
        return $Url -replace 'https://github\.com', $env:GITHUB_PROXY
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
    $response = Invoke-RestMethod -Uri $Url -Headers $headers -ErrorAction SilentlyContinue
    return $response
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

    Write-Host ""
    info "Peri Agent Installer (Windows)"
    info "-------------------------------"

    $Platform = Detect-Platform
    $AssetName = "peri-${Platform}.zip"
    $ExeName = "peri.exe"

    # Fetch release info
    if ($env:PERI_INSTALL_VERSION) {
        $VersionTag = $env:PERI_INSTALL_VERSION
        step "Fetching release: $VersionTag..."
        try {
            $Release = Invoke-GitHubApi "$GitHubApi/releases/tags/$VersionTag"
        } catch {
            error "Failed to fetch release '$VersionTag'. Does this tag exist?"
            exit 1
        }
    } else {
        step "Fetching latest agent release..."
        try {
            $Releases = Invoke-GitHubApi "$GitHubApi/releases?per_page=30"
        } catch {
            error "Failed to fetch releases from GitHub."
            exit 1
        }

        # Find latest agent-* tag
        $VersionTag = ($Releases | Where-Object { $_.tag_name -like "agent-*" } | Select-Object -First 1).tag_name
        if (-not $VersionTag) {
            error "No agent release found."
            exit 1
        }

        try {
            $Release = Invoke-GitHubApi "$GitHubApi/releases/tags/$VersionTag"
        } catch {
            error "Failed to fetch release '$VersionTag'."
            exit 1
        }
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
        error "Download failed: $_"
        exit 1
    }

    # Extract
    step "Extracting..."
    try {
        Expand-Archive -Path $ZipPath -DestinationPath $VersionDir -Force
    } catch {
        error "Extraction failed: $_"
        exit 1
    }
    Remove-Item -Force $ZipPath -ErrorAction SilentlyContinue

    # Zip contains peri-<platform>.exe (e.g. peri-windows-x86_64.exe), find and rename to peri.exe
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
    $wfCmd = Get-Command peri-workflow -ErrorAction SilentlyContinue
    if ($wfCmd) {
        info "Workflow runner found: $($wfCmd.Source)"
    } else {
        warn "peri-workflow not found. Install for multi-agent workflow support:"
        Write-Host "    npm install -g @peri-code/workflow"
        if (-not (Get-Command node -ErrorAction SilentlyContinue)) {
            Write-Host ""
            warn "Node.js is also required. Install it from https://nodejs.org/"
        }
        Write-Host ""
    }

    Write-Host ""
    info "Installation complete! Version: $VersionTag"
    Write-Host ""
    info "Open a new terminal and run 'peri' to start."
    Write-Host ""
}

Main
