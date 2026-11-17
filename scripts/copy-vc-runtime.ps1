# Copies the x64 Visual C++ runtime DLLs into <target> (app-local, next to obs.dll and
# replayr-obs.exe), so the OBS helper also starts on a Windows without the VC++
# Redistributable.
#
#   scripts/copy-vc-runtime.ps1 <target-dir>      e.g. scripts/copy-vc-runtime.ps1 obs
#
# The set is what the PE import tables of fetch-obs.sh's output (OBS 32.1.0) and the
# helper ask for: vcruntime140.dll (nearly every file, and the helper), vcruntime140_1.dll
# and msvcp140.dll (the C++ modules: libobs-d3d11, libobs-winrt, win-wasapi, obs-ffmpeg,
# obs-qsv11, srt, the encoder test exes). Nothing imports concrt140, msvcp140_1 or
# msvcp140_2; the 32-bit game capture files link the runtime statically. The Universal
# CRT (api-ms-win-crt-*, ucrtbase) is part of Windows 10 and later.
#
# Takes them from the newest VC\Redist\MSVC\<version>\x64\Microsoft.VC14*.CRT of the
# installed Visual Studio (found with vswhere and under %ProgramFiles%\Microsoft Visual
# Studio) and fails when there is none. Runs in Windows PowerShell 5.1 and pwsh 7.
param([Parameter(Mandatory = $true)][string]$Target)
$ErrorActionPreference = 'Stop'

$dlls = 'vcruntime140.dll', 'vcruntime140_1.dll', 'msvcp140.dll'

if (-not (Test-Path -LiteralPath $Target -PathType Container)) {
    throw "target folder '$Target' does not exist (run fetch-obs.sh first)"
}

# Visual Studio installation folders.
$roots = @()
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
if (Test-Path -LiteralPath $vswhere) {
    $roots += @(& $vswhere -products '*' -property installationPath)
    if ($LASTEXITCODE -ne 0) {
        Write-Warning "vswhere exited with $LASTEXITCODE; searching $env:ProgramFiles only"
        $global:LASTEXITCODE = 0
    }
}
$vsDir = Join-Path $env:ProgramFiles 'Microsoft Visual Studio'
if (Test-Path -LiteralPath $vsDir) {
    # <year or major version>\<edition>, e.g. 2022\Enterprise
    $roots += @(Get-ChildItem -LiteralPath $vsDir -Directory -ErrorAction SilentlyContinue |
        ForEach-Object { Get-ChildItem -LiteralPath $_.FullName -Directory -ErrorAction SilentlyContinue } |
        ForEach-Object { $_.FullName })
}
$roots = @($roots | Where-Object { $_ } | Select-Object -Unique)

# Every VC\Redist\MSVC\<version>\x64\Microsoft.VC14*.CRT; only numeric version folders.
$candidates = @(foreach ($root in $roots) {
    $redist = Join-Path $root 'VC\Redist\MSVC'
    if (-not (Test-Path -LiteralPath $redist)) { continue }
    foreach ($ver in @(Get-ChildItem -LiteralPath $redist -Directory | Where-Object { $_.Name -match '^\d+(\.\d+){1,3}$' })) {
        $x64 = Join-Path $ver.FullName 'x64'
        if (-not (Test-Path -LiteralPath $x64)) { continue }
        foreach ($crt in @(Get-ChildItem -LiteralPath $x64 -Directory | Where-Object { $_.Name -like 'Microsoft.VC14*.CRT' })) {
            [pscustomobject]@{ Version = [version]$ver.Name; Path = $crt.FullName }
        }
    }
})

# The newest one that has all the DLLs.
$source = $null
foreach ($c in @($candidates | Sort-Object Version -Descending)) {
    $missing = @($dlls | Where-Object { -not (Test-Path -LiteralPath (Join-Path $c.Path $_)) })
    if ($missing.Count -eq 0) { $source = $c; break }
    Write-Warning "$($c.Path) lacks $($missing -join ', ')"
}
if (-not $source) {
    throw ("no Visual C++ runtime found: no VC\Redist\MSVC\<version>\x64\Microsoft.VC14*.CRT with " +
        "$($dlls -join ', ') under: $(if ($roots) { $roots -join '; ' } else { '(no Visual Studio found)' })")
}
Write-Host "Using $($source.Path)"

foreach ($name in $dlls) {
    $from = Join-Path $source.Path $name
    $to = Join-Path $Target $name
    Copy-Item -LiteralPath $from -Destination $to -Force
    if (-not (Test-Path -LiteralPath $to -PathType Leaf)) { throw "copying $name to $to failed" }
    Write-Host "Copied $name $((Get-Item -LiteralPath $to).VersionInfo.FileVersion)"
}
