use crate::homebrew::module::{ModuleJson, ModuleSegment, MODULE_SCHEMA_VERSION};
use crate::input::{Block, Function, Module, Op, Terminator};
use serde_json;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const MAX_BLOCK_INSTRUCTIONS: usize = 10_000;
const MAX_FUNCTION_INSTRUCTIONS: usize = 200_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiftMode {
    Stub,
    Decode,
    /// Like Decode, but an unsupported instruction ends its block with a runtime trap
    /// instead of failing the whole lift.
    Partial,
}

#[derive(Debug)]
pub struct LiftOptions {
    pub module_json_path: PathBuf,
    pub out_dir: PathBuf,
    pub entry_name: String,
    /// Guest address to start decoding from; defaults to the start of the text segment.
    pub entry_address: Option<u64>,
    pub mode: LiftMode,
}

#[derive(Debug)]
pub struct LiftReport {
    pub module_json_path: PathBuf,
    pub functions_emitted: usize,
    pub warnings: Vec<String>,
}

pub fn lift_homebrew(options: LiftOptions) -> Result<LiftReport, String> {
    if options.entry_name.trim().is_empty() {
        return Err("entry name must be non-empty".to_string());
    }

    let module_json_path = absolute_path(&options.module_json_path)?;
    let out_dir = absolute_path(&options.out_dir)?;

    let module_src = fs::read_to_string(&module_json_path).map_err(|err| err.to_string())?;
    let module_json: ModuleJson =
        serde_json::from_str(&module_src).map_err(|err| err.to_string())?;

    validate_homebrew_module(&module_json)?;

    let base_dir = module_json_path
        .parent()
        .ok_or_else(|| "homebrew module.json has no parent directory".to_string())?;

    match options.mode {
        LiftMode::Stub => lift_stub(&options, &module_json, base_dir, &out_dir),
        LiftMode::Decode | LiftMode::Partial => {
            lift_decode(&options, &module_json, base_dir, &out_dir)
        }
    }
}

fn validate_homebrew_module(module_json: &ModuleJson) -> Result<(), String> {
    if module_json.schema_version != MODULE_SCHEMA_VERSION {
        return Err(format!(
            "unsupported homebrew module schema version: {}",
            module_json.schema_version
        ));
    }
    if module_json.module_type != "homebrew" {
        return Err(format!(
            "unsupported module type for homebrew lifter: {}",
            module_json.module_type
        ));
    }
    if module_json.modules.is_empty() {
        return Err("homebrew module list is empty".to_string());
    }
    Ok(())
}

fn lift_stub(
    options: &LiftOptions,
    module_json: &ModuleJson,
    base_dir: &Path,
    out_dir: &Path,
) -> Result<LiftReport, String> {
    let mut warnings = Vec::new();

    if module_json.modules.len() > 1 {
        warnings.push(format!(
            "homebrew lifter emitted a stub entry for {} modules without decoding instructions",
            module_json.modules.len()
        ));
    } else {
        warnings
            .push("homebrew lifter emitted a stub entry without decoding instructions".to_string());
    }

    let segments = collect_segments(module_json, base_dir)?;
    if segments.is_empty() {
        warnings.push("homebrew module contains no segments".to_string());
    }

    ensure_dir(out_dir)?;

    let lifted = Module {
        arch: "aarch64".to_string(),
        segments: Vec::new(),
        functions: vec![Function {
            name: options.entry_name.clone(),
            address: None,
            ops: vec![Op::Ret],
            blocks: Vec::new(),
        }],
        imports: Vec::new(),
    };

    let output_path = out_dir.join("module.json");
    let output_json = serde_json::to_string_pretty(&lifted).map_err(|err| err.to_string())?;
    fs::write(&output_path, output_json).map_err(|err| err.to_string())?;

    Ok(LiftReport {
        module_json_path: output_path,
        functions_emitted: 1,
        warnings,
    })
}

fn lift_decode(
    options: &LiftOptions,
    module_json: &ModuleJson,
    base_dir: &Path,
    out_dir: &Path,
) -> Result<LiftReport, String> {
    let mut warnings = Vec::new();
    if module_json.modules.len() > 1 {
        warnings.push(format!(
            "homebrew lifter decoding only the first text segment across {} modules",
            module_json.modules.len()
        ));
    }

    let segments = collect_segments(module_json, base_dir)?;
    let text_segment = find_text_segment(&segments, &mut warnings)?;
    let text = load_text_segment(&text_segment, &mut warnings)?;

    let mut functions = Vec::new();
    let mut pending = BTreeSet::new();
    let mut seen = BTreeSet::new();
    let mut name_table = FunctionNames::new();
    let entry = options.entry_address.unwrap_or(text.base_addr);
    name_table.seed_entry(entry, options.entry_name.clone());

    pending.insert(entry);

    while let Some(func_addr) = pop_first(&mut pending) {
        if !text.contains(func_addr) {
            warnings.push(format!(
                "function entry 0x{func_addr:x} is outside text segment"
            ));
            continue;
        }
        if seen.contains(&func_addr) {
            continue;
        }
        seen.insert(func_addr);

        let func_name = name_table.name_for(func_addr);
        let mut state = DecodeState::new();
        let partial = matches!(options.mode, LiftMode::Partial);
        let (blocks, call_targets) = decode_function(
            &text,
            func_addr,
            partial,
            &mut state,
            &mut warnings,
            &mut name_table,
        )?;

        for target in call_targets {
            if !text.contains(target) {
                warnings.push(format!("call target 0x{target:x} is outside text segment"));
                continue;
            }
            pending.insert(target);
        }

        functions.push(Function {
            name: func_name,
            address: Some(func_addr),
            ops: Vec::new(),
            blocks,
        });
    }

    if functions.is_empty() {
        warnings.push("decoded zero functions from text segment".to_string());
    }

    ensure_dir(out_dir)?;

    let lifted = Module {
        arch: "aarch64".to_string(),
        segments: Vec::new(),
        functions,
        imports: Vec::new(),
    };

    let output_path = out_dir.join("module.json");
    let output_json = serde_json::to_string_pretty(&lifted).map_err(|err| err.to_string())?;
    fs::write(&output_path, output_json).map_err(|err| err.to_string())?;

    Ok(LiftReport {
        module_json_path: output_path,
        functions_emitted: lifted.functions.len(),
        warnings,
    })
}

#[derive(Debug, Clone)]
struct SegmentInfo {
    name: String,
    permissions: String,
    memory_offset: u64,
    file_size: u64,
    path: PathBuf,
}

#[derive(Debug, Clone)]
struct TextSegment {
    bytes: Vec<u8>,
    base_addr: u64,
}

impl TextSegment {
    fn contains(&self, addr: u64) -> bool {
        let end = self.base_addr + self.bytes.len() as u64;
        addr >= self.base_addr && addr < end
    }

    fn read_word(&self, addr: u64) -> Result<u32, String> {
        if !self.contains(addr) {
            return Err(format!("address 0x{addr:x} outside text segment"));
        }
        let offset = addr - self.base_addr;
        if offset % 4 != 0 {
            return Err(format!("unaligned instruction address 0x{addr:x}"));
        }
        let idx = offset as usize;
        if idx + 4 > self.bytes.len() {
            return Err(format!("instruction read overrun at 0x{addr:x}"));
        }
        Ok(u32::from_le_bytes([
            self.bytes[idx],
            self.bytes[idx + 1],
            self.bytes[idx + 2],
            self.bytes[idx + 3],
        ]))
    }
}

fn collect_segments(module_json: &ModuleJson, base_dir: &Path) -> Result<Vec<SegmentInfo>, String> {
    let mut segments = Vec::new();
    for module in &module_json.modules {
        for segment in &module.segments {
            let segment_path = resolve_segment_path(base_dir, segment);
            if !segment_path.exists() {
                return Err(format!(
                    "segment file not found: {}",
                    segment_path.display()
                ));
            }
            segments.push(SegmentInfo {
                name: segment.name.clone(),
                permissions: segment.permissions.clone(),
                memory_offset: segment.memory_offset,
                file_size: segment.file_size,
                path: segment_path,
            });
        }
    }
    Ok(segments)
}

fn find_text_segment(
    segments: &[SegmentInfo],
    warnings: &mut Vec<String>,
) -> Result<SegmentInfo, String> {
    let mut candidates = segments
        .iter()
        .filter(|segment| segment.name == "text" || segment.permissions.contains('x'));
    let first = candidates
        .next()
        .cloned()
        .ok_or_else(|| "no executable text segment found in homebrew module".to_string())?;
    if candidates.next().is_some() {
        warnings.push("multiple executable segments found; using the first".to_string());
    }
    Ok(first)
}

