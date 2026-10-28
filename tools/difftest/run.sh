#!/usr/bin/env bash
# Differential test for the AArch64 lifter (macOS on Apple Silicon only).
# Runs seq.s on the host CPU and through intake -> lift -> Rust, then compares registers.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
HERE="$ROOT/tools/difftest"
BUILD="$HERE/build"
# Registers compared. x16 holds the output pointer in the native harness, and x18 is
# reserved by the macOS ABI; seq.s uses neither.
REGS="0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 17 19 20 21 22 23 24 25 26 27 28"
# SIMD&FP registers compared; v8-v15 are callee-saved on macOS, so sequences avoid them.
VREGS="0 1 2 3 4 5 6 7 16 17 18 19 20 21 22 23 24 25 26 27 28 29"

cargo build -q --release -p recomp-cli --manifest-path "$ROOT/Cargo.toml"
RECOMP="$ROOT/target/release/recomp"
rm -rf "$BUILD"
status=0
for SEQ in "${@:-$HERE/seq.s $HERE/simd.s $HERE/cond.s}"; do
for SEQ in $SEQ; do
NAME="$(basename "$SEQ" .s)"
B="$BUILD/$NAME"
mkdir -p "$B"

# 1. Native reference: call the sequence (entry at its first instruction) from a wrapper
#    that saves callee-saved registers and stores the result registers to out[].
python3 -I - "$SEQ" "$B/native.s" "$REGS" "$VREGS" <<'PY'
import sys
seq, out, regs, vregs = sys.argv[1], sys.argv[2], sys.argv[3].split(), sys.argv[4].split()
lines = [".globl _run", ".p2align 2", "_run:",
         "stp x19, x20, [sp, #-96]!", "stp x21, x22, [sp, #16]", "stp x23, x24, [sp, #32]",
         "stp x25, x26, [sp, #48]", "stp x27, x28, [sp, #64]", "stp x29, x30, [sp, #80]",
         "str x0, [sp, #-16]!"]
# Start from zeroed registers so both sides agree on values the sequence never writes.
lines += [f"mov x{r}, xzr" for r in regs] + [f"movi v{r}.2d, #0" for r in vregs]
lines += ["bl seq_entry", "ldr x16, [sp], #16"]
lines += [f"str x{r}, [x16, #{i * 8}]" for i, r in enumerate(regs)]
lines += [f"add x16, x16, #{len(regs) * 8}"]
lines += [f"str q{r}, [x16, #{i * 16}]" for i, r in enumerate(vregs)]
lines += ["ldp x21, x22, [sp, #16]", "ldp x23, x24, [sp, #32]", "ldp x25, x26, [sp, #48]",
          "ldp x27, x28, [sp, #64]", "ldp x29, x30, [sp, #80]", "ldp x19, x20, [sp], #96", "ret",
          "seq_entry:", open(seq).read()]
open(out, "w").write("\n".join(lines) + "\n")
PY
clang -O0 -DREGS="\"$REGS\"" -DVREGS="\"$VREGS\"" "$HERE/native.c" "$B/native.s" -o "$B/native"
"$B/native" > "$B/native.txt"

# 2. Recompiled: assemble the sequence into raw words and pack them as an NRO .text.
clang -c -target arm64-apple-macos "$SEQ" -o "$B/lift.o"
otool -tX "$B/lift.o" | awk '{ for (i = 2; i <= NF; i++) print $i }' > "$B/words.txt"
python3 -I "$ROOT/samples/homebrew-intake/generate.py" --no-assets \
  --text-words "$B/words.txt" --out-dir "$B/hb" > /dev/null
"$RECOMP" homebrew-intake --module "$B/hb/inputs/homebrew.nro" \
  --provenance "$B/hb/provenance.toml" --out-dir "$B/intake" > /dev/null
"$RECOMP" homebrew-lift --module-json "$B/intake/module.json" --out-dir "$B/lift" > /dev/null

MODULE="$B/lift/module.json"
cat > "$B/lift/provenance.toml" <<TOML
schema_version = "1"

[title]
name = "Lifter differential test"
title_id = "0100000000000001"
version = "1.0.0"
region = "US"

[collection]
device = "synthetic"
collected_at = "2026-10-28"
notes = "Synthetic instruction sequence from tools/difftest."

[collection.tool]
name = "tools/difftest/run.sh"
version = "1.0"

[[inputs]]
path = "module.json"
format = "lifted_json"
sha256 = "$(shasum -a 256 "$MODULE" | cut -d' ' -f1)"
size = $(wc -c < "$MODULE" | tr -d ' ')
role = "lifted_module"
TOML
"$RECOMP" run --module "$MODULE" --config "$ROOT/samples/minimal/title.toml" \
  --provenance "$B/lift/provenance.toml" --out-dir "$B/out" \
  --runtime-path "$ROOT/crates/recomp-runtime" > /dev/null
printf '\n[workspace]\n' >> "$B/out/Cargo.toml"

# Print the guest registers from the CPU context once the entry function returns.
python3 -I - "$B/out/src/main.rs" "$REGS" "$VREGS" <<'PY'
import sys
path, regs, vregs = sys.argv[1], sys.argv[2].split(), sys.argv[3].split()
src = open(path).read()
call = "entry(&mut ctx)?;"
prints = "".join(f'println!("x{r}={{:016x}}", ctx.x[{r}] as u64); ' for r in regs)
prints += "".join(f'println!("v{r}={{:032x}}", ctx.simd[{r}]); ' for r in vregs)
open(path, "w").write(src.replace(call, call + " " + prints, 1))
PY
cargo run -q --manifest-path "$B/out/Cargo.toml" 2> /dev/null | grep '^[xv]' > "$B/recomp.txt" || true

if diff "$B/native.txt" "$B/recomp.txt"; then
  echo "$NAME: MATCH ($(wc -l < "$B/native.txt" | tr -d ' ') registers)"
else
  echo "$NAME: MISMATCH (left: native, right: recompiled)" >&2
  status=1
fi
done
done
exit $status
