use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
pub struct Module {
    pub arch: String,
    #[serde(default)]
    pub segments: Vec<ModuleSegment>,
    pub functions: Vec<Function>,
    /// Imported symbols bound to placeholder guest addresses; calls to them go to the
    /// runtime's high-level emulation table.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<Import>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Import {
    pub address: u64,
    pub name: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ModuleSegment {
    pub name: String,
    pub base: u64,
    pub size: u64,
    pub permissions: ModuleSegmentPermissions,
    #[serde(default)]
    pub init_path: Option<String>,
    #[serde(default)]
    pub init_size: Option<u64>,
    #[serde(default)]
    pub zero_fill: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ModuleSegmentPermissions {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Function {
    pub name: String,
    /// Guest entry address; functions with one are reachable through indirect calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<u64>,
    #[serde(default)]
    pub ops: Vec<Op>,
    #[serde(default)]
    pub blocks: Vec<Block>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Block {
    pub label: String,
    pub start: u64,
    #[serde(default)]
    pub ops: Vec<Op>,
    pub terminator: Terminator,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    ConstI64 {
        dst: String,
        imm: i64,
    },
    AddI64 {
        dst: String,
        lhs: String,
        rhs: String,
    },
    MovI64 {
        dst: String,
        src: String,
    },
    SubI64 {
        dst: String,
        lhs: String,
        rhs: String,
    },
    AndI64 {
        dst: String,
        lhs: String,
        rhs: String,
    },
    OrI64 {
        dst: String,
        lhs: String,
        rhs: String,
    },
    XorI64 {
        dst: String,
        lhs: String,
        rhs: String,
    },
    CmpI64 {
        lhs: String,
        rhs: String,
    },
    CmnI64 {
        lhs: String,
        rhs: String,
    },
    TestI64 {
        lhs: String,
        rhs: String,
    },
    MulI64 {
        dst: String,
        lhs: String,
        rhs: String,
    },
    /// CCMP/CCMN: if `cond` holds, set flags from `lhs - rhs` (or `lhs + rhs` when `negate`),
    /// otherwise set NZCV to the immediate `nzcv`.
    CondCmpI64 {
        cond: String,
        lhs: String,
        rhs: String,
        negate: bool,
        nzcv: u8,
    },
    SelectI64 {
        dst: String,
        cond: String,
        lhs: String,
        rhs: String,
    },
    LslI64 {
        dst: String,
        lhs: String,
        rhs: String,
    },
    LsrI64 {
        dst: String,
        lhs: String,
        rhs: String,
    },
    AsrI64 {
        dst: String,
        lhs: String,
        rhs: String,
    },
    PcRel {
        dst: String,
        pc: i64,
        offset: i64,
    },
    LoadI8 {
        dst: String,
        addr: String,
        #[serde(default)]
        offset: i64,
    },
    LoadI16 {
        dst: String,
        addr: String,
        #[serde(default)]
        offset: i64,
    },
    LoadI32 {
        dst: String,
        addr: String,
        #[serde(default)]
        offset: i64,
    },
    LoadI64 {
        dst: String,
        addr: String,
        #[serde(default)]
        offset: i64,
    },
    StoreI8 {
        src: String,
        addr: String,
        #[serde(default)]
        offset: i64,
    },
    StoreI16 {
        src: String,
        addr: String,
        #[serde(default)]
        offset: i64,
    },
    StoreI32 {
        src: String,
        addr: String,
        #[serde(default)]
        offset: i64,
    },
    StoreI64 {
        src: String,
        addr: String,
        #[serde(default)]
        offset: i64,
    },
    /// Loads `size` bytes (1, 2, 4, 8 or 16) into a SIMD&FP register, zeroing the rest.
    LoadV {
        dst: String,
        addr: String,
        #[serde(default)]
        offset: i64,
        size: u8,
    },
    /// Stores the low `size` bytes of a SIMD&FP register.
    StoreV {
        src: String,
        addr: String,
        #[serde(default)]
        offset: i64,
        size: u8,
    },
    /// Copies the low `bytes` bytes of a SIMD&FP register and zeroes the rest.
    MovV {
        dst: String,
        src: String,
        bytes: u8,
    },
    ConstV {
        dst: String,
        lo: u64,
        hi: u64,
    },
    /// Reads lane `lane` (`bits` wide, zero-extended) of a SIMD&FP register into a GPR.
    VToX {
        dst: String,
        src: String,
        bits: u8,
        lane: u8,
    },
    /// Writes the low `bits` of a GPR into lane `lane`; `zero_rest` clears the other lanes.
    XToV {
        dst: String,
        src: String,
        bits: u8,
        lane: u8,
        zero_rest: bool,
    },
    Br {
        target: String,
    },
    BrCond {
        cond: String,
        then: String,
        #[serde(rename = "else")]
        else_target: String,
    },
    Call {
        target: String,
    },
    Syscall {
        name: String,
        args: Vec<String>,
    },
    Ret,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Terminator {
    Br {
        target: String,
    },
    BrCond {
        cond: String,
        then: String,
        #[serde(rename = "else")]
        else_target: String,
    },
    Call {
        target: String,
        next: String,
    },
    BrIndirect {
        reg: String,
    },
    CallIndirect {
        reg: String,
        next: String,
    },
    /// An instruction the lifter could not translate; raises a runtime error if reached.
    Trap {
        pc: u64,
        word: u32,
    },
    Ret,
}

impl Module {
    pub fn validate_arch(&self) -> Result<(), String> {
        if self.arch != "aarch64" {
            return Err(format!("unsupported arch: {}", self.arch));
        }
        Ok(())
    }
}
