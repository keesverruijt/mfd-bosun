# One-line install of the latest mfd-bosun on Windows, in PowerShell:
#   irm https://github.com/keesverruijt/mfd-bosun/releases/latest/download/install.ps1 | iex
# Attached as install.ps1 to each GitHub release (see .github/workflows/release.yml); with
# $env:MFD_BOSUN_NIGHTLY = 1 set first it installs the latest build of main instead. Puts mfd-bosun.exe in
# %LOCALAPPDATA%\Programs\mfd-bosun and that folder on the user's PATH; no administrator needed. Run it
# again to update.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'  # Invoke-WebRequest is many times slower with its progress bar

$repo = 'https://github.com/keesverruijt/mfd-bosun/releases'
$url = if ($env:MFD_BOSUN_URL) { $env:MFD_BOSUN_URL }
       elseif ($env:MFD_BOSUN_NIGHTLY) { "$repo/download/nightly" }
       else { "$repo/latest/download" }
$dir = Join-Path $env:LOCALAPPDATA 'Programs\mfd-bosun'
$exe = Join-Path $dir 'mfd-bosun.exe'

# an x86_64 build only: Windows on Arm runs it under emulation
Write-Host 'Downloading mfd-bosun for Windows'
New-Item -ItemType Directory -Force -Path $dir | Out-Null
Get-Process mfd-bosun -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exe } | Stop-Process -Force
Invoke-WebRequest -UseBasicParsing -Uri "$url/mfd-bosun-windows-x86_64.exe" -OutFile $exe

$path = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not (($path -split ';') -contains $dir)) {
  [Environment]::SetEnvironmentVariable('Path', (@($path, $dir) | Where-Object { $_ }) -join ';', 'User')
  $env:Path = "$env:Path;$dir"
}
Write-Host "mfd-bosun is installed in ${dir}: run mfd-bosun to start it (a new window finds it on the PATH)"
