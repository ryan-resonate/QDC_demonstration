# Build the qdc-core WASM artifact and emit it to ./pkg/ for the static site.
#
# Usage:
#   pwsh ./build.ps1            # release build (default)
#   pwsh ./build.ps1 -Dev       # faster dev build, larger artifact

param(
    [switch]$Dev
)

$ErrorActionPreference = "Stop"
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $scriptDir

$mode = if ($Dev) { "--dev" } else { "--release" }

Write-Host "[build.ps1] wasm-pack build crates/qdc-core $mode --target web --out-dir ../../pkg"
wasm-pack build crates/qdc-core $mode --target web --out-dir ../../pkg --out-name qdc_core
if ($LASTEXITCODE -ne 0) {
    Write-Error "wasm-pack failed (exit $LASTEXITCODE)"
    exit $LASTEXITCODE
}

Write-Host "[build.ps1] artifact:"
Get-ChildItem ./pkg | Format-Table Name, Length
