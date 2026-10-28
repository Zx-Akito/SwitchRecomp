# Tools

## `switch/`: retail module coverage analysis

Measures how much of a real Switch executable the lifter can decode. Works only on dumps
you made from your own console; never commit extracted code or keys.

Setup: `python3 -m venv .venv && .venv/bin/pip install -r tools/switch/requirements.txt`

1. Unpack the NSP with [hactool](https://github.com/SciresM/hactool):
   `hactool -k prod.keys -t pfs0 --outdir nsp game.nsp`
   (hactool rejects key files containing 34-digit `mariko_*` entries; filter those lines out.)
2. Extract the ExeFS of the program NCA. This also works for update NCAs, whose BKTR
   RomFS hactool cannot process without the base game:
   `.venv/bin/python -I tools/switch/nca_exefs.py prod.keys nsp/<id>.tik nsp/<program>.nca exefs`
3. Decompress an NSO: `.venv/bin/python -I tools/switch/nso_text.py exefs/main nso/main`
4. Measure and report:

   ```sh
   cargo build --release -p recomp-pipeline --example coverage
   ./target/release/examples/coverage nso/main/text.bin nso/main/unsupported.out
   .venv/bin/python -I tools/switch/report.py nso/main
   ```

## `difftest/`: lifter differential test

`tools/difftest/run.sh` runs `seq.s` on the host CPU and through intake, lift and Rust
codegen, then compares the result registers. Requires macOS on Apple Silicon. Add
instructions to `seq.s` to cover new decoder paths.
