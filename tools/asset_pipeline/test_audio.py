import math
import struct
import unittest

from .audio import _rolling_pcm, _write_wav


def ramp_pcm(seconds: float, rate: int, db_per_s: float) -> bytes:
    """Mono S16LE whose amplitude grows by ``db_per_s`` decibels per second."""
    count = int(seconds * rate)
    step = math.pow(10.0, db_per_s / 20.0 / rate)
    samples = []
    amplitude = 4000.0
    for index in range(count):
        amplitude *= step
        samples.append(int(amplitude * math.sin(2.0 * math.pi * 440.0 * index / rate)))
    return struct.pack(f'<{count}h', *samples)


def envelope_db(pcm: bytes, rate: int) -> list[float]:
    count = len(pcm) // 2
    values = struct.unpack(f'<{count}h', pcm)
    step = max(count // 10, 1)
    out = []
    for index in range(0, 10 * step, step):
        window = values[index:index + step]
        mean = sum(float(v) * v for v in window) / len(window)
        out.append(10.0 * math.log10(max(mean, 1e-9)))
    return out


class RollingAudioTests(unittest.TestCase):
    def test_builtin_ramp_is_flattened(self):
        pcm = ramp_pcm(20.0, 44100, 0.5)
        before = envelope_db(pcm, 44100)
        flat = _rolling_pcm(pcm, 1, 44100)
        after = envelope_db(flat, 44100)
        self.assertGreater(before[-1] - before[0], 8.0)
        self.assertLess(after[-1] - after[0], 1.5)

    def test_steady_input_is_left_alone(self):
        pcm = ramp_pcm(20.0, 44100, 0.0)
        before = envelope_db(pcm, 44100)
        after = envelope_db(_rolling_pcm(pcm, 1, 44100), 44100)
        self.assertLess(abs((after[-1] - after[0]) - (before[-1] - before[0])), 1.0)

    def test_output_length_and_range_are_preserved(self):
        pcm = ramp_pcm(20.0, 44100, 0.5)
        flat = _rolling_pcm(pcm, 1, 44100)
        self.assertEqual(len(flat), len(pcm))
        self.assertTrue(all(-32768 <= v <= 32767 for v in struct.unpack(f'<{len(flat)//2}h', flat)))

    def test_stereo_keeps_channels_interleaved(self):
        mono = ramp_pcm(20.0, 44100, 0.5)
        values = struct.unpack(f'<{len(mono)//2}h', mono)
        stereo = struct.pack(f'<{len(values)*2}h', *(v for v in values for _ in range(2)))
        flat = _rolling_pcm(stereo, 2, 44100)
        self.assertEqual(len(flat), len(stereo))
        self.assertEqual(len(flat) // 4, len(values))

    def test_short_input_is_passed_through(self):
        pcm = struct.pack('<64h', *([1000] * 64))
        self.assertEqual(_rolling_pcm(pcm, 1, 44100), pcm)

    def test_written_wav_reports_its_size(self):
        import tempfile
        from pathlib import Path

        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'rolling' / 'probe.wav'
            pcm = ramp_pcm(2.0, 8000, 0.5)
            _write_wav(path, 1, 8000, pcm)
            body = path.read_bytes()
            self.assertEqual(body[:4], b'RIFF')
            self.assertEqual(struct.unpack_from('<I', body, 4)[0], len(body) - 8)
            self.assertEqual(body[12:16], b'fmt ')
            self.assertEqual(struct.unpack_from('<I', body, 16)[0], 16)
            self.assertEqual(body[36:40], b'data')
            self.assertEqual(struct.unpack_from('<I', body, 40)[0], len(pcm))
