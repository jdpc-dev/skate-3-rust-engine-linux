"""Decode owned Skate 3 disc audio into the private asset tree.

Skate 3 stores gameplay audio as EA Audio Core (codec 3 / XMA2) streams inside EA
"EB" archives. This export turns a small, gameplay-relevant slice of them into plain
WAV files the engine can stream through Bevy's audio plugin:

* ``grains.big`` -- the fourteen surface rolling loops, named by material. These map
  onto the native audio-surface ids (``EncodeRwSurfaceId`` low seven bits) that the
  collision surfaces already carry, so rolling follows the actual surface.
* ``wheels.big`` -- wheel-spin sounds.
* ``audiofiles.big`` -- the ``Skate_Collisions`` and ``Skate_Metal`` SPLC banks, from
  which representative impact and grind one-shots are selected by duration.

The selection is deliberately conservative and recorded in ``audio.json`` so a wrong
pick can be retuned by editing the manifest rather than the code. Decoding needs PyAV
(``pip install av``); when it is missing the group is reported unavailable, exactly
like the disc's other optional content, and setup still succeeds.
"""
from __future__ import annotations

import json
import struct
from pathlib import Path

from tools.owned_game.big import BigArchive
from tools.owned_game.splc import Bank

# Native audio surface ids from EncodeRwSurfaceId (owned_world_material_addon
# _AUDIO_NAMES). Index is the low seven bits of a packed collision surface.
AUDIO_SURFACES = [
    "Undefined", "Asphalt_Smooth", "Asphalt_Rough", "Concrete_Polished",
    "Concrete_Rough", "Concrete_Aggregate", "Wood_Ramp", "Plywood", "Dirt",
    "Metal", "Grass", "Metal_Solid_Round_1", "Metal_Solid_Round_1_Up",
    "Metal_Solid_Round_2", "Metal_Solid_Square_1", "Metal_Solid_Square_2",
    "Metal_Hollow_Round_1", "Metal_Hollow_Round_1_Dead", "Metal_Hollow_Round_1_Dn",
    "Metal_Hollow_Round_2", "Metal_Hollow_Round_2_Dead", "Metal_Hollow_Round_2_Dn",
    "Metal_Hollow_Round_3", "Metal_Hollow_Round_4", "Metal_Hollow_Square_1",
    "Metal_Hollow_Square_2", "Metal_Hollow_Square_3", "Metal_Hollow_Square_3_Dead",
    "Metal_Hollow_Square_4", "Metal_Hollow_1", "Metal_Hollow_2", "Metal_Sheet",
    "Metal_Complex_1", "Metal_Complex_2", "Metal_Complex_3", "Metal_Complex_4",
    "Metal_Complex_5", "Metal_Complex_6", "Metal_Complex_7", "Metal_Complex_8",
    "Metal_Complex_Debris", "Wood_1", "Wood_1_Up", "Wood_2", "Wood_3", "Wood_3_Up",
    "Wood_4", "Plastic_1", "Plastic_2", "Plastic_3", "Plastic_4", "Glass_Thick_Large",
    "Glass_Thin_Small", "Concrete_Curb", "Concrete_Bench", "Leaves", "Bush",
    "Pottery", "Paper", "Cardboard", "Garbage_Bag", "Garbage_Spill", "Bottle",
    "Tile_Ceramic", "Marble_or_Slate", "Brick_Smooth", "Brick_Coarse",
    "Manhole_Metal", "Metal_Grate_Sewer", "Metal_Grate_Planter", "DeepSnow",
    "PackedSnow", "Ice", "Antennas", "Chandelier", "Plexiglass_Small",
    "Plexiglass_Large", "Potted_Plant", "Crumpled_Paper", "Cloth", "Pop_Can",
    "Paper_Cup", "Wire_Cable", "VolleyBall", "OilDrum", "DMORail", "Fruit",
    "Plastic_Bottle", "Drum_Pylon", "Metal_Rail_4", "Wood_5", "Metal_Ramp",
    "Complex_Plastic_1", "Max_Mappable_Surface",
]

