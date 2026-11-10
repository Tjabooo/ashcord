//! Erlang External Term Format, the binary gateway encoding the Discord desktop client uses.

use crate::val::Val;
use anyhow::{Result, bail};
use std::io::Read;

const VERSION: u8 = 131;
const NEW_FLOAT: u8 = 70;
const COMPRESSED: u8 = 80;
const SMALL_INTEGER: u8 = 97;
const INTEGER: u8 = 98;
const FLOAT: u8 = 99;
const ATOM: u8 = 100;
const SMALL_TUPLE: u8 = 104;
const LARGE_TUPLE: u8 = 105;
const NIL: u8 = 106;
const STRING: u8 = 107;
const LIST: u8 = 108;
const BINARY: u8 = 109;
const SMALL_BIG: u8 = 110;
const LARGE_BIG: u8 = 111;
const SMALL_ATOM: u8 = 115;
const MAP: u8 = 116;
const ATOM_UTF8: u8 = 118;
const SMALL_ATOM_UTF8: u8 = 119;

pub fn decode(buf: &[u8]) -> Result<Val> {
    let mut r = Reader { buf, pos: 0 };
    if r.u8()? != VERSION {
        bail!("not an ETF payload");
    }
    let v = r.term(0)?;
    if r.pos != buf.len() {
        bail!("trailing bytes after ETF term");
    }
    Ok(v)
}

pub fn encode(v: &Val) -> Vec<u8> {
    let mut out = vec![VERSION];
    write(v, &mut out);
    out
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.buf.len() {
            bail!("ETF payload cut short");
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<usize> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]) as usize)
    }
    fn u32(&mut self) -> Result<usize> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
    }

    fn atom(&mut self, len: usize) -> Result<Val> {
        let s = String::from_utf8_lossy(self.take(len)?).into_owned();
        Ok(match s.as_str() {
            "nil" | "null" => Val::Null,
            "true" => Val::Bool(true),
            "false" => Val::Bool(false),
            _ => Val::Atom(s),
        })
    }

    fn big(&mut self, n: usize) -> Result<Val> {
        let sign = self.u8()?;
        let digits = self.take(n)?;
        if n > 16 {
            bail!("ETF integer too large");
        }
        let mut v: u128 = 0;
        for (i, d) in digits.iter().enumerate() {
            v |= (*d as u128) << (8 * i);
        }
        let v = i128::try_from(v)?;
        let v = if sign == 0 { v } else { -v };
        Ok(match i64::try_from(v) {
            Ok(n) => Val::Int(n),
            Err(_) => Val::Big(v),
        })
    }

    fn term(&mut self, depth: usize) -> Result<Val> {
        if depth > 256 {
            bail!("ETF nesting too deep");
        }
        let tag = self.u8()?;
        Ok(match tag {
            SMALL_INTEGER => Val::Int(self.u8()? as i64),
            INTEGER => {
                let b = self.take(4)?;
                Val::Int(i32::from_be_bytes([b[0], b[1], b[2], b[3]]) as i64)
            }
            NEW_FLOAT => {
                let b = self.take(8)?;
                Val::Float(f64::from_be_bytes(b.try_into()?))
            }
            FLOAT => {
                let s = String::from_utf8_lossy(self.take(31)?)
                    .trim_end_matches('\0')
                    .to_string();
                Val::Float(s.trim().parse()?)
            }
            ATOM | ATOM_UTF8 => {
                let n = self.u16()?;
                self.atom(n)?
            }
            SMALL_ATOM | SMALL_ATOM_UTF8 => {
                let n = self.u8()? as usize;
                self.atom(n)?
            }
            SMALL_TUPLE | LARGE_TUPLE => {
                let n = if tag == SMALL_TUPLE {
                    self.u8()? as usize
                } else {
                    self.u32()?
                };
                let mut v = Vec::with_capacity(n.min(4096));
                for _ in 0..n {
                    v.push(self.term(depth + 1)?);
                }
                Val::Tuple(v)
            }
            NIL => Val::List(Vec::new()),
            STRING => {
                let n = self.u16()?;
                Val::Charlist(self.take(n)?.to_vec())
            }
            LIST => {
                let n = self.u32()?;
                let mut v = Vec::with_capacity(n.min(4096));
                for _ in 0..n {
                    v.push(self.term(depth + 1)?);
                }
                match self.term(depth + 1)? {
                    Val::List(t) if t.is_empty() => {}
                    _ => bail!("improper ETF list"),
                }
                Val::List(v)
            }
            BINARY => {
                let n = self.u32()?;
                let b = self.take(n)?;
                match std::str::from_utf8(b) {
                    Ok(s) => Val::Str(s.to_string()),
                    Err(_) => Val::Bin(b.to_vec()),
                }
            }
            SMALL_BIG => {
                let n = self.u8()? as usize;
                self.big(n)?
            }
            LARGE_BIG => {
                let n = self.u32()?;
                self.big(n)?
            }
            MAP => {
                let n = self.u32()?;
                let mut m = Vec::with_capacity(n.min(4096));
                for _ in 0..n {
                    let k = self.term(depth + 1)?;
                    let v = self.term(depth + 1)?;
                    m.push((k, v));
                }
                Val::Map(m)
            }
            COMPRESSED => {
                let size = self.u32()?;
                let rest = &self.buf[self.pos..];
                let mut inner = Vec::with_capacity(size.min(64 << 20));
                let mut z = flate2::read::ZlibDecoder::new(rest);
                z.read_to_end(&mut inner)?;
                self.pos = self.buf.len();
                let mut r = Reader {
                    buf: &inner,
                    pos: 0,
                };
                r.term(depth + 1)?
            }
            other => bail!("unsupported ETF tag {other}"),
        })
    }
}

