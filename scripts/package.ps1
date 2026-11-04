<#
.SYNOPSIS
  Build Shifter.vst3 (release) and package it as an MSI in dist\.
  Needs Rust (MSVC) and WiX v5: dotnet tool install --global wix --version 5.0.2
#>
param(
  [string] $TargetDir = $(if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { 'D:\ai\Projects\bt-shifter-build' }),
  [switch] $SkipBuild
)
$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$env:CARGO_TARGET_DIR = $TargetDir

$Version = $null
$inPkg = $false
foreach ($line in Get-Content (Join-Path $Root 'Cargo.toml')) {
  if ($line -match '^\s*\[') { $inPkg = ($line.Trim() -eq '[workspace.package]'); continue }
  if ($inPkg -and $line -match '^\s*version\s*=\s*"([^"]+)"') { $Version = $Matches[1]; break }
}
if (-not $Version) { throw 'no [workspace.package] version in Cargo.toml' }

if (-not $SkipBuild) {
  Push-Location $Root
  try {
    cargo build --release --locked -p soundcraft-shifter-vst3 --lib
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
  } finally { Pop-Location }
}
$dll = Join-Path $TargetDir 'release\Shifter.dll'
if (-not (Test-Path $dll)) { throw "not built: $dll" }

$Bundle = Join-Path $TargetDir 'package\Shifter.vst3'
Remove-Item -Recurse -Force $Bundle -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force (Join-Path $Bundle 'Contents\x86_64-win') | Out-Null
New-Item -ItemType Directory -Force (Join-Path $Bundle 'Contents\Resources') | Out-Null
Copy-Item $dll (Join-Path $Bundle 'Contents\x86_64-win\Shifter.vst3')

$Dist = Join-Path $Root 'dist'
New-Item -ItemType Directory -Force $Dist | Out-Null
$Msi = Join-Path $Dist "Shifter-VST3-$Version-x64.msi"
wix build (Join-Path $Root 'installer\shifter.wxs') -arch x64 -d "Version=$Version" -d "BundleDir=$Bundle" -o $Msi
if ($LASTEXITCODE -ne 0) { throw "wix build failed ($LASTEXITCODE)" }
Remove-Item -Force -ErrorAction SilentlyContinue ([IO.Path]::ChangeExtension($Msi, '.wixpdb'))

Write-Output "MSI: $Msi"
Write-Output "SHA256: $((Get-FileHash $Msi -Algorithm SHA256).Hash)"
