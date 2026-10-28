//! printf-family formatting for HLE: format strings read from guest memory, arguments
//! taken from registers/stack (variadic calls) or from an AAPCS64 `va_list`.

use crate::{mem_load_u64, mem_load_u8, Cpu, RuntimeResult};

/// Source of variadic arguments.
pub(crate) trait Args {
    fn gp(&mut self) -> RuntimeResult<u64>;
    fn fp(&mut self) -> RuntimeResult<f64>;
}

/// Arguments of a variadic call: general registers from `first_gp`, then the stack;
/// floating-point arguments in v0-v7, then the stack.
pub(crate) struct RegisterArgs<'a> {
    ctx: &'a Cpu,
    gp: usize,
    fp: usize,
    stack: u64,
}

impl<'a> RegisterArgs<'a> {
    pub(crate) fn new(ctx: &'a Cpu, first_gp: usize) -> Self {
        Self {
            ctx,
            gp: first_gp,
            fp: 0,
            stack: ctx.x[31] as u64,
        }
    }

    fn from_stack(&mut self) -> RuntimeResult<u64> {
        let value = mem_load_u64(self.stack)?;
        self.stack += 8;
        Ok(value)
    }
}

impl Args for RegisterArgs<'_> {
    fn gp(&mut self) -> RuntimeResult<u64> {
        if self.gp < 8 {
            self.gp += 1;
            Ok(self.ctx.x[self.gp - 1] as u64)
        } else {
            self.from_stack()
        }
    }

    fn fp(&mut self) -> RuntimeResult<f64> {
        if self.fp < 8 {
            self.fp += 1;
            Ok(f64::from_bits(self.ctx.simd[self.fp - 1] as u64))
        } else {
            Ok(f64::from_bits(self.from_stack()?))
        }
    }
}

/// AAPCS64 va_list: { void* stack; void* gr_top; void* vr_top; int gr_offs; int vr_offs }.
pub(crate) struct VaList {
    stack: u64,
    gr_top: u64,
    vr_top: u64,
    gr_offs: i32,
    vr_offs: i32,
}

impl VaList {
    pub(crate) fn read(address: u64) -> RuntimeResult<Self> {
        let offs = mem_load_u64(address + 24)?;
        Ok(Self {
            stack: mem_load_u64(address)?,
            gr_top: mem_load_u64(address + 8)?,
            vr_top: mem_load_u64(address + 16)?,
            gr_offs: offs as u32 as i32,
            vr_offs: (offs >> 32) as u32 as i32,
        })
    }

    fn from_stack(&mut self) -> RuntimeResult<u64> {
        let value = mem_load_u64(self.stack)?;
        self.stack += 8;
        Ok(value)
    }
}

impl Args for VaList {
    fn gp(&mut self) -> RuntimeResult<u64> {
        if self.gr_offs < 0 {
            let value = mem_load_u64(self.gr_top.wrapping_add(self.gr_offs as i64 as u64))?;
            self.gr_offs += 8;
            Ok(value)
        } else {
            self.from_stack()
        }
    }

    fn fp(&mut self) -> RuntimeResult<f64> {
        if self.vr_offs < 0 {
            let value = mem_load_u64(self.vr_top.wrapping_add(self.vr_offs as i64 as u64))?;
            self.vr_offs += 16;
            Ok(f64::from_bits(value))
        } else {
            Ok(f64::from_bits(self.from_stack()?))
        }
    }
}

fn read_cstr(address: u64) -> RuntimeResult<Vec<u8>> {
    let mut bytes = Vec::new();
    if address == 0 {
        return Ok(b"(null)".to_vec());
    }
    loop {
        let byte = mem_load_u8(address + bytes.len() as u64)? as u8;
        if byte == 0 {
            return Ok(bytes);
        }
        bytes.push(byte);
    }
}

fn pad(body: Vec<u8>, width: usize, left: bool, zero: bool) -> Vec<u8> {
    if body.len() >= width {
        return body;
    }
    let fill = width - body.len();
    if left {
        let mut out = body;
        out.resize(width, b' ');
        out
    } else if zero {
        // Zero padding goes after a sign or 0x prefix.
        let prefix = body
            .iter()
            .take_while(|b| matches!(b, b'-' | b'+' | b' '))
            .count()
            + if body.get(1).is_some_and(|b| *b == b'x' || *b == b'X') && body[0] == b'0' {
                2
            } else {
                0
            };
        let mut out = body[..prefix].to_vec();
        out.extend(std::iter::repeat_n(b'0', fill));
        out.extend_from_slice(&body[prefix..]);
        out
    } else {
        let mut out = vec![b' '; fill];
        out.extend(body);
        out
    }
}

