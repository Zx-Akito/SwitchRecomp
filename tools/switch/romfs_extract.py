"""Extract the RomFS of a program NCA, optionally applying an update NCA's BKTR patch.

Usage:
  romfs_extract.py <prod.keys> <base.tik> <base.nca> <out_dir> [<update.tik> <update.nca>]
  romfs_extract.py <prod.keys> <base.tik> <base.nca> --list

With an update, the patched RomFS is reconstructed the way the console does it: the
update section's relocation table maps each virtual offset either to the base section or to
patch data, which is AES-CTR-EX encrypted with per-subsection counter generations.
NCA compressed storage (LZ4 blocks, firmware 12+) sits on top of that: base and patch
bytes are combined still compressed, then the update's compression table decompresses
the result.
"""
import bisect
import struct

import lz4.block
import sys
from pathlib import Path

from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes

BUCKET = 0x4000
CHUNK = 0x10_0000


def load_keys(path):
    keys = {}
    for line in Path(path).read_text().splitlines():
        if "=" in line:
            name, value = (s.strip() for s in line.split("=", 1))
            keys[name] = value
    return keys


def aes(key, mode):
    return Cipher(algorithms.AES(key), mode)


def ctr_crypt(key, iv_upper, offset, data):
    """AES-CTR where the low 8 counter bytes are the big-endian block index."""
    iv = iv_upper + (offset >> 4).to_bytes(8, "big")
    dec = aes(key, modes.CTR(iv)).decryptor()
    return dec.update(data) + dec.finalize()


class Nca:
    def __init__(self, path, keys, tik):
        self.f = open(path, "rb")
        hk = bytes.fromhex(keys["header_key"])
        raw = self.f.read(0xC00)
        hdr = bytearray()
        for sector in range(6):
            dec = aes(hk, modes.XTS(sector.to_bytes(16, "big"))).decryptor()
            hdr += dec.update(raw[sector * 0x200:(sector + 1) * 0x200]) + dec.finalize()
        if hdr[0x200:0x204] != b"NCA3":
            sys.exit(f"{path}: bad NCA header (wrong header_key?)")
        self.hdr = bytes(hdr)
        key_gen = max(hdr[0x206], hdr[0x220])
        key_gen = key_gen - 1 if key_gen else 0
        kek = bytes.fromhex(keys[f"titlekek_{key_gen:02x}"])
        dec = aes(kek, modes.ECB()).decryptor()
        self.key = dec.update(Path(tik).read_bytes()[0x180:0x190]) + dec.finalize()

    def romfs_section(self):
        for i in range(4):
            fs = self.hdr[0x400 + i * 0x200:0x600 + i * 0x200]
            start, end = struct.unpack_from("<II", self.hdr, 0x240 + i * 0x10)
            if end and fs[2] == 0:  # fs_type RomFS
                return Section(self, fs, start * 0x200, (end - start) * 0x200)
        sys.exit("no RomFS section")


class Section:
    def __init__(self, nca, fs, offset, size):
        self.nca, self.fs, self.offset, self.size = nca, fs, offset, size
        self.enc_type = fs[4]
        self.ctr_upper = fs[0x140:0x148][::-1]
        # IVFC: the last level (6) is the RomFS image.
        levels = fs[0x8 + 0x10:]
        self.romfs_offset, self.romfs_size = struct.unpack_from("<QQ", levels, 5 * 0x18)
        # Compressed storage covers the RomFS level only: its table, entry offsets and
        # virtual space are all relative to the start of level 6.
        # Built on first read: in an update section the table lives in the patched image.
        self.compressed = None

    def read(self, offset, size):
        """Reads the section's virtual image (through compressed storage if present)."""
        compression = self.fs[0x178:0x1A0]
        if self.compressed is None and compression[0x10:0x14] == b"BKTR":
            table_offset, table_size = struct.unpack_from("<QQ", compression)
            self.compressed = CompressedStorage(self, self.romfs_offset, table_offset, table_size)
        if self.compressed and offset >= self.romfs_offset:
            return self.compressed.read(offset - self.romfs_offset, size)
        return self.read_raw(offset, size)

    def read_raw(self, offset, size, iv_upper=None):
        """Reads `size` bytes at section `offset`, decrypting with plain AES-CTR."""
        aligned = offset & ~0xF
        self.nca.f.seek(self.offset + aligned)
        data = self.nca.f.read(size + offset - aligned)
        if self.enc_type in (3, 4):
            data = ctr_crypt(self.nca.key, iv_upper or self.ctr_upper, self.offset + aligned, data)
        return data[offset - aligned:]