_SMOOTH = ("concrete_smooth",)
_SMOOTH_NAMES = {"ice", "marble_or_slate", "tile_ceramic", "brick_smooth", "brick_coarse",
                 "plexiglass_small", "plexiglass_large", "glass_thick_large", "glass_thin_small",
                 "plastic_1", "plastic_2", "plastic_3", "plastic_4", "complex_plastic_1"}
_SOFT_NAMES = {"dirt", "grass", "bush", "leaves", "deepsnow", "packedsnow", "potted_plant",
               "cloth", "garbage_bag", "garbage_spill", "cardboard", "paper", "crumpled_paper"}


def surface_grain(name: str) -> str:
    """Map a native audio-surface name to the closest rolling-loop base name."""
    key = name.lower()
    if key.startswith("asphalt_smooth"):
        return "asphalt_smooth"
    if key.startswith("asphalt_rough") or key == "oildrum":
        return "asphalt_rough"
    if key.startswith("concrete_aggregate"):
        return "concrete_aggregate"
    if key.startswith("concrete_polished") or key in _SMOOTH_NAMES:
        return "concrete_smooth"
    if key.startswith("concrete_rough") or key.startswith("concrete_curb") or key.startswith("concrete_bench"):
        return "concrete_rough"
    if key.startswith("wood") or key.startswith("plywood"):
        return "wood_ramp"
    if key.startswith("metal") or key in {"manhole_metal", "metal_grate_sewer", "metal_grate_planter",
                                          "dmorail", "drum_pylon"}:
        return "metal_smooth"
    if key in _SOFT_NAMES:
        return "concrete_aggregate"
    return "concrete_rough"


