"""Summarise lifter gaps for one decompressed NSO.

Usage: report.py <nso_dir>
<nso_dir> holds text.bin (from nso_text.py) and unsupported.out (from the
recomp-pipeline `coverage` example).
"""
import struct
import sys
from collections import Counter
from pathlib import Path

import capstone

d = Path(sys.argv[1])
text = (d / "text.bin").read_bytes()
raw = (d / "unsupported.out").read_bytes()

md = capstone.Cs(capstone.CS_ARCH_ARM64, capstone.CS_MODE_ARM)
unsupported_words = set()
by_mnemonic = Counter()
by_class = Counter()


def classify(mn, ops):
    if mn.startswith(("ld", "st", "prfm")):
        if "q" in ops.split(",")[0] or any(r in ops for r in ("v0", "v1", ".16b", ".4s", ".2d", ".8h")) or ops[:1] in "sdqbh" and ops[1:2].isdigit():
            return "SIMD/FP load-store"
        if mn.startswith(("ldx", "stx", "lda", "stl", "ldadd", "cas", "swp")):
            return "atomics/exclusive"
        return "integer load-store (pairs, pre/post-index, register offset)"
    first = ops.split(",")[0].strip()
    if first[:1] in "sdhqv" and first[1:2].isdigit() or mn.startswith(("f", "scvtf", "ucvtf")):
        return "SIMD/FP arithmetic"
    if mn in ("svc", "mrs", "msr", "dmb", "dsb", "isb", "brk", "hint", "clrex", "sys"):
        return "system (svc/mrs/barriers)"
    if mn in ("blr", "braa", "blraa"):
        return "indirect call (blr)"
    if mn == "invalid":
        return "not code (data/padding)"
    return "integer ALU (imm logical, bitfield, mul/div, csel, extend...)"


for i in range(0, len(raw), 8):
    word, count = struct.unpack_from("<II", raw, i)
    unsupported_words.add(word)
    insn = next(md.disasm_lite(struct.pack("<I", word), 0), None)
    mn, ops = (insn[2], insn[3]) if insn else ("invalid", "")
    by_mnemonic[mn] += count
    by_class[classify(mn, ops)] += count

words = struct.unpack(f"<{len(text) // 4}I", text[: len(text) // 4 * 4])
total = len(words)
svcs = Counter((w >> 5) & 0xFFFF for w in words if w & 0xFFE0001F == 0xD4000001)
bl_targets = {i + ((w & 0x3FFFFFF) ^ 0x2000000) - 0x2000000 for i, w in enumerate(words) if w & 0xFC000000 == 0x94000000}
bad = sum(by_mnemonic.values())

# ponytail: functions are approximated as the spans between BL targets; tail-called or
# vtable-only functions are merged into their neighbours.
starts = sorted(t for t in bl_targets | {0} if 0 <= t < total) + [total]
liftable = sum(
    1 for a, b in zip(starts, starts[1:]) if not any(w in unsupported_words for w in words[a:b])
)

print(f"instructions: {total:,}  lifter-supported: {100 * (total - bad) / total:.1f}%")
print(f"distinct bl targets (function estimate): {len(bl_targets):,}")
print(f"functions fully liftable: {liftable:,} / {len(starts) - 1:,} = {100 * liftable / (len(starts) - 1):.1f}%")
print("unsupported by class:")
for k, v in by_class.most_common():
    print(f"  {100 * v / total:5.1f}%  {k}")
print("top unsupported mnemonics:")
for k, v in by_mnemonic.most_common(20):
    print(f"  {v:>10,}  {k}")
print("svc numbers used:", ", ".join(f"0x{n:02x}" for n in sorted(svcs)))