fn atom(s: &str, out: &mut Vec<u8>) {
    let b = s.as_bytes();
    if b.len() < 256 {
        out.push(SMALL_ATOM_UTF8);
        out.push(b.len() as u8);
    } else {
        out.push(ATOM_UTF8);
        out.extend_from_slice(&(b.len().min(u16::MAX as usize) as u16).to_be_bytes());
    }
    out.extend_from_slice(&b[..b.len().min(u16::MAX as usize)]);
}

fn big(v: i128, out: &mut Vec<u8>) {
    let (sign, mut mag) = if v < 0 {
        (1u8, v.unsigned_abs())
    } else {
        (0u8, v as u128)
    };
    let mut digits = Vec::new();
    while mag > 0 {
        digits.push((mag & 0xff) as u8);
        mag >>= 8;
    }
    out.push(SMALL_BIG);
    out.push(digits.len() as u8);
    out.push(sign);
    out.extend_from_slice(&digits);
}

fn binary(b: &[u8], out: &mut Vec<u8>) {
    out.push(BINARY);
    out.extend_from_slice(&(b.len() as u32).to_be_bytes());
    out.extend_from_slice(b);
}

fn write(v: &Val, out: &mut Vec<u8>) {
    match v {
        Val::Null => atom("nil", out),
        Val::Bool(b) => atom(if *b { "true" } else { "false" }, out),
        Val::Int(n) => {
            if (0..=255).contains(n) {
                out.push(SMALL_INTEGER);
                out.push(*n as u8);
            } else if let Ok(i) = i32::try_from(*n) {
                out.push(INTEGER);
                out.extend_from_slice(&i.to_be_bytes());
            } else {
                big(*n as i128, out);
            }
        }
        Val::Big(n) => big(*n, out),
        Val::Float(f) => {
            out.push(NEW_FLOAT);
            out.extend_from_slice(&f.to_be_bytes());
        }
        Val::Str(s) => binary(s.as_bytes(), out),
        Val::Bin(b) => binary(b, out),
        Val::Atom(s) => atom(s, out),
        Val::Charlist(b) if b.len() <= u16::MAX as usize => {
            out.push(STRING);
            out.extend_from_slice(&(b.len() as u16).to_be_bytes());
            out.extend_from_slice(b);
        }
        Val::Charlist(b) => write(
            &Val::List(b.iter().map(|c| Val::Int(*c as i64)).collect()),
            out,
        ),
        Val::List(items) => {
            if !items.is_empty() {
                out.push(LIST);
                out.extend_from_slice(&(items.len() as u32).to_be_bytes());
                for i in items {
                    write(i, out);
                }
            }
            out.push(NIL);
        }
        Val::Tuple(items) => {
            if items.len() < 256 {
                out.push(SMALL_TUPLE);
                out.push(items.len() as u8);
            } else {
                out.push(LARGE_TUPLE);
                out.extend_from_slice(&(items.len() as u32).to_be_bytes());
            }
            for i in items {
                write(i, out);
            }
        }
        Val::Map(m) => {
            out.push(MAP);
            out.extend_from_slice(&(m.len() as u32).to_be_bytes());
            for (k, v) in m {
                write(k, out);
                write(v, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Val {
        Val::Map(vec![
            (Val::Atom("op".into()), Val::Int(0)),
            (Val::Atom("t".into()), Val::Atom("MESSAGE_CREATE".into())),
            (Val::Atom("s".into()), Val::Int(70000)),
            (
                Val::Atom("d".into()),
                Val::Map(vec![
                    (Val::Atom("id".into()), Val::Int(1170000000000000000)),
                    (
                        Val::Atom("content".into()),
                        Val::Str("hej <:ember:123>".into()),
                    ),
                    (Val::Atom("neg".into()), Val::Int(-5)),
                    (Val::Atom("f".into()), Val::Float(1.25)),
                    (Val::Atom("nonce".into()), Val::Null),
                    (Val::Atom("pinned".into()), Val::Bool(false)),
                    (Val::Atom("empty".into()), Val::List(vec![])),
                    (Val::Atom("chars".into()), Val::Charlist(vec![1, 2, 3])),
                    (
                        Val::Atom("tuple".into()),
                        Val::Tuple(vec![Val::Int(1), Val::Str("x".into())]),
                    ),
                    (Val::Atom("huge".into()), Val::Big(1 << 70)),
                ]),
            ),
        ])
    }

    #[test]
    fn roundtrip() {
        let v = sample();
        let bytes = encode(&v);
        assert_eq!(decode(&bytes).unwrap(), v);
        // And re-encoding the decoded value is byte-identical.
        assert_eq!(encode(&decode(&bytes).unwrap()), bytes);
    }

    #[test]
    fn decodes_legacy_atoms_and_ints() {
        // {"a" => 300} with ATOM_EXT key and INTEGER_EXT value.
        let bytes = [
            VERSION, MAP, 0, 0, 0, 1, ATOM, 0, 1, b'a', INTEGER, 0, 0, 1, 44,
        ];
        let v = decode(&bytes).unwrap();
        assert_eq!(v.get("a"), Some(&Val::Int(300)));
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode(&[1, 2, 3]).is_err());
        assert!(decode(&[VERSION, BINARY, 0, 0, 0, 9, b'x']).is_err());
    }
}
