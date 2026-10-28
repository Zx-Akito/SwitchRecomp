use recomp_pipeline::homebrew::{lift_homebrew, LiftMode, LiftOptions};
use serde_json::Value;
use std::fs;

#[test]
fn homebrew_lift_emits_stub_module() {
    let temp = tempfile::tempdir().expect("tempdir");
    let base_dir = temp.path();
    let segment_dir = base_dir.join("segments/sample");
    fs::create_dir_all(&segment_dir).expect("segment dir");
    let segment_path = segment_dir.join("text.bin");
    fs::write(&segment_path, [0_u8; 16]).expect("segment data");

    let module_json = format!(
        r#"{{
  "schema_version": "1",
  "module_type": "homebrew",
  "modules": [
    {{
      "name": "sample",
      "format": "nro",
      "input_path": "module.nro",
      "input_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "input_size": 16,
      "build_id": "deadbeef",
      "segments": [
        {{
          "name": "text",
          "file_offset": 0,
          "file_size": 16,
          "memory_offset": 0,
          "memory_size": 16,
          "permissions": "r-x",
          "output_path": "{}"
        }}
      ],
      "bss": {{ "size": 0, "memory_offset": 0 }}
    }}
  ]
}}"#,
        segment_path.strip_prefix(base_dir).unwrap().display()
    );

    let module_path = base_dir.join("module.json");
    fs::write(&module_path, module_json).expect("write module.json");

    let out_dir = base_dir.join("lifted");
    let report = lift_homebrew(LiftOptions {
        module_json_path: module_path,
        out_dir: out_dir.clone(),
        entry_name: "entry".to_string(),
        entry_address: None,
        mode: LiftMode::Stub,
    })
    .expect("lift homebrew");

    assert_eq!(report.functions_emitted, 1);
    assert!(!report.warnings.is_empty());

    let lifted_path = out_dir.join("module.json");
    let lifted_src = fs::read_to_string(lifted_path).expect("read lifted module");
    let lifted_json: Value = serde_json::from_str(&lifted_src).expect("parse lifted module");
    assert_eq!(
        lifted_json.get("arch").and_then(|v| v.as_str()),
        Some("aarch64")
    );
    let functions = lifted_json
        .get("functions")
        .and_then(|v| v.as_array())
        .expect("functions");
    assert_eq!(functions.len(), 1);
    assert_eq!(
        functions[0].get("name").and_then(|v| v.as_str()),
        Some("entry")
    );
    let ops = functions[0]
        .get("ops")
        .and_then(|v| v.as_array())
        .expect("ops");
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].get("op").and_then(|v| v.as_str()), Some("ret"));
}

#[test]
fn homebrew_lift_decodes_minimal_block() {
    let temp = tempfile::tempdir().expect("tempdir");
    let base_dir = temp.path();
    let segment_dir = base_dir.join("segments/sample");
    fs::create_dir_all(&segment_dir).expect("segment dir");
    let segment_path = segment_dir.join("text.bin");

    let words = [movz_x(0, 7), movz_x(1, 35), add_reg_x(2, 0, 1), ret_x(30)];
    let mut bytes = Vec::new();
    for word in words {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    fs::write(&segment_path, &bytes).expect("segment data");

    let module_json = format!(
        r#"{{
  "schema_version": "1",
  "module_type": "homebrew",
  "modules": [
    {{
      "name": "sample",
      "format": "nro",
      "input_path": "module.nro",
      "input_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "input_size": {input_size},
      "build_id": "deadbeef",
      "segments": [
        {{
          "name": "text",
          "file_offset": 0,
          "file_size": {file_size},
          "memory_offset": 0,
          "memory_size": {file_size},
          "permissions": "r-x",
          "output_path": "{path}"
        }}
      ],
      "bss": {{ "size": 0, "memory_offset": 0 }}
    }}
  ]
}}"#,
        input_size = bytes.len(),
        file_size = bytes.len(),
        path = segment_path.strip_prefix(base_dir).unwrap().display()
    );

    let module_path = base_dir.join("module.json");
    fs::write(&module_path, module_json).expect("write module.json");

    let out_dir = base_dir.join("lifted");
    let report = lift_homebrew(LiftOptions {
        module_json_path: module_path,
        out_dir: out_dir.clone(),
        entry_name: "entry".to_string(),
        entry_address: None,
        mode: LiftMode::Decode,
    })
    .expect("lift homebrew decode");

    assert_eq!(report.functions_emitted, 1);

    let lifted_path = out_dir.join("module.json");
    let lifted_src = fs::read_to_string(lifted_path).expect("read lifted module");
    let lifted_json: Value = serde_json::from_str(&lifted_src).expect("parse lifted module");
    let functions = lifted_json
        .get("functions")
        .and_then(|v| v.as_array())
        .expect("functions");
    let blocks = functions[0]
        .get("blocks")
        .and_then(|v| v.as_array())
        .expect("blocks");
    assert_eq!(blocks.len(), 1);
    let ops = blocks[0]
        .get("ops")
        .and_then(|v| v.as_array())
        .expect("block ops");
    assert!(ops.len() >= 3);
    assert_eq!(ops[0].get("op").and_then(|v| v.as_str()), Some("const_i64"));
    assert_eq!(ops[1].get("op").and_then(|v| v.as_str()), Some("const_i64"));
    assert_eq!(ops[2].get("op").and_then(|v| v.as_str()), Some("add_i64"));
    let terminator = blocks[0]
        .get("terminator")
        .and_then(|v| v.get("op"))
        .and_then(|v| v.as_str());
    assert_eq!(terminator, Some("ret"));
}

