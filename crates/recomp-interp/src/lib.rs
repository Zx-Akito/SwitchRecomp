//! Fallback for guest code that was not recompiled ahead of time.
//!
//! The generated dispatch table sends unknown addresses here. A function is decoded from the
//! mapped `text` region on first use (the same lifter, in partial mode), its IR is resolved
//! to register slots, and it is interpreted directly against the shared CPU context. Calls
//! out of interpreted code go back through the dispatch table, so recompiled code, HLE and
//! interpreted code can call each other freely. Semantics mirror the Rust codegen in
//! `recomp_pipeline::pipeline`.

use recomp_pipeline::homebrew::TextImage;
use recomp_pipeline::input::{Block, Op, Terminator};
use recomp_runtime::{self as rt, Cpu, RuntimeError, RuntimeResult};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// Runs the guest function at `address` by interpretation.
pub fn run(ctx: &mut Cpu, address: u64) -> RuntimeResult<()> {
    function(address)?.execute(ctx)
}

struct Cache {
    text: Option<TextImage>,
    functions: HashMap<u64, Arc<Function>>,
}

fn function(address: u64) -> RuntimeResult<Arc<Function>> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(|| {
            Mutex::new(Cache {
                text: None,
                functions: HashMap::new(),
            })
        })
        .lock()
        .unwrap();
    if let Some(function) = cache.functions.get(&address) {
        return Ok(function.clone());
    }
    if cache.text.is_none() {
        let (base, bytes) =
            rt::region_snapshot("text").ok_or(RuntimeError::UnknownCallTarget { address })?;
        cache.text = Some(TextImage::new(bytes, base));
    }
    let text = cache.text.as_ref().unwrap();
    if !text.contains(address) || address % 4 != 0 {
        return Err(RuntimeError::UnknownCallTarget { address });
    }
    eprintln!("[interp] {address:#x}");
    let blocks = text
        .lift_function(address)
        .map_err(|_| RuntimeError::UnknownCallTarget { address })?;
    let function = Arc::new(
        Function::compile(&blocks).map_err(|_| RuntimeError::UnknownCallTarget { address })?,
    );
    cache.functions.insert(address, function.clone());
    Ok(function)
}

#[derive(Clone, Copy)]
enum Reg {
    X(usize),
    V(usize),
    Temp(usize),
}

#[derive(Clone, Copy)]
enum Cond {
    Eq,
    Ne,
    Cs,
    Cc,
    Mi,
    Pl,
    Vs,
    Vc,
    Hi,
    Ls,
    Ge,
    Lt,
    Gt,
    Le,
    Al,
}

impl Cond {
    fn parse(name: &str) -> Result<Self, String> {
        Ok(match name {
            "eq" => Cond::Eq,
            "ne" => Cond::Ne,
            "cs" | "hs" => Cond::Cs,
            "cc" | "lo" => Cond::Cc,
            "mi" => Cond::Mi,
            "pl" => Cond::Pl,
            "vs" => Cond::Vs,
            "vc" => Cond::Vc,
            "hi" => Cond::Hi,
            "ls" => Cond::Ls,
            "ge" => Cond::Ge,
            "lt" => Cond::Lt,
            "gt" => Cond::Gt,
            "le" => Cond::Le,
            "al" => Cond::Al,
            _ => return Err(format!("unknown condition {name}")),
        })
    }

    fn holds(self, ctx: &Cpu) -> bool {
        match self {
            Cond::Eq => ctx.z,
            Cond::Ne => !ctx.z,
            Cond::Cs => ctx.c,
            Cond::Cc => !ctx.c,
            Cond::Mi => ctx.n,
            Cond::Pl => !ctx.n,
            Cond::Vs => ctx.v,
            Cond::Vc => !ctx.v,
            Cond::Hi => ctx.c && !ctx.z,
            Cond::Ls => !ctx.c || ctx.z,
            Cond::Ge => ctx.n == ctx.v,
            Cond::Lt => ctx.n != ctx.v,
            Cond::Gt => !ctx.z && ctx.n == ctx.v,
            Cond::Le => ctx.z || ctx.n != ctx.v,
            Cond::Al => true,
        }
    }
}

