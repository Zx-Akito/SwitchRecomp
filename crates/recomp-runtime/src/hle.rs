//! High-level emulation of imported SDK functions, keyed by mangled symbol name.
//!
//! Recompiled code calls imports through placeholder addresses; the generated dispatch
//! table routes those here. Arguments and results follow AAPCS64 in `ctx.x`.

use crate::{Cpu, RuntimeError, RuntimeResult};
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

/// With `RECOMP_HLE_LENIENT=1`, unknown imports are logged and return 0 instead of
/// stopping execution. Results are not trustworthy then; it only maps which imports a
/// title reaches.
pub fn call(ctx: &mut Cpu, symbol: &str) -> RuntimeResult<()> {
    log_first_call(symbol);
    match symbol {
        // void nn::oe::SetCpuBoostMode(nn::oe::CpuBoostMode): host clocks are not modelled.
        "_ZN2nn2oe15SetCpuBoostModeENS0_12CpuBoostModeE" => Ok(()),
        // void nn::util::ReferSymbol(const void*): only keeps a symbol linked.
        "_ZN2nn4util11ReferSymbolEPKv" => Ok(()),
        _ if lenient() => {
            eprintln!("[hle] UNIMPLEMENTED (lenient, returning 0): {symbol}");
            ctx.x[0] = 0;
            Ok(())
        }
        _ => Err(RuntimeError::UnimplementedImport {
            symbol: symbol.to_string(),
        }),
    }
}

fn lenient() -> bool {
    static LENIENT: OnceLock<bool> = OnceLock::new();
    *LENIENT.get_or_init(|| std::env::var_os("RECOMP_HLE_LENIENT").is_some_and(|v| v == "1"))
}

fn log_first_call(symbol: &str) {
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let mut seen = SEEN.get_or_init(Default::default).lock().unwrap();
    if seen.insert(symbol.to_string()) {
        eprintln!("[hle] {symbol}");
    }
}