fn load_text_segment(
    segment: &SegmentInfo,
    warnings: &mut Vec<String>,
) -> Result<TextSegment, String> {
    let bytes = fs::read(&segment.path).map_err(|err| err.to_string())?;
    if bytes.len() as u64 != segment.file_size {
        warnings.push(format!(
            "text segment size mismatch: manifest {} bytes, file {} bytes",
            segment.file_size,
            bytes.len()
        ));
    }
    if bytes.len() % 4 != 0 {
        return Err(format!(
            "text segment size is not 4-byte aligned: {} bytes",
            bytes.len()
        ));
    }
    Ok(TextSegment {
        bytes,
        base_addr: segment.memory_offset,
    })
}

#[derive(Debug, Default)]
struct DecodeState {
    temp_counter: u32,
}

impl DecodeState {
    fn new() -> Self {
        Self { temp_counter: 0 }
    }

    fn next_temp(&mut self, prefix: &str) -> String {
        let name = format!("{prefix}{}", self.temp_counter);
        self.temp_counter += 1;
        name
    }
}

struct FunctionNames {
    names: BTreeMap<u64, String>,
}

impl FunctionNames {
    fn new() -> Self {
        Self {
            names: BTreeMap::new(),
        }
    }

    fn name_for(&mut self, addr: u64) -> String {
        if let Some(name) = self.names.get(&addr) {
            return name.clone();
        }
        let name = format!("fn_{addr:016x}");
        self.names.insert(addr, name.clone());
        name
    }

    fn seed_entry(&mut self, addr: u64, name: String) {
        self.names.insert(addr, name);
    }
}

/// Guest text mapped at an address, for decoding functions on demand (the runtime
/// interpreter fallback uses this for code that was not recompiled ahead of time).
pub struct TextImage(TextSegment);

impl TextImage {
    pub fn new(bytes: Vec<u8>, base_addr: u64) -> Self {
        Self(TextSegment { bytes, base_addr })
    }

    pub fn contains(&self, addr: u64) -> bool {
        self.0.contains(addr)
    }

    /// Decodes the function at `entry` in partial mode: unsupported instructions become
    /// trap terminators. Direct call targets are named `fn_<address as 16 hex digits>`.
    pub fn lift_function(&self, entry: u64) -> Result<Vec<Block>, String> {
        let mut state = DecodeState::new();
        let mut warnings = Vec::new();
        let mut names = FunctionNames::new();
        let (blocks, _) =
            decode_function(&self.0, entry, true, &mut state, &mut warnings, &mut names)?;
        Ok(blocks)
    }
}

fn decode_function(
    text: &TextSegment,
    entry: u64,
    partial: bool,
    state: &mut DecodeState,
    warnings: &mut Vec<String>,
    names: &mut FunctionNames,
) -> Result<(Vec<Block>, Vec<u64>), String> {
    let mut blocks = BTreeMap::new();
    let mut pending = BTreeSet::new();
    let mut call_targets = BTreeSet::new();
    let mut decoded_count = 0_usize;

    pending.insert(entry);

    while let Some(addr) = pop_first(&mut pending) {
        if blocks.contains_key(&addr) {
            continue;
        }

        let (block, block_targets, block_calls, block_insts) =
            decode_block(text, addr, partial, state, warnings, names)?;
        decoded_count += block_insts;
        if decoded_count > MAX_FUNCTION_INSTRUCTIONS {
            return Err(format!(
                "function decode limit exceeded ({} instructions)",
                MAX_FUNCTION_INSTRUCTIONS
            ));
        }

        for target in block_targets {
            pending.insert(target);
        }
        for target in block_calls {
            call_targets.insert(target);
        }
        blocks.insert(addr, block);
    }

    // Code generation and the interpreter both start at the first block.
    let entry_block = blocks.remove(&entry);
    Ok((
        entry_block
            .into_iter()
            .chain(blocks.into_values())
            .collect(),
        call_targets.into_iter().collect(),
    ))
}

fn decode_block(
    text: &TextSegment,
    start: u64,
    partial: bool,
    state: &mut DecodeState,
    warnings: &mut Vec<String>,
    names: &mut FunctionNames,
) -> Result<(Block, Vec<u64>, Vec<u64>, usize), String> {
    if start % 4 != 0 {
        return Err(format!("unaligned block start 0x{start:x}"));
    }
    if !text.contains(start) {
        return Err(format!("block start 0x{start:x} outside text segment"));
    }

    let mut ops = Vec::new();
    let mut pc = start;
    let mut inst_count = 0_usize;

    loop {
        if inst_count >= MAX_BLOCK_INSTRUCTIONS {
            return Err(format!(
                "block decode limit exceeded ({} instructions)",
                MAX_BLOCK_INSTRUCTIONS
            ));
        }
        let word = text.read_word(pc)?;
        let decoded = match decode_one(word, pc, state, &mut ops, warnings) {
            Ok(decoded) => decoded,
            Err(err) if partial => {
                warnings.push(err);
                DecodedOutcome::Terminate(TermInfo::Trap { pc, word })
            }
            Err(err) => return Err(err),
        };
        inst_count += 1;

        match decoded {
            DecodedOutcome::Continue => {
                pc = pc.wrapping_add(4);
                continue;
            }
            DecodedOutcome::Terminate(term) => {
                let (terminator, block_targets, call_targets) = lower_terminator(term, names);
                let block = Block {
                    label: block_label(start),
                    start,
                    ops,
                    terminator,
                };
                return Ok((block, block_targets, call_targets, inst_count));
            }
        }
    }
}

fn lower_terminator(term: TermInfo, names: &mut FunctionNames) -> (Terminator, Vec<u64>, Vec<u64>) {
    match term {
        TermInfo::Br { target } => (
            Terminator::Br {
                target: block_label(target),
            },
            vec![target],
            Vec::new(),
        ),
        TermInfo::BrCond {
            cond,
            then_target,
            else_target,
        } => (
            Terminator::BrCond {
                cond,
                then: block_label(then_target),
                else_target: block_label(else_target),
            },
            vec![then_target, else_target],
            Vec::new(),
        ),
        TermInfo::Call { target, next } => {
            let name = names.name_for(target);
            (
                Terminator::Call {
                    target: name,
                    next: block_label(next),
                },
                vec![next],
                vec![target],
            )
        }
        TermInfo::BrIndirect { reg } => (
            Terminator::BrIndirect { reg: reg_name(reg) },
            Vec::new(),
            Vec::new(),
        ),
        TermInfo::CallIndirect { reg, next } => (
            Terminator::CallIndirect {
                reg: reg_name(reg),
                next: block_label(next),
            },
            vec![next],
            Vec::new(),
        ),
        TermInfo::Trap { pc, word } => (Terminator::Trap { pc, word }, Vec::new(), Vec::new()),
        TermInfo::Ret => (Terminator::Ret, Vec::new(), Vec::new()),
    }
}

#[derive(Debug)]
enum DecodedOutcome {
    Continue,
    Terminate(TermInfo),
}

#[derive(Debug)]
enum TermInfo {
    Br {
        target: u64,
    },
    BrCond {
        cond: String,
        then_target: u64,
        else_target: u64,
    },
    Call {
        target: u64,
        next: u64,
    },
    BrIndirect {
        reg: u8,
    },
    CallIndirect {
        reg: u8,
        next: u64,
    },
    Trap {
        pc: u64,
        word: u32,
    },
    Ret,
}

