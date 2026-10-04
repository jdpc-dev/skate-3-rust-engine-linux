#!/usr/bin/env bash
# Package the staged Linux build into a portable archive.
#
# Runs build-linux.sh first unless SKIP_BUILD=1, then copies run/ (minus the
# per-copy data/ and logs/) plus the project README and license into
# target/skate3rust-linux-x64.tar.gz. Players extract the archive and run
# skate3rust; the first launch runs the setup helper.
set -euo pipefail

ProjectRoot=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
RunDirectory=${RUN_DIRECTORY:-"$ProjectRoot/run"}
Name=skate3rust-linux-x64
Archive="$ProjectRoot/target/$Name.tar.gz"

if [[ ${SKIP_BUILD:-0} != 1 ]]; then
    "$ProjectRoot/scripts/build-linux.sh"
fi

if [[ ! -x $RunDirectory/skate3rust ]]; then
    echo "Missing $RunDirectory/skate3rust. Run scripts/build-linux.sh first." >&2
    exit 1
fi
if [[ ! -f $RunDirectory/support/setup-linux.sh ]]; then
    echo "Missing $RunDirectory/support/setup-linux.sh; rebuild to stage the setup helper." >&2
    exit 1
fi

Stage=$(mktemp -d)
trap 'rm -rf "$Stage"' EXIT
Root="$Stage/$Name"
mkdir -p "$Root"
# data/ is per-copy game content and logs/ is runtime output; neither ships.
tar -C "$RunDirectory" \
    --exclude='./data' --exclude='./logs' -cf - . | tar -C "$Root" -xf -
cp "$ProjectRoot/README.md" "$ProjectRoot/LICENSE" "$Root/"

mkdir -p "$ProjectRoot/target"
tar -C "$Stage" -czf "$Archive" "$Name"
( cd "$ProjectRoot/target" && sha256sum "$Name.tar.gz" > "$Name.tar.gz.sha256" )

echo "Release package: $Archive"
echo "Checksum:        $Archive.sha256"
