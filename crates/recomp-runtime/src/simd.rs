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

fn map1(a: u128, esize: u32, q: bool, f: impl Fn(u64) -> u64) -> u128 {
    map2(a, 0, esize, q, |x, _| f(x))
}

fn lanes(a: u128, esize: u32, q: bool) -> impl Iterator<Item = u64> {
    let mask = lane_mask(esize);
    (0..(if q { 128 } else { 64 }) / esize).map(move |lane| (a >> (lane * esize)) as u64 & mask)
}

// Two-register misc forms use only `a`.
pub fn cnt(a: u128, _b: u128, _d: u128, esize: u32, q: bool) -> u128 {
    map1(a, esize, q, |x| x.count_ones() as u64)
}
pub fn not(a: u128, _b: u128, _d: u128, _esize: u32, q: bool) -> u128 {
    clip(!a, q)
}
pub fn neg(a: u128, _b: u128, _d: u128, esize: u32, q: bool) -> u128 {
    map1(a, esize, q, |x| x.wrapping_neg())
}
pub fn abs(a: u128, _b: u128, _d: u128, esize: u32, q: bool) -> u128 {
    map1(a, esize, q, |x| sign(x, esize).unsigned_abs())
}

// Across-lanes reductions write a scalar to the low element and zero the rest.
pub fn addv(a: u128, _b: u128, _d: u128, esize: u32, q: bool) -> u128 {
    (lanes(a, esize, q).fold(0_u64, u64::wrapping_add) & lane_mask(esize)) as u128
}
pub fn uaddlv(a: u128, _b: u128, _d: u128, esize: u32, q: bool) -> u128 {
    (lanes(a, esize, q).sum::<u64>() & lane_mask(2 * esize)) as u128
}
pub fn saddlv(a: u128, _b: u128, _d: u128, esize: u32, q: bool) -> u128 {
    let sum: i64 = lanes(a, esize, q).map(|x| sign(x, esize)).sum();
    (sum as u64 & lane_mask(2 * esize)) as u128
}
pub fn umaxv(a: u128, _b: u128, _d: u128, esize: u32, q: bool) -> u128 {
    lanes(a, esize, q).max().unwrap_or(0) as u128
}
pub fn uminv(a: u128, _b: u128, _d: u128, esize: u32, q: bool) -> u128 {
    lanes(a, esize, q).min().unwrap_or(0) as u128
}
pub fn smaxv(a: u128, _b: u128, _d: u128, esize: u32, q: bool) -> u128 {
    let max = lanes(a, esize, q)
        .map(|x| sign(x, esize))
        .max()
        .unwrap_or(0);
    (max as u64 & lane_mask(esize)) as u128
}
pub fn sminv(a: u128, _b: u128, _d: u128, esize: u32, q: bool) -> u128 {
    let min = lanes(a, esize, q)
        .map(|x| sign(x, esize))
        .min()
        .unwrap_or(0);
    (min as u64 & lane_mask(esize)) as u128
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
        "cnt" => cnt,
        "not" => not,
        "neg" => neg,
        "abs" => abs,
        "addv" => addv,
        "uaddlv" => uaddlv,
        "saddlv" => saddlv,
        "umaxv" => umaxv,
        "uminv" => uminv,
        "smaxv" => smaxv,
        "sminv" => sminv,
        _ => return None,
    })
}