fn movz_x(dst: u8, imm: u16) -> u32 {
    let hw = 0_u32;
    let sf = 1_u32;
    let opc = 0b10_u32;
    let fixed = 0b100101_u32;
    (sf << 31) | (opc << 29) | (fixed << 23) | (hw << 21) | ((imm as u32) << 5) | (dst as u32)
}

fn add_reg_x(dst: u8, lhs: u8, rhs: u8) -> u32 {
    let sf = 1_u32;
    let op = 0_u32;
    let s = 0_u32;
    let opcode = 0b01011_u32;
    let shift = 0_u32;
    let imm6 = 0_u32;
    (sf << 31)
        | (op << 30)
        | (s << 29)
        | (opcode << 24)
        | (shift << 22)
        | (imm6 << 10)
        | ((rhs as u32) << 16)
        | ((lhs as u32) << 5)
        | (dst as u32)
}

fn ret_x(reg: u8) -> u32 {
    0xD65F0000 | ((reg as u32) << 5)
}

#[test]
fn homebrew_lift_discovers_call_targets() {
    let temp = tempfile::tempdir().expect("tempdir");
    let base_dir = temp.path();
    let segment_dir = base_dir.join("segments/sample");
    fs::create_dir_all(&segment_dir).expect("segment dir");
    let segment_path = segment_dir.join("text.bin");

    let mut bytes = vec![0_u8; 0x24];
    let bl = bl_to(0x0, 0x20);
    bytes[0..4].copy_from_slice(&bl.to_le_bytes());
    let ret0 = ret_x(30);
    bytes[4..8].copy_from_slice(&ret0.to_le_bytes());
    let ret1 = ret_x(30);
    bytes[0x20..0x24].copy_from_slice(&ret1.to_le_bytes());
    fs::write(&segment_path, &bytes).expect("segment data");

    let module_json = format!(
        r#"{{
  "schema_version": "1",
  "module_type": "homebrew",
  "modules": [
    {{
      "name": "sample",
      "format": "nro",
      "input_path": "module.nro",
      "input_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "input_size": {input_size},
      "build_id": "deadbeef",
      "segments": [
        {{
          "name": "text",
          "file_offset": 0,
          "file_size": {file_size},
          "memory_offset": 0,
          "memory_size": {file_size},
          "permissions": "r-x",
          "output_path": "{path}"
        }}
      ],
      "bss": {{ "size": 0, "memory_offset": 0 }}
    }}
  ]
}}"#,
        input_size = bytes.len(),
        file_size = bytes.len(),
        path = segment_path.strip_prefix(base_dir).unwrap().display()
    );

    let module_path = base_dir.join("module.json");
    fs::write(&module_path, module_json).expect("write module.json");

    let out_dir = base_dir.join("lifted");
    let report = lift_homebrew(LiftOptions {
        module_json_path: module_path,
        out_dir: out_dir.clone(),
        entry_name: "entry".to_string(),
        entry_address: None,
        mode: LiftMode::Decode,
    })
    .expect("lift homebrew decode");

    assert_eq!(report.functions_emitted, 2);

    let lifted_path = out_dir.join("module.json");
    let lifted_src = fs::read_to_string(lifted_path).expect("read lifted module");
    let lifted_json: Value = serde_json::from_str(&lifted_src).expect("parse lifted module");
    let functions = lifted_json
        .get("functions")
        .and_then(|v| v.as_array())
        .expect("functions");
    assert_eq!(functions.len(), 2);
    let names: Vec<_> = functions
        .iter()
        .filter_map(|func| func.get("name").and_then(|v| v.as_str()))
        .collect();
    assert!(names.contains(&"entry"));
    assert!(names.contains(&"fn_0000000000000020"));
}