/// Formats `format` (guest address) with `args`, returning the bytes (no terminator).
pub(crate) fn format(format: u64, args: &mut dyn Args) -> RuntimeResult<Vec<u8>> {
    let fmt = read_cstr(format)?;
    let mut out = Vec::new();
    let mut i = 0;
    while i < fmt.len() {
        if fmt[i] != b'%' {
            out.push(fmt[i]);
            i += 1;
            continue;
        }
        i += 1;
        let (mut left, mut plus, mut space, mut zero, mut alt) =
            (false, false, false, false, false);
        while let Some(&c) = fmt.get(i) {
            match c {
                b'-' => left = true,
                b'+' => plus = true,
                b' ' => space = true,
                b'0' => zero = true,
                b'#' => alt = true,
                _ => break,
            }
            i += 1;
        }
        let mut width = 0_usize;
        if fmt.get(i) == Some(&b'*') {
            let w = args.gp()? as i32;
            left |= w < 0;
            width = w.unsigned_abs() as usize;
            i += 1;
        }
        while let Some(d) = fmt.get(i).filter(|c| c.is_ascii_digit()) {
            width = width * 10 + (d - b'0') as usize;
            i += 1;
        }
        let mut precision = None;
        if fmt.get(i) == Some(&b'.') {
            i += 1;
            let mut p = 0_usize;
            if fmt.get(i) == Some(&b'*') {
                p = (args.gp()? as i32).max(0) as usize;
                i += 1;
            }
            while let Some(d) = fmt.get(i).filter(|c| c.is_ascii_digit()) {
                p = p * 10 + (d - b'0') as usize;
                i += 1;
            }
            precision = Some(p);
        }
        // Length modifiers: only "hh"/"h" narrow; everything else is a 64-bit slot.
        let mut bits = 32;
        while let Some(&c) = fmt.get(i) {
            match c {
                b'h' => bits = if bits == 16 { 8 } else { 16 },
                b'l' | b'z' | b'j' | b't' | b'q' | b'L' => bits = 64,
                _ => break,
            }
            i += 1;
        }
        let Some(&conv) = fmt.get(i) else { break };
        i += 1;
        let narrow = |v: u64| {
            if bits == 64 {
                v
            } else {
                v & ((1_u64 << bits) - 1)
            }
        };
        let signed = |v: u64| {
            if bits == 64 {
                v as i64
            } else {
                ((v << (64 - bits)) as i64) >> (64 - bits)
            }
        };
        let body: Vec<u8> = match conv {
            b'd' | b'i' => {
                let v = signed(args.gp()?);
                let sign = if v < 0 {
                    "-"
                } else if plus {
                    "+"
                } else if space {
                    " "
                } else {
                    ""
                };
                let mut digits = v.unsigned_abs().to_string();
                if let Some(p) = precision {
                    digits = format!("{digits:0>p$}");
                }
                format!("{sign}{digits}").into_bytes()
            }
            b'u' | b'x' | b'X' | b'o' => {
                let v = narrow(args.gp()?);
                let mut digits = match conv {
                    b'u' => v.to_string(),
                    b'x' => format!("{v:x}"),
                    b'X' => format!("{v:X}"),
                    _ => format!("{v:o}"),
                };
                if let Some(p) = precision {
                    digits = format!("{digits:0>p$}");
                }
                let prefix = match (alt && v != 0, conv) {
                    (true, b'x') => "0x",
                    (true, b'X') => "0X",
                    (true, b'o') => "0",
                    _ => "",
                };
                format!("{prefix}{digits}").into_bytes()
            }
            b'p' => format!("0x{:x}", args.gp()?).into_bytes(),
            b'c' => vec![args.gp()? as u8],
            b's' => {
                let mut s = read_cstr(args.gp()?)?;
                if let Some(p) = precision {
                    s.truncate(p);
                }
                s
            }
            b'f' | b'F' | b'e' | b'E' | b'g' | b'G' => {
                let v = args.fp()?;
                let p = precision.unwrap_or(6);
                let sign = if v.is_sign_negative() {
                    ""
                } else if plus {
                    "+"
                } else if space {
                    " "
                } else {
                    ""
                };
                let text = match conv {
                    b'f' | b'F' => format!("{v:.p$}"),
                    b'e' | b'E' => format_exp(v, p, conv == b'E'),
                    _ => format_general(v, p.max(1), alt),
                };
                format!("{sign}{text}").into_bytes()
            }
            b'%' => b"%".to_vec(),
            b'n' => {
                args.gp()?; // writing the count back is not supported
                Vec::new()
            }
            other => vec![b'%', other],
        };
        // C ignores the 0 flag with '-' and, for integers, when a precision is given.
        let float = matches!(conv, b'f' | b'F' | b'e' | b'E' | b'g' | b'G');
        let zero_pad = zero && !left && (precision.is_none() || float);
        out.extend(pad(body, width, left, zero_pad));
    }
    Ok(out)
}

/// %e: d.ddde±XX with C's two-digit minimum exponent.
fn format_exp(v: f64, precision: usize, upper: bool) -> String {
    let text = format!("{v:.precision$e}");
    let (mantissa, exp) = text.split_once('e').unwrap_or((&text, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let e = if upper { 'E' } else { 'e' };
    let sign = if exp < 0 { '-' } else { '+' };
    format!("{mantissa}{e}{sign}{:02}", exp.abs())
}

/// %g: %e or %f with `precision` significant digits, trailing zeros removed unless `#`.
fn format_general(v: f64, precision: usize, alt: bool) -> String {
    if v == 0.0 {
        return "0".to_string();
    }
    let exp = v.abs().log10().floor() as i32;
    let text = if exp < -4 || exp >= precision as i32 {
        format_exp(v, precision - 1, false)
    } else {
        let decimals = (precision as i32 - 1 - exp).max(0) as usize;
        format!("{v:.decimals$}")
    };
    if alt || !text.contains('.') {
        return text;
    }
    let (number, exponent) = match text.find('e') {
        Some(at) => (&text[..at], &text[at..]),
        None => (text.as_str(), ""),
    };
    let number = number.trim_end_matches('0').trim_end_matches('.');
    format!("{number}{exponent}")
}
