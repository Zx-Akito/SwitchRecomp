//! High-level emulation of imported SDK functions, keyed by mangled symbol name.
//!
//! Recompiled code calls imports through placeholder addresses; the generated dispatch
//! table routes those here. Arguments and results follow AAPCS64 in `ctx.x`.

use crate::{
    mem_load_u64, mem_load_u8, mem_read_bytes, mem_store_u64, mem_write_bytes, Cpu, RuntimeError,
    RuntimeResult,
};
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

/// Calls guest code at `address` through the registered dispatch table.
pub fn call_guest(ctx: &mut Cpu, address: u64) -> RuntimeResult<()> {
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
        // ponytail: guest code runs on one host thread, so mutexes never contend: lock
        // and try-lock always succeed. Needs real host locks once guest threads run.
        // nn::os::{Initialize,Finalize,Lock,Unlock}Mutex(MutexType*) and the C nnos* forms.
        "_ZN2nn2os15InitializeMutexEPNS0_9MutexTypeEbi"
        | "_ZN2nn2os13FinalizeMutexEPNS0_9MutexTypeE"
        | "_ZN2nn2os9LockMutexEPNS0_9MutexTypeE"
        | "_ZN2nn2os11UnlockMutexEPNS0_9MutexTypeE"
        | "nnosInitializeMutex"
        | "nnosFinalizeMutex"
        | "nnosLockMutex"
        | "nnosUnlockMutex" => Ok(()),
        // bool nn::os::TryLockMutex(MutexType*)
        "_ZN2nn2os12TryLockMutexEPNS0_9MutexTypeE" => {
            ctx.x[0] = 1;
            Ok(())
        }
        // ponytail: semaphores never block on one host thread; acquiring an empty one
        // returns immediately. Needs real counting once guest threads run.
        "_ZN2nn2os19InitializeSemaphoreEPNS0_13SemaphoreTypeEii"
        | "_ZN2nn2os17FinalizeSemaphoreEPNS0_13SemaphoreTypeE"
        | "_ZN2nn2os16ReleaseSemaphoreEPNS0_13SemaphoreTypeEi"
        | "_ZN2nn2os16ReleaseSemaphoreEPNS0_13SemaphoreTypeE"
        | "_ZN2nn2os16AcquireSemaphoreEPNS0_13SemaphoreTypeE" => Ok(()),
        "_ZN2nn2os19TryAcquireSemaphoreEPNS0_13SemaphoreTypeE"
        | "_ZN2nn2os21TimedAcquireSemaphoreEPNS0_13SemaphoreTypeENS_8TimeSpanE" => {
            ctx.x[0] = 1;
            Ok(())
        }
        // C library (musl in the SDK). Pointers and sizes follow AAPCS64 in x0-x2.
        "memset" => {
            let (dst, value, len) = (ctx.x[0] as u64, ctx.x[1] as u8, ctx.x[2] as usize);
            mem_write_bytes(dst, &vec![value; len])
        }
        "memcpy" | "memmove" => {
            let (dst, src, len) = (ctx.x[0] as u64, ctx.x[1] as u64, ctx.x[2] as usize);
            let bytes = mem_read_bytes(src, len)?;
            mem_write_bytes(dst, &bytes)
        }
        "memcmp" => {
            let len = ctx.x[2] as usize;
            let a = mem_read_bytes(ctx.x[0] as u64, len)?;
            let b = mem_read_bytes(ctx.x[1] as u64, len)?;
            ctx.x[0] = byte_difference(&a, &b);
            Ok(())
        }
        "memchr" => {
            let (src, value, len) = (ctx.x[0] as u64, ctx.x[1] as u8, ctx.x[2] as usize);
            let bytes = mem_read_bytes(src, len)?;
            ctx.x[0] = bytes
                .iter()
                .position(|b| *b == value)
                .map_or(0, |i| (src + i as u64) as i64);
            Ok(())
        }
        "strlen" => {
            ctx.x[0] = read_cstr(ctx.x[0] as u64, usize::MAX)?.len() as i64;
            Ok(())
        }
        "strnlen" => {
            ctx.x[0] = read_cstr(ctx.x[0] as u64, ctx.x[1] as usize)?.len() as i64;
            Ok(())
        }
        "strcmp" | "strncmp" => {
            let limit = if symbol == "strncmp" {
                ctx.x[2] as usize
            } else {
                usize::MAX
            };
            let mut a = read_cstr(ctx.x[0] as u64, limit)?;
            let mut b = read_cstr(ctx.x[1] as u64, limit)?;
            // Compare including the terminator, within the limit.
            a.push(0);
            b.push(0);
            let n = a.len().min(b.len()).min(limit);
            ctx.x[0] = byte_difference(&a[..n], &b[..n]);
            Ok(())
        }
        "strcpy" => {
            let mut bytes = read_cstr(ctx.x[1] as u64, usize::MAX)?;
            bytes.push(0);
            mem_write_bytes(ctx.x[0] as u64, &bytes)
        }
        "strncpy" => {
            let len = ctx.x[2] as usize;
            let mut bytes = read_cstr(ctx.x[1] as u64, len)?;
            bytes.resize(len, 0);
            mem_write_bytes(ctx.x[0] as u64, &bytes)
        }
        "strchr" | "strrchr" => {
            let (src, value) = (ctx.x[0] as u64, ctx.x[1] as u8);
            let mut bytes = read_cstr(src, usize::MAX)?;
            bytes.push(0);
            let found = if symbol == "strchr" {
                bytes.iter().position(|b| *b == value)
            } else {
                bytes.iter().rposition(|b| *b == value)
            };
            ctx.x[0] = found.map_or(0, |i| (src + i as u64) as i64);
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

/// Bytes of the NUL-terminated string at `address`, at most `limit` of them.
fn read_cstr(address: u64, limit: usize) -> RuntimeResult<Vec<u8>> {
    let mut bytes = Vec::new();
    while bytes.len() < limit {
        let byte = mem_load_u8(address + bytes.len() as u64)? as u8;
        if byte == 0 {
            break;
        }
        bytes.push(byte);
    }
    Ok(bytes)
}

/// C comparison result: difference of the first differing bytes as unsigned chars.
fn byte_difference(a: &[u8], b: &[u8]) -> i64 {
    a.iter()
        .zip(b)
        .find(|(x, y)| x != y)
        .map_or(0, |(x, y)| *x as i64 - *y as i64)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{init_memory, MemoryLayout, MemoryPermissions, MemoryRegionSpec};

    #[test]
    fn libc_memory_and_string_functions() {
        // Guest memory is process-global: use the memory tests' layout and stay clear of
        // the addresses they touch.
        let _ = init_memory(MemoryLayout::new(vec![
            MemoryRegionSpec::new("data", 0x1000, 0x40, MemoryPermissions::read_write()),
            MemoryRegionSpec::new("ro", 0x2000, 0x40, MemoryPermissions::read_only()),
        ]));
        let mut ctx = Cpu::default();
        let mut libc = |name: &str, args: &[i64]| {
            ctx.x[..args.len()].copy_from_slice(args);
            call(&mut ctx, name).unwrap();
            ctx.x[0]
        };
        libc("memset", &[0x1020, b'a' as i64, 4]);
        libc("memset", &[0x1024, 0, 1]);
        assert_eq!(libc("strlen", &[0x1020]), 4);
        libc("strcpy", &[0x1028, 0x1020]);
        assert_eq!(libc("strcmp", &[0x1020, 0x1028]), 0);
        libc("memset", &[0x102B, b'b' as i64, 1]);
        assert!(libc("strcmp", &[0x1020, 0x1028]) < 0);
        assert_eq!(libc("strncmp", &[0x1020, 0x1028, 3]), 0);
        assert_eq!(libc("strchr", &[0x1028, b'b' as i64]), 0x102B);
        assert_eq!(libc("memchr", &[0x1020, b'z' as i64, 4]), 0);
        libc("memcpy", &[0x1030, 0x1028, 5]);
        assert_eq!(libc("memcmp", &[0x1030, 0x1028, 5]), 0);
    }
}