class BucketTree:
    """Single-level bucket tree: a 0x4000 root node, then 0x4000 entry sets.

    Entry layouts (first field is the start offset):
      relocation  0x14  <QQI  virt, phys, is_patch
      subsection  0x10  <QII  offset, padding, ctr generation
      compression 0x18  <qqBbHI  virt, phys, type, level, reserved, phys_size
    """

    def __init__(self, section, offset, size, fmt):
        blob = section.read_raw(offset, size)
        entry_size = struct.calcsize(fmt)
        count, self.end = struct.unpack_from("<IQ", blob, 4)
        self.starts, self.entries = [], []
        for b in range(count):
            base = BUCKET * (b + 1)
            n = struct.unpack_from("<I", blob, base + 4)[0]
            for e in range(n):
                entry = struct.unpack_from(fmt, blob, base + 0x10 + e * entry_size)
                self.entries.append(entry)
                self.starts.append(entry[0])

    def find(self, offset):
        return bisect.bisect_right(self.starts, offset) - 1


class PatchedImage:
    """Virtual RomFS section image of base + BKTR update (decompressed by `read`)."""

    def __init__(self, base, update):
        self.base, self.update = base, update
        info = update.fs[0x100:0x140]
        roff, rsize = struct.unpack_from("<QQ", info, 0)
        soff, ssize = struct.unpack_from("<QQ", info, 0x20)
        self.reloc = BucketTree(update, roff, rsize, "<QQI")
        self.subs = BucketTree(update, soff, ssize, "<QII")
        self.romfs_offset = update.romfs_offset
        compression = update.fs[0x178:0x1A0]
        self.compressed = None
        if compression[0x10:0x14] == b"BKTR":
            table_offset, table_size = struct.unpack_from("<QQ", compression)
            self.compressed = CompressedStorage(self, self.romfs_offset, table_offset, table_size)

    def read_patch(self, phys, size):
        out = bytearray()
        while size:
            i = self.subs.find(phys)
            start, _, gen = self.subs.entries[i]
            end = self.subs.starts[i + 1] if i + 1 < len(self.subs.starts) else phys + size
            n = min(size, end - phys)
            iv = self.update.ctr_upper[:4] + gen.to_bytes(4, "big")
            out += self.update.read_raw(phys, n, iv)
            phys += n
            size -= n
        return bytes(out)

    def read(self, virt, size):
        if self.compressed and virt >= self.romfs_offset:
            return self.compressed.read(virt - self.romfs_offset, size)
        return self.read_raw(virt, size)

    def read_raw(self, virt, size):
        """Indirect storage: still-compressed bytes from the base section or the patch."""
        out = bytearray()
        while size:
            i = self.reloc.find(virt)
            start, phys, is_patch = self.reloc.entries[i]
            end = self.reloc.starts[i + 1] if i + 1 < len(self.reloc.starts) else virt + size
            n = min(size, end - virt)
            p = phys + (virt - start)
            out += self.read_patch(p, n) if is_patch else self.base.read_raw(p, n)
            virt += n
            size -= n
        return bytes(out)


