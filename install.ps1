# Nexum installer (Windows). Usage: iwr .../install.ps1 | iex
$ErrorActionPreference = "Stop"
$repo = "Salaou-Hasan/nexum"
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "aarch64" } else { "x86_64" }
$asset = "nx-windows-$arch.exe"
$latest = (Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest").tag_name
$url = "https://github.com/$repo/releases/download/$latest/$asset"
$dest = Join-Path $env:LOCALAPPDATA "Nexum"
New-Item -ItemType Directory -Force -Path $dest | Out-Null
$out = Join-Path $dest "nx.exe"
Write-Host "Downloading $url ..."
Invoke-WebRequest -Uri $url -OutFile $out
# Add to user PATH if missing
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if (-not ($userPath -split ";" | Where-Object { $_ -eq $dest })) {
    [Environment]::SetEnvironmentVariable("Path", "$userPath;$dest", "User")
    Write-Host "Added $dest to user PATH (restart shell to take effect)."
}
Write-Host "Installed nx to $out"