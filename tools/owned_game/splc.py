"""SPLC (``.bnk``) sound banks used by Skate 3's audio effect banks.

Layout (big-endian), recovered from ``Skate_Collisions.bnk``/``Skate_Metal.bnk``::

    0x00  "SPLC"
    0x04  u32 version (3)
    0x08  u32 directory_offset
    0x0C  u32 event_count
    0x10  u32 unknown
    0x14  u32 0
    0x18  u32 sound_count
    0x1C  ... bank name, NUL-terminated

At ``directory_offset`` a 60-byte header precedes ``sound_count`` 12-byte records::

    +0  u32 start   byte offset of the sound, relative to the sample area
    +4  u32 end
    +8  u32 name_hash

The sample area begins immediately after the records; every sound is an EAAC stream
whose 8-byte header is at ``start``.
"""
from __future__ import annotations

import struct
from dataclasses import dataclass

from .binary import FormatError, Reader

HEADER_SIZE = 0x30
DIRECTORY_PREFIX = 60
RECORD_SIZE = 12


@dataclass(frozen=True)
class Sample:
    index: int
    start: int
    end: int
    name_hash: int

    @property
    def size(self) -> int:
        return self.end - self.start


class Bank:
    """A parsed SPLC bank. Holds the whole member; exposes samples and their bytes."""

    def __init__(self, data: bytes):
        reader = Reader(data, "SPLC")
        if reader.bytes(0, 4) != b"SPLC":
            raise FormatError("not an SPLC bank")
        version = reader.u32be(4)
        if version != 3:
            raise FormatError(f"unsupported SPLC version {version}")
        self.data = data
        self.directory_offset = reader.u32be(8)
        self.event_count = reader.u32be(0x0C)
        self.sound_count = reader.u32be(0x18)
        name_at = 0x1C
        name_end = data.find(b"\0", name_at, min(len(data), name_at + 64))
        self.name = data[name_at : name_end if name_end != -1 else name_at].decode(
            "utf-8", errors="replace"
        )

        records_start = self.directory_offset + DIRECTORY_PREFIX
        records_end = records_start + self.sound_count * RECORD_SIZE
        if self.sound_count == 0 or records_end > len(data):
            raise FormatError(f"SPLC directory for {self.name!r} exceeds the member")
        self.samples_offset = records_end
        self.samples: list[Sample] = []
        for index in range(self.sound_count):
            at = records_start + index * RECORD_SIZE
            start, end, name_hash = struct.unpack_from(">III", data, at)
            if start > end or self.samples_offset + end > len(data):
                raise FormatError(f"{self.name!r}: sample {index} is out of bounds")
            self.samples.append(Sample(index, start, end, name_hash))

    def sound(self, sample: Sample) -> bytes:
        base = self.samples_offset + sample.start
        return self.data[base : base + sample.size]
