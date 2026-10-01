#!/usr/bin/env bash
# Build and stage the Linux/Unix equivalent of BUILD.bat + scripts/Build.ps1.
# Unlike Build.ps1 there is no DLL dependency walk (the ELF links only system
# libraries) and no Steam relay staging (steam_api64.dll is Windows-only).
set -euo pipefail

ProjectRoot=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
TargetDirectory=${TARGET_DIRECTORY:-"$ProjectRoot/target"}
RunDirectory=${RUN_DIRECTORY:-"$ProjectRoot/run"}
cd "$ProjectRoot"

if ! command -v cargo >/dev/null; then
    echo 'cargo is required. Install Rust from https://rustup.rs and reload your shell.' >&2
    exit 1
fi

# Release builds are opt-in; the dev profile already compiles at opt-level 3.
Profile=(debug)
CargoFlags=()
if [[ ${RELEASE:-0} == 1 ]]; then
    Profile=(release)
    CargoFlags=(--release)
fi

echo "Building skate3rust (${Profile[0]} profile)"
# dev-dynamic is a Windows incremental-build convenience that leaves Bevy
# shared objects to locate at runtime; link them in instead.
cargo build --locked -p skate-game --bin skate3rust --no-default-features \
    --target-dir "$TargetDirectory" "${CargoFlags[@]}"

Executable="$TargetDirectory/${Profile[0]}/skate3rust"
if [[ ! -x $Executable ]]; then
    echo "Missing executable: $Executable" >&2
    exit 1
fi

# The asset converters treat this as optional and fall back to a much slower
# pure-Python RefPack decoder, so only fail the build if rustc is present but
# the library will not produce a loadable object.
echo 'Building the native RefPack converter'
mkdir -p "$TargetDirectory/native"
RefpackSource=tools/asset_pipeline/refpack_native.rs
RefpackOutput="$TargetDirectory/native/refpack.${LIBSUFFIX:-so}"
if ! rustc --edition 2024 --crate-type cdylib -C opt-level=3 -C panic=abort \
        "$RefpackSource" -o "$RefpackOutput"; then
    echo "Warning: native RefPack converter failed; conversion will use the slower Python decoder." >&2
fi

echo "Staging $RunDirectory"
mkdir -p "$RunDirectory"
install -m 0755 "$Executable" "$RunDirectory/skate3rust"
# The character customiser has its own release fingerprint, exactly as the
# packaged Windows release records it, so a launch can tell an unprepared
# library from a stale one.
if python3 -c 'import numpy, PIL' >/dev/null 2>&1; then
    CharacterCustomiser=$(python3 -m tools.asset_pipeline.customiser_setup --fingerprint)
    printf '{"character_customiser":"%s"}\n' "$CharacterCustomiser" > "$RunDirectory/release.json"
else
    echo "Warning: numpy/Pillow are missing, so release.json cannot record the character customiser fingerprint." >&2
fi
# Mods, maps and assets all resolve from the executable directory.
if [[ -d $ProjectRoot/mods ]]; then
    mkdir -p "$RunDirectory/mods"
    for mod in "$ProjectRoot"/mods/*.zip "$ProjectRoot"/mods/README.md; do
        [[ -f $mod ]] && install -m 0644 "$mod" "$RunDirectory/mods/"
    done
fi
if [[ -d $ProjectRoot/maps ]]; then
    mkdir -p "$RunDirectory/maps"
    for map in "$ProjectRoot"/maps/*.skate; do
        [[ -f $map ]] && install -m 0644 "$map" "$RunDirectory/maps/"
    done
fi
# The converter loads this accelerator when the engine is built in place.
if [[ -f $RefpackOutput ]]; then
    install -m 0644 "$RefpackOutput" "$RunDirectory/refpack.${LIBSUFFIX:-so}"
fi

echo "Ready: $RunDirectory/skate3rust"
if [[ ! -f $RunDirectory/data/installation.json ]]; then
    cat >&2 <<EOF

No installation found. Extract your Xbox 360 Skate 3 disc and convert it:

  ./scripts/extract-iso-wine.sh /path/to/skate3.iso ~/sk3-disc
  python3 tools/prepare_assets.py --game-root ~/sk3-disc \\
      --output "$RunDirectory/data" --game-exe "$RunDirectory/skate3rust"
EOF
elif ! python3 - "$RunDirectory" <<'PY'
import json,pathlib,sys
root=pathlib.Path(sys.argv[1])
release,installation=root/'release.json',root/'data'/'installation.json'
if not release.is_file() or not installation.is_file():raise SystemExit(0)
expected=json.loads(release.read_text(encoding='utf-8-sig')).get('character_customiser')
current=root/'data'/json.loads(installation.read_text())['directory']/'assets/private/customisation/current.json'
prepared=json.loads(current.read_text()).get('fingerprint') if current.is_file() else None
raise SystemExit(0 if expected is None or prepared==expected else 1)
PY
then
    cat >&2 <<EOF

Character customiser assets are missing or were built by other tools. The skater
is unchanged until they are prepared:

  python3 tools/prepare_assets.py --game-root ~/sk3-disc \\
      --output "$RunDirectory/data" --game-exe "$RunDirectory/skate3rust" --character-only
EOF
fi