fn decode_one(
    word: u32,
    pc: u64,
    state: &mut DecodeState,
    ops: &mut Vec<Op>,
    _warnings: &mut Vec<String>,
) -> Result<DecodedOutcome, String> {
    // Hints (NOP, YIELD, PAC/BTI landing pads) and CLREX/DSB/DMB/ISB carry no state the
    // single-threaded translation models.
    if word & 0xFFFF_F01F == 0xD503_201F
        || (word & 0xFFFF_F01F == 0xD503_301F && matches!((word >> 5) & 7, 2 | 4 | 5 | 6))
    {
        return Ok(DecodedOutcome::Continue);
    }
    if is_ret(word) {
        return Ok(DecodedOutcome::Terminate(TermInfo::Ret));
    }
    if let Some(reg) = decode_br_register(word) {
        return Ok(DecodedOutcome::Terminate(TermInfo::BrIndirect { reg }));
    }
    // BLR
    if word & 0xFFFF_FC1F == 0xD63F_0000 {
        return Ok(DecodedOutcome::Terminate(TermInfo::CallIndirect {
            reg: ((word >> 5) & 0x1F) as u8,
            next: pc.wrapping_add(4),
        }));
    }
    if let Some(target) = decode_branch_imm(word, pc) {
        return Ok(DecodedOutcome::Terminate(TermInfo::Br { target }));
    }
    if let Some(target) = decode_branch_link(word, pc) {
        return Ok(DecodedOutcome::Terminate(TermInfo::Call {
            target,
            next: pc.wrapping_add(4),
        }));
    }
    if let Some((cond, target)) = decode_branch_cond(word, pc)? {
        return Ok(DecodedOutcome::Terminate(TermInfo::BrCond {
            cond,
            then_target: target,
            else_target: pc.wrapping_add(4),
        }));
    }

    if let Some((is_nz, reg, target)) = decode_cbz(word, pc)? {
        let zero = state.next_temp("imm");
        ops.push(Op::ConstI64 {
            dst: zero.clone(),
            imm: 0,
        });
        ops.push(Op::CmpI64 {
            lhs: reg_name(reg),
            rhs: zero,
        });
        let cond = if is_nz { "ne" } else { "eq" };
        return Ok(DecodedOutcome::Terminate(TermInfo::BrCond {
            cond: cond.to_string(),
            then_target: target,
            else_target: pc.wrapping_add(4),
        }));
    }

    if let Some((is_nz, reg, bit, target)) = decode_tbz(word, pc)? {
        let tmp_shift = state.next_temp("tmp");
        let tmp_mask = state.next_temp("tmp");
        let tmp_one = state.next_temp("imm");
        let tmp_zero = state.next_temp("imm");
        ops.push(Op::ConstI64 {
            dst: tmp_one.clone(),
            imm: 1,
        });
        ops.push(Op::ConstI64 {
            dst: tmp_zero.clone(),
            imm: 0,
        });
        ops.push(Op::ConstI64 {
            dst: tmp_shift.clone(),
            imm: bit as i64,
        });
        ops.push(Op::LsrI64 {
            dst: tmp_mask.clone(),
            lhs: reg_name(reg),
            rhs: tmp_shift,
        });
        ops.push(Op::AndI64 {
            dst: tmp_mask.clone(),
            lhs: tmp_mask.clone(),
            rhs: tmp_one,
        });
        ops.push(Op::CmpI64 {
            lhs: tmp_mask,
            rhs: tmp_zero,
        });
        let cond = if is_nz { "ne" } else { "eq" };
        return Ok(DecodedOutcome::Terminate(TermInfo::BrCond {
            cond: cond.to_string(),
            then_target: target,
            else_target: pc.wrapping_add(4),
        }));
    }

    if let Some(decoded) = decode_mov_wide(word)? {
        emit_mov_wide(decoded, state, ops);
        return Ok(DecodedOutcome::Continue);
    }

    if let Some(decoded) = decode_adr(word, pc)? {
        ops.push(decoded);
        return Ok(DecodedOutcome::Continue);
    }

    if let Some(decoded) = decode_add_sub_immediate(word)? {
        emit_add_sub_immediate(decoded, state, ops);
        return Ok(DecodedOutcome::Continue);
    }

    if decode_integer_alu(word, state, ops) {
        return Ok(DecodedOutcome::Continue);
    }

    if decode_simd_move(word, state, ops) {
        return Ok(DecodedOutcome::Continue);
    }

    if let Some((access, status)) = decode_exclusive(word) {
        emit_load_store(access, state, ops);
        if let Some(rs) = status {
            let dst = dst_zr(rs, state);
            ops.push(Op::ConstI64 { dst, imm: 0 });
        }
        return Ok(DecodedOutcome::Continue);
    }

    if let Some(decoded) = decode_load_store(word)? {
        emit_load_store(decoded, state, ops);
        return Ok(DecodedOutcome::Continue);
    }

    Err(format!(
        "unsupported instruction 0x{word:08x} at 0x{pc:016x}"
    ))
}

#[derive(Debug, Clone, Copy)]
struct MovWide {
    dst: u8,
    kind: MovWideKind,
    imm: u16,
    shift: u8,
    is_32: bool,
}

#[derive(Debug, Clone, Copy)]
enum MovWideKind {
    MovZ,
    MovN,
    MovK,
}

fn decode_mov_wide(word: u32) -> Result<Option<MovWide>, String> {
    let sf = (word >> 31) & 0x1;
    let opc = (word >> 29) & 0x3;
    let fixed = (word >> 23) & 0x3F;
    if fixed != 0b100101 {
        return Ok(None);
    }
    let hw = ((word >> 21) & 0x3) as u8;
    let imm16 = ((word >> 5) & 0xFFFF) as u16;
    let rd = (word & 0x1F) as u8;
    let kind = match opc {
        0b00 => MovWideKind::MovN,
        0b10 => MovWideKind::MovZ,
        0b11 => MovWideKind::MovK,
        _ => return Ok(None),
    };
    Ok(Some(MovWide {
        dst: rd,
        kind,
        imm: imm16,
        shift: hw,
        is_32: sf == 0,
    }))
}

fn emit_mov_wide(decoded: MovWide, state: &mut DecodeState, ops: &mut Vec<Op>) {
    let shift_bits = (decoded.shift as u32) * 16;
    let imm_value = (decoded.imm as u64) << shift_bits;
    let dst = dst_zr(decoded.dst, state);
    match decoded.kind {
        MovWideKind::MovZ | MovWideKind::MovN => {
            let value = if matches!(decoded.kind, MovWideKind::MovN) {
                !imm_value
            } else {
                imm_value
            };
            ops.push(Op::ConstI64 {
                dst: dst.clone(),
                imm: value as i64,
            });
        }
        MovWideKind::MovK => {
            mask_in_place(&dst, !(0xFFFF_u64 << shift_bits), state, ops);
            let imm = const_temp(imm_value as i64, state, ops);
            ops.push(Op::OrI64 {
                dst: dst.clone(),
                lhs: dst.clone(),
                rhs: imm,
            });
        }
    }
    if decoded.is_32 {
        zero_extend_32(&dst, state, ops);
    }
}

fn decode_adr(word: u32, pc: u64) -> Result<Option<Op>, String> {
    let op = (word >> 31) & 0x1;
    let fixed = (word >> 24) & 0x1F;
    if fixed != 0b10000 {
        return Ok(None);
    }
    let immlo = (word >> 29) & 0x3;
    let immhi = (word >> 5) & 0x7FFFF;
    let rd = (word & 0x1F) as u8;
    let imm = ((immhi << 2) | immlo) as i64;
    let signed = sign_extend(imm, 21);
    if op == 0 {
        return Ok(Some(Op::PcRel {
            dst: reg_name(rd),
            pc: pc as i64,
            offset: signed,
        }));
    }
    let page = (pc as i64) & !0xFFF;
    let offset = signed << 12;
    Ok(Some(Op::PcRel {
        dst: reg_name(rd),
        pc: page,
        offset,
    }))
}

#[derive(Debug, Clone, Copy)]
struct AddSubImm {
    dst: u8,
    src: u8,
    imm: u64,
    is_sub: bool,
    set_flags: bool,
    is_32: bool,
}

fn decode_add_sub_immediate(word: u32) -> Result<Option<AddSubImm>, String> {
    let sf = (word >> 31) & 0x1;
    let op = (word >> 30) & 0x1;
    let s = (word >> 29) & 0x1;
    let opcode = (word >> 24) & 0x1F;
    if opcode != 0b10001 {
        return Ok(None);
    }
    let shift = (word >> 22) & 0x3;
    if shift > 1 {
        return Ok(None);
    }
    let imm12 = (word >> 10) & 0xFFF;
    let rn = ((word >> 5) & 0x1F) as u8;
    let rd = (word & 0x1F) as u8;
    let imm = (imm12 as u64) << (shift * 12);
    Ok(Some(AddSubImm {
        dst: rd,
        src: rn,
        imm,
        is_sub: op == 1,
        set_flags: s == 1,
        is_32: sf == 0,
    }))
}

fn emit_add_sub_immediate(decoded: AddSubImm, state: &mut DecodeState, ops: &mut Vec<Op>) {
    let imm = const_temp(decoded.imm as i64, state, ops);
    // Immediate forms address SP with register 31, except the flag-setting destination.
    let dst = if decoded.set_flags && decoded.dst == 31 {
        None
    } else {
        Some(reg_name(decoded.dst))
    };
    emit_add_sub(
        &reg_name(decoded.src),
        &imm,
        dst,
        decoded.is_sub,
        decoded.set_flags,
        decoded.is_32,
        state,
        ops,
    );
}