#[derive(Clone, Copy)]
enum BinOp {
    Add,
    Sub,
    And,
    Or,
    Xor,
    Mul,
    Lsl,
    Lsr,
    Asr,
}

enum Instr {
    Const(Reg, i64),
    Bin(BinOp, Reg, Reg, Reg),
    Mov(Reg, Reg),
    Compare {
        lhs: Reg,
        rhs: Reg,
        add: bool,
    },
    Test(Reg, Reg),
    Select {
        dst: Reg,
        cond: Cond,
        lhs: Reg,
        rhs: Reg,
    },
    CondCompare {
        cond: Cond,
        lhs: Reg,
        rhs: Reg,
        add: bool,
        nzcv: u8,
    },
    Load {
        dst: Reg,
        addr: Reg,
        offset: i64,
        size: u8,
    },
    Store {
        src: Reg,
        addr: Reg,
        offset: i64,
        size: u8,
    },
    LoadV {
        dst: Reg,
        addr: Reg,
        offset: i64,
        size: u8,
    },
    StoreV {
        src: Reg,
        addr: Reg,
        offset: i64,
        size: u8,
    },
    MovV {
        dst: Reg,
        src: Reg,
        bytes: u8,
    },
    ConstV(Reg, u128),
    Vec {
        f: rt::simd::VecFn,
        dst: Reg,
        lhs: Reg,
        rhs: Reg,
        esize: u32,
        q: bool,
    },
    VToX {
        dst: Reg,
        src: Reg,
        bits: u8,
        lane: u8,
    },
    XToV {
        dst: Reg,
        src: Reg,
        bits: u8,
        lane: u8,
        zero_rest: bool,
    },
}

enum Exit {
    Br(usize),
    BrCond(Cond, usize, usize),
    Call(u64, usize),
    CallIndirect(Reg, usize),
    TailIndirect(Reg),
    Trap(u64, u32),
    Ret,
}

struct CompiledBlock {
    instrs: Vec<Instr>,
    exit: Exit,
}

struct Function {
    blocks: Vec<CompiledBlock>,
    temps: usize,
}

struct Resolver {
    temps: HashMap<String, usize>,
}

impl Resolver {
    fn reg(&mut self, name: &str) -> Reg {
        let file = name.chars().next();
        if let (Some(file @ ('x' | 'v')), Ok(index)) = (file, name[1..].parse::<usize>()) {
            if index < 32 {
                return if file == 'x' {
                    Reg::X(index)
                } else {
                    Reg::V(index)
                };
            }
        }
        let next = self.temps.len();
        Reg::Temp(*self.temps.entry(name.to_string()).or_insert(next))
    }
}

fn label_address(label: &str, prefix: &str) -> Result<u64, String> {
    label
        .strip_prefix(prefix)
        .and_then(|hex| u64::from_str_radix(hex, 16).ok())
        .ok_or_else(|| format!("unexpected label {label}"))
}