def _write_wav(path: Path, channels: int, rate: int, pcm: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fmt = struct.pack("<HHIIHH", 1, channels, rate, rate * channels * 2, channels * 2, 16)
    body = (b"WAVE" + b"fmt " + struct.pack("<I", len(fmt)) + fmt
            + b"data" + struct.pack("<I", len(pcm)) + pcm)
    path.write_bytes(b"RIFF" + struct.pack("<I", len(body)) + body)


def _member(archive: BigArchive, name: str) -> bytes:
    entry = next((e for e in archive.entries if e.path.split("/")[-1] == name), None)
    if entry is None:
        raise FileNotFoundError(f"{name} is not in {archive.path.name}")
    return archive.read(entry)


def _rolling(eaac, game_root: Path, out: Path) -> dict:
    """Decode the grain archive and build the surface -> file map."""
    archive = BigArchive(game_root / "data/audio/grains.big")
    decoded: dict[str, dict] = {}
    for entry in archive.entries:
        stem = Path(entry.path).stem
        if not stem.endswith(("_soft", "_hard")):
            continue
        header, pcm = eaac.decode_member(archive.read(entry))
        _write_wav(out / "rolling" / f"{stem}.wav", header.channels, header.sample_rate, pcm)
        decoded[stem] = {"channels": header.channels, "rate": header.sample_rate}
    if not decoded:
        raise RuntimeError("grains.big contained no rolling loops")

    files: dict[str, dict[str, str]] = {}
    for stem in decoded:
        base, _, variant = stem.rpartition("_")
        files.setdefault(base, {})[variant] = f"rolling/{stem}.wav"
    for variants in files.values():
        # metal_smooth_hard and x_jet_rolling have no soft/hard pair.
        for variant in ("soft", "hard"):
            if variant not in variants:
                variants[variant] = next(iter(variants.values()))

    surfaces = {str(index): surface_grain(name) for index, name in enumerate(AUDIO_SURFACES)}
    for index, base in surfaces.items():
        if base not in files:
            surfaces[index] = "concrete_rough"
    return {"default": "concrete_rough", "surfaces": surfaces, "files": files}


def _candidates(eaac, bank: Bank) -> list[tuple[float, bytes]]:
    """Parse every sample header without decoding; retain duration and bytes."""
    out: list[tuple[float, bytes]] = []
    for sample in bank.samples:
        sound = bank.sound(sample)
        header = eaac.Header.parse(sound, 0)
        if header is None or header.channels != 1:
            continue
        out.append((header.num_samples / header.sample_rate, sound))
    return out


def _select(candidates: list[tuple[float, bytes]], low: float, high: float,
            count: int) -> list[tuple[float, bytes]]:
    window = sorted((c for c in candidates if low <= c[0] <= high), key=lambda c: c[0])
    if not window:
        return []
    if len(window) <= count:
        return window
    step = len(window) / count
    return [window[int(index * step)] for index in range(count)]


def _one_shots(eaac, game_root: Path, out: Path, report) -> dict:
    archive = BigArchive(game_root / "data/audio/audiofiles.big")
    result: dict[str, list[str]] = {}
    plan = {
        "Skate_Collisions.bnk": {
            "pop": (0.03, 0.13, 3),
            "land": (0.13, 0.40, 6),
            "bail": (0.40, 1.40, 4),
        },
        "Skate_Metal.bnk": {"grind": (0.20, 1.20, 6)},
    }
    for bank_name, categories in plan.items():
        bank = Bank(_member(archive, bank_name))
        candidates = _candidates(eaac, bank)
        report(f"Selecting {len(candidates)} decoded effects from {bank_name}")
        for category, (low, high, count) in categories.items():
            chosen = _select(candidates, low, high, count)
            files: list[str] = []
            for index, (_, sound) in enumerate(chosen):
                header, pcm = eaac.decode(sound)
                name = f"sfx/{category}_{index}.wav"
                _write_wav(out / name, header.channels, header.sample_rate, pcm)
                files.append(name)
            if files:
                result[category] = files
    return result


def _wheels(eaac, game_root: Path, out: Path) -> dict:
    archive = BigArchive(game_root / "data/audio/wheels.big")
    files: dict[str, str] = {}
    for entry in archive.entries:
        if not entry.path.endswith(".snr"):
            continue
        stem = Path(entry.path).stem.lower()
        header, pcm = eaac.decode_member(archive.read(entry))
        _write_wav(out / "wheel" / f"{stem}.wav", header.channels, header.sample_rate, pcm)
        files[stem] = f"wheel/{stem}.wav"
    return files


def export_audio(game_root: Path, stage: Path, report, log) -> None:
    """Decode the gameplay audio slice into ``stage/assets/private/audio``."""
    from tools.owned_game import eaac
    from .optional_content import CONTENT_ERRORS, note

    private = stage / "assets/private"
    availability = private / "audio-availability.json"
    if not eaac.available():
        note(availability, "Gameplay audio", RuntimeError("PyAV is not installed (pip install av)"), report=report)
        return
    out = private / "audio"
    report("Decoding owned disc audio (skateboard rolling and effects)")
    try:
        rolling = _rolling(eaac, game_root, out)
    except CONTENT_ERRORS as error:
        note(availability, "Gameplay audio", error, report=report)
        return
    one_shots: dict[str, list[str]] = {}
    try:
        one_shots = _one_shots(eaac, game_root, out, report)
    except CONTENT_ERRORS as error:
        report(f"Gameplay effects could not be decoded: {error}")
    wheel: dict[str, str] = {}
    try:
        wheel = _wheels(eaac, game_root, out)
    except CONTENT_ERRORS as error:
        report(f"Wheel audio could not be decoded: {error}")
    manifest = {"version": 1, "rolling": rolling, "one_shots": one_shots, "wheel": wheel}
    (out / "audio.json").write_text(json.dumps(manifest, indent=1), encoding="utf-8")
    availability.unlink(missing_ok=True)
    effects = sum(len(files) for files in one_shots.values())
    report(f"Decoded {len(rolling['files'])} rolling surfaces and {effects} gameplay effects")
