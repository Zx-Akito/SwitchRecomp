// Usage: coverage <text.bin> <unsupported.out>
// Writes each unsupported instruction word (u32 LE) with its count (u32 LE).
use recomp_pipeline::homebrew::lift::probe_word;
use std::collections::HashMap;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let text = std::fs::read(&args[1]).expect("read text");
    let mut unsupported: HashMap<u32, u32> = HashMap::new();
    let total = text.len() / 4;
    for (i, chunk) in text.chunks_exact(4).enumerate() {
        let word = u32::from_le_bytes(chunk.try_into().unwrap());
        if !probe_word(word, (i * 4) as u64) {
            *unsupported.entry(word).or_default() += 1;
        }
    }
    let mut out = Vec::new();
    for (word, count) in &unsupported {
        out.extend_from_slice(&word.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
    }
    std::fs::write(&args[2], out).expect("write");
    let bad: u64 = unsupported.values().map(|&c| c as u64).sum();
    println!(
        "total={total} supported={} unsupported={bad}",
        total as u64 - bad
    );
}
