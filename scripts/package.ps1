<#
.SYNOPSIS
  Build the VST3 plugins (release) and package each as an MSI in dist\.
  Needs Rust (MSVC) and WiX v5: dotnet tool install --global wix --version 5.0.2
.EXAMPLE
  scripts\package.ps1 -Plugin Plumb
#>
param(
  [ValidateSet('Shifter', 'Plumb', 'Grit', 'All')] [string] $Plugin = 'All',
  [string] $TargetDir = $(if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { 'D:\ai\Projects\bt-shifter-build' }),
  [switch] $SkipBuild
)
$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$env:CARGO_TARGET_DIR = $TargetDir

# Fixed GUIDs keep upgrades working across versions; never change them.
$Plugins = @{
  Shifter = @{ Crate = 'soundcraft-shifter-vst3'; Upgrade = '7C3D9A52-4B1E-4F86-A0D7-2E5B8C61F394'; Bin = 'B4E1A7D3-62C9-4D0F-9A58-1F7E3C2D8B60'; Res = 'D96F2C18-E3A4-47B5-8C71-5A0B9E4F6D23' }
  Plumb   = @{ Crate = 'soundcraft-plumb-vst3';   Upgrade = '3A8E5C17-9D24-4B6F-8E01-C7F2A5D93B48'; Bin = '5F2B8D61-A3C7-4E90-B1D4-6C8E0F7A2395'; Res = 'E1C4A9F2-7B36-4D58-9A0E-3D5F8B2C6174' }
  Grit    = @{ Crate = 'soundcraft-grit-vst3';    Upgrade = '9B6D2E84-1F57-4A3C-B890-D4E7C1A56F23'; Bin = '2D9F7A15-C8E4-4B63-A5F1-8E0B3C6D7492'; Res = 'A7E3C5B8-4F19-4D26-8C70-1B9D6F2E4A85' }
}

$Version = $null
$inPkg = $false
foreach ($line in Get-Content (Join-Path $Root 'Cargo.toml')) {
  if ($line -match '^\s*\[') { $inPkg = ($line.Trim() -eq '[workspace.package]'); continue }
  if ($inPkg -and $line -match '^\s*version\s*=\s*"([^"]+)"') { $Version = $Matches[1]; break }
}
if (-not $Version) { throw 'no [workspace.package] version in Cargo.toml' }

$Names = if ($Plugin -eq 'All') { 'Shifter', 'Plumb', 'Grit' } else { @($Plugin) }
$Dist = Join-Path $Root 'dist'
New-Item -ItemType Directory -Force $Dist | Out-Null

foreach ($Name in $Names) {
  $P = $Plugins[$Name]
  if (-not $SkipBuild) {
    Push-Location $Root
    try {
      cargo build --release --locked -p $P.Crate --lib
      if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
    } finally { Pop-Location }
  }
  $dll = Join-Path $TargetDir "release\$Name.dll"
  if (-not (Test-Path $dll)) { throw "not built: $dll" }

  $Bundle = Join-Path $TargetDir "package\$Name.vst3"
  Remove-Item -Recurse -Force $Bundle -ErrorAction SilentlyContinue
  New-Item -ItemType Directory -Force (Join-Path $Bundle 'Contents\x86_64-win') | Out-Null
  New-Item -ItemType Directory -Force (Join-Path $Bundle 'Contents\Resources') | Out-Null
  Copy-Item $dll (Join-Path $Bundle "Contents\x86_64-win\$Name.vst3")

  $Msi = Join-Path $Dist "$Name-VST3-$Version-x64.msi"
  wix build (Join-Path $Root 'installer\plugin.wxs') -arch x64 -d "Name=$Name" -d "Version=$Version" -d "BundleDir=$Bundle" `
    -d "UpgradeCode=$($P.Upgrade)" -d "GuidBin=$($P.Bin)" -d "GuidRes=$($P.Res)" -o $Msi
  if ($LASTEXITCODE -ne 0) { throw "wix build failed ($LASTEXITCODE)" }
  Remove-Item -Force -ErrorAction SilentlyContinue ([IO.Path]::ChangeExtension($Msi, '.wixpdb'))

  Write-Output "MSI: $Msi"
  Write-Output "SHA256: $((Get-FileHash $Msi -Algorithm SHA256).Hash)"
}
