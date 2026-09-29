#!/usr/bin/env bash
# Launch the game. Unix equivalent of PLAY.bat + scripts/Launch.ps1.
#
# Assets are resolved from <executable dir>/data/installation.json, so this
# deliberately never passes --assets: doing so bypasses that resolution.
# Pass a .skate file to load a specific map, otherwise the saved default is used.
set -euo pipefail

ProjectRoot=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
RunDirectory=${RUN_DIRECTORY:-"$ProjectRoot/run"}
Map=${1:-}

if [[ ! -x $RunDirectory/skate3rust ]]; then
    echo "Game is not built. Run ./scripts/build-linux.sh first." >&2
    exit 1
fi

mkdir -p "$RunDirectory/logs"
Log="$RunDirectory/logs/game-$(date +%Y%m%d-%H%M%S).log"
ErrorLog="${Log%.log}.stderr.log"

Arguments=()
if [[ -n $Map ]]; then
    MapPath=$(realpath -m "$Map")
    if [[ ${MapPath##*.} != skate ]]; then
        echo 'Select a .skate map file.' >&2
        exit 1
    fi
    Arguments+=(--map "$MapPath")
    echo "Map: $MapPath"
fi

echo 'Starting Skate 3 Rust Engine. Use your XInput controller; Esc opens difficulty/graphics/pause settings.'
echo "Log: $Log"

# Run from the staging directory so relative user files land beside the game.
set +e
( cd "$RunDirectory" && ./skate3rust "${Arguments[@]+"${Arguments[@]}"}" ) \
    >"$Log" 2>"$ErrorLog"
Status=$?
set -e

if (( Status != 0 )); then
    tail -n 30 "$ErrorLog" || true
    echo "Game exited with code $Status. Log: $ErrorLog" >&2
fi
exit $Status
