#!/usr/bin/env bash
# Build a self-contained Linux AppImage that asks for the Skate 3 ISO/default.xex
# on first launch and converts the disc itself.
#
# Bundles the game binary, the Python asset pipeline plus a portable CPython
# with numpy/Pillow, and a native extract-xiso. Graphics drivers, Vulkan, audio
# and the window system come from the host, as with any game.
set -euo pipefail

ProjectRoot=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
RunDirectory=${RUN_DIRECTORY:-"$ProjectRoot/run"}
Arch=${ARCH:-x86_64}
Name=skate-3-rust-engine-linux-$Arch
WorkDirectory=${WORK_DIRECTORY:-"$ProjectRoot/target/appimage"}
Cache="$WorkDirectory/cache"
AppDir="$WorkDirectory/AppDir"
Payload="$AppDir/usr/share/skate3rust"
Output="$ProjectRoot/target/$Name.AppImage"

# Pinned build inputs. Update the matching sha256 when bumping a version.
PbsRelease=20261003
PbsAsset="cpython-3.13.16+${PbsRelease}-x86_64-unknown-linux-gnu-install_only.tar.gz"
PbsUrl="https://github.com/astral-sh/python-build-standalone/releases/download/${PbsRelease}/${PbsAsset}"
PbsSha=0a0272910b10417c659a9312fb3f2d7a6d774da7bd510999be7a3ba83273dc1f
NumpyVersion=2.2.6
PillowVersion=11.3.0
# PyAV enables gameplay audio decoding; the wheel bundles its FFmpeg libraries.
PyAVVersion=19.0.1
XisoUrl='https://github.com/XboxDev/extract-xiso/releases/download/build-202505152050/extract-xiso_Linux.zip'
XisoSha=982bbfefc9255d51f5348a477d7135d68abf81c0af9600e5728edb1246cfa200
ToolUrl='https://github.com/AppImage/AppImageKit/releases/download/continuous/appimagetool-x86_64.AppImage'
ToolSha=b90f4a8b18967545fda78a445b27680a1642f1ef9488ced28b65398f2be7add2

fetch() {
    local url=$1 sha=$2 destination=$3
    if [[ -f $destination ]] && echo "$sha  $destination" | sha256sum --check --status 2>/dev/null; then
        return
    fi
    mkdir -p "$(dirname "$destination")"
    echo "Downloading $(basename "$destination")"
    curl --fail --location --silent --show-error "$url" -o "$destination.part"
    if ! echo "$sha  $destination.part" | sha256sum --check --status; then
        rm -f "$destination.part"
        echo "Checksum mismatch for $url" >&2
        exit 1
    fi
    mv "$destination.part" "$destination"
}

if [[ ${SKIP_BUILD:-0} != 1 ]]; then
    "$ProjectRoot/scripts/build-linux.sh"
fi
if [[ ! -x $RunDirectory/skate3rust ]]; then
    echo "Missing $RunDirectory/skate3rust. Run scripts/build-linux.sh first." >&2
    exit 1
fi
if [[ ! -x $RunDirectory/support/setup-linux.sh ]]; then
    echo "Missing setup helper; rebuild to stage support/." >&2
    exit 1
fi

# Portable Python with the asset pipeline dependencies. The install_only bundle
# ships an 80 MB static interpreter and an unused 137 MB shared library; drop
# the latter and the tooling we will never call at run time.
PythonRoot="$WorkDirectory/python"
fetch "$PbsUrl" "$PbsSha" "$Cache/$PbsAsset"
if [[ ! -f $WorkDirectory/.python-complete ]]; then
    rm -rf "$PythonRoot"
    mkdir -p "$PythonRoot"
    tar -xzf "$Cache/$PbsAsset" -C "$PythonRoot" --strip-components=1
    "$PythonRoot/bin/python3" -m pip install --no-warn-script-location --no-cache-dir \
        "numpy==$NumpyVersion" "Pillow==$PillowVersion" "av==$PyAVVersion"
    rm -rf "$PythonRoot"/lib/libpython*.so* "$PythonRoot/include" "$PythonRoot/lib/pkgconfig" \
        "$PythonRoot"/share/terminfo "$PythonRoot"/share/man "$PythonRoot"/share/doc \
        "$PythonRoot"/lib/python*/test "$PythonRoot"/lib/python*/idlelib "$PythonRoot"/lib/python*/tkinter \
        "$PythonRoot"/lib/python*/turtledemo "$PythonRoot"/lib/python*/ensurepip "$PythonRoot"/lib/python*/lib2to3 \
        "$PythonRoot"/lib/python*/site-packages/pip* "$PythonRoot"/lib/python*/site-packages/setuptools* \
        "$PythonRoot"/lib/python*/site-packages/pkg_resources \
        "$PythonRoot"/lib/tcl* "$PythonRoot"/lib/tk* "$PythonRoot"/lib/itcl* "$PythonRoot"/lib/libthread* \
        "$PythonRoot"/lib/libtcl* "$PythonRoot"/lib/libtk*
    rm -f "$PythonRoot"/bin/pip* "$PythonRoot"/bin/idle* "$PythonRoot"/lib/python*/lib-dynload/_tkinter*
    "$PythonRoot/bin/python3" -c 'import numpy, PIL' || { echo 'Portable Python is unusable' >&2; exit 1; }
    touch "$WorkDirectory/.python-complete"
