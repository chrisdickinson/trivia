$ErrorActionPreference = "Stop"

$PluginRoot = if ($env:CLAUDE_PLUGIN_ROOT) { $env:CLAUDE_PLUGIN_ROOT } else { (Resolve-Path "$PSScriptRoot\..").Path }
$BinDir = Join-Path $PluginRoot "bin"
$Bin = Join-Path $BinDir "trivia.exe"

# If binary already exists, we're done
if (Test-Path $Bin) { exit 0 }

New-Item -ItemType Directory -Force -Path $BinDir | Out-Null

$Repo = "chrisdickinson/trivia"
$Target = "x86_64-pc-windows-msvc"
$ReleaseUrl = "https://github.com/$Repo/releases/latest/download/trivia-$Target.zip"
$TmpZip = Join-Path $env:TEMP "trivia-release.zip"

try {
    Invoke-WebRequest -Uri $ReleaseUrl -OutFile $TmpZip -UseBasicParsing
    Expand-Archive -Path $TmpZip -DestinationPath $BinDir -Force
    Remove-Item $TmpZip -ErrorAction SilentlyContinue
    exit 0
} catch {
    Remove-Item $TmpZip -ErrorAction SilentlyContinue
}

# Fallback: build from source
if (Get-Command cargo -ErrorAction SilentlyContinue) {
    Write-Host "Building trivia from source..." -ForegroundColor Yellow

    $WwwDir = Join-Path $PluginRoot "apps\cli\www"
    if (-not (Test-Path (Join-Path $WwwDir "dist")) -and (Get-Command npm -ErrorAction SilentlyContinue)) {
        Push-Location $WwwDir
        npm ci
        npm run build
        Pop-Location
    }

    cargo build --manifest-path (Join-Path $PluginRoot "apps\cli\Cargo.toml") --release --quiet
    Copy-Item (Join-Path $PluginRoot "target\release\trivia.exe") $Bin
    exit 0
}

Write-Error "Error: could not install trivia. Install cargo or download a release."
exit 1
