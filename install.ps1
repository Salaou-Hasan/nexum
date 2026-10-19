# Nexum installer (Windows). Usage: iwr .../install.ps1 | iex
#
# It installs the Nexum MSI: the compiler, the file icon and the editor
# extension. The installer is per user, so it needs no administrator rights.
$ErrorActionPreference = "Stop"
$repo = "Salaou-Hasan/nexum"
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "aarch64" } else { "x86_64" }
$release = Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest"
$tag = $release.tag_name
$version = $tag.TrimStart("v")
$msi = "Nexum-$version-x64.msi"
$url = "https://github.com/$repo/releases/download/$tag/$msi"
$dest = Join-Path $env:TEMP $msi
Write-Host "Downloading $url ..."
Invoke-WebRequest -Uri $url -OutFile $dest
Write-Host "Installing Nexum $version ..."
# The MSI installs the compiler into %LOCALAPPDATA%\Programs\Nexum, puts it
# on the PATH, registers the icon for .nx files and installs the editor
# extension.
$process = Start-Process msiexec.exe -ArgumentList "/i", "$dest", "/quiet", "/norestart" -Wait -PassThru
if ($process.ExitCode -ne 0) {
    Write-Host "The installer failed (exit code $($process.ExitCode))." -ForegroundColor Red
    exit $process.ExitCode
}
$installed = Join-Path $env:LOCALAPPDATA "Programs\Nexum\nx.exe"
Write-Host "Installed Nexum $version to $installed"
Write-Host "The editor extension was installed if Visual Studio Code was found."