#[test]
fn homebrew_lift_builds_conditional_blocks() {
    let temp = tempfile::tempdir().expect("tempdir");
    let base_dir = temp.path();
    let segment_dir = base_dir.join("segments/sample");
    fs::create_dir_all(&segment_dir).expect("segment dir");
    let segment_path = segment_dir.join("text.bin");

    let cbz = cbz_to(0x0, 0x8, 0);
    let ret = ret_x(30);
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&cbz.to_le_bytes());
    bytes.extend_from_slice(&ret.to_le_bytes());
    bytes.extend_from_slice(&ret.to_le_bytes());
    fs::write(&segment_path, &bytes).expect("segment data");

    let module_json = format!(
        r#"{{
  "schema_version": "1",
  "module_type": "homebrew",
  "modules": [
    {{
      "name": "sample",
      "format": "nro",
      "input_path": "module.nro",
      "input_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "input_size": {input_size},
      "build_id": "deadbeef",
      "segments": [
        {{
          "name": "text",
          "file_offset": 0,
          "file_size": {file_size},
          "memory_offset": 0,
          "memory_size": {file_size},
          "permissions": "r-x",
          "output_path": "{path}"
        }}
      ],
      "bss": {{ "size": 0, "memory_offset": 0 }}
    }}
  ]
}}"#,
        input_size = bytes.len(),
        file_size = bytes.len(),
        path = segment_path.strip_prefix(base_dir).unwrap().display()
    );

    let module_path = base_dir.join("module.json");
    fs::write(&module_path, module_json).expect("write module.json");

    let out_dir = base_dir.join("lifted");
    let report = lift_homebrew(LiftOptions {
        module_json_path: module_path,
        out_dir: out_dir.clone(),
        entry_name: "entry".to_string(),
        entry_address: None,
        mode: LiftMode::Decode,
    })
    .expect("lift homebrew decode");

    assert_eq!(report.functions_emitted, 1);

    let lifted_path = out_dir.join("module.json");
    let lifted_src = fs::read_to_string(lifted_path).expect("read lifted module");
    let lifted_json: Value = serde_json::from_str(&lifted_src).expect("parse lifted module");
    let functions = lifted_json
        .get("functions")
        .and_then(|v| v.as_array())
        .expect("functions");
    let blocks = functions[0]
        .get("blocks")
        .and_then(|v| v.as_array())
        .expect("blocks");
    assert!(blocks.len() >= 2);
    let first_term = blocks[0]
        .get("terminator")
        .and_then(|v| v.get("op"))
        .and_then(|v| v.as_str());
    assert_eq!(first_term, Some("br_cond"));
}

