# Builds a portable Windows package of Blanket: dist\Blanket-<version>-windows-x64.zip
#
# The zip holds blanket.exe plus the GStreamer runtime it needs for audio/video in Servo
# tabs: the GStreamer DLLs and the plugins Servo loads from the executable's folder
# (Servo's own plugin lists). Compatibility tabs use the system WebView2 runtime, which
# ships with Windows 10/11, so it is not bundled.
#
# Usage:  . .\scripts\dev-env.ps1; .\scripts\package.ps1 [-SkipBuild]

param([switch]$SkipBuild)

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

$gst = $env:GSTREAMER_1_0_ROOT_MSVC_X86_64
if (-not $gst) { $gst = "C:\Program Files\gstreamer\1.0\msvc_x86_64\" }
$gst = $gst.TrimEnd('\')
if (-not (Test-Path "$gst\bin\gstreamer-1.0-0.dll")) {
    throw "GStreamer MSVC x86_64 (runtime + development) not found at $gst. See docs/BUILDING.md."
}

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value

if (-not $SkipBuild) {
    $env:GSTREAMER_1_0_ROOT_MSVC_X86_64 = "$gst\"
    $env:PKG_CONFIG = "$gst\bin\pkg-config.exe"
    $env:PKG_CONFIG_PATH = "$gst\lib\pkgconfig"
    $env:PATH = "$gst\bin;$env:PATH"
    $ErrorActionPreference = "Continue"
    cargo build -p blanket --release --features media
    if ($LASTEXITCODE -ne 0) { throw "build failed" }
    $ErrorActionPreference = "Stop"
}

$name = "Blanket-$version-windows-x64"
$stage = Join-Path $repo "dist\$name"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force $stage | Out-Null

Copy-Item target\release\blanket.exe $stage

# ANGLE (EGL over Direct3D), which Servo's rendering loads at startup. The mozangle
# build script leaves it in its OUT_DIR; without it the browser cannot open a window.
$angle = Get-ChildItem target\release\build -Recurse -Filter libEGL.dll |
    Where-Object { $_.DirectoryName -like "*mozangle-*" } |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $angle) { throw "ANGLE (libEGL.dll) not found under target\release\build" }
Copy-Item (Join-Path $angle.DirectoryName "libEGL.dll"), (Join-Path $angle.DirectoryName "libGLESv2.dll") $stage

# GStreamer core and its dependencies.
Copy-Item "$gst\bin\*.dll" $stage

# The plugins Servo loads, from Servo's own lists for this version.
$servoSrc = Get-ChildItem "$env:USERPROFILE\.cargo\registry\src\*\servo-0.8.0" -Directory | Select-Object -First 1
$plugins = foreach ($list in "common.rs.in", "windows.rs.in") {
    Get-Content (Join-Path $servoSrc.FullName "gstreamer_plugin_lists\$list") |
        Select-String -Pattern '"([^"]+)"' -AllMatches |
        ForEach-Object { $_.Matches } | ForEach-Object { $_.Groups[1].Value }
}
$missing = @()
foreach ($plugin in $plugins | Sort-Object -Unique) {
    $file = "$gst\lib\gstreamer-1.0\$plugin.dll"
    if (Test-Path $file) { Copy-Item $file $stage } else { $missing += $plugin }
}
if ($missing) { Write-Warning "GStreamer plugins not found (skipped): $($missing -join ', ')" }

Copy-Item README.md, LICENSE* $stage -ErrorAction SilentlyContinue

$zip = Join-Path $repo "dist\$name.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path "$stage\*" -DestinationPath $zip -CompressionLevel Optimal
$size = "{0:N0} MB" -f ((Get-Item $zip).Length / 1MB)
$hash = (Get-FileHash $zip -Algorithm SHA256).Hash
Write-Host "Package: $zip ($size)"
Write-Host "SHA256:  $hash"

# Installer (Inno Setup 6): dist\Blanket-<version>-setup.exe
$iscc = @("$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe", "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe") |
    Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) {
    Write-Warning "Inno Setup 6 not found (winget install JRSoftware.InnoSetup); skipping the installer."
    return
}
$icon = Get-ChildItem target\release\build -Recurse -Filter blanket.ico | Sort-Object LastWriteTime -Descending | Select-Object -First 1
& $iscc /Qp "/DAppVersion=$version" "/DSourceDir=$stage" "/DIconFile=$($icon.FullName)" "/O$(Join-Path $repo 'dist')" packaging\installer\blanket.iss
if ($LASTEXITCODE -ne 0) { throw "installer build failed" }
$setup = Join-Path $repo "dist\Blanket-$version-setup.exe"
Write-Host ("Setup:   $setup ({0:N0} MB)" -f ((Get-Item $setup).Length / 1MB))
Write-Host "SHA256:  $((Get-FileHash $setup -Algorithm SHA256).Hash)"
