//! Advanced SIMD lane operations shared by generated code and the interpreter.
//!
//! Every operation takes the two source vectors, the previous destination (for
//! bitwise-select forms), the element size in bits and whether the full 128-bit register is
//! used (`q`); with `q == false` the upper 64 bits of the result are zero.

pub type VecFn = fn(u128, u128, u128, u32, bool) -> u128;

fn lane_mask(esize: u32) -> u64 {
    u64::MAX >> (64 - esize)
}

fn sign(value: u64, esize: u32) -> i64 {
    ((value << (64 - esize)) as i64) >> (64 - esize)
}

fn clip(value: u128, q: bool) -> u128 {
    if q {
        value
    } else {
        value & u64::MAX as u128
    }
}

fn map2(a: u128, b: u128, esize: u32, q: bool, f: impl Fn(u64, u64) -> u64) -> u128 {
    let mask = lane_mask(esize);
    let mut out = 0_u128;
    for lane in 0..128 / esize {
        let shift = lane * esize;
        let x = (a >> shift) as u64 & mask;
        let y = (b >> shift) as u64 & mask;
        out |= ((f(x, y) & mask) as u128) << shift;
    }
    clip(out, q)
}

fn all_ones(condition: bool) -> u64 {
    if condition {
        u64::MAX
    } else {
        0
    }
}

pub fn add(a: u128, b: u128, _d: u128, esize: u32, q: bool) -> u128 {
    map2(a, b, esize, q, u64::wrapping_add)
}
pub fn sub(a: u128, b: u128, _d: u128, esize: u32, q: bool) -> u128 {
    map2(a, b, esize, q, u64::wrapping_sub)
}
pub fn mul(a: u128, b: u128, _d: u128, esize: u32, q: bool) -> u128 {
    map2(a, b, esize, q, u64::wrapping_mul)
}
pub fn and(a: u128, b: u128, _d: u128, _esize: u32, q: bool) -> u128 {
    clip(a & b, q)
}
pub fn bic(a: u128, b: u128, _d: u128, _esize: u32, q: bool) -> u128 {
    clip(a & !b, q)
}
pub fn orr(a: u128, b: u128, _d: u128, _esize: u32, q: bool) -> u128 {
    clip(a | b, q)
}
pub fn orn(a: u128, b: u128, _d: u128, _esize: u32, q: bool) -> u128 {
    clip(a | !b, q)
}
pub fn eor(a: u128, b: u128, _d: u128, _esize: u32, q: bool) -> u128 {
    clip(a ^ b, q)
}
/// BSL: bits from `a` where the destination is set, else from `b`.
pub fn bsl(a: u128, b: u128, d: u128, _esize: u32, q: bool) -> u128 {
    clip((d & a) | (!d & b), q)
}
/// BIT: insert bits of `a` into the destination where `b` is set.
pub fn bit(a: u128, b: u128, d: u128, _esize: u32, q: bool) -> u128 {
    clip((d & !b) | (a & b), q)
}
/// BIF: insert bits of `a` into the destination where `b` is clear.
pub fn bif(a: u128, b: u128, d: u128, _esize: u32, q: bool) -> u128 {
    clip((d & b) | (a & !b), q)
}
pub fn cmeq(a: u128, b: u128, _d: u128, esize: u32, q: bool) -> u128 {
    map2(a, b, esize, q, |x, y| all_ones(x == y))
}
pub fn cmtst(a: u128, b: u128, _d: u128, esize: u32, q: bool) -> u128 {
    map2(a, b, esize, q, |x, y| all_ones(x & y != 0))
}
pub fn cmgt(a: u128, b: u128, _d: u128, esize: u32, q: bool) -> u128 {
    map2(a, b, esize, q, |x, y| {
        all_ones(sign(x, esize) > sign(y, esize))
    })
}
pub fn cmge(a: u128, b: u128, _d: u128, esize: u32, q: bool) -> u128 {
    map2(a, b, esize, q, |x, y| {
        all_ones(sign(x, esize) >= sign(y, esize))
    })
}
pub fn cmhi(a: u128, b: u128, _d: u128, esize: u32, q: bool) -> u128 {
    map2(a, b, esize, q, |x, y| all_ones(x > y))
}
pub fn cmhs(a: u128, b: u128, _d: u128, esize: u32, q: bool) -> u128 {
    map2(a, b, esize, q, |x, y| all_ones(x >= y))
}
pub fn smax(a: u128, b: u128, _d: u128, esize: u32, q: bool) -> u128 {
    map2(a, b, esize, q, |x, y| {
        if sign(x, esize) >= sign(y, esize) {
            x
        } else {
            y
        }
    })
}
pub fn smin(a: u128, b: u128, _d: u128, esize: u32, q: bool) -> u128 {
    map2(a, b, esize, q, |x, y| {
        if sign(x, esize) <= sign(y, esize) {
            x
        } else {
            y
        }
    })
}
pub fn umax(a: u128, b: u128, _d: u128, esize: u32, q: bool) -> u128 {
    map2(a, b, esize, q, u64::max)
}
pub fn umin(a: u128, b: u128, _d: u128, esize: u32, q: bool) -> u128 {
    map2(a, b, esize, q, u64::min)
}

/// Looks up an operation by the name the lifter records in `vec_op`.
pub fn by_name(name: &str) -> Option<VecFn> {
    Some(match name {
        "add" => add,
        "sub" => sub,
        "mul" => mul,
        "and" => and,
        "bic" => bic,
        "orr" => orr,
        "orn" => orn,
        "eor" => eor,
        "bsl" => bsl,
        "bit" => bit,
        "bif" => bif,
        "cmeq" => cmeq,
        "cmtst" => cmtst,
        "cmgt" => cmgt,
        "cmge" => cmge,
        "cmhi" => cmhi,
        "cmhs" => cmhs,
        "smax" => smax,
        "smin" => smin,
        "umax" => umax,
        "umin" => umin,
        _ => return None,
    })
}