#[test]
fn homebrew_lift_handles_indirect_branch() {
    let temp = tempfile::tempdir().expect("tempdir");
    let base_dir = temp.path();
    let segment_dir = base_dir.join("segments/sample");
    fs::create_dir_all(&segment_dir).expect("segment dir");
    let segment_path = segment_dir.join("text.bin");

    let br = br_x(1);
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&br.to_le_bytes());
    fs::write(&segment_path, &bytes).expect("segment data");

    let module_json = format!(
        r#"{{
  "schema_version": "1",
  "module_type": "homebrew",
  "modules": [
    {{
      "name": "sample",
      "format": "nro",
      "input_path": "module.nro",
      "input_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "input_size": {input_size},
      "build_id": "deadbeef",
      "segments": [
        {{
          "name": "text",
          "file_offset": 0,
          "file_size": {file_size},
          "memory_offset": 0,
          "memory_size": {file_size},
          "permissions": "r-x",
          "output_path": "{path}"
        }}
      ],
      "bss": {{ "size": 0, "memory_offset": 0 }}
    }}
  ]
}}"#,
        input_size = bytes.len(),
        file_size = bytes.len(),
        path = segment_path.strip_prefix(base_dir).unwrap().display()
    );

    let module_path = base_dir.join("module.json");
    fs::write(&module_path, module_json).expect("write module.json");

    let out_dir = base_dir.join("lifted");
    lift_homebrew(LiftOptions {
        module_json_path: module_path,
        out_dir: out_dir.clone(),
        entry_name: "entry".to_string(),
        entry_address: None,
        mode: LiftMode::Decode,
    })
    .expect("lift homebrew decode");

    let lifted_path = out_dir.join("module.json");
    let lifted_src = fs::read_to_string(lifted_path).expect("read lifted module");
    let lifted_json: Value = serde_json::from_str(&lifted_src).expect("parse lifted module");
    let functions = lifted_json
        .get("functions")
        .and_then(|v| v.as_array())
        .expect("functions");
    let blocks = functions[0]
        .get("blocks")
        .and_then(|v| v.as_array())
        .expect("blocks");
    let term = blocks[0]
        .get("terminator")
        .and_then(|v| v.get("op"))
        .and_then(|v| v.as_str());
    assert_eq!(term, Some("br_indirect"));
}

#[test]
fn homebrew_lift_decodes_load_store_ops() {
    let temp = tempfile::tempdir().expect("tempdir");
    let base_dir = temp.path();
    let segment_dir = base_dir.join("segments/sample");
    fs::create_dir_all(&segment_dir).expect("segment dir");
    let segment_path = segment_dir.join("text.bin");

    let ldr = ldr_x_imm(0, 1, 16);
    let str = str_x_imm(0, 1, 16);
    let ret = ret_x(30);
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&ldr.to_le_bytes());
    bytes.extend_from_slice(&str.to_le_bytes());
    bytes.extend_from_slice(&ret.to_le_bytes());
    fs::write(&segment_path, &bytes).expect("segment data");

    let module_json = format!(
        r#"{{
  "schema_version": "1",
  "module_type": "homebrew",
  "modules": [
    {{
      "name": "sample",
      "format": "nro",
      "input_path": "module.nro",
      "input_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "input_size": {input_size},
      "build_id": "deadbeef",
      "segments": [
        {{
          "name": "text",
          "file_offset": 0,
          "file_size": {file_size},
          "memory_offset": 0,
          "memory_size": {file_size},
          "permissions": "r-x",
          "output_path": "{path}"
        }}
      ],
      "bss": {{ "size": 0, "memory_offset": 0 }}
    }}
  ]
}}"#,
        input_size = bytes.len(),
        file_size = bytes.len(),
        path = segment_path.strip_prefix(base_dir).unwrap().display()
    );

    let module_path = base_dir.join("module.json");
    fs::write(&module_path, module_json).expect("write module.json");

    let out_dir = base_dir.join("lifted");
    lift_homebrew(LiftOptions {
        module_json_path: module_path,
        out_dir: out_dir.clone(),
        entry_name: "entry".to_string(),
        entry_address: None,
        mode: LiftMode::Decode,
    })
    .expect("lift homebrew decode");

    let lifted_path = out_dir.join("module.json");
    let lifted_src = fs::read_to_string(lifted_path).expect("read lifted module");
    let lifted_json: Value = serde_json::from_str(&lifted_src).expect("parse lifted module");
    let functions = lifted_json
        .get("functions")
        .and_then(|v| v.as_array())
        .expect("functions");
    let ops = functions[0]
        .get("blocks")
        .and_then(|v| v.as_array())
        .expect("blocks")[0]
        .get("ops")
        .and_then(|v| v.as_array())
        .expect("ops");
    assert_eq!(ops[0].get("op").and_then(|v| v.as_str()), Some("load_i64"));
    assert_eq!(ops[1].get("op").and_then(|v| v.as_str()), Some("store_i64"));
}