impl Function {
    fn compile(blocks: &[Block]) -> Result<Self, String> {
        let index: HashMap<&str, usize> = blocks
            .iter()
            .enumerate()
            .map(|(i, block)| (block.label.as_str(), i))
            .collect();
        let target = |label: &str| {
            index
                .get(label)
                .copied()
                .ok_or_else(|| format!("unknown block {label}"))
        };
        let mut resolver = Resolver {
            temps: HashMap::new(),
        };
        let mut compiled = Vec::with_capacity(blocks.len());
        for block in blocks {
            let instrs = block
                .ops
                .iter()
                .map(|op| compile_op(op, &mut resolver))
                .collect::<Result<Vec<_>, _>>()?;
            let exit = match &block.terminator {
                Terminator::Br { target: label } => Exit::Br(target(label)?),
                Terminator::BrCond {
                    cond,
                    then,
                    else_target,
                } => Exit::BrCond(Cond::parse(cond)?, target(then)?, target(else_target)?),
                Terminator::Call { target: name, next } => {
                    Exit::Call(label_address(name, "fn_")?, target(next)?)
                }
                Terminator::CallIndirect { reg, next } => {
                    Exit::CallIndirect(resolver.reg(reg), target(next)?)
                }
                Terminator::BrIndirect { reg } => Exit::TailIndirect(resolver.reg(reg)),
                Terminator::Trap { pc, word } => Exit::Trap(*pc, *word),
                Terminator::Ret => Exit::Ret,
            };
            compiled.push(CompiledBlock { instrs, exit });
        }
        // The lifter emits the entry block first.
        Ok(Function {
            blocks: compiled,
            temps: resolver.temps.len(),
        })
    }

    fn execute(&self, ctx: &mut Cpu) -> RuntimeResult<()> {
        let mut temps = vec![0_u128; self.temps];
        let mut current = 0;
        loop {
            let block = &self.blocks[current];
            for instr in &block.instrs {
                step(instr, ctx, &mut temps)?;
            }
            current = match block.exit {
                Exit::Br(next) => next,
                Exit::BrCond(cond, then, otherwise) => {
                    if cond.holds(ctx) {
                        then
                    } else {
                        otherwise
                    }
                }
                Exit::Call(address, next) => {
                    rt::hle::call_guest(ctx, address)?;
                    next
                }
                Exit::CallIndirect(reg, next) => {
                    let address = get(ctx, &temps, reg) as u64;
                    rt::hle::call_guest(ctx, address)?;
                    next
                }
                Exit::TailIndirect(reg) => {
                    let address = get(ctx, &temps, reg) as u64;
                    return rt::hle::call_guest(ctx, address);
                }
                Exit::Trap(pc, word) => {
                    return Err(RuntimeError::UnsupportedInstruction { pc, word })
                }
                Exit::Ret => return Ok(()),
            };
        }
    }
}

