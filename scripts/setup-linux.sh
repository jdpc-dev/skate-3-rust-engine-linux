#!/usr/bin/env bash
# First-run setup for the Linux release and AppImage.
#
# The game invokes this automatically when the asset base has no
# installation.json. It asks for an Xbox 360 ISO or a default.xex, extracts the
# ISO when needed and runs the owned-disc conversion into --base.
#
# The AppImage sets SKATE3_PYTHON, SKATE3_TOOLS_DIR and SKATE3_EXTRACT_XISO so
# nothing has to be installed. Source checkouts fall back to the system python3
# and to extract-iso-wine.sh (which needs Wine only for ISO sources).
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
Source=${SOURCE:-${SKATE3_SOURCE:-}}
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

# A bundled AppImage points these at its own payload; a source checkout uses
# the system interpreter and the tools shipped beside the binary.
Python=${SKATE3_PYTHON:-${PYTHON:-python3}}
ToolsDir=${SKATE3_TOOLS_DIR:-$GameDir/tools}
Extractor=${SKATE3_EXTRACT_XISO:-}
if [[ -z $Extractor && -x $ScriptDir/extract-xiso ]]; then
    Extractor=$ScriptDir/extract-xiso
fi

notify() {
    command -v notify-send >/dev/null && notify-send "$1" "$2" 2>/dev/null || true
}
gui_error() {
    if command -v zenity >/dev/null; then
        zenity --error --title='Skate 3 Rust Engine' --text="$1" 2>/dev/null || true
    elif command -v kdialog >/dev/null; then
        kdialog --error "$1" 2>/dev/null || true
    fi
}
fail() {
    echo "$1" >&2
    notify 'Skate 3 Rust Engine' "$1"
    gui_error "$1"
    exit 1
}

# The launcher captures the game's stdout/stderr into log files, so prefer the
# controlling terminal. An AppImage opened from a file manager has no terminal,
# so fall back to a desktop file chooser, then to the inherited streams.
ask_source() {
    if { exec 3</dev/tty; } 2>/dev/null && { exec 4>/dev/tty; } 2>/dev/null; then
        printf '\nSkate 3 Rust Engine setup\nSelect your Skate 3 Xbox 360 ISO or default.xex.\nLeave empty to cancel.\n> ' >&4
        IFS= read -r Source <&3 || true
    elif command -v zenity >/dev/null; then
        Source=$(zenity --file-selection --title='Select your Skate 3 ISO or default.xex' \
            --file-filter='Skate 3 (ISO or default.xex) | *.iso default.xex' \
            --file-filter='All files | *' 2>/dev/null) || true
    elif command -v kdialog >/dev/null; then
        Source=$(kdialog --getopenfilename "$HOME" '*.iso default.xex|Skate 3' 2>/dev/null) || true
    else
        printf 'Select your Skate 3 Xbox 360 ISO or default.xex: ' >&2
        IFS= read -r Source || true
    fi
    # Tolerate paths pasted with surrounding quotes or whitespace.
    Source=$(printf '%s' "$Source" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' \
        -e "s/^['\"]//" -e "s/['\"]$//")
    [[ -n $Source ]] || fail 'Setup cancelled: no game selected.'
}

[[ -n $Source ]] || ask_source
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
                fail 'Select default.xex inside your extracted Skate 3 game folder.'
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
                mkdir -p "$GameRoot"
                echo "Extracting $IsoPath"
                if [[ -n $Extractor ]]; then
                    "$Extractor" -x -d "$GameRoot" "$IsoPath"
                elif [[ -x $ScriptDir/extract-iso-wine.sh ]]; then
                    SKATE3_SETUP_QUIET=1 "$ScriptDir/extract-iso-wine.sh" "$IsoPath" "$GameRoot"
                else
                    fail 'No ISO extractor available. Unpack the complete package or install Wine.'
                fi
            fi
            CleanupSource=1
            ;;
        *)
            fail "Unsupported file: $Source (select a .iso or default.xex)"
            ;;
    esac
else
    fail "No such file or directory: $Source"
fi

if ! command -v "$Python" >/dev/null; then
    fail "python3 is required for asset preparation. Install it or use the AppImage."
fi
if ! "$Python" -c 'import numpy, PIL' >/dev/null 2>&1; then
    fail "python3 is missing numpy/Pillow. Run: $Python -m pip install numpy Pillow"
fi

Prepare="$ToolsDir/prepare_assets.py"
if [[ ! -f $Prepare ]]; then
    fail "Missing $Prepare. Unpack the complete package."
fi

Log="$Base/setup-run.log"
echo "Preparing Skate 3 assets into $Base (this can take a while). Log: $Log"
set +e
"$Python" "$Prepare" --game-root "$GameRoot" --output "$Base" --game-exe "$GameExe" 2>&1 | tee -a "$Log"
Status=${PIPESTATUS[0]}
set -e
if (( Status != 0 )); then
    fail "Setup failed. See $Log (and $Base/setup-error.log if present)."
fi

if [[ ! -f $Base/installation.json ]]; then
    fail 'Setup did not publish an installation marker.'
fi

# The extracted disc is only a source. Keep it after a failure so a retry does
# not re-extract, discard it once the installation is complete.
if [[ $CleanupSource = 1 ]]; then
    rm -rf "$GameRoot"
fi

echo 'Setup complete.'
notify 'Skate 3 Rust Engine' 'Assets are ready. Starting the game...'
