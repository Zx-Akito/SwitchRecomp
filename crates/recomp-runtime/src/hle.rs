//! High-level emulation of imported SDK functions, keyed by mangled symbol name.
//!
//! Recompiled code calls imports through placeholder addresses; the generated dispatch
//! table routes those here. Arguments and results follow AAPCS64 in `ctx.x`.

use crate::{mem_load_u64, mem_store_u64, Cpu, RuntimeError, RuntimeResult};
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Guest memory reserved for objects the HLE layer hands out (the title's memory layout
/// must map a writable "hle" region here).
pub const HLE_SCRATCH_BASE: u64 = 0x3_0000_0000;
/// nn::os::ThreadType returned by GetCurrentThread for the main thread.
const MAIN_THREAD: u64 = HLE_SCRATCH_BASE;
/// Switch system tick frequency (19.2 MHz).
const TICK_HZ: u128 = 19_200_000;
const RESULT_SUCCESS: i64 = 0;

/// Entry into recompiled code by guest address (the generated `dispatch`).
pub type GuestCall = fn(&mut Cpu, u64) -> RuntimeResult<()>;

static GUEST_DISPATCH: OnceLock<GuestCall> = OnceLock::new();

/// Registers the generated dispatch table so emulated functions can call back into the
/// guest (callbacks, initializers, thread entry points).
pub fn set_guest_dispatch(dispatch: GuestCall) {
    let _ = GUEST_DISPATCH.set(dispatch);
}

fn call_guest(ctx: &mut Cpu, address: u64) -> RuntimeResult<()> {
    let dispatch = GUEST_DISPATCH
        .get()
        .ok_or(RuntimeError::UnknownCallTarget { address })?;
    dispatch(ctx, address)
}

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
        // u64 nn::os::GetThreadAvailableCoreMask(): applications may use cores 0-2.
        "_ZN2nn2os26GetThreadAvailableCoreMaskEv" => {
            ctx.x[0] = 0b111;
            Ok(())
        }
        // nn::os::ThreadType* nn::os::GetCurrentThread()
        "_ZN2nn2os16GetCurrentThreadEv" => {
            ctx.x[0] = MAIN_THREAD as i64;
            Ok(())
        }
        // ponytail: scheduling hints are ignored until guest threads exist.
        // void nn::os::SetThreadCoreMask(ThreadType*, int, u64)
        "_ZN2nn2os17SetThreadCoreMaskEPNS0_10ThreadTypeEim" => Ok(()),
        // int nn::os::ChangeThreadPriority(ThreadType*, int): returns the previous priority.
        "_ZN2nn2os20ChangeThreadPriorityEPNS0_10ThreadTypeEi" => {
            ctx.x[0] = 0;
            Ok(())
        }
        // nn::Tick nn::os::GetSystemTick() / GetSystemTickOrdered()
        "_ZN2nn2os13GetSystemTickEv" | "_ZN2nn2os20GetSystemTickOrderedEv" => {
            ctx.x[0] = system_tick();
            Ok(())
        }
        // nn::TimeSpan nn::os::ConvertToTimeSpan(nn::os::Tick): nanoseconds.
        "_ZN2nn2os17ConvertToTimeSpanENS0_4TickE" => {
            ctx.x[0] = (ctx.x[0] as i128 * 1_000_000_000 / TICK_HZ as i128) as i64;
            Ok(())
        }
        // nn::Result nn::time::Initialize()
        "_ZN2nn4time10InitializeEv" => {
            ctx.x[0] = RESULT_SUCCESS;
            Ok(())
        }
        // nn::Result StandardUserSystemClock::GetSystemClockContext(SystemClockContext*):
        // a zeroed 0x20-byte context (offset 0, unnamed steady clock).
        "_ZN2nn4time23StandardUserSystemClock21GetSystemClockContextEPNS0_18SystemClockContextE" => {
            let out = ctx.x[0] as u64;
            for offset in (0..0x20).step_by(8) {
                mem_store_u64(out + offset, 0)?;
            }
            ctx.x[0] = RESULT_SUCCESS;
            Ok(())
        }
        // nn::Result StandardUserSystemClock::GetCurrentTime(PosixTime*): host wall clock.
        "_ZN2nn4time23StandardUserSystemClock14GetCurrentTimeEPNS0_9PosixTimeE" => {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            mem_store_u64(ctx.x[0] as u64, now)?;
            ctx.x[0] = RESULT_SUCCESS;
            Ok(())
        }
        // void std::__1::__call_once(volatile unsigned long& flag, void* arg, void (*func)(void*))
        // libc++ flag states: 0 not run, 1 running, ~0 done.
        "_ZNSt3__111__call_onceERVmPvPFvS2_E" => {
            let (flag, arg, func) = (ctx.x[0] as u64, ctx.x[1], ctx.x[2] as u64);
            if mem_load_u64(flag)? != u64::MAX {
                mem_store_u64(flag, 1)?;
                ctx.x[0] = arg;
                call_guest(ctx, func)?;
                mem_store_u64(flag, u64::MAX)?;
            }
            Ok(())
        }
        // Allocator registration and notification switches have no host-side effect yet.
        "_ZN2nn2fs12SetAllocatorEPFPvmEPFvS1_mE"
        | "_ZN5movie12SetAllocatorEPFPvmmS0_EPFvS0_S0_EPFS0_S0_mS0_ES0_"
        | "_ZN2nn2vi10InitializeEv"
        | "_ZN2nn2oe28SetResumeNotificationEnabledEb"
        | "_ZN2nn2oe44SetPerformanceModeChangedNotificationEnabledEb"
        | "_ZN2nn2oe42SetOperationModeChangedNotificationEnabledEb" => Ok(()),
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

fn system_tick() -> i64 {
    static START: OnceLock<Instant> = OnceLock::new();
    let elapsed = START.get_or_init(Instant::now).elapsed().as_nanos();
    (elapsed * TICK_HZ / 1_000_000_000) as i64
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