fn const_temp(value: i64, state: &mut DecodeState, ops: &mut Vec<Op>) -> String {
    let temp = state.next_temp("imm");
    ops.push(Op::ConstI64 {
        dst: temp.clone(),
        imm: value,
    });
    temp
}

/// Source operand where register 31 encodes XZR.
fn src_zr(reg: u8, state: &mut DecodeState, ops: &mut Vec<Op>) -> String {
    if reg == 31 {
        const_temp(0, state, ops)
    } else {
        reg_name(reg)
    }
}

/// Destination where register 31 encodes XZR (the result is discarded).
fn dst_zr(reg: u8, state: &mut DecodeState) -> String {
    if reg == 31 {
        state.next_temp("zr")
    } else {
        reg_name(reg)
    }
}

fn copy_temp(src: &str, state: &mut DecodeState, ops: &mut Vec<Op>) -> String {
    let temp = state.next_temp("tmp");
    ops.push(Op::MovI64 {
        dst: temp.clone(),
        src: src.to_string(),
    });
    temp
}

/// Sign-extends the low `bits` of `reg` in place.
fn sign_extend_in_place(reg: &str, bits: u32, state: &mut DecodeState, ops: &mut Vec<Op>) {
    let k = const_temp(64 - bits as i64, state, ops);
    ops.push(Op::LslI64 {
        dst: reg.to_string(),
        lhs: reg.to_string(),
        rhs: k.clone(),
    });
    ops.push(Op::AsrI64 {
        dst: reg.to_string(),
        lhs: reg.to_string(),
        rhs: k,
    });
}

fn mask_in_place(reg: &str, mask: u64, state: &mut DecodeState, ops: &mut Vec<Op>) {
    let m = const_temp(mask as i64, state, ops);
    ops.push(Op::AndI64 {
        dst: reg.to_string(),
        lhs: reg.to_string(),
        rhs: m,
    });
}

fn low_mask(width: u32) -> u64 {
    if width >= 64 {
        u64::MAX
    } else {
        (1 << width) - 1
    }
}

/// Applies LSL (0), LSR (1) or ASR (2) by `amount`, honouring 32-bit operand width.
fn emit_shifted(
    src: &str,
    kind: u32,
    amount: u32,
    is_32: bool,
    state: &mut DecodeState,
    ops: &mut Vec<Op>,
) -> String {
    if amount == 0 {
        return src.to_string();
    }
    let value = copy_temp(src, state, ops);
    if is_32 && kind == 1 {
        zero_extend_32(&value, state, ops);
    } else if is_32 && kind == 2 {
        sign_extend_in_place(&value, 32, state, ops);
    }
    let k = const_temp(amount as i64, state, ops);
    let (dst, lhs) = (value.clone(), value.clone());
    ops.push(match kind {
        0 => Op::LslI64 { dst, lhs, rhs: k },
        1 => Op::LsrI64 { dst, lhs, rhs: k },
        _ => Op::AsrI64 { dst, lhs, rhs: k },
    });
    value
}

/// 32-bit flags: shifting both operands into the top half makes the 64-bit flag
/// computation produce exactly the 32-bit N, Z, C and V.
fn flag_operand(src: &str, is_32: bool, state: &mut DecodeState, ops: &mut Vec<Op>) -> String {
    if !is_32 {
        return src.to_string();
    }
    emit_shifted(src, 0, 32, false, state, ops)
}

#[allow(clippy::too_many_arguments)]
fn emit_add_sub(
    lhs: &str,
    rhs: &str,
    dst: Option<String>,
    is_sub: bool,
    set_flags: bool,
    is_32: bool,
    state: &mut DecodeState,
    ops: &mut Vec<Op>,
) {
    if set_flags {
        let lhs = flag_operand(lhs, is_32, state, ops);
        let rhs = flag_operand(rhs, is_32, state, ops);
        ops.push(if is_sub {
            Op::CmpI64 { lhs, rhs }
        } else {
            Op::CmnI64 { lhs, rhs }
        });
    }
    let Some(dst) = dst else {
        return;
    };
    let (lhs, rhs) = (lhs.to_string(), rhs.to_string());
    ops.push(if is_sub {
        Op::SubI64 {
            dst: dst.clone(),
            lhs,
            rhs,
        }
    } else {
        Op::AddI64 {
            dst: dst.clone(),
            lhs,
            rhs,
        }
    });
    if is_32 {
        zero_extend_32(&dst, state, ops);
    }
}

/// opc: 0 AND, 1 ORR, 2 EOR, 3 ANDS. `dst` of None discards the result.
fn emit_logical(
    opc: u32,
    lhs: &str,
    rhs: &str,
    dst: String,
    is_32: bool,
    state: &mut DecodeState,
    ops: &mut Vec<Op>,
) {
    let (lhs, rhs) = (lhs.to_string(), rhs.to_string());
    ops.push(match opc {
        1 => Op::OrI64 {
            dst: dst.clone(),
            lhs,
            rhs,
        },
        2 => Op::XorI64 {
            dst: dst.clone(),
            lhs,
            rhs,
        },
        _ => Op::AndI64 {
            dst: dst.clone(),
            lhs,
            rhs,
        },
    });
    if is_32 {
        zero_extend_32(&dst, state, ops);
    }
    if opc == 3 {
        let value = flag_operand(&dst, is_32, state, ops);
        ops.push(Op::TestI64 {
            lhs: value.clone(),
            rhs: value,
        });
    }
}

/// DecodeBitMasks for logical immediates; None for reserved encodings.
fn decode_bit_mask(n: u32, imms: u32, immr: u32, is_32: bool) -> Option<u64> {
    let combined = (n << 6) | (!imms & 0x3F);
    if combined == 0 || (is_32 && n == 1) {
        return None;
    }
    let len = 31 - combined.leading_zeros();
    if len < 1 {
        return None;
    }
    let size = 1_u32 << len;
    let levels = size - 1;
    let (s, r) = (imms & levels, immr & levels);
    if s == levels {
        return None;
    }
    let ones = low_mask(s + 1);
    let elem = if r == 0 {
        ones
    } else {
        ((ones >> r) | (ones << (size - r))) & low_mask(size)
    };
    let mut value = 0_u64;
    let mut pos = 0;
    while pos < 64 {
        value |= elem << pos;
        pos += size;
    }
    Some(if is_32 { value & 0xFFFF_FFFF } else { value })
}

