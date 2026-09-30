# Linux build and play

The engine builds and runs natively on Linux. The Windows `BUILD.bat` / `PLAY.bat`
flow is replaced by three scripts under `scripts/`:

| Windows | Linux |
| --- | --- |
| `BUILD.bat` + `scripts/Build.ps1` | `scripts/build-linux.sh` |
| ISO extraction inside the setup helper | `scripts/extract-iso-wine.sh` |
| `PLAY.bat` + `scripts/Launch.ps1` | `scripts/play-linux.sh` |

There is no bundled Python runtime, installer, auto-updater or Steam relay on
Linux; assets are prepared explicitly with the tools in `tools/`.

## Prerequisites

- **Rust** (stable) installed with [rustup](https://rustup.rs). The workspace uses
  edition 2024, so the toolchain must be 1.85 or newer.
- **A C toolchain and `pkg-config`** for the native dependencies.
- **Bevy/winit system libraries.** On Debian/Ubuntu:

  ```bash
  sudo apt install build-essential pkg-config libasound2-dev libudev-dev \
      libx11-dev libxcursor-dev libxi-dev libxrandr-dev \
      libwayland-dev libxkbcommon-dev
  ```

  On Fedora:

  ```bash
  sudo dnf install gcc pkgconf-pkg-config alsa-lib-devel systemd-devel \
      libX11-devel libXcursor-devel libXi-devel libXrandr-devel \
      wayland-devel libxkbcommon-devel
  ```

  `libasound2-dev`/`alsa-lib-devel` is needed for audio; `libudev-dev`/
  `systemd-devel` is needed for gamepad discovery (gilrs).
- **Python 3** for asset conversion, with `numpy` and `Pillow`
  (`python3 -m pip install numpy Pillow`). `PyAV` (`python3 -m pip install av`)
  is optional and only enables gameplay audio decoding.
- **Wine** is required only for the one-time ISO extraction step.

## Build

```bash
./scripts/build-linux.sh
```

The script builds `skate3rust` and the native RefPack decoder, then stages the
executable, `mods/`, `maps/` and `refpack.so` into `run/`. It deliberately skips
the Windows DLL dependency walk and Steam relay staging. A release build is
opt-in and otherwise the dev profile is used (already `opt-level = 3`):

```bash
RELEASE=1 ./scripts/build-linux.sh
```

Overridable environment variables: `TARGET_DIRECTORY` (default `./target`),
`RUN_DIRECTORY` (default `./run`) and `LIBSUFFIX` (default `so`).

If the native RefPack converter fails to build the script warns and continues;
conversion then falls back to a slower pure-Python decoder.

## Prepare assets

Extract the disc once via Wine, then convert it. Both steps only need the game
files you already own; no assets are downloaded.

```bash
./scripts/extract-iso-wine.sh /path/to/skate3.iso ~/sk3-disc
python3 tools/prepare_assets.py --game-root ~/sk3-disc \
    --output run/data --game-exe run/skate3rust
```

`extract-iso-wine.sh` downloads the hash-pinned XboxDev extract-xiso build and
runs it under Wine, then verifies the required `data/big/*.big` archives are
present. If you already have an extracted disc, skip it and point
`--game-root` at that folder.

`prepare_assets.py` writes the converted installation to `run/data` and records
`run/data/installation.json`, which the engine reads on launch. Conversion can
take a while; native RefPack decompression and batched texture decoding keep it
close to the Windows setup timings.

## Play

```bash
./scripts/play-linux.sh            # saved map (University by default)
./scripts/play-linux.sh map.skate  # a specific map
```

The launcher runs the executable from `run/` so relative files land beside the
game, and writes `run/logs/game-<timestamp>.log` (plus a `.stderr.log`). Press
Escape for graphics, difficulty and map settings. Use an XInput-style gamepad.

## Controllers

Non-Windows hosts read gamepads through
[gilrs](https://gitlab.com/gilrs-project/gilrs) in
`crates/skate-game/src/input/platform.rs`, translating SDL's device database
onto the same XInput button, trigger and axis layout the Windows path reads from
XInput. Up to four pads are tracked, slots stay stable across frames and
hot-plugs, and an unmapped element reads as rest instead of failing. No
controller currently means no input, so connect a pad before launching.

## Audio

Gameplay audio is decoded from the owned disc during asset preparation. XMA2
decoding needs `PyAV`; when it is missing, setup records the audio group as
unavailable and the game still runs silently. See
[gameplay audio](audio.md) for the format and mapping details.

## Differences from Windows

- No bundled Python, setup GUI or `installation.json` refresh transactions.
  Re-run `prepare_assets.py` to refresh assets.
- No Steam relay staging (`steam_api64.dll` is Windows-only).
- No in-place auto-update or numbered Windows release packaging.
- Performance timeline capture (`--trace`) is Windows-only.

## Troubleshooting

- **`cargo` not found** — install Rust from rustup and reload your shell.
- **ALSA / `libasound` link errors** — install the ALSA development package
  listed above.
- **`libudev` link errors** — install `libudev-dev` / `systemd-devel`.
- **No controller** — confirm the pad is recognized by `evdev`/hidraw and that
  the `udev` package is installed.
- **`wine is required`** — install Wine, or skip ISO extraction and pass an
  already extracted disc to `prepare_assets.py`.
- **Game exits immediately** — check the newest `run/logs/*.stderr.log`.
