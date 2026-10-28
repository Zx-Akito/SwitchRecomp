"""Extract the ExeFS (section 0) of a titlekey-encrypted program NCA.

Works on update NCAs whose RomFS is BKTR, which hactool refuses without a base NCA.
Usage: nca_exefs.py <prod.keys> <ticket.tik> <program.nca> <out_dir>
"""
import struct
import sys
from pathlib import Path

from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes


def load_keys(path):
    keys = {}
    for line in Path(path).read_text().splitlines():
        if "=" in line:
            name, value = (s.strip() for s in line.split("=", 1))
            keys[name] = value
    return keys


def aes(key, mode):
    return Cipher(algorithms.AES(key), mode)


def decrypt_header(raw, header_key):
    # Nintendo XTS: 0x200 sectors, tweak is the big-endian sector number.
    out = bytearray()
    for sector in range(len(raw) // 0x200):
        tweak = sector.to_bytes(16, "big")
        dec = aes(header_key, modes.XTS(tweak)).decryptor()
        out += dec.update(raw[sector * 0x200 : (sector + 1) * 0x200]) + dec.finalize()
    return bytes(out)


def ctr_read(f, key, section_ctr, offset, size):
    aligned = offset & ~0xF
    f.seek(aligned)
    data = f.read(size + (offset - aligned))
    iv = section_ctr[::-1] + (aligned >> 4).to_bytes(8, "big")
    dec = aes(key, modes.CTR(iv)).decryptor()
    return (dec.update(data) + dec.finalize())[offset - aligned :]


def main():
    keys_path, tik_path, nca_path, out_dir = sys.argv[1:5]
    keys = load_keys(keys_path)
    out = Path(out_dir)
    out.mkdir(parents=True, exist_ok=True)

    with open(nca_path, "rb") as f:
        hdr = decrypt_header(f.read(0xC00), bytes.fromhex(keys["header_key"]))
        if hdr[0x200:0x204] != b"NCA3":
            sys.exit(f"bad NCA magic {hdr[0x200:0x204]!r} (wrong header_key?)")
        content_type = hdr[0x205]
        key_gen = max(hdr[0x206], hdr[0x220])
        key_gen = key_gen - 1 if key_gen else 0
        program_id = struct.unpack_from("<Q", hdr, 0x210)[0]
        print(f"program_id={program_id:016x} content_type={content_type} key_gen={key_gen}")
        if content_type != 0:
            sys.exit("not a Program NCA")

        tik = Path(tik_path).read_bytes()
        titlekek = bytes.fromhex(keys[f"titlekek_{key_gen:02x}"])
        dec = aes(titlekek, modes.ECB()).decryptor()
        section_key = dec.update(tik[0x180:0x190]) + dec.finalize()

        start_media, _end_media = struct.unpack_from("<II", hdr, 0x240)
        fs = hdr[0x400:0x600]
        fs_type, _hash_type, enc_type = fs[2], fs[3], fs[4]
        if fs_type != 1 or enc_type != 3:
            sys.exit(f"section 0 is not a CTR-encrypted PFS0 (fs_type={fs_type} enc={enc_type})")
        pfs_off, pfs_size = struct.unpack_from("<QQ", fs, 0x40)
        base = start_media * 0x200 + pfs_off
        section_ctr = fs[0x140:0x148]

        head = ctr_read(f, section_key, section_ctr, base, 0x10)
        magic, count, strsz = struct.unpack_from("<4sII", head)
        if magic != b"PFS0":
            sys.exit(f"bad PFS0 magic {magic!r} (wrong titlekey/key_gen?)")
        table = ctr_read(f, section_key, section_ctr, base, 0x10 + count * 0x18 + strsz)
        strings = table[0x10 + count * 0x18 :]
        data_start = base + 0x10 + count * 0x18 + strsz
        for i in range(count):
            off, size, name_off, _ = struct.unpack_from("<QQII", table, 0x10 + i * 0x18)
            name = strings[name_off : strings.index(b"\0", name_off)].decode()
            (out / name).write_bytes(ctr_read(f, section_key, section_ctr, data_start + off, size))
            print(f"{size:>10}  {name}")


if __name__ == "__main__":
    main()