/// Integer data-processing forms: add/sub (shifted and extended register), logical
/// (shifted register and immediate), SBFM/UBFM, conditional select, and multiply-add.
/// Returns false without emitting anything when `word` is none of these.
fn decode_integer_alu(word: u32, state: &mut DecodeState, ops: &mut Vec<Op>) -> bool {
    let is_32 = word >> 31 == 0;
    let width = if is_32 { 32 } else { 64 };
    let rd = (word & 0x1F) as u8;
    let rn = ((word >> 5) & 0x1F) as u8;
    let rm = ((word >> 16) & 0x1F) as u8;

    // Add/sub, shifted register: sf op S 01011 shift 0 Rm imm6 Rn Rd.
    if word & 0x1F20_0000 == 0x0B00_0000 {
        let (kind, amount) = ((word >> 22) & 3, (word >> 10) & 0x3F);
        if kind == 3 || amount >= width {
            return false;
        }
        let set_flags = (word >> 29) & 1 == 1;
        let lhs = src_zr(rn, state, ops);
        let rhs = src_zr(rm, state, ops);
        let rhs = emit_shifted(&rhs, kind, amount, is_32, state, ops);
        let dst = (rd != 31).then(|| reg_name(rd));
        emit_add_sub(
            &lhs,
            &rhs,
            dst,
            (word >> 30) & 1 == 1,
            set_flags,
            is_32,
            state,
            ops,
        );
        return true;
    }

    // Add/sub, extended register: sf op S 01011 00 1 Rm option imm3 Rn Rd (Rn/Rd may be SP).
    if word & 0x1FE0_0000 == 0x0B20_0000 {
        let (option, amount) = ((word >> 13) & 7, (word >> 10) & 7);
        if amount > 4 {
            return false;
        }
        let set_flags = (word >> 29) & 1 == 1;
        let rhs = src_zr(rm, state, ops);
        let rhs = copy_temp(&rhs, state, ops);
        match option {
            0 => mask_in_place(&rhs, 0xFF, state, ops),
            1 => mask_in_place(&rhs, 0xFFFF, state, ops),
            2 => zero_extend_32(&rhs, state, ops),
            4 => sign_extend_in_place(&rhs, 8, state, ops),
            5 => sign_extend_in_place(&rhs, 16, state, ops),
            6 => sign_extend_in_place(&rhs, 32, state, ops),
            _ => {}
        }
        let rhs = emit_shifted(&rhs, 0, amount, false, state, ops);
        let dst = (!(set_flags && rd == 31)).then(|| reg_name(rd));
        emit_add_sub(
            &reg_name(rn),
            &rhs,
            dst,
            (word >> 30) & 1 == 1,
            set_flags,
            is_32,
            state,
            ops,
        );
        return true;
    }

    // Logical, shifted register: sf opc 01010 shift N Rm imm6 Rn Rd.
    if word & 0x1F00_0000 == 0x0A00_0000 {
        let (kind, amount) = ((word >> 22) & 3, (word >> 10) & 0x3F);
        if kind == 3 || amount >= width {
            return false;
        }
        let lhs = src_zr(rn, state, ops);
        let rhs = src_zr(rm, state, ops);
        let mut rhs = emit_shifted(&rhs, kind, amount, is_32, state, ops);
        if (word >> 21) & 1 == 1 {
            // BIC/ORN/EON/BICS invert the second operand.
            let ones = const_temp(-1, state, ops);
            let inverted = state.next_temp("tmp");
            ops.push(Op::XorI64 {
                dst: inverted.clone(),
                lhs: rhs,
                rhs: ones,
            });
            rhs = inverted;
        }
        let dst = dst_zr(rd, state);
        emit_logical((word >> 29) & 3, &lhs, &rhs, dst, is_32, state, ops);
        return true;
    }

    // Logical, immediate: sf opc 100100 N immr imms Rn Rd.
    if (word >> 23) & 0x3F == 0b10_0100 {
        let Some(mask) = decode_bit_mask(
            (word >> 22) & 1,
            (word >> 10) & 0x3F,
            (word >> 16) & 0x3F,
            is_32,
        ) else {
            return false;
        };
        let opc = (word >> 29) & 3;
        let lhs = src_zr(rn, state, ops);
        let imm = const_temp(mask as i64, state, ops);
        // AND/ORR/EOR may write SP; ANDS writes XZR.
        let dst = if opc == 3 {
            dst_zr(rd, state)
        } else {
            reg_name(rd)
        };
        emit_logical(opc, &lhs, &imm, dst, is_32, state, ops);
        return true;
    }

    // Bitfield SBFM/UBFM: sf opc 100110 N immr imms Rn Rd (BFM is not handled).
    if (word >> 23) & 0x3F == 0b10_0110 {
        let opc = (word >> 29) & 3;
        let (n, immr, imms) = ((word >> 22) & 1, (word >> 16) & 0x3F, (word >> 10) & 0x3F);
        if opc == 1 || opc == 3 || n == is_32 as u32 || immr >= width || imms >= width {
            return false;
        }
        let signed = opc == 0;
        let src = src_zr(rn, state, ops);
        let value = copy_temp(&src, state, ops);
        if imms >= immr {
            // SBFX/UBFX (and ASR/LSR/SXTW/UXTB... aliases): extract bits imms..immr.
            if signed {
                let up = const_temp(63 - imms as i64, state, ops);
                let down = const_temp((63 - imms + immr) as i64, state, ops);
                ops.push(Op::LslI64 {
                    dst: value.clone(),
                    lhs: value.clone(),
                    rhs: up,
                });
                ops.push(Op::AsrI64 {
                    dst: value.clone(),
                    lhs: value.clone(),
                    rhs: down,
                });
            } else {
                let k = const_temp(immr as i64, state, ops);
                ops.push(Op::LsrI64 {
                    dst: value.clone(),
                    lhs: value.clone(),
                    rhs: k,
                });
                mask_in_place(&value, low_mask(imms - immr + 1), state, ops);
            }
        } else {
            // SBFIZ/UBFIZ (and LSL alias): insert the low imms+1 bits at width-immr.
            if signed {
                sign_extend_in_place(&value, imms + 1, state, ops);
            } else {
                mask_in_place(&value, low_mask(imms + 1), state, ops);
            }
            let k = const_temp((width - immr) as i64, state, ops);
            ops.push(Op::LslI64 {
                dst: value.clone(),
                lhs: value.clone(),
                rhs: k,
            });
        }
        let dst = dst_zr(rd, state);
        ops.push(Op::MovI64 {
            dst: dst.clone(),
            src: value,
        });
        if is_32 {
            zero_extend_32(&dst, state, ops);
        }
        return true;
    }

    // Conditional select: sf op 0 11010100 Rm cond op2 Rn Rd.
    if word & 0x3FE0_0800 == 0x1A80_0000 {
        let Some(cond) = cond_code(((word >> 12) & 0xF) as u8) else {
            return false;
        };
        let then_value = src_zr(rn, state, ops);
        let else_src = src_zr(rm, state, ops);
        let else_value = match ((word >> 30) & 1, (word >> 10) & 1) {
            (0, 0) => else_src,
            (0, _) => {
                // CSINC (CSET/CINC)
                let one = const_temp(1, state, ops);
                let t = state.next_temp("tmp");
                ops.push(Op::AddI64 {
                    dst: t.clone(),
                    lhs: else_src,
                    rhs: one,
                });
                t
            }
            (_, 0) => {
                // CSINV (CSETM/CINV)
                let ones = const_temp(-1, state, ops);
                let t = state.next_temp("tmp");
                ops.push(Op::XorI64 {
                    dst: t.clone(),
                    lhs: else_src,
                    rhs: ones,
                });
                t
            }
            _ => {
                // CSNEG (CNEG)
                let zero = const_temp(0, state, ops);
                let t = state.next_temp("tmp");
                ops.push(Op::SubI64 {
                    dst: t.clone(),
                    lhs: zero,
                    rhs: else_src,
                });
                t
            }
        };
        let dst = dst_zr(rd, state);
        ops.push(Op::SelectI64 {
            dst: dst.clone(),
            cond: cond.to_string(),
            lhs: then_value,
            rhs: else_value,
        });
        if is_32 {
            zero_extend_32(&dst, state, ops);
        }
        return true;
    }

    // CCMP/CCMN (register or immediate): sf op 1 11010010 Rm|imm5 cond i 0 Rn 0 nzcv.
    if word & 0x3FE0_0410 == 0x3A40_0000 {
        let Some(cond) = cond_code(((word >> 12) & 0xF) as u8) else {
            return false;
        };
        let lhs = src_zr(rn, state, ops);
        let rhs = if (word >> 11) & 1 == 1 {
            const_temp(rm as i64, state, ops)
        } else {
            src_zr(rm, state, ops)
        };
        let lhs = flag_operand(&lhs, is_32, state, ops);
        let rhs = flag_operand(&rhs, is_32, state, ops);
        ops.push(Op::CondCmpI64 {
            cond: cond.to_string(),
            lhs,
            rhs,
            negate: (word >> 30) & 1 == 0,
            nzcv: (word & 0xF) as u8,
        });
        return true;
    }

    // Multiply-add: sf 00 11011 op31 Rm o0 Ra Rn Rd (MADD/MSUB, SMADDL/UMADDL and subtracting forms).
    if word & 0x7F00_0000 == 0x1B00_0000 {
        let op31 = (word >> 21) & 7;
        let widen = match (op31, is_32) {
            (0, _) => None,
            (1, false) => Some(true),
            (5, false) => Some(false),
            _ => return false, // SMULH/UMULH and reserved
        };
        let ra = ((word >> 10) & 0x1F) as u8;
        let mut lhs = src_zr(rn, state, ops);
        let mut rhs = src_zr(rm, state, ops);
        if let Some(signed) = widen {
            for operand in [&mut lhs, &mut rhs] {
                let value = copy_temp(operand, state, ops);
                if signed {
                    sign_extend_in_place(&value, 32, state, ops);
                } else {
                    zero_extend_32(&value, state, ops);
                }
                *operand = value;
            }
        }
        let product = state.next_temp("tmp");
        ops.push(Op::MulI64 {
            dst: product.clone(),
            lhs,
            rhs,
        });
        let acc = src_zr(ra, state, ops);
        let dst = dst_zr(rd, state);
        ops.push(if (word >> 15) & 1 == 1 {
            Op::SubI64 {
                dst: dst.clone(),
                lhs: acc,
                rhs: product,
            }
        } else {
            Op::AddI64 {
                dst: dst.clone(),
                lhs: acc,
                rhs: product,
            }
        });
        if is_32 {
            zero_extend_32(&dst, state, ops);
        }
        return true;
    }

    false
}

