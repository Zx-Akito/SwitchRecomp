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

rm -rf "$BUILD"
mkdir -p "$BUILD"

# 1. Native reference: call seq.s (entry at its first instruction) from a wrapper that
#    saves callee-saved registers and stores the result registers to out[].
python3 -I - "$HERE/seq.s" "$BUILD/native.s" $REGS <<'PY'
import sys
seq, out, regs = sys.argv[1], sys.argv[2], sys.argv[3:]
lines = [".globl _run", ".p2align 2", "_run:",
         "stp x19, x20, [sp, #-96]!", "stp x21, x22, [sp, #16]", "stp x23, x24, [sp, #32]",
         "stp x25, x26, [sp, #48]", "stp x27, x28, [sp, #64]", "stp x29, x30, [sp, #80]",
         "str x0, [sp, #-16]!", "bl seq_entry", "ldr x16, [sp], #16"]
lines += [f"str x{r}, [x16, #{i * 8}]" for i, r in enumerate(regs)]
lines += ["ldp x21, x22, [sp, #16]", "ldp x23, x24, [sp, #32]", "ldp x25, x26, [sp, #48]",
          "ldp x27, x28, [sp, #64]", "ldp x29, x30, [sp, #80]", "ldp x19, x20, [sp], #96", "ret",
          "seq_entry:", open(seq).read()]
open(out, "w").write("\n".join(lines) + "\n")
PY
clang -O0 -DREGS="\"$REGS\"" "$HERE/native.c" "$BUILD/native.s" -o "$BUILD/native"
"$BUILD/native" > "$BUILD/native.txt"

# 2. Recompiled: assemble seq.s into raw words and pack them as an NRO .text.
clang -c -target arm64-apple-macos "$HERE/seq.s" -o "$BUILD/lift.o"
otool -tX "$BUILD/lift.o" | awk '{ for (i = 2; i <= NF; i++) print $i }' > "$BUILD/words.txt"
python3 -I "$ROOT/samples/homebrew-intake/generate.py" --no-assets \
  --text-words "$BUILD/words.txt" --out-dir "$BUILD/hb" > /dev/null

cargo build -q --release -p recomp-cli --manifest-path "$ROOT/Cargo.toml"
RECOMP="$ROOT/target/release/recomp"
"$RECOMP" homebrew-intake --module "$BUILD/hb/inputs/homebrew.nro" \
  --provenance "$BUILD/hb/provenance.toml" --out-dir "$BUILD/intake" > /dev/null
"$RECOMP" homebrew-lift --module-json "$BUILD/intake/module.json" --out-dir "$BUILD/lift" > /dev/null

MODULE="$BUILD/lift/module.json"
cat > "$BUILD/lift/provenance.toml" <<TOML
schema_version = "1"

[title]
name = "Lifter differential test"
title_id = "0100000000000001"
version = "1.0.0"
region = "US"

[collection]
device = "synthetic"
collected_at = "2026-10-28"
notes = "Synthetic instruction sequence from tools/difftest/seq.s."

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
  --provenance "$BUILD/lift/provenance.toml" --out-dir "$BUILD/out" \
  --runtime-path "$ROOT/crates/recomp-runtime" > /dev/null
printf '\n[workspace]\n' >> "$BUILD/out/Cargo.toml"

# Print the result registers just before the entry function returns.
python3 -I - "$BUILD/out/src/main.rs" $REGS <<'PY'
import sys
path, regs = sys.argv[1], sys.argv[2:]
src = open(path).read()
prints = "".join(f'println!("x{r}={{:016x}}", x{r} as u64); ' for r in regs)
at = src.index("return Ok(());", src.index("fn entry("))
open(path, "w").write(src[:at] + prints + src[at:])
PY
cargo run -q --manifest-path "$BUILD/out/Cargo.toml" 2> /dev/null | grep '^x' > "$BUILD/recomp.txt"

if diff "$BUILD/native.txt" "$BUILD/recomp.txt"; then
  echo "MATCH: $(wc -l < "$BUILD/native.txt" | tr -d ' ') registers identical"
else
  echo "MISMATCH (left: native, right: recompiled)" >&2
  exit 1
fi