#[test]
fn homebrew_lift_rejects_oversized_block() {
    let temp = tempfile::tempdir().expect("tempdir");
    let base_dir = temp.path();
    let segment_dir = base_dir.join("segments/sample");
    fs::create_dir_all(&segment_dir).expect("segment dir");
    let segment_path = segment_dir.join("text.bin");

    let nop = 0xD503_201F_u32;
    let mut bytes = Vec::new();
    for _ in 0..10_001 {
        bytes.extend_from_slice(&nop.to_le_bytes());
    }
    fs::write(&segment_path, &bytes).expect("segment data");

    let module_json = format!(
        r#"{{
  "schema_version": "1",
  "module_type": "homebrew",
  "modules": [
    {{
      "name": "sample",
      "format": "nro",
      "input_path": "module.nro",
      "input_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "input_size": {input_size},
      "build_id": "deadbeef",
      "segments": [
        {{
          "name": "text",
          "file_offset": 0,
          "file_size": {file_size},
          "memory_offset": 0,
          "memory_size": {file_size},
          "permissions": "r-x",
          "output_path": "{path}"
        }}
      ],
      "bss": {{ "size": 0, "memory_offset": 0 }}
    }}
  ]
}}"#,
        input_size = bytes.len(),
        file_size = bytes.len(),
        path = segment_path.strip_prefix(base_dir).unwrap().display()
    );

    let module_path = base_dir.join("module.json");
    fs::write(&module_path, module_json).expect("write module.json");

    let out_dir = base_dir.join("lifted");
    let err = lift_homebrew(LiftOptions {
        module_json_path: module_path,
        out_dir: out_dir.clone(),
        entry_name: "entry".to_string(),
        entry_address: None,
        mode: LiftMode::Decode,
    })
    .expect_err("expected decode failure");

    assert!(err.contains("block decode limit exceeded"));
}

fn bl_to(from: u64, target: u64) -> u32 {
    let delta = target as i64 - from as i64;
    let imm26 = (delta >> 2) & 0x03FF_FFFF;
    0x9400_0000 | (imm26 as u32)
}

fn cbz_to(from: u64, target: u64, reg: u8) -> u32 {
    let delta = target as i64 - from as i64;
    let imm19 = (delta >> 2) & 0x7FFFF;
    0xB400_0000 | ((imm19 as u32) << 5) | (reg as u32)
}

fn br_x(reg: u8) -> u32 {
    0xD61F_0000 | ((reg as u32) << 5)
}

fn ldr_x_imm(rt: u8, rn: u8, offset: u64) -> u32 {
    let size = 3_u32;
    let imm12 = (offset >> size) as u32;
    0x3900_0000 | (size << 30) | (1 << 22) | (imm12 << 10) | ((rn as u32) << 5) | (rt as u32)
}

fn str_x_imm(rt: u8, rn: u8, offset: u64) -> u32 {
    let size = 3_u32;
    let imm12 = (offset >> size) as u32;
    0x3900_0000 | (size << 30) | (imm12 << 10) | ((rn as u32) << 5) | (rt as u32)
}

