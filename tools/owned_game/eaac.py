"""EA Audio Core (EAAC codec 3 / XMA2) decoding for owned Skate 3 data.

This module owns the container arithmetic -- stream headers, block chains and the
per-XMA-context chunk split -- and a thin binding to libavcodec (through PyAV) that
turns those chunks back into PCM. It deliberately does not know where a stream came
from, so the same routine decodes ``.grain``/``.snr`` members, SPLC ``.bnk`` sounds
and (later) bank contents.

Stream layout (big-endian), established by the ``skate3-audio`` reverse engineering::

    header1  [4b version][4b codec][6b channel_config][18b sample_rate]
    header2  [2b stream_type][1b loop][29b num_samples]
    then     block chain: {flags<<24 | size, num_samples}[payload]

Each block payload is ``ceil(channels / 2)`` XMA contexts, each preceded by a u32
length field ``length = (field - 18) >> 2``. Contexts must be decoded through a
persistent decoder (one ``AVCodecContext`` per context, flushed at the end); a fresh
decoder per chunk loses 64 samples of MDCT overlap at every boundary.

PyAV is an optional runtime dependency: importing it lazily keeps the rest of setup
working on hosts without it, and callers should treat :class:`Unavailable` as the
"decoder not installed" signal.
"""
from __future__ import annotations

import struct
from dataclasses import dataclass

from .binary import FormatError


class Unavailable(RuntimeError):
    """Raised when the XMA decoder (PyAV/libavcodec) is not installed."""


def _av():
    try:
        import av  # noqa: PLC0415 - optional dependency, probed on demand
    except ImportError as error:  # pragma: no cover - depends on the host
        raise Unavailable("PyAV (pip install av) is required to decode retail audio") from error
    return av


def available() -> bool:
    """True when the XMA decoder can be used on this host."""
    try:
        _av()
        return True
    except Unavailable:
        return False


@dataclass(frozen=True)
class Header:
    version: int
    codec: int
    channels: int
    sample_rate: int
    num_samples: int
    looping: bool

    @classmethod
    def parse(cls, data: bytes, at: int = 0) -> "Header | None":
        if at < 0 or at + 8 > len(data):
            return None
        h1, h2 = struct.unpack_from(">II", data, at)
        header = cls(
            version=(h1 >> 28) & 0xF,
            codec=(h1 >> 24) & 0xF,
            channels=((h1 >> 18) & 0x3F) + 1,
            sample_rate=h1 & 0x3FFFF,
            num_samples=h2 & 0x1FFF_FFFF,
            looping=(h2 >> 29) & 1 != 0,
        )
        return header if header.plausible() else None

    def plausible(self) -> bool:
        return (
            self.version == 0
            and self.codec == 3
            and self.channels in (1, 2, 3, 4, 5, 6)
            and self.sample_rate in (36_000, 44_100, 48_000)
            and 0 < self.num_samples <= 200_000_000
        )


@dataclass(frozen=True)
class Block:
    offset: int
    size: int
    num_samples: int
    flags: int

    @property
    def payload(self) -> tuple[int, int]:
        return self.offset + 8, self.offset + self.size


def walk_blocks(data: bytes, at: int) -> list[Block]:
    """Walk the block chain at ``at`` until the buffer ends."""
    blocks: list[Block] = []
    while at + 8 <= len(data):
        word, num_samples = struct.unpack_from(">II", data, at)
        size = word & 0x00FF_FFFF
        if size <= 8 or at + size > len(data):
            break
        blocks.append(Block(at, size, num_samples, (word >> 24) & 0xFF))
        at += size
    return blocks


def context_count(channels: int) -> int:
    return (channels + 1) // 2


