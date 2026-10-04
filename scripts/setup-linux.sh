#!/usr/bin/env bash
# First-run setup for the Linux release.
#
# The game invokes this automatically when <game dir>/data/installation.json is
# missing or stale. It asks for an Xbox 360 ISO or a default.xex, extracts the
# ISO with Wine when needed, and runs the owned-disc conversion. The converted
# assets land in <base>, the same layout the Windows setup helper publishes.
#
# Requires python3 with numpy and Pillow (see docs/linux.md). Wine is only
# needed when the source is an ISO; an already extracted disc skips it.
set -euo pipefail

usage() {
    cat >&2 <<EOF
Usage: $0 --base DIR --game-exe PATH [--source ISO|default.xex|DIR] [--refresh]

  --base       Directory that receives installation.json and the assets.
  --game-exe   Path to the skate3rust executable.
  --source     Game source. Skips the interactive prompt when given.
  --refresh    Reuse the existing installation and only rebuild changed groups.
EOF
}

Base=
GameExe=
Source=${SOURCE:-}
while [[ $# -gt 0 ]]; do
    case $1 in
        --base) Base=$2; shift 2 ;;
        --game-exe) GameExe=$2; shift 2 ;;
        --source) Source=$2; shift 2 ;;
        # prepare_assets.py refreshes an existing installation automatically.
        --refresh) shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "Unknown option: $1" >&2; usage; exit 1 ;;
    esac
done
if [[ -z $Base || -z $GameExe ]]; then
    usage
    exit 1
fi

ScriptDir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
GameExe=$(realpath -m "$GameExe")
GameDir=$(cd "$(dirname "$GameExe")" && pwd)
Base=$(realpath -m "$Base")
mkdir -p "$Base"

# The launcher captures the game's stdout/stderr into log files, so read the
# prompt from the controlling terminal instead. Fall back to the redirected
# streams when there is no terminal (for example a fully scripted install).
ask_source() {
    # Actually open the controlling terminal: a device node can be present
    # while no terminal is attached, in which case reading it fails. Fall back
    # to the inherited streams so a redirected install still works.
    local in_fd=0 out_fd=1
    if { exec 3</dev/tty; } 2>/dev/null && { exec 4>/dev/tty; } 2>/dev/null; then
        in_fd=3
        out_fd=4
    fi
    printf '\nSkate 3 Rust Engine setup\nSelect your Skate 3 Xbox 360 ISO or default.xex.\nLeave empty to cancel.\n> ' >&"$out_fd"
    IFS= read -r Source <&"$in_fd" || true
    # Tolerate paths pasted with surrounding quotes or whitespace.
    Source=$(printf '%s' "$Source" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' \
        -e "s/^['\"]//" -e "s/['\"]$//")
    if [[ -z $Source ]]; then
        echo 'Setup cancelled: no game selected.' >&2
        exit 1
    fi
}

if [[ -z $Source ]]; then
    ask_source
fi
# Expand a leading ~ the way a shell would.
Source=${Source/#\~/$HOME}

CleanupSource=0
if [[ -d $Source ]]; then
    GameRoot=$(realpath -m "$Source")
elif [[ -f $Source ]]; then
    Lower=$(basename "$Source" | tr '[:upper:]' '[:lower:]')
    case ${Lower##*.} in
        xex)
            if [[ $Lower != default.xex ]]; then
                echo 'Select default.xex inside your extracted Skate 3 game folder.' >&2
                exit 1
            fi
            GameRoot=$(cd "$(dirname "$Source")" && pwd)
            ;;
        iso)
            IsoPath=$(realpath -m "$Source")
            GameRoot="$Base/source-disc"
            if [[ -f "$GameRoot/default.xex" && -d "$GameRoot/data" ]]; then
                echo "Reusing the disc extracted at $GameRoot"
            else
                rm -rf "$GameRoot"
                if [[ ! -x $ScriptDir/extract-iso-wine.sh ]]; then
                    echo "Missing extract-iso-wine.sh beside $0" >&2
                    exit 1
                fi
                echo "Extracting $IsoPath"
                SKATE3_SETUP_QUIET=1 "$ScriptDir/extract-iso-wine.sh" "$IsoPath" "$GameRoot"
            fi
            CleanupSource=1
            ;;
        *)
            echo "Unsupported file: $Source (select a .iso or default.xex)" >&2
            exit 1
            ;;
    esac
else
    echo "No such file or directory: $Source" >&2
    exit 1
fi

Python=${PYTHON:-python3}
if ! command -v "$Python" >/dev/null; then
    echo 'python3 is required for asset preparation. Install it and retry.' >&2
    exit 1
fi
if ! "$Python" -c 'import numpy, PIL' >/dev/null 2>&1; then
    echo "python3 is missing numpy/Pillow. Run: $Python -m pip install numpy Pillow" >&2
    exit 1
fi

Prepare="$GameDir/tools/prepare_assets.py"
if [[ ! -f $Prepare ]]; then
    echo "Missing $Prepare. Unpack the complete Linux package." >&2
    exit 1
fi

echo "Preparing Skate 3 assets into $Base (this can take a while)"
"$Python" "$Prepare" --game-root "$GameRoot" --output "$Base" --game-exe "$GameExe"

if [[ ! -f $Base/installation.json ]]; then
    echo 'Setup did not publish an installation marker.' >&2
    exit 1
fi

# The extracted disc is only a source; the conversion copies what the game
# needs. Keep it after a failure so a retry does not re-extract, discard it
# once the installation is complete.
if [[ $CleanupSource = 1 ]]; then
    rm -rf "$GameRoot"
fi

echo 'Setup complete.'
