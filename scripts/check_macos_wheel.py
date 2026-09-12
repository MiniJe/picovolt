"""Check universal2 wheel claims against the minimum OS in both Mach-O slices."""
from pathlib import Path
import re
import struct
import sys
import zipfile


def verify(path):
    match = re.search(r'-macosx_(\d+)_(\d+)_universal2\.whl$', path.name)
    if not match:
        raise ValueError('Expected a versioned universal2 wheel')
    declared = (int(match[1]), int(match[2]), 0)
    with zipfile.ZipFile(path) as archive:
        libraries = [n for n in archive.namelist() if n.endswith('/libpicovolt.dylib')]
        if len(libraries) != 1:
            raise ValueError('Expected exactly one bundled PicoVolt library')
        data = archive.read(libraries[0])
    magic, count = struct.unpack_from('>II', data)
    if magic != 0xcafebabe or count != 2:
        raise ValueError('Expected a two-architecture fat Mach-O')
    versions = {}
    for i in range(count):
        cpu, _, offset, size, _ = struct.unpack_from('>IIIII', data, 8 + i * 20)
        if offset + size > len(data) or size < 32 or cpu in versions:
            raise ValueError('Invalid Mach-O slice')
        magic, _, _, _, commands, command_size, _, _ = struct.unpack_from('<8I', data, offset)
        if magic != 0xfeedfacf or command_size + 32 > size:
            raise ValueError('Invalid 64-bit Mach-O header')
        pos, end, minimum = offset + 32, offset + 32 + command_size, None
        for _ in range(commands):
            if pos + 8 > end:
                raise ValueError('Truncated load command')
            command, length = struct.unpack_from('<II', data, pos)
            if length < 8 or pos + length > end:
                raise ValueError('Invalid load command length')
            if command == 0x24 and length >= 16:
                minimum = struct.unpack_from('<I', data, pos + 8)[0]
            elif command == 0x32 and length >= 24:
                minimum = struct.unpack_from('<I', data, pos + 12)[0]
            pos += length
        if minimum is None:
            raise ValueError('Missing minimum macOS version')
        versions[cpu] = (minimum >> 16, (minimum >> 8) & 255, minimum & 255)
    if set(versions) != {0x1000007, 0x100000c}:
        raise ValueError('Missing x86-64 or arm64 slice')
    if versions[0x1000007] > declared or versions[0x100000c] > max(declared, (11, 0, 0)):
        raise ValueError(f'Wheel claims {declared}, but library requires {versions}')
    return versions


if __name__ == '__main__':
    for filename in sys.argv[1:]:
        print(Path(filename).name, verify(Path(filename)))
