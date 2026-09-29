#!/usr/bin/env bash
# One-time Xbox 360 disc extraction via Wine.
#
# The bundled setup helper downloads a Win64 extract-xiso build and runs it
# directly, which cannot execute on Linux (tools/asset_pipeline/install.py).
# Extracting the disc here first means prepare_assets.py takes its
# already-extracted branch and never touches that Windows binary.
set -euo pipefail

ProjectRoot=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
XISO_URL='https://github.com/XboxDev/extract-xiso/releases/download/build-202505152050/extract-xiso-Win64_Release.zip'
XISO_SHA='fec88d03c7efd6205ab09be4abba70c0afd0eb27a5709f0a6235b828ba5ac11e'

Iso=${1:-}
Destination=${2:-}
if [[ -z $Iso || -z $Destination ]]; then
    echo "Usage: $0 /path/to/skate3.iso /path/to/extracted/disc" >&2
    exit 1
fi
Iso=$(realpath -m "$Iso")
if [[ ! -f $Iso ]]; then
    echo "No such ISO: $Iso" >&2
    exit 1
fi
if ! command -v wine >/dev/null; then
    echo 'wine is required to run the Windows-only disc extractor.' >&2
    exit 1
fi
mkdir -p "$Destination"
Destination=$(realpath -m "$Destination")

ToolsDirectory="$ProjectRoot/target/xbox-tools/extract-xiso"
Archive="$ToolsDirectory/extract-xiso-Win64_Release.zip"
mkdir -p "$ToolsDirectory"

if [[ -f $Archive ]] && echo "$XISO_SHA  $Archive" | sha256sum --check --status; then
    echo 'Using the cached extract-xiso download.'
else
    echo 'Downloading extract-xiso'
    curl --fail --location --silent --show-error "$XISO_URL" -o "$Archive.part"
    mv "$Archive.part" "$Archive"
    if ! echo "$XISO_SHA  $Archive" | sha256sum --check --status; then
        rm -f "$Archive"
        echo 'extract-xiso download checksum mismatch.' >&2
        exit 1
    fi
fi

if [[ ! -f $ToolsDirectory/.complete ]]; then
    python3 - "$Archive" "$ToolsDirectory" <<'PY'
import pathlib,stat,sys,zipfile
archive,destination=sys.argv[1],sys.argv[2]
root=pathlib.Path(destination).resolve()
with zipfile.ZipFile(archive) as package:
    for info in package.infolist():
        # This archive was produced on Windows, so its entries keep backslash
        # separators that zipfile treats as literal filename characters. Wine
        # cannot open such a path, so treat them as real directories.
        name=info.filename.replace('\\','/')
        target=(root/name).resolve()
        if target!=root and root not in target.parents:
            raise SystemExit('Unsafe path in tool archive: '+info.filename)
        if (info.external_attr>>16)&0o170000==0o120000:
            raise SystemExit('Tool archive contains a symbolic link')
        if info.is_dir():
            target.mkdir(parents=True,exist_ok=True)
            continue
        target.parent.mkdir(parents=True,exist_ok=True)
        target.write_bytes(package.read(info))
        if name.lower().endswith('.exe'):
            target.chmod(target.stat().st_mode|stat.S_IXUSR)
PY
    touch "$ToolsDirectory/.complete"
fi

Extractor=$(find "$ToolsDirectory" -name 'extract-xiso.exe' -print -quit)
if [[ -z $Extractor ]]; then
    echo 'extract-xiso.exe missing from the downloaded archive.' >&2
    exit 1
fi

mkdir -p "$Destination"
echo "Extracting $Iso to $Destination"
WINEDEBUG=${WINEDEBUG:--all} wine "$Extractor" -x "$Iso" -d "$Destination"

# Fail here rather than deep inside the converters.
for required in default.xex data/big/miscload.big data/big/miscboot.big \
                data/big/db.big data/content/createacharacter.big; do
    if [[ ! -f $Destination/$required ]]; then
        echo "Extraction incomplete: missing $required" >&2
        exit 1
    fi
done

echo "Ready. Convert the disc with:"
echo "  python3 $ProjectRoot/tools/prepare_assets.py --game-root $Destination \\"
echo "      --output $ProjectRoot/run/data --game-exe $ProjectRoot/run/skate3rust"
