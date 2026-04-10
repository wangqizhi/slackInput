<#
.SYNOPSIS
    Build and publish a GitHub Release for SlackInput.
.DESCRIPTION
    1. Runs `cargo build --release`
    2. Copies the binary as SlackInput-win11.exe
    3. Generates a changelog from git commits since the last tag
    4. Creates a GitHub release via `gh release create`
.PARAMETER TagName
    The tag/version to create (e.g. v0.1.0). If omitted, auto-generates from Cargo.toml version.
.PARAMETER Draft
    Create the release as a draft.
#>
param(
    [string]$TagName,
    [switch]$Draft
)

$ErrorActionPreference = "Stop"

# ---------- helpers ----------
function Assert-Command($cmd) {
    if (-not (Get-Command $cmd -ErrorAction SilentlyContinue)) {
        Write-Error "Required command '$cmd' not found. Please install it first."
        exit 1
    }
}

# ---------- pre-checks ----------
Assert-Command "cargo"
Assert-Command "gh"
Assert-Command "git"

# ---------- resolve version tag ----------
if (-not $TagName) {
    $cargoToml = Get-Content -Path "$PSScriptRoot\Cargo.toml" -Raw
    if ($cargoToml -match 'version\s*=\s*"([^"]+)"') {
        $TagName = "v$($Matches[1])"
    } else {
        Write-Error "Cannot determine version from Cargo.toml"
        exit 1
    }
}

Write-Host "==> Building release..." -ForegroundColor Cyan
cargo build --release
if ($LASTEXITCODE -ne 0) { Write-Error "cargo build failed"; exit 1 }

# ---------- prepare artifact ----------
$srcExe  = "$PSScriptRoot\target\release\SlackInput.exe"
$distDir = "$PSScriptRoot\dist"
$distExe = "$distDir\SlackInput-win11.exe"

if (-not (Test-Path $srcExe)) {
    Write-Error "Build artifact not found: $srcExe"
    exit 1
}

if (-not (Test-Path $distDir)) { New-Item -ItemType Directory -Path $distDir | Out-Null }
Copy-Item $srcExe $distExe -Force
Write-Host "==> Artifact ready: $distExe" -ForegroundColor Cyan

# ---------- generate changelog ----------
$lastTag = git describe --tags --abbrev=0 2>$null
if ($lastTag) {
    $range = "$lastTag..HEAD"
} else {
    $range = "HEAD"
}

$commits = git log $range --pretty=format:"%s" --no-merges 2>$null
if (-not $commits) { $commits = @() }
if ($commits -is [string]) { $commits = @($commits) }

$feats   = [System.Collections.ArrayList]::new()
$fixes   = [System.Collections.ArrayList]::new()
$others  = [System.Collections.ArrayList]::new()

foreach ($msg in $commits) {
    if ($msg -match '^feat[\(:]') {
        [void]$feats.Add($msg)
    } elseif ($msg -match '^fix[\(:]') {
        [void]$fixes.Add($msg)
    } else {
        [void]$others.Add($msg)
    }
}

$body = "## What's Changed`n`n"
if ($feats.Count -gt 0) {
    $body += "### 🚀 Features`n"
    foreach ($f in $feats) { $body += "- $f`n" }
    $body += "`n"
}
if ($fixes.Count -gt 0) {
    $body += "### 🐛 Bug Fixes`n"
    foreach ($f in $fixes) { $body += "- $f`n" }
    $body += "`n"
}
if ($others.Count -gt 0) {
    $body += "### 📦 Other Changes`n"
    foreach ($f in $others) { $body += "- $f`n" }
    $body += "`n"
}

Write-Host "==> Release notes:" -ForegroundColor Cyan
Write-Host $body

# ---------- create release ----------
$ghArgs = @("release", "create", $TagName, $distExe, "--title", $TagName, "--notes", $body)
if ($Draft) { $ghArgs += "--draft" }

Write-Host "==> Creating GitHub release $TagName ..." -ForegroundColor Cyan
gh @ghArgs
if ($LASTEXITCODE -ne 0) { Write-Error "gh release create failed"; exit 1 }

Write-Host "==> Done! Release $TagName published." -ForegroundColor Green
