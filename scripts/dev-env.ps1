# Loads the toolchain environment needed to build Blanket (and Servo) on Windows:
# MSVC + Windows SDK (via vcvars64.bat), LLVM (clang-cl, lld-link, libclang), CMake, Ninja, Cargo.
#
# Usage:  . .\scripts\dev-env.ps1

$ErrorActionPreference = "Stop"

$vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
if (-not (Test-Path $vswhere)) {
    throw "Visual Studio Build Tools not found. See docs/BUILDING.md."
}
$vsPath = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $vsPath) {
    throw "MSVC x64 build tools are not installed in Visual Studio."
}
$vcvars = Join-Path $vsPath "VC\Auxiliary\Build\vcvars64.bat"

# Import the variables set by vcvars64.bat into this PowerShell session.
cmd /c "`"$vcvars`" >nul && set" | ForEach-Object {
    if ($_ -match "^([^=]+)=(.*)$") {
        Set-Item -Path "env:$($matches[1])" -Value $matches[2]
    }
}

$extra = @(
    (Join-Path $env:USERPROFILE ".cargo\bin"),
    (Join-Path $env:ProgramFiles "LLVM\bin"),
    (Join-Path $env:ProgramFiles "CMake\bin")
)
$ninja = Get-ChildItem (Join-Path $env:LOCALAPPDATA "Microsoft\WinGet\Packages") -Filter ninja.exe -Recurse -ErrorAction SilentlyContinue | Select-Object -First 1
if ($ninja) { $extra += $ninja.DirectoryName }
$env:PATH = ($extra + $env:PATH) -join ";"
$env:LIBCLANG_PATH = Join-Path $env:ProgramFiles "LLVM\bin"

# Stylo's build script runs Python 3 to generate code. Prefer a uv-managed interpreter
# (`uv python install 3.12`) because the Microsoft Store "python.exe" alias is a stub.
if (-not $env:PYTHON3) {
    $uv = Get-Command uv -ErrorAction SilentlyContinue
    if ($uv) {
        $python = & $uv.Source python find 3.12 2>$null
        if ($LASTEXITCODE -eq 0 -and $python) { $env:PYTHON3 = $python.Trim() }
    }
}
if (-not $env:PYTHON3) {
    throw "Python 3 not found. Run 'uv python install 3.12' or set PYTHON3. See docs/BUILDING.md."
}

Write-Host "Blanket build environment ready (MSVC: $vsPath)"