fn compile_op(op: &Op, r: &mut Resolver) -> Result<Instr, String> {
    let bin = |kind, dst: &str, lhs: &str, rhs: &str, r: &mut Resolver| {
        Instr::Bin(kind, r.reg(dst), r.reg(lhs), r.reg(rhs))
    };
    Ok(match op {
        Op::ConstI64 { dst, imm } => Instr::Const(r.reg(dst), *imm),
        Op::AddI64 { dst, lhs, rhs } => bin(BinOp::Add, dst, lhs, rhs, r),
        Op::SubI64 { dst, lhs, rhs } => bin(BinOp::Sub, dst, lhs, rhs, r),
        Op::AndI64 { dst, lhs, rhs } => bin(BinOp::And, dst, lhs, rhs, r),
        Op::OrI64 { dst, lhs, rhs } => bin(BinOp::Or, dst, lhs, rhs, r),
        Op::XorI64 { dst, lhs, rhs } => bin(BinOp::Xor, dst, lhs, rhs, r),
        Op::MulI64 { dst, lhs, rhs } => bin(BinOp::Mul, dst, lhs, rhs, r),
        Op::LslI64 { dst, lhs, rhs } => bin(BinOp::Lsl, dst, lhs, rhs, r),
        Op::LsrI64 { dst, lhs, rhs } => bin(BinOp::Lsr, dst, lhs, rhs, r),
        Op::AsrI64 { dst, lhs, rhs } => bin(BinOp::Asr, dst, lhs, rhs, r),
        Op::MovI64 { dst, src } => Instr::Mov(r.reg(dst), r.reg(src)),
        Op::CmpI64 { lhs, rhs } => Instr::Compare {
            lhs: r.reg(lhs),
            rhs: r.reg(rhs),
            add: false,
        },
        Op::CmnI64 { lhs, rhs } => Instr::Compare {
            lhs: r.reg(lhs),
            rhs: r.reg(rhs),
            add: true,
        },
        Op::TestI64 { lhs, rhs } => Instr::Test(r.reg(lhs), r.reg(rhs)),
        Op::SelectI64 {
            dst,
            cond,
            lhs,
            rhs,
        } => Instr::Select {
            dst: r.reg(dst),
            cond: Cond::parse(cond)?,
            lhs: r.reg(lhs),
            rhs: r.reg(rhs),
        },
        Op::CondCmpI64 {
            cond,
            lhs,
            rhs,
            negate,
            nzcv,
        } => Instr::CondCompare {
            cond: Cond::parse(cond)?,
            lhs: r.reg(lhs),
            rhs: r.reg(rhs),
            add: *negate,
            nzcv: *nzcv,
        },
        Op::PcRel { dst, pc, offset } => Instr::Const(r.reg(dst), pc.wrapping_add(*offset)),
        Op::LoadI8 { dst, addr, offset } => load(r, dst, addr, *offset, 1),
        Op::LoadI16 { dst, addr, offset } => load(r, dst, addr, *offset, 2),
        Op::LoadI32 { dst, addr, offset } => load(r, dst, addr, *offset, 4),
        Op::LoadI64 { dst, addr, offset } => load(r, dst, addr, *offset, 8),
        Op::StoreI8 { src, addr, offset } => store(r, src, addr, *offset, 1),
        Op::StoreI16 { src, addr, offset } => store(r, src, addr, *offset, 2),
        Op::StoreI32 { src, addr, offset } => store(r, src, addr, *offset, 4),
        Op::StoreI64 { src, addr, offset } => store(r, src, addr, *offset, 8),
        Op::LoadV {
            dst,
            addr,
            offset,
            size,
        } => Instr::LoadV {
            dst: r.reg(dst),
            addr: r.reg(addr),
            offset: *offset,
            size: *size,
        },
        Op::StoreV {
            src,
            addr,
            offset,
            size,
        } => Instr::StoreV {
            src: r.reg(src),
            addr: r.reg(addr),
            offset: *offset,
            size: *size,
        },
        Op::MovV { dst, src, bytes } => Instr::MovV {
            dst: r.reg(dst),
            src: r.reg(src),
            bytes: *bytes,
        },
        Op::ConstV { dst, lo, hi } => {
            Instr::ConstV(r.reg(dst), ((*hi as u128) << 64) | *lo as u128)
        }
        Op::VecOp {
            func,
            dst,
            lhs,
            rhs,
            esize,
            q,
        } => Instr::Vec {
            f: rt::simd::by_name(func).ok_or_else(|| format!("unknown vector op {func}"))?,
            dst: r.reg(dst),
            lhs: r.reg(lhs),
            rhs: r.reg(rhs),
            esize: *esize as u32,
            q: *q,
        },
        Op::VToX {
            dst,
            src,
            bits,
            lane,
        } => Instr::VToX {
            dst: r.reg(dst),
            src: r.reg(src),
            bits: *bits,
            lane: *lane,
        },
        Op::XToV {
            dst,
            src,
            bits,
            lane,
            zero_rest,
        } => Instr::XToV {
            dst: r.reg(dst),
            src: r.reg(src),
            bits: *bits,
            lane: *lane,
            zero_rest: *zero_rest,
        },
        other => return Err(format!("op not valid inside a lifted block: {other:?}")),
    })
}

fn load(r: &mut Resolver, dst: &str, addr: &str, offset: i64, size: u8) -> Instr {
    Instr::Load {
        dst: r.reg(dst),
        addr: r.reg(addr),
        offset,
        size,
    }
}

fn store(r: &mut Resolver, src: &str, addr: &str, offset: i64, size: u8) -> Instr {
    Instr::Store {
        src: r.reg(src),
        addr: r.reg(addr),
        offset,
        size,
    }
}

