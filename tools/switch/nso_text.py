"""Decompress NSO segments. Usage: nso_text.py <nso> <out_dir>  (writes text.bin, rodata.bin, data.bin)"""
import struct
import sys
from pathlib import Path

import lz4.block


def main():
    raw = Path(sys.argv[1]).read_bytes()
    out = Path(sys.argv[2])
    out.mkdir(parents=True, exist_ok=True)
    if raw[:4] != b"NSO0":
        sys.exit("not an NSO")
    flags = struct.unpack_from("<I", raw, 0xC)[0]
    comp_sizes = struct.unpack_from("<III", raw, 0x60)
    for i, name in enumerate(("text", "rodata", "data")):
        file_off, mem_off, size = struct.unpack_from("<III", raw, 0x10 + i * 0x10)
        blob = raw[file_off : file_off + comp_sizes[i]]
        if flags & (1 << i):
            blob = lz4.block.decompress(blob, uncompressed_size=size)
        (out / f"{name}.bin").write_bytes(blob)
        print(f"{name:7} mem=0x{mem_off:08x} size={size:>10}")


if __name__ == "__main__":
    main()
