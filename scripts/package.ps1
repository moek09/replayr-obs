# Builds replayr-obs.exe and assembles the folder Replayr ships as obs\ next to
# replayr.exe, then zips it.
#
#   scripts/package.ps1 <version>      e.g. scripts/package.ps1 0.1.0
#
# Leaves obs\ and dist\replayr-obs-<version>-windows-x64.zip (+ .sha256) in the
# repository root. Needs Git Bash (for fetch-obs.sh), Python 3, Rust (MSVC) and
# Visual Studio (for the VC++ runtime). Runs in Windows PowerShell 5.1 and pwsh 7.
param([Parameter(Mandatory = $true)][string]$Version)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

function Invoke-Checked([string]$what, [scriptblock]$run) {
    & $run
    if ($LASTEXITCODE -ne 0) { throw "$what failed (exit code $LASTEXITCODE)" }
}

# Git Bash, not WSL's bash.exe, which `bash` can resolve to on Windows.
$bash = Join-Path $env:ProgramFiles 'Git\bin\bash.exe'
if (-not (Test-Path -LiteralPath $bash)) { $bash = 'bash' }
$python = if (Get-Command python -ErrorAction SilentlyContinue) { 'python' } else { 'python3' }

Invoke-Checked 'fetch-obs.sh' { & $bash scripts/fetch-obs.sh obs }
& "$PSScriptRoot\copy-vc-runtime.ps1" obs
Invoke-Checked 'cargo build' { cargo build --release -p replayr-obs-host }
Copy-Item target\release\replayr-obs.exe obs\replayr-obs.exe

# The licences and where the source is, next to the binaries.
Copy-Item LICENSE obs\LICENSE-replayr-obs.txt
$commit = (git rev-parse HEAD).Trim()
$repo = if ($env:GITHUB_REPOSITORY) { "$env:GITHUB_SERVER_URL/$env:GITHUB_REPOSITORY" } else { 'https://github.com/moek09/replayr-obs' }
@"
replayr-obs $Version (commit $commit)

replayr-obs.exe is free software under the GNU General Public License,
version 3 or (at your option) any later version: LICENSE-replayr-obs.txt.
Its source code, exactly as built here:
$repo/tree/$commit
The Rust crates it uses (libobs-rs and others) are listed with their
versions in Cargo.lock there; their sources are on https://crates.io.

OBS Studio 32.1.0 (GNU GPL 2.0 or later, LICENSE-OBS.txt), unmodified and
pruned to what replayr-obs.exe needs, from
https://github.com/obsproject/obs-studio/releases/tag/32.1.0
Source: https://github.com/obsproject/obs-studio/tree/32.1.0
The libraries OBS builds for its release (FFmpeg, x264, ...):
https://github.com/obsproject/obs-deps

The Visual C++ runtime DLLs are redistributed under the Visual Studio
redistributable license terms.
"@ | Set-Content -Encoding utf8 obs\SOURCE.txt

# What Replayr's installer and the Modern engine need.
foreach ($f in 'replayr-obs.exe', 'obs.dll', 'obs-ffmpeg-mux.exe', 'LICENSE-OBS.txt',
               'LICENSE-replayr-obs.txt', 'SOURCE.txt',
               'vcruntime140.dll', 'vcruntime140_1.dll', 'msvcp140.dll',
               'obs-plugins\64bit\win-capture.dll', 'data\libobs\default.effect') {
    if (-not (Test-Path "obs\$f")) { throw "obs\$f is missing" }
}

# Python's zipfile writes forward slashes, so the zip unpacks the same everywhere.
New-Item -ItemType Directory -Force dist | Out-Null
$name = "replayr-obs-$Version-windows-x64.zip"
$zip = Join-Path $root "dist\$name"
Remove-Item -LiteralPath $zip -ErrorAction SilentlyContinue
Invoke-Checked 'zip' { & $python -I -c 'import shutil, sys; shutil.make_archive(sys.argv[1], "zip", sys.argv[2])' ($zip -replace '\.zip$', '') (Join-Path $root 'obs') }
$sha = (Get-FileHash -Algorithm SHA256 -LiteralPath $zip).Hash.ToLowerInvariant()
"$sha  $name" | Set-Content -Encoding ascii "$zip.sha256"
Write-Host "$name ($([math]::Round((Get-Item $zip).Length / 1MB, 1)) MB)"
Write-Host "SHA-256: $sha"