fi

fetch "$XisoUrl" "$XisoSha" "$Cache/extract-xiso_Linux.zip"
XisoBin="$WorkDirectory/extract-xiso"
if [[ ! -x $XisoBin ]]; then
    rm -rf "$WorkDirectory/xiso"
    mkdir -p "$WorkDirectory/xiso"
    ( cd "$WorkDirectory/xiso" && unzip -o "$Cache/extract-xiso_Linux.zip" >/dev/null )
    install -m 0755 "$WorkDirectory/xiso/extract-xiso" "$XisoBin"
fi

fetch "$ToolUrl" "$ToolSha" "$Cache/appimagetool.AppImage"
chmod 0755 "$Cache/appimagetool.AppImage"

echo "Assembling $AppDir"
rm -rf "$AppDir"
mkdir -p "$AppDir/usr/bin" "$Payload"
install -m 0755 "$RunDirectory/skate3rust" "$AppDir/usr/bin/skate3rust"
cp -a "$RunDirectory/support" "$AppDir/usr/bin/support"
cp -a "$RunDirectory/tools" "$Payload/tools"
# Record the customiser fingerprint with the bundled interpreter so the
# AppImage is self-consistent even when the build host lacks numpy/Pillow.
Fingerprint=$(cd "$Payload" && "$PythonRoot/bin/python3" -m tools.asset_pipeline.customiser_setup --fingerprint)
printf '{"character_customiser":"%s"}\n' "$Fingerprint" > "$AppDir/usr/bin/release.json"
for directory in mods maps; do
    [[ -e $RunDirectory/$directory ]] && cp -a "$RunDirectory/$directory" "$AppDir/usr/bin/$directory"
done
cp -a "$PythonRoot" "$Payload/python"
install -m 0755 "$XisoBin" "$Payload/extract-xiso"

# The AppImage marks the data directory writable and points the helper at the
# bundled payload. $APPDIR is provided by the AppImage runtime.
"$Payload/python/bin/python3" - "$ProjectRoot/docs/images/skating-crab.png" "$AppDir/skate3rust.png" <<'PY'
import sys
from PIL import Image
image = Image.open(sys.argv[1]).convert('RGBA')
image.thumbnail((256, 256))
image.save(sys.argv[2])
PY
cp "$AppDir/skate3rust.png" "$AppDir/.DirIcon"
cat > "$AppDir/skate3rust.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=Skate 3 Rust Engine
Comment=Skate 3 reverse-engineered engine
Exec=skate3rust
Icon=skate3rust
Categories=Game;
Terminal=false
EOF
cat > "$AppDir/AppRun" <<'EOF'
#!/bin/sh
# The runtime always sets APPDIR; derive it when AppRun runs from an extract.
if [ -z "$APPDIR" ]; then
    APPDIR="$(cd "$(dirname "$0")" && pwd)"
fi
export SKATE3_DATA_DIR="${SKATE3_DATA_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/skate3rust}"
export SKATE3_PYTHON="$APPDIR/usr/share/skate3rust/python/bin/python3"
export SKATE3_TOOLS_DIR="$APPDIR/usr/share/skate3rust/tools"
export SKATE3_EXTRACT_XISO="$APPDIR/usr/share/skate3rust/extract-xiso"
exec "$APPDIR/usr/bin/skate3rust" "$@"
EOF
chmod 0755 "$AppDir/AppRun"

echo "Building $Output"
mkdir -p "$ProjectRoot/target"
VERSION=${RELEASE_TAG:-development} ARCH=$Arch \
    "$Cache/appimagetool.AppImage" --appimage-extract-and-run --no-appstream "$AppDir" "$Output"
( cd "$ProjectRoot/target" && sha256sum "$Name.AppImage" > "$Name.AppImage.sha256" )

echo "AppImage:  $Output"
echo "Checksum:  $Output.sha256"