class CompressedStorage:
    """NCA compressed storage: virtual ranges stored raw (0), as zeros (1) or LZ4 (3).

    `section` is any source with read_raw(offset, size): an NCA section, or the indirect
    storage of a patched image.
    """

    def __init__(self, section, base, offset, size):
        self.section, self.base = section, base
        self.tree = BucketTree(section, base + offset, size, "<qqBbHI")
        self.cached = (None, b"")

    def block(self, i):
        if self.cached[0] != i:
            virt, phys, _, _, _, phys_size = self.tree.entries[i]
            end = self.tree.starts[i + 1] if i + 1 < len(self.tree.starts) else self.tree.end
            raw = self.section.read_raw(self.base + phys, phys_size)
            self.cached = (i, lz4.block.decompress(raw, uncompressed_size=end - virt))
        return self.cached[1]

    def read(self, virt, size):
        out = bytearray()
        while size:
            i = self.tree.find(virt)
            start, phys, kind, _, _, _ = self.tree.entries[i]
            end = self.tree.starts[i + 1] if i + 1 < len(self.tree.starts) else self.tree.end
            n = min(size, end - virt)
            if kind == 0:
                out += self.section.read_raw(self.base + phys + virt - start, n)
            elif kind == 1:
                out += bytes(n)
            elif kind == 3:
                out += self.block(i)[virt - start:virt - start + n]
            else:
                sys.exit(f"unsupported compression type {kind}")
            virt += n
            size -= n
        return bytes(out)


class BaseImage:
    def __init__(self, section):
        self.section = section
        self.romfs_offset = section.romfs_offset

    def read(self, virt, size):
        return self.section.read(virt, size)


def walk_romfs(image):
    """Yields (path, data_offset_in_image, size) for every file."""
    base = image.romfs_offset
    hdr = image.read(base, 0x50)
    (_, _, _, dir_meta_off, dir_meta_size, _, _, file_meta_off, file_meta_size,
     data_off) = struct.unpack_from("<10Q", hdr)
    dirs = image.read(base + dir_meta_off, dir_meta_size)
    files = image.read(base + file_meta_off, file_meta_size)

    def name(blob, at, length):
        return blob[at:at + length].decode("utf-8", "replace")

    stack = [(0, "")]
    while stack:
        d, prefix = stack.pop()
        _, sibling, child, first_file, _, nlen = struct.unpack_from("<IIIIII", dirs, d)
        f = first_file
        while f != 0xFFFFFFFF:
            _, next_file, off, size, _, flen = struct.unpack_from("<IIQQII", files, f)
            yield f"{prefix}/{name(files, f + 0x20, flen)}", base + data_off + off, size
            f = next_file
        c = child
        while c != 0xFFFFFFFF:
            _, csib, _, _, _, clen = struct.unpack_from("<IIIIII", dirs, c)
            stack.append((c, f"{prefix}/{name(dirs, c + 0x18, clen)}"))
            c = csib


def main():
    keys = load_keys(sys.argv[1])
    base = Nca(sys.argv[3], keys, sys.argv[2]).romfs_section()
    if len(sys.argv) > 6:
        update = Nca(sys.argv[6], keys, sys.argv[5]).romfs_section()
        if update.enc_type != 4:
            sys.exit("update RomFS is not a BKTR patch")
        image = PatchedImage(base, update)
    else:
        image = BaseImage(base)
    if sys.argv[4] == "--list":
        for path, _, size in walk_romfs(image):
            print(f"{size:>12}  {path}")
        return
    out = Path(sys.argv[4])
    total = 0
    for path, offset, size in walk_romfs(image):
        dest = out / path.lstrip("/")
        dest.parent.mkdir(parents=True, exist_ok=True)
        with dest.open("wb") as f:
            for at in range(0, size, CHUNK):
                f.write(image.read(offset + at, min(CHUNK, size - at)))
        total += size
    print(f"extracted {total / 1e9:.2f} GB to {out}")


if __name__ == "__main__":
    main()