/// VFPExpandImm: 8-bit floating-point immediate to an IEEE single (`bits` 32) or double.
fn vfp_expand_imm(imm8: u32, bits: u32) -> u64 {
    let a = (imm8 >> 7) as u64 & 1;
    let b = (imm8 >> 6) as u64 & 1;
    let cd = (imm8 >> 4) as u64 & 3;
    let efgh = imm8 as u64 & 0xF;
    if bits == 32 {
        (a << 31)
            | ((b ^ 1) << 30)
            | (if b == 1 { 0x1F << 25 } else { 0 })
            | (cd << 23)
            | (efgh << 19)
    } else {
        (a << 63)
            | ((b ^ 1) << 62)
            | (if b == 1 { 0xFF << 54 } else { 0 })
            | (cd << 52)
            | (efgh << 48)
    }
}

fn replicate(value: u64, bits: u32) -> u64 {
    (0..64 / bits).fold(0, |acc, i| acc | (value << (i * bits)))
}

/// SIMD&FP register moves: MOVI/MVNI/FMOV (immediate), FMOV between general and SIMD&FP
/// registers, FMOV (register), MOV (vector), UMOV, INS (general) and DUP (general).
/// Returns false without emitting anything when `word` is none of these.
fn decode_simd_move(word: u32, state: &mut DecodeState, ops: &mut Vec<Op>) -> bool {
    let rd = (word & 0x1F) as u8;
    let rn = ((word >> 5) & 0x1F) as u8;
    let q = (word >> 30) & 1 == 1;
    let vd = format!("v{rd}");
    let vn = format!("v{rn}");

    // Advanced SIMD modified immediate: 0 Q op 0111100000 abc cmode o2 1 defgh Rd.
    if word & 0x9FF8_0C00 == 0x0F00_0400 {
        let op = (word >> 29) & 1;
        let cmode = (word >> 12) & 0xF;
        let imm8 = (((word >> 16) & 7) << 5) | ((word >> 5) & 0x1F);
        let (imm64, invertible) = match cmode {
            0b0000 | 0b0010 | 0b0100 | 0b0110 => (
                replicate((imm8 as u64) << (8 * ((cmode >> 1) & 3)), 32),
                true,
            ),
            0b1000 | 0b1010 => (
                replicate((imm8 as u64) << (8 * ((cmode >> 1) & 1)), 16),
                true,
            ),
            0b1100 => (replicate(((imm8 as u64) << 8) | 0xFF, 32), true),
            0b1101 => (replicate(((imm8 as u64) << 16) | 0xFFFF, 32), true),
            0b1110 if op == 0 => (replicate(imm8 as u64, 8), false),
            0b1110 => (
                (0..8).fold(0, |acc, i| {
                    acc | if imm8 >> i & 1 == 1 {
                        0xFF << (i * 8)
                    } else {
                        0
                    }
                }),
                false,
            ),
            0b1111 if op == 0 => (replicate(vfp_expand_imm(imm8, 32), 32), false),
            0b1111 if q => (vfp_expand_imm(imm8, 64), false),
            _ => return false, // ORR/BIC (immediate) and reserved encodings
        };
        let imm64 = if op == 1 && invertible { !imm64 } else { imm64 };
        ops.push(Op::ConstV {
            dst: vd,
            lo: imm64,
            hi: if q { imm64 } else { 0 },
        });
        return true;
    }

    // FMOV (scalar, immediate): 00011110 type 1 imm8 100 00000 Rd.
    if word & 0xFF20_1FE0 == 0x1E20_1000 {
        let imm8 = (word >> 13) & 0xFF;
        let value = match (word >> 22) & 3 {
            0 => vfp_expand_imm(imm8, 32),
            1 => vfp_expand_imm(imm8, 64),
            _ => return false,
        };
        ops.push(Op::ConstV {
            dst: vd,
            lo: value,
            hi: 0,
        });
        return true;
    }

    // FMOV (general): sf 0011110 type 1 rmode opcode 000000 Rn Rd.
    if word & 0x7F20_FC00 == 0x1E20_0000 {
        let fields = (
            word >> 31,
            (word >> 22) & 3,
            (word >> 19) & 3,
            (word >> 16) & 7,
        );
        let (bits, lane, to_general) = match fields {
            (0, 0, 0, 6) => (32, 0, true),
            (0, 0, 0, 7) => (32, 0, false),
            (1, 1, 0, 6) => (64, 0, true),
            (1, 1, 0, 7) => (64, 0, false),
            (1, 2, 1, 6) => (64, 1, true),
            (1, 2, 1, 7) => (64, 1, false),
            _ => return false, // conversions are not handled here
        };
        if to_general {
            let dst = dst_zr(rd, state);
            ops.push(Op::VToX {
                dst,
                src: vn,
                bits,
                lane,
            });
        } else {
            let src = src_zr(rn, state, ops);
            ops.push(Op::XToV {
                dst: vd,
                src,
                bits,
                lane,
                zero_rest: lane == 0,
            });
        }
        return true;
    }

    // FMOV (register): 00011110 type 1 0000 00 10000 Rn Rd.
    if word & 0xFF3F_FC00 == 0x1E20_4000 {
        let bytes = match (word >> 22) & 3 {
            0 => 4,
            1 => 8,
            _ => return false,
        };
        ops.push(Op::MovV {
            dst: vd,
            src: vn,
            bytes,
        });
        return true;
    }

    // MOV (vector), i.e. ORR Vd, Vn, Vn: 0 Q 001110101 Rm 000111 Rn Rd.
    if word & 0xBFE0_FC00 == 0x0EA0_1C00 && ((word >> 16) & 0x1F) as u8 == rn {
        ops.push(Op::MovV {
            dst: vd,
            src: vn,
            bytes: if q { 16 } else { 8 },
        });
        return true;
    }

    // Advanced SIMD three same (integer and bitwise): 0 Q U 01110 size 1 Rm opcode 1 Rn Rd.
    if word & 0x9F20_0400 == 0x0E20_0400 {
        let size = (word >> 22) & 3;
        let u = (word >> 29) & 1;
        let op = match ((word >> 11) & 0x1F, u, size) {
            (0b10000, 0, _) => "add",
            (0b10000, 1, _) => "sub",
            (0b10011, 0, 0..=2) => "mul",
            (0b00011, 0, 0) => "and",
            (0b00011, 0, 1) => "bic",
            (0b00011, 0, 2) => "orr",
            (0b00011, 0, 3) => "orn",
            (0b00011, 1, 0) => "eor",
            (0b00011, 1, 1) => "bsl",
            (0b00011, 1, 2) => "bit",
            (0b00011, 1, 3) => "bif",
            (0b10001, 0, _) => "cmtst",
            (0b10001, 1, _) => "cmeq",
            (0b00110, 0, _) => "cmgt",
            (0b00110, 1, _) => "cmhi",
            (0b00111, 0, _) => "cmge",
            (0b00111, 1, _) => "cmhs",
            (0b01100, 0, 0..=2) => "smax",
            (0b01100, 1, 0..=2) => "umax",
            (0b01101, 0, 0..=2) => "smin",
            (0b01101, 1, 0..=2) => "umin",
            _ => return false,
        };
        if size == 3 && !q && !matches!((word >> 11) & 0x1F, 0b00011) {
            return false; // 64-bit lanes need the full register
        }
        let rm = ((word >> 16) & 0x1F) as u8;
        // ORR Vd, Vn, Vn is MOV (vector), handled below with the same result.
        ops.push(Op::VecOp {
            func: op.to_string(),
            dst: vd,
            lhs: vn,
            rhs: format!("v{rm}"),
            esize: 8 << size,
            q,
        });
        return true;
    }

    // UMOV / INS (general) / DUP (general): 0 Q 0 01110000 imm5 0 imm4 1 Rn Rd.
    if word & 0x9FE0_8400 == 0x0E00_0400 {
        let imm5 = (word >> 16) & 0x1F;
        if imm5 & 0xF == 0 {
            return false;
        }
        let size_log2 = imm5.trailing_zeros();
        let bits = 8_u8 << size_log2;
        let lane = (imm5 >> (size_log2 + 1)) as u8;
        match ((word >> 29) & 1, (word >> 11) & 0xF) {
            // UMOV Wd/Xd, Vn.T[lane]
            (0, 0b0111) => {
                let dst = dst_zr(rd, state);
                ops.push(Op::VToX {
                    dst,
                    src: vn,
                    bits,
                    lane,
                });
            }
            // INS Vd.T[lane], Wn/Xn
            (0, 0b0011) if q => {
                let src = src_zr(rn, state, ops);
                ops.push(Op::XToV {
                    dst: vd,
                    src,
                    bits,
                    lane,
                    zero_rest: false,
                });
            }
            // DUP Vd.T, Wn/Xn
            (0, 0b0001) => {
                if bits == 64 && !q {
                    return false;
                }
                let src = src_zr(rn, state, ops);
                let lanes = (if q { 128 } else { 64 }) / bits as u32;
                for lane in 0..lanes {
                    ops.push(Op::XToV {
                        dst: vd.clone(),
                        src: src.clone(),
                        bits,
                        lane: lane as u8,
                        zero_rest: lane == 0,
                    });
                }
            }
            _ => return false,
        }
        return true;
    }

    false
}

