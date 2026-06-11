# install.ps1 — download the latest mcmove.exe from GitHub Releases and put it on PATH.
#
# One-liner (no files to download first):
#   powershell -ExecutionPolicy Bypass -Command "irm https://raw.githubusercontent.com/zeriaxdev/mcmove/main/install.ps1 | iex"
#
# Or save this file and: right-click -> Run with PowerShell.
# No admin rights needed — installs for the current user only.

$ErrorActionPreference = "Stop"
try { [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 } catch {}

$repo = "zeriaxdev/mcmove"
$headers = @{ "User-Agent" = "mcmove-installer" }

Write-Host "Finding the latest mcmove release..."
# Use the list endpoint (newest first) so pre-releases like 0.6.0-alpha.1 are included.
$releases = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases" -Headers $headers
$rel = $releases | Where-Object { -not $_.draft } | Select-Object -First 1
if (-not $rel) { throw "no releases found for $repo" }

$asset = $rel.assets | Where-Object { $_.name -like "*windows*.exe" } | Select-Object -First 1
if (-not $asset) { $asset = $rel.assets | Where-Object { $_.name -like "*.exe" } | Select-Object -First 1 }
if (-not $asset) { throw "release $($rel.tag_name) has no .exe asset" }

$dest = Join-Path $env:LOCALAPPDATA "Programs\mcmove"
New-Item -ItemType Directory -Force -Path $dest | Out-Null
$exe = Join-Path $dest "mcmove.exe"

Write-Host "Downloading $($asset.name)  ($($rel.tag_name))..."
Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $exe -Headers $headers
Unblock-File $exe   # clear the downloaded-from-internet mark so SmartScreen is quieter

# Add the folder to the USER PATH (persistent), only if it isn't already there.
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($userPath -notlike "*$dest*") {
    $newPath = if ([string]::IsNullOrEmpty($userPath)) { $dest } else { "$userPath;$dest" }
    [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
    Write-Host "Added to your PATH: $dest" -ForegroundColor Green
} else {
    Write-Host "Already on your PATH: $dest" -ForegroundColor Yellow
}
$env:Path = "$env:Path;$dest"   # usable in this window immediately

Write-Host ""
Write-Host "Installed mcmove $($rel.tag_name)." -ForegroundColor Green
& $exe --version
Write-Host ""
Write-Host "Open a NEW terminal, then e.g.:"
Write-Host "    mcmove pack apply <code-or-file> `"C:\Path\To\Your\Instance`" --dry-run"