def context_widths(channels: int) -> list[int]:
    return [2] * (channels // 2) + ([1] if channels % 2 else [])


def chunk_length(field: int) -> int | None:
    if field < 18:
        return None
    length = (field - 18) >> 2
    return length if length > 0 else None


def split_block(payload: bytes, contexts: int) -> list[bytes] | None:
    """Split one block payload into its per-context chunks, or ``None`` if unsound."""
    chunks: list[bytes] = []
    pos = 0
    for _ in range(contexts):
        if pos + 4 > len(payload):
            return None
        field = struct.unpack_from(">I", payload, pos)[0]
        length = chunk_length(field)
        if length is None or pos + 4 + length > len(payload):
            return None
        chunks.append(payload[pos + 4 : pos + 4 + length])
        pos += 4 + length
    # The final block of a stream is padded to an alignment boundary.
    return chunks if len(payload) - pos <= 64 else None


def find_header(data: bytes) -> int | None:
    """Locate a stream header: at offset 0, behind a leading offset word, or by scan."""
    if Header.parse(data, 0) is not None:
        return 0
    if len(data) >= 4:
        lead = struct.unpack_from(">I", data, 0)[0]
        if 0 < lead <= len(data) - 8 and Header.parse(data, lead) is not None:
            return lead
    for at in range(0, len(data) - 8, 2):
        if Header.parse(data, at) is not None:
            return at
    return None


def _extradata(width: int, rate: int, block_align: int, block_count: int) -> bytes:
    mask = {1: 0x4, 2: 0x3}.get(width, 0x3)
    return struct.pack(
        "<HIIIIIIIBBH",
        1,
        mask,
        0,
        block_align,
        0,
        0,
        0,
        0,
        0,
        4,
        block_count & 0xFFFF,
    )


def _decode_context(av, chunks: list[bytes], width: int, rate: int) -> bytes:
    """Decode one persistent XMA context to interleaved little-endian int16."""
    import numpy as np  # noqa: PLC0415 - numpy ships with the setup toolchain

    context = av.CodecContext.create("xma2", "r")
    context.extradata = _extradata(width, rate, 2048, 0)
    context.sample_rate = rate
    context.layout = "mono" if width == 1 else "stereo"
    context.open()

    pieces: list[bytes] = []
    for chunk in chunks:
        for frame in context.decode(av.Packet(chunk)):
            samples = np.clip(frame.to_ndarray(), -1.0, 1.0)
            pieces.append((samples * 32767.0).astype("<i2").T.tobytes())
    # Flushing recovers the frames the decoder held back while buffering.
    for frame in context.decode(None):
        samples = np.clip(frame.to_ndarray(), -1.0, 1.0)
        pieces.append((samples * 32767.0).astype("<i2").T.tobytes())
    return b"".join(pieces)


def decode(data: bytes, header_at: int | None = None) -> tuple[Header, bytes]:
    """Decode an EAAC stream to ``(header, interleaved S16LE PCM)``.

    Raises :class:`Unavailable` when PyAV is missing and :class:`FormatError` when the
    container cannot be decoded.
    """
    av = _av()
    import numpy as np  # noqa: PLC0415

    header_at = find_header(data) if header_at is None else header_at
    if header_at is None:
        raise FormatError("no EAAC stream header found")
    header = Header.parse(data, header_at)
    if header is None:
        raise FormatError(f"implausible EAAC header at {header_at:#x}")

    blocks = walk_blocks(data, header_at + 8)
    if not blocks:
        raise FormatError("EAAC stream has no blocks")

    contexts = context_count(header.channels)
    widths = context_widths(header.channels)
    per_context: list[list[bytes]] = [[] for _ in range(contexts)]
    for block in blocks:
        start, end = block.payload
        spans = split_block(data[start:end], contexts)
        if spans is None:
            raise FormatError(f"block at {block.offset:#x} does not split into {contexts} contexts")
        for index, span in enumerate(spans):
            per_context[index].append(span)

    decoded = [
        _decode_context(av, per_context[index], widths[index], header.sample_rate)
        for index in range(contexts)
    ]

    if contexts == 1:
        return header, decoded[0]

    # Interleave channel-major contexts back into a single S16LE stream.
    frames = min(len(pcm) // (2 * widths[i]) for i, pcm in enumerate(decoded))
    arrays = [np.frombuffer(pcm, dtype="<i2").reshape(-1, widths[i]) for i, pcm in enumerate(decoded)]
    interleaved = np.concatenate([a[:frames] for a in arrays], axis=1)
    return header, interleaved.astype("<i2").tobytes()


def decode_member(data: bytes) -> tuple[Header, bytes]:
    """Decode a standalone audio member (``.grain``, ``.snr``, ``.sps``)."""
    return decode(data)
