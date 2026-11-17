#!/usr/bin/env bash
# Assembles the pruned OBS Studio folder that ships next to replayr.exe as obs\.
#
#   scripts/fetch-obs.sh <target-dir>      e.g. scripts/fetch-obs.sh obs
#
# Downloads the official OBS Studio Windows x64 zip from the obsproject/obs-studio
# GitHub release, checks its SHA-256, and copies only what replayr-obs.exe needs
# (libobs, the d3d11/winrt graphics modules, game/window/monitor capture, WASAPI
# audio, the FFmpeg/x264/NVENC/QSV encoders, image source) plus every DLL those
# import. Runs in Git Bash on Windows and on Linux; needs bash, curl,
# sha256sum and Python 3. replayr-obs.exe itself is copied in by scripts/package.ps1.
#
# The zip is cached in $OBS_CACHE_DIR (default: .obs-cache in the repository).
set -euo pipefail

OBS_VERSION="32.1.0"
OBS_ASSET="OBS-Studio-${OBS_VERSION}-Windows-x64.zip"
OBS_URL="https://github.com/obsproject/obs-studio/releases/download/${OBS_VERSION}/${OBS_ASSET}"
# SHA-256 of the zip above (computed once from the release download).
OBS_SHA256="7c742f250dc1355ca4a7ccfbcd07a1b63f53ea46f4e052bce2426d8d324ee90d"

# Modules replayr-obs.exe uses: obs-plugins/64bit/<name>.dll + data/obs-plugins/<name>/.
PLUGINS=(win-capture win-wasapi obs-ffmpeg obs-x264 obs-nvenc obs-qsv11 image-source)
# Next to obs.dll: graphics modules and the helper processes OBS starts.
BIN_EXTRA=(libobs-d3d11.dll libobs-winrt.dll obs-ffmpeg-mux.exe obs-nvenc-test.exe obs-amf-test.exe obs-qsv-test.exe)

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
[ $# -eq 1 ] || { echo "usage: $0 <target-dir>" >&2; exit 2; }
target="$1"

# The first Python 3 that runs: on Windows "python3" is often the Microsoft
# Store stub, which only prints an install hint.
PY=""
for candidate in python3 python py; do
    if command -v "$candidate" >/dev/null 2>&1 \
        && "$candidate" -c 'import sys; sys.exit(sys.version_info[0] != 3)' >/dev/null 2>&1; then
        PY="$(command -v "$candidate")"
        break
    fi
done
[ -n "$PY" ] || { echo "Python 3 is needed" >&2; exit 1; }

cache="${OBS_CACHE_DIR:-$here/../.obs-cache}"
mkdir -p "$cache"
zip="$cache/$OBS_ASSET"

if [ ! -f "$zip" ] || [ "$(sha256sum "$zip" | cut -d' ' -f1)" != "$OBS_SHA256" ]; then
    echo "Downloading $OBS_URL"
    rm -f "$zip"
    curl -fL --retry 3 --retry-delay 10 -o "$zip" "$OBS_URL"
fi
actual="$(sha256sum "$zip" | cut -d' ' -f1)"
if [ "$actual" != "$OBS_SHA256" ]; then
    echo "SHA-256 mismatch for $OBS_ASSET" >&2
    echo "  expected $OBS_SHA256" >&2
    echo "  actual   $actual" >&2
    exit 1
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
echo "Extracting"
"$PY" -I -c 'import sys, zipfile; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])' "$zip" "$work"

src_bin="$work/bin/64bit"
src_plug="$work/obs-plugins/64bit"
[ -f "$src_bin/obs.dll" ] || { echo "obs.dll not found in the zip" >&2; exit 1; }

# DLLs the kept binaries import, resolved recursively inside bin/64bit.
roots=(obs.dll "${BIN_EXTRA[@]}")
plugin_files=()
for p in "${PLUGINS[@]}"; do
    [ -f "$src_plug/$p.dll" ] || { echo "plugin $p.dll not found in the zip" >&2; exit 1; }
    plugin_files+=("$src_plug/$p.dll")
done
# Windows Python ends lines with \r\n, and Git Bash can keep the \r on a
# name ("missing avcodec-61.dll\r in the zip"): tr drops every \r.
deps=()
while IFS= read -r line; do
    [ -n "$line" ] && deps+=("$line")
done < <(cd "$src_bin" && "$PY" -I "$here/pe-deps.py" . "${roots[@]}" "${plugin_files[@]}" | tr -d '\r')

rm -rf "$target"
mkdir -p "$target/obs-plugins/64bit" "$target/data/obs-plugins"

for f in obs.dll "${BIN_EXTRA[@]}" "${deps[@]}"; do
    [ -f "$src_bin/$f" ] || { echo "missing $f in the zip" >&2; exit 1; }
    cp "$src_bin/$f" "$target/$f"
done

cp -r "$work/data/libobs" "$target/data/libobs"
for p in "${PLUGINS[@]}"; do
    cp "$src_plug/$p.dll" "$target/obs-plugins/64bit/$p.dll"
    [ -d "$work/data/obs-plugins/$p" ] && cp -r "$work/data/obs-plugins/$p" "$target/data/obs-plugins/$p"
done

# Debug symbols are not needed at run time.
find "$target" -name '*.pdb' -delete

# game capture must find its hook and helper files.
for f in graphics-hook32.dll graphics-hook64.dll inject-helper32.exe inject-helper64.exe \
         get-graphics-offsets32.exe get-graphics-offsets64.exe obs-vulkan32.json obs-vulkan64.json; do
    [ -f "$target/data/obs-plugins/win-capture/$f" ] || { echo "win-capture is missing $f" >&2; exit 1; }
done

license="$work/data/obs-studio/license/gplv2.txt"
if [ -f "$license" ]; then
    cp "$license" "$target/LICENSE-OBS.txt"
else
    echo "OBS licence text not found in the zip" >&2
    exit 1
fi

echo
echo "OBS Studio $OBS_VERSION assembled in $target:"
(cd "$target" && find . -type f | sort | grep -v '/locale/')
echo "(plus $(cd "$target" && find . -path '*/locale/*' -type f | wc -l | tr -d ' ') locale files)"
echo "Total: $(du -sk "$target" | cut -f1) KiB in $(cd "$target" && find . -type f | wc -l | tr -d ' ') files"
