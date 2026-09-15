<#
.SYNOPSIS
    Build and publish a GitHub Release for SlackInput.
.DESCRIPTION
    1. Runs `cargo build --release`
    2. Copies the binary as SlackInput-win11.exe
    3. Generates a changelog from git commits since the last tag
    4. Creates a GitHub release via `gh release create`
.PARAMETER TagName
    The tag/version to create (e.g. v0.3.0). If omitted, auto-generates from Cargo.toml version.
.PARAMETER Draft
    Create the release as a draft.
.PARAMETER NotesOnly
    Generate UTF-8 release notes without building or publishing.
#>
param(
    [string]$TagName,
    [switch]$Draft,
    [switch]$NotesOnly
)

$ErrorActionPreference = "Stop"

# Git emits UTF-8. Windows PowerShell 5.1 otherwise decodes native stdout
# using the console code page, which can corrupt Chinese commit subjects.
$releaseUtf8 = New-Object System.Text.UTF8Encoding($false)
[Console]::OutputEncoding = $releaseUtf8
$OutputEncoding = $releaseUtf8

# ---------- helpers ----------
function Assert-Command($cmd) {
    if (-not (Get-Command $cmd -ErrorAction SilentlyContinue)) {
        Write-Error "Required command '$cmd' not found. Please install it first."
        exit 1
    }
}

# ---------- pre-checks ----------
if (-not $NotesOnly) {
    Assert-Command "cargo"
    Assert-Command "gh"
}
Assert-Command "git"

# ---------- resolve version tag ----------
if (-not $TagName) {
    $cargoToml = Get-Content -Path "$PSScriptRoot\Cargo.toml" -Raw -Encoding UTF8
    if ($cargoToml -match 'version\s*=\s*"([^"]+)"') {
        $TagName = "v$($Matches[1])"
    } else {
        Write-Error "Cannot determine version from Cargo.toml"
        exit 1
    }
}

# ---------- prepare artifact ----------
$srcExe  = "$PSScriptRoot\target\release\SlackInput.exe"
$distDir = "$PSScriptRoot\dist"
$distExe = "$distDir\SlackInput-win11.exe"

if (-not (Test-Path $distDir)) { New-Item -ItemType Directory -Path $distDir | Out-Null }

if (-not $NotesOnly) {
    Write-Host "==> Building release..." -ForegroundColor Cyan
    cargo build --release --manifest-path "$PSScriptRoot\Cargo.toml"
    if ($LASTEXITCODE -ne 0) { Write-Error "cargo build failed"; exit 1 }

    if (-not (Test-Path $srcExe)) {
        Write-Error "Build artifact not found: $srcExe"
        exit 1
    }

    Copy-Item $srcExe $distExe -Force
    Write-Host "==> Artifact ready: $distExe" -ForegroundColor Cyan
}

# ---------- generate changelog ----------
$tags = @(git -C $PSScriptRoot tag --merged HEAD)
if ($LASTEXITCODE -ne 0) { throw "Cannot read Git tags" }
$lastTag = $null
if ($tags.Count -gt 0) {
    $lastTag = git -C $PSScriptRoot describe --tags --abbrev=0
    if ($LASTEXITCODE -ne 0) { throw "Cannot find previous release tag" }
}
if ($lastTag) {
    $range = "$lastTag..HEAD"
} else {
    $range = "HEAD"
}

$commits = git -C $PSScriptRoot -c i18n.logOutputEncoding=utf-8 log $range --pretty=format:"%s" --no-merges
if ($LASTEXITCODE -ne 0) { throw "Cannot read Git commit subjects" }
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
    $body += "### Features`n"
    foreach ($f in $feats) { $body += "- $f`n" }
    $body += "`n"
}
if ($fixes.Count -gt 0) {
    $body += "### Bug Fixes`n"
    foreach ($f in $fixes) { $body += "- $f`n" }
    $body += "`n"
}
if ($others.Count -gt 0) {
    $body += "### Other Changes`n"
    foreach ($f in $others) { $body += "- $f`n" }
    $body += "`n"
}

Write-Host "==> Release notes:" -ForegroundColor Cyan
Write-Host $body

# A file preserves Unicode and real newlines across PowerShell/gh versions.
$notesPath = Join-Path $distDir "release-notes.md"
[System.IO.File]::WriteAllText($notesPath, $body, $releaseUtf8)
Write-Host "==> UTF-8 release notes: $notesPath" -ForegroundColor Cyan
if ($NotesOnly) { return }

# ---------- create release ----------
$ghArgs = @("release", "create", $TagName, $distExe, "--title", $TagName, "--notes-file", $notesPath)
if ($Draft) { $ghArgs += "--draft" }

Write-Host "==> Creating GitHub release $TagName ..." -ForegroundColor Cyan
gh @ghArgs
if ($LASTEXITCODE -ne 0) { Write-Error "gh release create failed"; exit 1 }

Write-Host "==> Done! Release $TagName published." -ForegroundColor Green