fn get(ctx: &Cpu, temps: &[u128], reg: Reg) -> i64 {
    match reg {
        Reg::X(i) => ctx.x[i],
        Reg::V(i) => ctx.simd[i] as u64 as i64,
        Reg::Temp(i) => temps[i] as u64 as i64,
    }
}

fn set(ctx: &mut Cpu, temps: &mut [u128], reg: Reg, value: i64) {
    match reg {
        Reg::X(i) => ctx.x[i] = value,
        Reg::V(i) => ctx.simd[i] = value as u64 as u128,
        Reg::Temp(i) => temps[i] = value as u64 as u128,
    }
}

fn get_v(ctx: &Cpu, temps: &[u128], reg: Reg) -> u128 {
    match reg {
        Reg::X(i) => ctx.x[i] as u64 as u128,
        Reg::V(i) => ctx.simd[i],
        Reg::Temp(i) => temps[i],
    }
}

fn set_v(ctx: &mut Cpu, temps: &mut [u128], reg: Reg, value: u128) {
    match reg {
        Reg::X(i) => ctx.x[i] = value as u64 as i64,
        Reg::V(i) => ctx.simd[i] = value,
        Reg::Temp(i) => temps[i] = value,
    }
}

fn address(ctx: &Cpu, temps: &[u128], addr: Reg, offset: i64) -> u64 {
    (get(ctx, temps, addr) as u64).wrapping_add(offset as u64)
}

fn mem_load(address: u64, size: u8) -> RuntimeResult<u64> {
    match size {
        1 => rt::mem_load_u8(address),
        2 => rt::mem_load_u16(address),
        4 => rt::mem_load_u32(address),
        _ => rt::mem_load_u64(address),
    }
}

fn mem_store(address: u64, size: u8, value: u64) -> RuntimeResult<()> {
    match size {
        1 => rt::mem_store_u8(address, value),
        2 => rt::mem_store_u16(address, value),
        4 => rt::mem_store_u32(address, value),
        _ => rt::mem_store_u64(address, value),
    }
}

fn set_compare_flags(ctx: &mut Cpu, lhs: i64, rhs: i64, add: bool) {
    let (result, overflow, carry) = if add {
        let (result, overflow) = lhs.overflowing_add(rhs);
        (result, overflow, (lhs as u64).overflowing_add(rhs as u64).1)
    } else {
        let (result, overflow) = lhs.overflowing_sub(rhs);
        (
            result,
            overflow,
            !(lhs as u64).overflowing_sub(rhs as u64).1,
        )
    };
    ctx.n = result < 0;
    ctx.z = result == 0;
    ctx.c = carry;
    ctx.v = overflow;
}

