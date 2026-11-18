# Build the Windows binaries, the portable zip and the installer.
#
# Run from the repository root, in PowerShell:
#
#   powershell -ExecutionPolicy Bypass -File packaging\windows\build.ps1
#
# Needs, once:
#   - Rust (https://rustup.rs). rust-toolchain.toml picks the version.
#   - Visual Studio Build Tools with "Desktop development with C++"
#     (MSVC and the Windows SDK; gpui compiles its shaders with the SDK's
#     fxc.exe), and CMake (it comes with that workload).
#   - NSIS 3, for the installer: winget install NSIS.NSIS
#
# Output lands in dist\: oxidezap-<version>-windows-x86_64-portable.zip and
# oxidezap-<version>-windows-x86_64-instalador.exe.

param(
    [string]$Version = "0.1.0-stickers"
)

$ErrorActionPreference = "Stop"
Set-Location (Resolve-Path "$PSScriptRoot\..\..")

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "cargo not found. Install Rust from https://rustup.rs and open a new terminal."
}

$makensis = (Get-Command makensis -ErrorAction SilentlyContinue).Source
if (-not $makensis) {
    foreach ($candidate in @(
            "${env:ProgramFiles(x86)}\NSIS\makensis.exe",
            "$env:ProgramFiles\NSIS\makensis.exe")) {
        if (Test-Path $candidate) { $makensis = $candidate; break }
    }
}

Write-Host "==> cargo build --release (the first build takes a while)"
cargo build --release --bin oxidezap --bin oxidezapd --bin oxidezap-cli
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$name = "oxidezap-$Version-windows-x86_64"
$dist = Join-Path (Get-Location) "dist"
$stage = Join-Path $dist $name
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force $stage | Out-Null

foreach ($bin in "oxidezap", "oxidezapd", "oxidezap-cli") {
    Copy-Item "target\release\$bin.exe" $stage
}
Copy-Item "crates\gui\assets\fonts\noto-color-emoji\NotoColorEmoji-OFL.txt" $stage
Copy-Item "packaging\windows\LEIA-ME.txt" $stage

$zip = Join-Path $dist "$name-portable.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path $stage -DestinationPath $zip
Write-Host "==> portable: $zip"

if ($makensis) {
    $setup = Join-Path $dist "$name-instalador.exe"
    & $makensis -V2 "-DVERSION=$Version" "-DSTAGE=$stage" "-DOUTFILE=$setup" (Resolve-Path "packaging\windows\installer.nsi").Path
    if ($LASTEXITCODE -ne 0) { throw "makensis failed" }
    Write-Host "==> instalador: $setup"
} else {
    Write-Warning "NSIS not found, so no installer was built. Install it with: winget install NSIS.NSIS"
}