/// Load/store exclusive and ordered (LDXR/STXR, LDAXR/STLXR, LDXP/STXP, LDAR/STLR and
/// their LDLAR/STLLR forms): size 001000 o2 L o1 Rs o0 Rt2 Rn Rt.
///
/// ponytail: guest code runs on one host thread, so ordering is free and every
/// store-exclusive succeeds (status 0). Revisit once guest threads run concurrently.
fn decode_exclusive(word: u32) -> Option<(LoadStore, Option<u8>)> {
    if word & 0x3F00_0000 != 0x0800_0000 {
        return None;
    }
    let (o2, is_load, pair) = (
        (word >> 23) & 1,
        (word >> 22) & 1 == 1,
        (word >> 21) & 1 == 1,
    );
    if o2 == 1 && pair {
        return None; // CAS family
    }
    let size = if pair {
        if word >> 31 == 0 {
            return None;
        }
        2 + ((word >> 30) & 1) as u8
    } else {
        (word >> 30) as u8
    };
    let status = (o2 == 0 && !is_load).then_some(((word >> 16) & 0x1F) as u8);
    let access = LoadStore {
        is_load,
        vector: false,
        size,
        base: ((word >> 5) & 0x1F) as u8,
        rt: (word & 0x1F) as u8,
        rt2: pair.then_some(((word >> 10) & 0x1F) as u8),
        index: None,
        offset: 0,
        writeback: None,
        sign_extend_to: None,
    };
    Some((access, status))
}

#[derive(Debug, Clone, Copy)]
struct LoadStore {
    is_load: bool,
    /// SIMD&FP register transfer (B/H/S/D/Q); `size` 4 means 16 bytes.
    vector: bool,
    size: u8,
    base: u8,
    rt: u8,
    rt2: Option<u8>,
    /// Register-offset addressing: (rm, extend option, shift amount).
    index: Option<(u8, u8, u8)>,
    offset: i64,
    /// Amount added to the base register after the access (pre/post-index).
    writeback: Option<i64>,
    /// Sign-extending load: register width (32 or 64) the value is extended to.
    sign_extend_to: Option<u32>,
}

/// Integer and SIMD&FP loads/stores: unsigned-offset, unscaled, pre/post-index,
/// register-offset, sign-extending (LDRS*), and LDP/STP/LDNP/STNP/LDPSW.
fn decode_load_store(word: u32) -> Result<Option<LoadStore>, String> {
    let rn = ((word >> 5) & 0x1F) as u8;
    let rt = (word & 0x1F) as u8;
    let vector = (word >> 26) & 1 == 1;

    // Pair: opc 101 V 0 idx L imm7 Rt2 Rn Rt.
    if (word & 0x3A00_0000) == 0x2800_0000 {
        let is_load = (word >> 22) & 1 == 1;
        let (size, sign_extend_to) = match (word >> 30, is_load, vector) {
            (0b00, _, false) => (2, None),
            (0b10, _, false) => (3, None),
            (0b01, true, false) => (2, Some(64)), // LDPSW
            (0b00, _, true) => (2, None),         // S
            (0b01, _, true) => (3, None),         // D
            (0b10, _, true) => (4, None),         // Q
            _ => return Ok(None),
        };
        let imm = sign_extend(((word >> 15) & 0x7F) as i64, 7) << size;
        let (offset, writeback) = match (word >> 23) & 0x3 {
            0b00 | 0b10 => (imm, None),
            0b01 => (0, Some(imm)),
            _ => (imm, Some(imm)),
        };
        return Ok(Some(LoadStore {
            is_load,
            vector,
            size,
            base: rn,
            rt,
            rt2: Some(((word >> 10) & 0x1F) as u8),
            index: None,
            offset,
            writeback,
            sign_extend_to,
        }));
    }

    // Single register: size 111 V 0 x ...; opc 00 = store, 01 = zero-extending load.
    let unsigned_offset = (word & 0x3B00_0000) == 0x3900_0000;
    if !unsigned_offset && (word & 0x3B00_0000) != 0x3800_0000 {
        return Ok(None);
    }
    let opc = (word >> 22) & 0x3;
    let mut size = ((word >> 30) & 0x3) as u8;
    let sign_extend_to = match (opc, size, vector) {
        (0 | 1, _, _) => None,
        (2, 0..=2, false) => Some(64),
        (3, 0 | 1, false) => Some(32),
        (2 | 3, 0, true) => {
            size = 4; // Q
            None
        }
        _ => return Ok(None), // PRFM and unallocated
    };
    let mut access = LoadStore {
        is_load: opc & 1 == 1 || (!vector && opc != 0),
        vector,
        size,
        base: rn,
        rt,
        rt2: None,
        index: None,
        offset: 0,
        writeback: None,
        sign_extend_to,
    };
    if unsigned_offset {
        access.offset = (((word >> 10) & 0xFFF) as i64) << size;
        return Ok(Some(access));
    }
    if (word >> 21) & 1 == 1 {
        if (word >> 10) & 0x3 != 0b10 {
            return Ok(None); // atomics
        }
        let option = ((word >> 13) & 0x7) as u8;
        if option & 0b010 == 0 {
            return Ok(None); // reserved extend
        }
        let shift = if (word >> 12) & 1 == 1 { size } else { 0 };
        access.index = Some((((word >> 16) & 0x1F) as u8, option, shift));
        return Ok(Some(access));
    }
    let imm = sign_extend(((word >> 12) & 0x1FF) as i64, 9);
    match (word >> 10) & 0x3 {
        0b00 => access.offset = imm,
        0b01 => access.writeback = Some(imm),
        0b11 => {
            access.offset = imm;
            access.writeback = Some(imm);
        }
        _ => return Ok(None), // unprivileged LDTR/STTR
    }
    Ok(Some(access))
}

fn emit_load_store(decoded: LoadStore, state: &mut DecodeState, ops: &mut Vec<Op>) {
    let base = reg_name(decoded.base);
    let mut addr = base.clone();
    if let Some((rm, option, shift)) = decoded.index {
        addr = emit_index_address(&base, rm, option, shift, state, ops);
    } else if decoded.is_load
        && !decoded.vector
        && (decoded.rt == decoded.base || decoded.rt2 == Some(decoded.base))
    {
        // Keep the original base for the second half of the access.
        addr = state.next_temp("addr");
        ops.push(Op::MovI64 {
            dst: addr.clone(),
            src: base.clone(),
        });
    }

    let step = 1_i64 << decoded.size;
    for (i, rt) in [Some(decoded.rt), decoded.rt2]
        .into_iter()
        .flatten()
        .enumerate()
    {
        let offset = decoded.offset + i as i64 * step;
        let addr = addr.clone();
        if decoded.vector {
            let reg = format!("v{rt}");
            let size = 1_u8 << decoded.size;
            ops.push(if decoded.is_load {
                Op::LoadV {
                    dst: reg,
                    addr,
                    offset,
                    size,
                }
            } else {
                Op::StoreV {
                    src: reg,
                    addr,
                    offset,
                    size,
                }
            });
        } else if decoded.is_load {
            // A load into XZR is discarded.
            let dst = if rt == 31 {
                state.next_temp("zr")
            } else {
                reg_name(rt)
            };
            let loaded = dst.clone();
            ops.push(match decoded.size {
                0 => Op::LoadI8 { dst, addr, offset },
                1 => Op::LoadI16 { dst, addr, offset },
                2 => Op::LoadI32 { dst, addr, offset },
                _ => Op::LoadI64 { dst, addr, offset },
            });
            if let Some(width) = decoded.sign_extend_to {
                sign_extend_in_place(&loaded, 8 << decoded.size, state, ops);
                if width == 32 {
                    zero_extend_32(&loaded, state, ops);
                }
            }
        } else {
            let src = if rt == 31 {
                let zero = state.next_temp("imm");
                ops.push(Op::ConstI64 {
                    dst: zero.clone(),
                    imm: 0,
                });
                zero
            } else {
                reg_name(rt)
            };
            ops.push(match decoded.size {
                0 => Op::StoreI8 { src, addr, offset },
                1 => Op::StoreI16 { src, addr, offset },
                2 => Op::StoreI32 { src, addr, offset },
                _ => Op::StoreI64 { src, addr, offset },
            });
        }
    }

    if let Some(amount) = decoded.writeback {
        let imm = state.next_temp("imm");
        ops.push(Op::ConstI64 {
            dst: imm.clone(),
            imm: amount,
        });
        ops.push(Op::AddI64 {
            dst: base.clone(),
            lhs: base,
            rhs: imm,
        });
    }
}