fn step(instr: &Instr, ctx: &mut Cpu, temps: &mut [u128]) -> RuntimeResult<()> {
    match *instr {
        Instr::Const(dst, value) => set(ctx, temps, dst, value),
        Instr::Bin(kind, dst, lhs, rhs) => {
            let (a, b) = (get(ctx, temps, lhs), get(ctx, temps, rhs));
            let value = match kind {
                BinOp::Add => a.wrapping_add(b),
                BinOp::Sub => a.wrapping_sub(b),
                BinOp::And => a & b,
                BinOp::Or => a | b,
                BinOp::Xor => a ^ b,
                BinOp::Mul => a.wrapping_mul(b),
                BinOp::Lsl => ((a as u64) << ((b as u64) & 63)) as i64,
                BinOp::Lsr => ((a as u64) >> ((b as u64) & 63)) as i64,
                BinOp::Asr => a >> ((b as u64) & 63),
            };
            set(ctx, temps, dst, value);
        }
        Instr::Mov(dst, src) => {
            let value = get(ctx, temps, src);
            set(ctx, temps, dst, value);
        }
        Instr::Compare { lhs, rhs, add } => {
            let (a, b) = (get(ctx, temps, lhs), get(ctx, temps, rhs));
            set_compare_flags(ctx, a, b, add);
        }
        Instr::Test(lhs, rhs) => {
            let result = get(ctx, temps, lhs) & get(ctx, temps, rhs);
            ctx.n = result < 0;
            ctx.z = result == 0;
            ctx.c = false;
            ctx.v = false;
        }
        Instr::Select {
            dst,
            cond,
            lhs,
            rhs,
        } => {
            let value = if cond.holds(ctx) {
                get(ctx, temps, lhs)
            } else {
                get(ctx, temps, rhs)
            };
            set(ctx, temps, dst, value);
        }
        Instr::CondCompare {
            cond,
            lhs,
            rhs,
            add,
            nzcv,
        } => {
            if cond.holds(ctx) {
                let (a, b) = (get(ctx, temps, lhs), get(ctx, temps, rhs));
                set_compare_flags(ctx, a, b, add);
            } else {
                ctx.n = nzcv & 8 != 0;
                ctx.z = nzcv & 4 != 0;
                ctx.c = nzcv & 2 != 0;
                ctx.v = nzcv & 1 != 0;
            }
        }
        Instr::Load {
            dst,
            addr,
            offset,
            size,
        } => {
            let value = mem_load(address(ctx, temps, addr, offset), size)?;
            set(ctx, temps, dst, value as i64);
        }
        Instr::Store {
            src,
            addr,
            offset,
            size,
        } => {
            let value = get(ctx, temps, src) as u64;
            mem_store(address(ctx, temps, addr, offset), size, value)?;
        }
        Instr::LoadV {
            dst,
            addr,
            offset,
            size,
        } => {
            let at = address(ctx, temps, addr, offset);
            let value = if size == 16 {
                let lo = rt::mem_load_u64(at)?;
                let hi = rt::mem_load_u64(at.wrapping_add(8))?;
                ((hi as u128) << 64) | lo as u128
            } else {
                mem_load(at, size)? as u128
            };
            set_v(ctx, temps, dst, value);
        }
        Instr::StoreV {
            src,
            addr,
            offset,
            size,
        } => {
            let at = address(ctx, temps, addr, offset);
            let value = get_v(ctx, temps, src);
            if size == 16 {
                rt::mem_store_u64(at, value as u64)?;
                rt::mem_store_u64(at.wrapping_add(8), (value >> 64) as u64)?;
            } else {
                mem_store(at, size, value as u64)?;
            }
        }
        Instr::MovV { dst, src, bytes } => {
            let value = get_v(ctx, temps, src);
            let value = if bytes >= 16 {
                value
            } else {
                value & ((1_u128 << (bytes as u32 * 8)) - 1)
            };
            set_v(ctx, temps, dst, value);
        }
        Instr::ConstV(dst, value) => set_v(ctx, temps, dst, value),
        Instr::Vec {
            f,
            dst,
            lhs,
            rhs,
            esize,
            q,
        } => {
            let value = f(
                get_v(ctx, temps, lhs),
                get_v(ctx, temps, rhs),
                get_v(ctx, temps, dst),
                esize,
                q,
            );
            set_v(ctx, temps, dst, value);
        }
        Instr::VToX {
            dst,
            src,
            bits,
            lane,
        } => {
            let mask = u64::MAX >> (64 - bits as u32);
            let value = (get_v(ctx, temps, src) >> (lane as u32 * bits as u32)) as u64 & mask;
            set(ctx, temps, dst, value as i64);
        }
        Instr::XToV {
            dst,
            src,
            bits,
            lane,
            zero_rest,
        } => {
            let mask = u64::MAX >> (64 - bits as u32);
            let shift = lane as u32 * bits as u32;
            let value = (((get(ctx, temps, src) as u64) & mask) as u128) << shift;
            let merged = if zero_rest {
                value
            } else {
                (get_v(ctx, temps, dst) & !((mask as u128) << shift)) | value
            };
            set_v(ctx, temps, dst, merged);
        }
    }
    Ok(())
}
