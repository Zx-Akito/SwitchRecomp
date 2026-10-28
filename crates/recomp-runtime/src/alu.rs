//! Integer operations shared by generated code and the interpreter, for instructions whose
//! AArch64 semantics differ from plain Rust operators (division by zero, 32-bit forms,
//! high multiplies, bit insertion). Every operation takes (`a`, `b`, previous destination).

pub type IntFn = fn(i64, i64, i64) -> i64;

fn low32(value: i64) -> i64 {
    value as u32 as i64
}

pub fn udiv(a: i64, b: i64, _d: i64) -> i64 {
    (a as u64).checked_div(b as u64).unwrap_or(0) as i64
}
pub fn sdiv(a: i64, b: i64, _d: i64) -> i64 {
    if b == 0 {
        0
    } else {
        a.wrapping_div(b)
    }
}
pub fn udiv32(a: i64, b: i64, _d: i64) -> i64 {
    (a as u32).checked_div(b as u32).unwrap_or(0) as i64
}
pub fn sdiv32(a: i64, b: i64, _d: i64) -> i64 {
    let (a, b) = (a as i32, b as i32);
    low32(if b == 0 { 0 } else { a.wrapping_div(b) } as i64)
}
pub fn umulh(a: i64, b: i64, _d: i64) -> i64 {
    ((a as u64 as u128 * b as u64 as u128) >> 64) as i64
}
pub fn smulh(a: i64, b: i64, _d: i64) -> i64 {
    ((a as i128 * b as i128) >> 64) as i64
}
pub fn lslv(a: i64, b: i64, _d: i64) -> i64 {
    ((a as u64) << (b as u64 & 63)) as i64
}
pub fn lsrv(a: i64, b: i64, _d: i64) -> i64 {
    ((a as u64) >> (b as u64 & 63)) as i64
}
pub fn asrv(a: i64, b: i64, _d: i64) -> i64 {
    a >> (b as u64 & 63)
}
pub fn rorv(a: i64, b: i64, _d: i64) -> i64 {
    (a as u64).rotate_right((b as u64 & 63) as u32) as i64
}
pub fn lslv32(a: i64, b: i64, _d: i64) -> i64 {
    low32(((a as u32) << (b as u32 & 31)) as i64)
}
pub fn lsrv32(a: i64, b: i64, _d: i64) -> i64 {
    low32(((a as u32) >> (b as u32 & 31)) as i64)
}
pub fn asrv32(a: i64, b: i64, _d: i64) -> i64 {
    low32(((a as i32) >> (b as u32 & 31)) as i64)
}
pub fn rorv32(a: i64, b: i64, _d: i64) -> i64 {
    low32((a as u32).rotate_right(b as u32 & 31) as i64)
}
pub fn clz(a: i64, _b: i64, _d: i64) -> i64 {
    a.leading_zeros() as i64
}
pub fn clz32(a: i64, _b: i64, _d: i64) -> i64 {
    (a as u32).leading_zeros() as i64
}
pub fn cls(a: i64, _b: i64, _d: i64) -> i64 {
    (a ^ (a >> 1)).leading_zeros() as i64 - 1
}
pub fn cls32(a: i64, _b: i64, _d: i64) -> i64 {
    let a = a as i32;
    (a ^ (a >> 1)).leading_zeros() as i64 - 1
}
pub fn rbit(a: i64, _b: i64, _d: i64) -> i64 {
    a.reverse_bits()
}
pub fn rbit32(a: i64, _b: i64, _d: i64) -> i64 {
    low32((a as u32).reverse_bits() as i64)
}
/// REV (64-bit): reverse all bytes.
pub fn rev(a: i64, _b: i64, _d: i64) -> i64 {
    a.swap_bytes()
}
/// REV (32-bit), and REV32 on each word of a 64-bit register when `b` is 64.
pub fn rev32(a: i64, b: i64, _d: i64) -> i64 {
    let lo = (a as u32).swap_bytes() as u64;
    if b == 64 {
        ((((a as u64) >> 32) as u32).swap_bytes() as u64) << 32 | lo
    } else {
        lo
    }
    .cast_signed()
}
/// REV16: reverse bytes in each halfword; `b` is the register width.
pub fn rev16(a: i64, b: i64, _d: i64) -> i64 {
    let x = a as u64;
    let swapped = ((x & 0x00FF_00FF_00FF_00FF) << 8) | ((x >> 8) & 0x00FF_00FF_00FF_00FF);
    if b == 64 {
        swapped as i64
    } else {
        low32(swapped as i64)
    }
}
/// BFM: `b` packs (width << 16) | (immr << 8) | imms; inserts the rotated source into the
/// previous destination as AArch64 DecodeBitMasks(immediate = false) describes.
pub fn bfm(a: i64, b: i64, d: i64) -> i64 {
    let (width, immr, imms) = (
        (b >> 16) as u32,
        ((b >> 8) & 0x3F) as u32,
        (b & 0x3F) as u32,
    );
    let ones = |n: u32| if n >= 64 { u64::MAX } else { (1_u64 << n) - 1 };
    let full = ones(width);
    let ror = |x: u64, r: u32| {
        let x = x & full;
        if r == 0 {
            x
        } else {
            ((x >> r) | (x << (width - r))) & full
        }
    };
    let wmask = ror(ones(imms + 1), immr);
    let tmask = ones((imms.wrapping_sub(immr) & (width - 1)) + 1);
    let dst = d as u64 & full;
    let bot = (dst & !wmask) | (ror(a as u64, immr) & wmask);
    ((dst & !tmask) | (bot & tmask)) as i64
}

/// MRS TPIDR_EL0: the main thread's TLS block.
pub fn tpidr(_a: i64, _b: i64, _d: i64) -> i64 {
    crate::hle::MAIN_THREAD_TLS as i64
}
/// MRS CNTVCT_EL0: the virtual counter, ticking at CNTFRQ_EL0.
pub fn cntvct(_a: i64, _b: i64, _d: i64) -> i64 {
    crate::hle::system_tick()
}

/// Looks up an operation by the name the lifter records in `int_op`.
pub fn by_name(name: &str) -> Option<IntFn> {
    Some(match name {
        "udiv" => udiv,
        "sdiv" => sdiv,
        "udiv32" => udiv32,
        "sdiv32" => sdiv32,
        "umulh" => umulh,
        "smulh" => smulh,
        "lslv" => lslv,
        "lsrv" => lsrv,
        "asrv" => asrv,
        "rorv" => rorv,
        "lslv32" => lslv32,
        "lsrv32" => lsrv32,
        "asrv32" => asrv32,
        "rorv32" => rorv32,
        "clz" => clz,
        "clz32" => clz32,
        "cls" => cls,
        "cls32" => cls32,
        "rbit" => rbit,
        "rbit32" => rbit32,
        "rev" => rev,
        "rev32" => rev32,
        "rev16" => rev16,
        "bfm" => bfm,
        "tpidr" => tpidr,
        "cntvct" => cntvct,
        _ => return None,
    })
}
