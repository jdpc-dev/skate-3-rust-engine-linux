"""Optional bundled native RefPack backend; source-only tools retain Python."""
import ctypes
from pathlib import Path

# The accelerator is built as refpack.dll on Windows and refpack.so elsewhere.
_stems=(Path(__file__).with_name('refpack'),Path(__file__).resolve().parents[2]/'target/native/refpack')

_library=None
for stem in _stems:
    for suffix in ('dll','so','dylib'):
        path=stem.with_name(stem.name+'.'+suffix)
        if not path.is_file():continue
        try:_library=ctypes.CDLL(str(path))
        except OSError:continue
        _library.skate_refpack.argtypes=[ctypes.c_char_p,ctypes.c_size_t,ctypes.c_void_p,ctypes.c_size_t,ctypes.c_size_t,ctypes.c_bool]
        _library.skate_refpack.restype=ctypes.c_int
        break
    if _library is not None:break

def decode(data,size,start,early=False):
    if _library is None:return None
    output=ctypes.create_string_buffer(size)
    if _library.skate_refpack(data,len(data),output,size,start,early):
        raise ValueError('Malformed RefPack command or declared output size')
    return output.raw
