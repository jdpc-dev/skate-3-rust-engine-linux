# Gameplay audio

Skateboard rolling loops and one-shot effects, decoded from the owned disc during
setup. The engine plays them through Bevy's audio plugin; no retail audio is
committed to the repository.

## Format

Skate 3 audio is EA Audio Core (codec 3 / XMA2) inside EA "EB" v3 archives under
`data/audio/`. Two big-endian u32s precede each stream:

```text
header1  [4b version][4b codec][6b channel_config][18b sample_rate]
header2  [2b stream_type][1b loop][29b num_samples]
then     block chain: {flags<<24 | size, num_samples}[payload]
```

Each block payload is `ceil(channels / 2)` XMA contexts; every context is preceded
by a length field `(field - 18) >> 2`. A context must be decoded through one
persistent `AVCodecContext` and flushed at the end — restarting per chunk loses 64
samples of MDCT overlap each time. `tools/owned_game/eaac.py` owns this arithmetic
and the PyAV binding; `tools/owned_game/splc.py` parses the SPLC `.bnk` banks.

## What is decoded

`tools/asset_pipeline/audio.py` is a setup group (`GROUPS` in `versions.py`). It
writes `assets/private/audio/`:

- `rolling/<material>_<soft|hard>.wav` — the `grains.big` surface rolling loops
  (`x_jet_rolling`, which is not a surface loop, is ignored). The audio-surface id
  packed into collision surfaces (`EncodeRwSurfaceId` low seven bits, matching the
  material table in
  `tools/vendor/university/tools/blender_owned_map/owned_world_material_addon/__init__.py`)
  selects the grain, so rolling follows the actual surface.
- `sfx/*.wav` — impact, bail and grind one-shots selected from
  `Skate_Collisions.bnk` and `Skate_Metal.bnk` by duration window.
- `wheel/*.wav` — wheel-spin sounds from `wheels.big`.
- `audio.json` — the manifest the runtime reads.

One-shot selection is deliberately conservative and recorded in the manifest; the
duration windows in `audio.py` are the place to retune it. The native sound-event
hashes that would map each gameplay event to an exact sound are not yet reversed,
so the current mapping is by effect class, not by name.

## Runtime

`crates/skate-game/src/audio.rs`:

- a loop whose grain follows `BoardContactReport.other_surface & 0x7f` and whose
  pitch/volume follow `riding.motion.speed`, crossfading when surface or hard/soft
  changes; and
- one-shots triggered by physical-state transitions (air→ground landing,
  ground→air pop, wipeout bail, grind entry).

Both are gated by `graphics_menu::gameplay_active` and `!replay.active`, the same
way the vehicle engine voice is.

## Dependency

Decoding needs PyAV (`pip install av`), whose bundled libavcodec provides the XMA2
decoder. When it is missing the group records `audio-availability.json` and setup
still succeeds, exactly like the disc's other optional content. Bevy's `wav`
feature is enabled in `crates/skate-game/Cargo.toml` to load the files.
