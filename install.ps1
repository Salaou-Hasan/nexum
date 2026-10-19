# Nexum installer (Windows). Usage: iwr .../install.ps1 | iex
#
# It downloads the compiler and hands over to `nx setup`, which asks about
# everything that is needed and then does it: the LLVM toolchain, the
# compiler with the file icon and the file association, and the Visual
# Studio Code extension.
$ErrorActionPreference = "Stop"
$repo = "Salaou-Hasan/nexum"
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "aarch64" } else { "x86_64" }
$exe = Join-Path $env:TEMP "nx-setup-$arch.exe"
Write-Host "Downloading the Nexum setup program ..."
Invoke-WebRequest -Uri "https://github.com/$repo/releases/latest/download/nx-windows-$arch.exe" -OutFile $exe
# A failed download gives the GitHub error page, which is not a program.
$head = [System.IO.File]::ReadAllBytes($exe)[0..1]
if ($head[0] -ne 0x4D -or $head[1] -ne 0x5A) {
    Remove-Item $exe -Force -ErrorAction SilentlyContinue
    Write-Host "The download did not give a program. Check that the release has a Windows binary." -ForegroundColor Red
    exit 1
}
& $exe setup
$code = $LASTEXITCODE
Remove-Item $exe -Force -ErrorAction SilentlyContinue
exit $code