/// addr = base + extend(rm) << shift, for UXTW (010), LSL/UXTX (011), SXTW (110), SXTX (111).
fn emit_index_address(
    base: &str,
    rm: u8,
    option: u8,
    shift: u8,
    state: &mut DecodeState,
    ops: &mut Vec<Op>,
) -> String {
    let index = state.next_temp("addr");
    // rm == 31 is XZR here.
    if rm == 31 {
        ops.push(Op::ConstI64 {
            dst: index.clone(),
            imm: 0,
        });
    } else {
        ops.push(Op::MovI64 {
            dst: index.clone(),
            src: reg_name(rm),
        });
    }
    match option {
        0b010 => zero_extend_32(&index, state, ops),
        0b110 => {
            let k = state.next_temp("imm");
            ops.push(Op::ConstI64 {
                dst: k.clone(),
                imm: 32,
            });
            ops.push(Op::LslI64 {
                dst: index.clone(),
                lhs: index.clone(),
                rhs: k.clone(),
            });
            ops.push(Op::AsrI64 {
                dst: index.clone(),
                lhs: index.clone(),
                rhs: k,
            });
        }
        _ => {}
    }
    if shift > 0 {
        let k = state.next_temp("imm");
        ops.push(Op::ConstI64 {
            dst: k.clone(),
            imm: shift as i64,
        });
        ops.push(Op::LslI64 {
            dst: index.clone(),
            lhs: index.clone(),
            rhs: k,
        });
    }
    ops.push(Op::AddI64 {
        dst: index.clone(),
        lhs: base.to_string(),
        rhs: index.clone(),
    });
    index
}

fn decode_branch_imm(word: u32, pc: u64) -> Option<u64> {
    if (word & 0xFC000000) != 0x14000000 {
        return None;
    }
    let imm26 = (word & 0x03FFFFFF) as i64;
    let offset = sign_extend(imm26 << 2, 28);
    Some((pc as i64 + offset) as u64)
}

fn decode_branch_link(word: u32, pc: u64) -> Option<u64> {
    if (word & 0xFC000000) != 0x94000000 {
        return None;
    }
    let imm26 = (word & 0x03FFFFFF) as i64;
    let offset = sign_extend(imm26 << 2, 28);
    Some((pc as i64 + offset) as u64)
}

fn decode_branch_cond(word: u32, pc: u64) -> Result<Option<(String, u64)>, String> {
    if (word & 0xFF000010) != 0x54000000 {
        return Ok(None);
    }
    let cond = (word & 0xF) as u8;
    let cond_str = cond_code(cond).ok_or_else(|| format!("unsupported condition {cond}"))?;
    let imm19 = ((word >> 5) & 0x7FFFF) as i64;
    let offset = sign_extend(imm19 << 2, 21);
    Ok(Some((cond_str.to_string(), (pc as i64 + offset) as u64)))
}

fn decode_cbz(word: u32, pc: u64) -> Result<Option<(bool, u8, u64)>, String> {
    let top = word & 0x7F000000;
    let is_cbz = top == 0x34000000 || top == 0xB4000000;
    let is_cbnz = top == 0x35000000 || top == 0xB5000000;
    if !is_cbz && !is_cbnz {
        return Ok(None);
    }
    let imm19 = ((word >> 5) & 0x7FFFF) as i64;
    let offset = sign_extend(imm19 << 2, 21);
    let rt = (word & 0x1F) as u8;
    Ok(Some((is_cbnz, rt, (pc as i64 + offset) as u64)))
}

fn decode_tbz(word: u32, pc: u64) -> Result<Option<(bool, u8, u8, u64)>, String> {
    let top = word & 0x7F000000;
    let is_tbz = top == 0x36000000 || top == 0xB6000000;
    let is_tbnz = top == 0x37000000 || top == 0xB7000000;
    if !is_tbz && !is_tbnz {
        return Ok(None);
    }
    let b5 = ((word >> 31) & 0x1) as u8;
    let b40 = ((word >> 19) & 0x1F) as u8;
    let bit = (b5 << 5) | b40;
    let imm14 = ((word >> 5) & 0x3FFF) as i64;
    let offset = sign_extend(imm14 << 2, 16);
    let rt = (word & 0x1F) as u8;
    Ok(Some((is_tbnz, rt, bit, (pc as i64 + offset) as u64)))
}

fn decode_br_register(word: u32) -> Option<u8> {
    if (word & 0xFFFFFC1F) != 0xD61F0000 {
        return None;
    }
    Some(((word >> 5) & 0x1F) as u8)
}

fn is_ret(word: u32) -> bool {
    (word & 0xFFFFFC1F) == 0xD65F0000
}

fn cond_code(code: u8) -> Option<&'static str> {
    match code {
        0x0 => Some("eq"),
        0x1 => Some("ne"),
        0x2 => Some("cs"),
        0x3 => Some("cc"),
        0x4 => Some("mi"),
        0x5 => Some("pl"),
        0x6 => Some("vs"),
        0x7 => Some("vc"),
        0x8 => Some("hi"),
        0x9 => Some("ls"),
        0xA => Some("ge"),
        0xB => Some("lt"),
        0xC => Some("gt"),
        0xD => Some("le"),
        0xE => Some("al"),
        _ => None,
    }
}

fn sign_extend(value: i64, bits: u8) -> i64 {
    let shift = 64 - bits as i64;
    (value << shift) >> shift
}

fn zero_extend_32(dst: &str, state: &mut DecodeState, ops: &mut Vec<Op>) {
    let mask = state.next_temp("imm");
    ops.push(Op::ConstI64 {
        dst: mask.clone(),
        imm: 0xFFFF_FFFF,
    });
    ops.push(Op::AndI64 {
        dst: dst.to_string(),
        lhs: dst.to_string(),
        rhs: mask,
    });
}

fn reg_name(reg: u8) -> String {
    format!("x{reg}")
}

fn block_label(addr: u64) -> String {
    format!("bb_{addr:016x}")
}

fn pop_first(set: &mut BTreeSet<u64>) -> Option<u64> {
    let first = set.iter().next().copied();
    if let Some(value) = first {
        set.remove(&value);
    }
    first
}

fn resolve_segment_path(base_dir: &Path, segment: &ModuleSegment) -> PathBuf {
    let path = Path::new(&segment.output_path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    }
}

fn ensure_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|err| err.to_string())
}

fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map_err(|err| err.to_string())
            .map(|cwd| cwd.join(path))
    }
}

/// Local diagnostic hook: true when the lifter can decode `word` at `pc`.
#[doc(hidden)]
pub fn probe_word(word: u32, pc: u64) -> bool {
    let mut state = DecodeState::new();
    let mut ops = Vec::new();
    let mut warnings = Vec::new();
    decode_one(word, pc, &mut state, &mut ops, &mut warnings).is_ok()
}

#[cfg(test)]
mod tests {
    use super::decode_bit_mask;

    #[test]
    fn bit_mask_matches_assembler_encodings() {
        // (N, imms, immr, is_32) taken from clang-assembled AND/ORR/EOR immediates.
        assert_eq!(decode_bit_mask(0, 3, 0, true), Some(0xF)); // and w4, w1, #0xf
        assert_eq!(
            decode_bit_mask(0, 39, 8, false),
            Some(0xFF00_FF00_FF00_FF00)
        );
        assert_eq!(
            decode_bit_mask(0, 60, 0, false),
            Some(0x5555_5555_5555_5555)
        );
        assert_eq!(decode_bit_mask(1, 31, 0, false), Some(0xFFFF_FFFF));
        assert_eq!(decode_bit_mask(0, 0, 1, true), Some(0x8000_0000)); // tst w1, #0x80000000
        assert_eq!(decode_bit_mask(0, 25, 24, true), Some(0xFFFF_FF03));
        assert_eq!(decode_bit_mask(1, 63, 0, false), None); // all-ones is reserved
        assert_eq!(decode_bit_mask(1, 0, 0, true), None); // N=1 invalid for 32-bit
    }
}
