"""Lay out a decompressed NSO the way rtld would, ahead of time.

Usage: nso_image.py <nso_file> <nso_dir> <out_dir>
  <nso_file>  original (compressed) NSO, for the segment header
  <nso_dir>   text.bin/rodata.bin/data.bin from nso_text.py

Writes to <out_dir>:
  text.bin, rodata.bin, data.bin   segment bytes with relocations applied (module base 0)
  segments.json                    "segments" list for a lifted module.json
  memory_layout.toml               [runtime.memory_layout] regions for title.toml
  imports.tsv / imports.json       placeholder address -> imported symbol name
                                   (imports.json is the lifted module.json "imports" list)

Imports are bound to unique placeholder addresses at IMPORT_BASE so an unresolved call
reports exactly which symbol it needed (UnknownCallTarget { address }).
"""
import json
import struct
import sys
from pathlib import Path

IMPORT_BASE = 0xF000_0000
IMPORT_STRIDE = 0x10
HEAP = ("heap", 0x1_0000_0000, 0x400_0000)  # 64 MiB
STACK = ("stack", 0x2_0000_0000, 0x10_0000)  # 1 MiB

R_AARCH64_ABS64 = 257
R_AARCH64_GLOB_DAT = 1025
R_AARCH64_JUMP_SLOT = 1026
R_AARCH64_RELATIVE = 1027
DT = {"PLTRELSZ": 2, "STRTAB": 5, "SYMTAB": 6, "RELA": 7, "RELASZ": 8, "JMPREL": 23,
      "RELRSZ": 35, "RELR": 36}


def main():
    nso_file, nso_dir, out_dir = (Path(p) for p in sys.argv[1:4])
    out_dir.mkdir(parents=True, exist_ok=True)
    header = nso_file.read_bytes()[:0x100]
    names = ("text", "rodata", "data")
    mem_offs = [struct.unpack_from("<I", header, 0x14 + i * 0x10)[0] for i in range(3)]
    bss_size = struct.unpack_from("<I", header, 0x3C)[0]
    blobs = [(nso_dir / f"{n}.bin").read_bytes() for n in names]

    image = bytearray(mem_offs[2] + len(blobs[2]) + bss_size)
    for off, blob in zip(mem_offs, blobs):
        image[off : off + len(blob)] = blob

    def u64(addr):
        return struct.unpack_from("<Q", image, addr)[0]

    def put64(addr, value):
        struct.pack_into("<Q", image, addr, value & 0xFFFF_FFFF_FFFF_FFFF)

    mod0 = struct.unpack_from("<I", image, 4)[0]
    if image[mod0 : mod0 + 4] != b"MOD0":
        sys.exit("MOD0 header not found")
    dyn = mod0 + struct.unpack_from("<i", image, mod0 + 4)[0]
    tags = {}
    while True:
        tag, val = struct.unpack_from("<qQ", image, dyn)
        dyn += 16
        if tag == 0:
            break
        tags.setdefault(tag, val)

    symtab, strtab = tags.get(DT["SYMTAB"]), tags.get(DT["STRTAB"])
    imports = {}

    def symbol(index):
        name_off, _info, _other, shndx, value, _size = struct.unpack_from(
            "<IBBHQQ", image, symtab + index * 24
        )
        if shndx:
            return value
        if index not in imports:
            end = image.index(b"\0", strtab + name_off)
            imports[index] = (IMPORT_BASE + len(imports) * IMPORT_STRIDE,
                              image[strtab + name_off : end].decode(errors="replace"))
        return imports[index][0]

    counts = {}
    for table, size in ((DT["RELA"], DT["RELASZ"]), (DT["JMPREL"], DT["PLTRELSZ"])):
        if table not in tags:
            continue
        for k in range(tags.get(size, 0) // 24):
            offset, info, addend = struct.unpack_from("<QQq", image, tags[table] + k * 24)
            rtype, sym = info & 0xFFFF_FFFF, info >> 32
            if rtype == R_AARCH64_RELATIVE:
                put64(offset, addend)
            elif rtype in (R_AARCH64_GLOB_DAT, R_AARCH64_JUMP_SLOT, R_AARCH64_ABS64):
                put64(offset, symbol(sym) + addend)
            else:
                sys.exit(f"unsupported relocation type {rtype} at {offset:#x}")
            counts[rtype] = counts.get(rtype, 0) + 1

    # RELR: packed relative relocations (address entries, then bitmaps of following words).
    if DT["RELR"] in tags:
        where = 0
        for k in range(tags.get(DT["RELRSZ"], 0) // 8):
            entry = u64(tags[DT["RELR"]] + k * 8)
            if entry & 1 == 0:
                put64(entry, u64(entry))
                where = entry + 8
            else:
                for bit in range(63):
                    if entry >> (bit + 1) & 1:
                        addr = where + bit * 8
                        put64(addr, u64(addr))
                where += 63 * 8
            counts["relr"] = counts.get("relr", 0) + 1

    ends = [mem_offs[1], mem_offs[2], len(image)]
    perms = [(True, False, True), (True, False, False), (True, True, False)]
    segments, regions = [], []
    for i, name in enumerate(names):
        start, end = mem_offs[i], ends[i]
        init = blobs[i] if i == 0 else image[start : start + len(blobs[i])]
        (out_dir / f"{name}.bin").write_bytes(init)
        read, write, execute = perms[i]
        permissions = {"read": read, "write": write, "execute": execute}
        segments.append({"name": name, "base": start, "size": end - start,
                         "permissions": permissions, "init_path": f"{name}.bin",
                         "init_size": len(init), "zero_fill": end - start > len(init)})
        regions.append((name, start, end - start, permissions))
    for name, base, size in (HEAP, STACK):
        regions.append((name, base, size, {"read": True, "write": True, "execute": False}))

    (out_dir / "segments.json").write_text(json.dumps(segments, indent=2))
    with (out_dir / "memory_layout.toml").open("w") as f:
        f.write("[runtime.memory_layout]\n")
        for name, base, size, p in regions:
            f.write(f"[[runtime.memory_layout.regions]]\nname = \"{name}\"\nbase = {base:#x}\n"
                    f"size = {size:#x}\npermissions = {{ read = {str(p['read']).lower()}, "
                    f"write = {str(p['write']).lower()}, execute = {str(p['execute']).lower()} }}\n\n")
    with (out_dir / "imports.tsv").open("w") as f:
        for addr, name in sorted(imports.values()):
            f.write(f"{addr:#x}\t{name}\n")
    (out_dir / "imports.json").write_text(
        json.dumps([{"address": a, "name": n} for a, n in sorted(imports.values())], indent=2)
    )

    print(f"image {len(image):#x} bytes, bss {bss_size:#x}, relocations {counts}, "
          f"imports {len(imports)}")


if __name__ == "__main__":
    main()