#[test]
fn homebrew_lift_decodes_pairs_and_indexed_addressing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let base_dir = temp.path();
    let segment_dir = base_dir.join("segments/sample");
    fs::create_dir_all(&segment_dir).expect("segment dir");
    let segment_path = segment_dir.join("text.bin");

    let words = [
        0xA9BF_7BFD_u32, // stp x29, x30, [sp, #-16]!
        0xF862_7820,     // ldr x0, [x1, x2, lsl #3]
        0xA8C1_7BFD,     // ldp x29, x30, [sp], #16
        0x3C10_0C00,     // str b0, [x0, #-256]!
        0x1E22_2820,     // fadd s0, s1, s2 (not supported yet)
    ];
    let mut bytes = Vec::new();
    for word in &words[..4] {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    bytes.extend_from_slice(&ret_x(30).to_le_bytes());
    fs::write(&segment_path, &bytes).expect("segment data");

    let module_json = format!(
        r#"{{
  "schema_version": "1",
  "module_type": "homebrew",
  "modules": [
    {{
      "name": "sample",
      "format": "nro",
      "input_path": "module.nro",
      "input_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "input_size": {size},
      "build_id": "deadbeef",
      "segments": [
        {{
          "name": "text",
          "file_offset": 0,
          "file_size": {size},
          "memory_offset": 0,
          "memory_size": {size},
          "permissions": "r-x",
          "output_path": "{path}"
        }}
      ],
      "bss": {{ "size": 0, "memory_offset": 0 }}
    }}
  ]
}}"#,
        size = bytes.len(),
        path = segment_path.strip_prefix(base_dir).unwrap().display()
    );
    let module_path = base_dir.join("module.json");
    fs::write(&module_path, module_json).expect("write module.json");

    let out_dir = base_dir.join("lifted");
    lift_homebrew(LiftOptions {
        module_json_path: module_path,
        out_dir: out_dir.clone(),
        entry_name: "entry".to_string(),
        entry_address: None,
        mode: LiftMode::Decode,
    })
    .expect("lift homebrew decode");

    let lifted: Value =
        serde_json::from_str(&fs::read_to_string(out_dir.join("module.json")).unwrap()).unwrap();
    let ops: Vec<String> = lifted["functions"][0]["blocks"][0]["ops"]
        .as_array()
        .expect("ops")
        .iter()
        .map(|op| op["op"].as_str().unwrap().to_string())
        .collect();
    let expected = [
        // stp pre-index: two stores at sp-16 / sp-8, then sp -= 16
        "store_i64",
        "store_i64",
        "const_i64",
        "add_i64",
        // ldr register offset: addr = x1 + (x2 << 3)
        "mov_i64",
        "const_i64",
        "lsl_i64",
        "add_i64",
        "load_i64",
        // ldp post-index: two loads at sp / sp+8, then sp += 16
        "load_i64",
        "load_i64",
        "const_i64",
        "add_i64",
        // str b0 pre-index: SIMD&FP store of one byte at x0-256, then x0 -= 256
        "store_v",
        "const_i64",
        "add_i64",
    ];
    assert_eq!(ops, expected);
    assert_eq!(lifted["functions"][0]["blocks"][0]["ops"][0]["offset"], -16);
    assert_eq!(lifted["functions"][0]["blocks"][0]["ops"][1]["offset"], -8);
    let store_v = &lifted["functions"][0]["blocks"][0]["ops"][13];
    assert_eq!(
        (store_v["src"].as_str(), store_v["size"].as_u64()),
        (Some("v0"), Some(1))
    );
    assert_eq!(store_v["offset"], -256);

    let simd_dir = base_dir.join("simd");
    fs::create_dir_all(simd_dir.join("segments/sample")).unwrap();
    fs::write(
        simd_dir.join("segments/sample/text.bin"),
        [words[4].to_le_bytes(), ret_x(30).to_le_bytes()].concat(),
    )
    .unwrap();
    fs::write(
        simd_dir.join("module.json"),
        fs::read_to_string(base_dir.join("module.json"))
            .unwrap()
            .replace(
                &format!("\"input_size\": {}", bytes.len()),
                "\"input_size\": 8",
            )
            .replace(
                &format!("\"file_size\": {}", bytes.len()),
                "\"file_size\": 8",
            )
            .replace(
                &format!("\"memory_size\": {}", bytes.len()),
                "\"memory_size\": 8",
            ),
    )
    .unwrap();
    let simd = lift_homebrew(LiftOptions {
        module_json_path: simd_dir.join("module.json"),
        out_dir: simd_dir.join("lifted"),
        entry_name: "entry".to_string(),
        entry_address: None,
        mode: LiftMode::Decode,
    });
    assert!(
        simd.is_err(),
        "unsupported instruction must fail a full decode"
    );

    lift_homebrew(LiftOptions {
        module_json_path: simd_dir.join("module.json"),
        out_dir: simd_dir.join("partial"),
        entry_name: "entry".to_string(),
        entry_address: None,
        mode: LiftMode::Partial,
    })
    .expect("partial lift tolerates unsupported instructions");
    let partial: Value =
        serde_json::from_str(&fs::read_to_string(simd_dir.join("partial/module.json")).unwrap())
            .unwrap();
    let terminator = &partial["functions"][0]["blocks"][0]["terminator"];
    assert_eq!(terminator["op"], "trap");
    assert_eq!(terminator["word"], words[4]);
}
