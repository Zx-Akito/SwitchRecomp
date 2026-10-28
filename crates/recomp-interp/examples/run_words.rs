// Interprets raw AArch64 words with no recompiled code and prints guest registers.
// Usage: run_words <words.txt> "<x regs>" "<v regs>"   (used by tools/difftest/run.sh)
use recomp_runtime::{
    self as rt, Cpu, MemoryInitSegment, MemoryLayout, MemoryPermissions, MemoryRegionSpec,
};

fn main() -> Result<(), rt::RuntimeError> {
    let args: Vec<String> = std::env::args().collect();
    let words: Vec<u32> = std::fs::read_to_string(&args[1])
        .expect("read words")
        .split_whitespace()
        .map(|w| u32::from_str_radix(w, 16).expect("hex word"))
        .collect();
    let text: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let size = (text.len() as u64).next_multiple_of(0x1000);
    rt::init_memory(MemoryLayout::new(vec![
        MemoryRegionSpec::new("text", 0, size, MemoryPermissions::new(true, false, true)),
        MemoryRegionSpec::new(
            "stack",
            0x3000_0000,
            0x4_0000,
            MemoryPermissions::new(true, true, false),
        ),
    ]))?;
    rt::apply_memory_image(
        &[MemoryInitSegment::new("text", 0, text.len() as u64, text)],
        &[],
    )?;
    rt::hle::set_guest_dispatch(recomp_interp::run);

    let mut ctx = Cpu::default();
    ctx.x[31] = 0x3004_0000;
    recomp_interp::run(&mut ctx, 0)?;
    for r in args[2].split_whitespace() {
        let r: usize = r.parse().unwrap();
        println!("x{r}={:016x}", ctx.x[r] as u64);
    }
    for r in args[3].split_whitespace() {
        let r: usize = r.parse().unwrap();
        println!("v{r}={:032x}", ctx.simd[r]);
    }
    Ok(())
}
