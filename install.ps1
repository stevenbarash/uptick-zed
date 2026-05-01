# Uptick installer for Windows.
#
# Downloads the latest released `uptick-lsp` binary from GitHub, verifies its
# .sha256 sidecar, and installs it under -Prefix\bin (default
# %LOCALAPPDATA%\Programs\uptick). Optionally clones the repo for the Zed
# dev extension.
#
# Usage (PowerShell):
#   irm https://raw.githubusercontent.com/stevenbarash/uptick-zed/main/install.ps1 | iex
#   # with options:
#   $script = irm https://raw.githubusercontent.com/stevenbarash/uptick-zed/main/install.ps1
#   & ([scriptblock]::Create($script)) -Clone -Version 0.4.0

[CmdletBinding()]
param(
    [string]$Prefix = (Join-Path $env:LOCALAPPDATA 'Programs\uptick'),
    [string]$Version = '',
    [switch]$Clone,
    [switch]$NoVerify
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Repo = 'stevenbarash/uptick-zed'

function Note($msg) { Write-Host "==> $msg" }
function Fail($msg) { Write-Error "install.ps1: $msg"; exit 1 }

# --- Detect target ---------------------------------------------------------
$arch = $env:PROCESSOR_ARCHITECTURE
switch ($arch) {
    'AMD64' { $target = 'x86_64-pc-windows-msvc' }
    default { Fail "Unsupported Windows architecture ($arch). Only x64 prebuilts are published today." }
}
Note "Detected target: $target"

# --- Resolve version --------------------------------------------------------
if ([string]::IsNullOrEmpty($Version)) {
    Note 'Resolving latest release tag...'
    try {
        $latest = Invoke-RestMethod -UseBasicParsing -Uri "https://api.github.com/repos/$Repo/releases/latest"
    } catch {
        Fail "Could not query GitHub releases API: $_"
    }
    $Version = $latest.tag_name -replace '^v',''
}
Note "Using version: $Version"

# --- Download + verify ------------------------------------------------------
$archive   = "uptick-lsp-$Version-$target.zip"
$baseUrl   = "https://github.com/$Repo/releases/download/v$Version"
$tmp       = Join-Path ([System.IO.Path]::GetTempPath()) "uptick-install-$([guid]::NewGuid())"
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    $archivePath = Join-Path $tmp $archive
    Note "Downloading $archive"
    Invoke-WebRequest -UseBasicParsing -Uri "$baseUrl/$archive" -OutFile $archivePath

    if (-not $NoVerify) {
        Note 'Verifying sha256'
        $sidecarPath = "$archivePath.sha256"
        Invoke-WebRequest -UseBasicParsing -Uri "$baseUrl/$archive.sha256" -OutFile $sidecarPath
        $expected = ((Get-Content $sidecarPath -Raw).Trim() -split '\s+')[0].ToLower()
        $actual   = (Get-FileHash -Algorithm SHA256 -Path $archivePath).Hash.ToLower()
        if ($expected -ne $actual) {
            Fail "Checksum mismatch: expected $expected, got $actual"
        }
    }

    Note 'Extracting'
    Expand-Archive -Path $archivePath -DestinationPath $tmp -Force
    $exe = Join-Path $tmp 'uptick-lsp.exe'
    if (-not (Test-Path $exe)) { Fail "Archive did not contain expected 'uptick-lsp.exe'" }

    $binDir = Join-Path $Prefix 'bin'
    if (-not (Test-Path $binDir)) { New-Item -ItemType Directory -Path $binDir | Out-Null }
    Copy-Item -Force -Path $exe -Destination (Join-Path $binDir 'uptick-lsp.exe')
    Note "Installed: $(Join-Path $binDir 'uptick-lsp.exe')"

    # --- PATH wiring (User scope) ------------------------------------------
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (-not $userPath) { $userPath = '' }
    $entries  = $userPath -split ';' | Where-Object { $_ -ne '' }
    if ($entries -notcontains $binDir) {
        Note "Appending $binDir to your User PATH"
        $newPath = if ($userPath) { "$userPath;$binDir" } else { $binDir }
        [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
        Write-Host "Open a new terminal for the PATH change to take effect."
    } else {
        Note "$binDir already on User PATH"
    }
} finally {
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $tmp
}

# --- Optional repo clone ----------------------------------------------------
if ($Clone) {
    $cloneDir = Join-Path $env:LOCALAPPDATA 'uptick-zed'
    if (Test-Path (Join-Path $cloneDir '.git')) {
        Note "Repo already cloned at $cloneDir; skipping."
    } else {
        if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
            Fail "git is required for --Clone but was not found on PATH."
        }
        Note "Cloning extension repo to $cloneDir"
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $cloneDir) | Out-Null
        git clone --depth 1 "https://github.com/$Repo.git" $cloneDir
    }
    Write-Host ""
    Write-Host "Next: in Zed, run 'zed: install dev extension' and select:"
    Write-Host "  $cloneDir"
}

Note 'Done.'
