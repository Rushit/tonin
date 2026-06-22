# install.ps1 — download and install the tonin CLI on Windows (PowerShell).
#
# Mirrors scripts/install.sh. `tonin upgrade` invokes this automatically on
# Windows; you can also run it directly.
#
# Usage:
#   # Install latest to %USERPROFILE%\.cargo\bin
#   irm https://raw.githubusercontent.com/Rushit/tonin/main/scripts/install.ps1 | iex
#
#   # With arguments, download then run:
#   $s = "$env:TEMP\tonin-install.ps1"
#   irm https://raw.githubusercontent.com/Rushit/tonin/main/scripts/install.ps1 -OutFile $s
#   & $s -WithToninHelm
#   & $s -Plugin "Rushit/tonin-helm"
#   & $s -Version v0.6.1 -Dir C:\tools\bin

[CmdletBinding()]
param(
    [string] $Version      = "",   # empty = latest release from GitHub
    [string] $Dir          = "",   # empty = %USERPROFILE%\.cargo\bin
    [string] $Plugin       = "",   # comma-separated: owner/repo[@vX.Y.Z],owner/repo2
    [switch] $WithToninHelm,       # back-compat alias for -Plugin Rushit/tonin-helm
    [string] $HelmVersion  = ""    # back-compat: pin tonin-helm with -WithToninHelm
)

$ErrorActionPreference = "Stop"
$RepoTonin = "Rushit/tonin"
$RepoHelm  = "Rushit/tonin-helm"

function Say($m) { Write-Host $m -ForegroundColor Cyan }
function Ok($m)  { Write-Host "[ok] $m" -ForegroundColor Green }
function Die($m) { Write-Error $m; exit 1 }

# ---------------------------------------------------------------------------
# Detect architecture -> Rust target triple. Only x86_64 Windows is published.
# ---------------------------------------------------------------------------
switch ($env:PROCESSOR_ARCHITECTURE) {
    "AMD64" { $target = "x86_64-pc-windows-msvc" }
    default {
        Die "Unsupported Windows architecture: $($env:PROCESSOR_ARCHITECTURE) (no pre-built binary; use 'cargo install tonin')."
    }
}

if (-not $Dir) { $Dir = Join-Path $env:USERPROFILE ".cargo\bin" }
New-Item -ItemType Directory -Force -Path $Dir | Out-Null

# ---------------------------------------------------------------------------
# Latest release tag for owner/repo via the GitHub API.
# ---------------------------------------------------------------------------
function Get-LatestTag($repo) {
    $url = "https://api.github.com/repos/$repo/releases/latest"
    return (Invoke-RestMethod -Uri $url -Headers @{ "User-Agent" = "tonin-install" }).tag_name
}

# ---------------------------------------------------------------------------
# Download + install one binary.
# ---------------------------------------------------------------------------
function Install-Binary($repo, $bin, $version, $target, $destDir) {
    $dest = Join-Path $destDir "$bin.exe"
    $want = $version.TrimStart("v")

    if (Test-Path $dest) {
        $current = ""
        try { $current = ((& $dest --version 2>$null) | Select-Object -First 1).Split(" ")[-1] } catch { }
        if ($current -and ($current -eq $want)) { Ok "$bin $current is already up to date."; return }
        if ($current) { Say "Upgrading ${bin}: v$current -> $version..." } else { Say "Installing $bin $version..." }
    } else {
        Say "Installing $bin $version..."
    }

    $url = "https://github.com/$repo/releases/download/$version/$bin-$target.zip"
    $tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("tonin-" + [System.IO.Path]::GetRandomFileName())
    New-Item -ItemType Directory -Force -Path $tmp | Out-Null
    try {
        $zip = Join-Path $tmp "archive.zip"
        Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $zip
        Expand-Archive -Path $zip -DestinationPath $tmp -Force
        $exe = Get-ChildItem -Path $tmp -Recurse -Filter "$bin.exe" | Select-Object -First 1
        if (-not $exe) { Die "Binary '$bin.exe' not found in archive." }

        # Windows refuses to overwrite a running .exe, but it DOES allow
        # renaming one. Move the current binary aside so `tonin upgrade` can
        # replace its own exe, then drop the new one in place. The stale
        # ".old-*" copy can't be deleted while the old process runs — clean it
        # up best-effort now and on the next run.
        Get-ChildItem -Path $destDir -Filter "$bin.exe.old-*" -ErrorAction SilentlyContinue |
            Remove-Item -Force -ErrorAction SilentlyContinue
        if (Test-Path $dest) {
            $old = "$dest.old-$PID"
            Move-Item -Path $dest -Destination $old -Force
            Remove-Item -Path $old -Force -ErrorAction SilentlyContinue
        }
        Copy-Item -Path $exe.FullName -Destination $dest -Force
        Ok "Installed $bin $version -> $dest"
    } finally {
        Remove-Item -Path $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
Say "Target:      $target"
Say "Install dir: $Dir"

if (-not $Version) { Say "Fetching latest tonin version..."; $Version = Get-LatestTag $RepoTonin }
if (-not $Version) { Die "Could not determine latest version. Pass -Version vX.Y.Z." }
Install-Binary $RepoTonin "tonin" $Version $target $Dir

# Plugins. Back-compat: -WithToninHelm [-HelmVersion vX] == -Plugin Rushit/tonin-helm[@vX].
$specs = @()
$specs += ($Plugin -split "," | ForEach-Object { $_.Trim() } | Where-Object { $_ -ne "" })
if ($WithToninHelm) {
    if ($HelmVersion) { $specs += "$RepoHelm@$HelmVersion" } else { $specs += $RepoHelm }
}

$installed = @()
foreach ($spec in $specs) {
    $repo = $spec
    $pv   = ""
    if ($spec.Contains("@")) { $parts = $spec.Split("@", 2); $repo = $parts[0]; $pv = $parts[1] }
    $bin = $repo.Split("/")[-1]   # owner/tonin-helm -> tonin-helm
    if (-not $pv) { Say "Fetching latest $bin version..."; $pv = Get-LatestTag $repo }
    if (-not $pv) { Die "Could not determine latest $bin version. Pass $repo@vX.Y.Z." }
    Install-Binary $repo $bin $pv $target $Dir
    $installed += $bin
}

if (($env:PATH -split ";") -notcontains $Dir) {
    Write-Host ""
    Write-Host "Note: $Dir is not on your PATH. Add it, e.g.:" -ForegroundColor Yellow
    Write-Host "        [Environment]::SetEnvironmentVariable('Path', `"$Dir;`" + [Environment]::GetEnvironmentVariable('Path','User'), 'User')"
}

Write-Host ""
Ok "Done! Run 'tonin --version' to verify."
foreach ($bin in $installed) {
    $name = $bin -replace "^tonin-", ""
    Ok "Run 'tonin $name --tonin-describe' to verify $bin."
}
