//! Gateway transport: payload encoding (JSON / ETF) and stream compression (zlib-stream / zstd-stream).
//!
//! Compression contexts are shared across a whole connection, so once Ashcord changes one message it
//! has to re-compress everything it sends to the client with its own context.

use crate::{etf, val::Val};
use anyhow::{Context, Result, bail};
use flate2::{Compress, Decompress, FlushCompress, FlushDecompress};
use zstd::stream::raw::{Decoder as ZDec, Encoder as ZEnc, InBuffer, Operation, OutBuffer};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Json,
    Etf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    None,
    Zlib,
    Zstd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Codec {
    pub encoding: Encoding,
    pub compression: Compression,
}

impl Codec {
    /// Read `encoding=` and `compress=` from the gateway URL query.
    pub fn from_query(query: &str) -> Result<Codec> {
        let mut c = Codec { encoding: Encoding::Json, compression: Compression::None };
        for pair in query.split('&') {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            match (k, v) {
                ("encoding", "json") => c.encoding = Encoding::Json,
                ("encoding", "etf") => c.encoding = Encoding::Etf,
                ("encoding", other) => bail!("unknown gateway encoding {other}"),
                ("compress", "zlib-stream") => c.compression = Compression::Zlib,
                ("compress", "zstd-stream") => c.compression = Compression::Zstd,
                ("compress", other) => bail!("unknown gateway compression {other}"),
                _ => {}
            }
        }
        Ok(c)
    }

    pub fn decode(&self, plain: &[u8]) -> Result<Val> {
        match self.encoding {
            Encoding::Json => Ok(Val::from_json(serde_json::from_slice(plain).context("gateway JSON")?)),
            Encoding::Etf => etf::decode(plain),
        }
    }

    pub fn encode(&self, v: &Val) -> Vec<u8> {
        match self.encoding {
            Encoding::Json => serde_json::to_vec(&v.to_json()).unwrap_or_default(),
            Encoding::Etf => etf::encode(v),
        }
    }
}

const ZLIB_SUFFIX: [u8; 4] = [0, 0, 0xff, 0xff];

/// Turns compressed frames from the server into whole plaintext payloads.
pub enum Inflater {
    None,
    Zlib { z: Box<Decompress>, buf: Vec<u8> },
    Zstd(Box<ZDec<'static>>),
}

impl Inflater {
    pub fn new(c: Compression) -> Result<Inflater> {
        Ok(match c {
            Compression::None => Inflater::None,
            Compression::Zlib => Inflater::Zlib { z: Box::new(Decompress::new(true)), buf: Vec::new() },
            Compression::Zstd => Inflater::Zstd(Box::new(ZDec::new()?)),
        })
    }

    /// Feed one websocket frame. Returns a payload once a whole one has arrived.
    pub fn push(&mut self, frame: &[u8]) -> Result<Option<Vec<u8>>> {
        match self {
            Inflater::None => Ok(Some(frame.to_vec())),
            Inflater::Zlib { z, buf } => {
                buf.extend_from_slice(frame);
                if buf.len() < 4 || buf[buf.len() - 4..] != ZLIB_SUFFIX {
                    return Ok(None);
                }
                let input = std::mem::take(buf);
                let mut out = Vec::with_capacity(input.len() * 4);
                let mut chunk = vec![0u8; 64 * 1024];
                let mut used = 0;
                loop {
                    let (i0, o0) = (z.total_in(), z.total_out());
                    z.decompress(&input[used..], &mut chunk, FlushDecompress::Sync).context("zlib-stream")?;
                    used += (z.total_in() - i0) as usize;
                    let n = (z.total_out() - o0) as usize;
                    out.extend_from_slice(&chunk[..n]);
                    if used >= input.len() && n < chunk.len() {
                        break;
                    }
                    if n == 0 && z.total_in() == i0 {
                        bail!("zlib-stream made no progress");
                    }
                }
                Ok(Some(out))
            }
            Inflater::Zstd(d) => {
                let mut input = InBuffer::around(frame);
                let mut out = Vec::with_capacity(frame.len() * 4);
                let mut chunk = vec![0u8; 64 * 1024];
                loop {
                    let mut ob = OutBuffer::around(&mut chunk[..]);
                    d.run(&mut input, &mut ob).context("zstd-stream")?;
                    let n = ob.pos();
                    out.extend_from_slice(&chunk[..n]);
                    if input.pos() >= frame.len() && n < chunk.len() {
                        break;
                    }
                }
                Ok(Some(out))
            }
        }
    }
}

/// Compresses plaintext payloads for the client with Ashcord's own stream context.
pub enum Deflater {
    None,
    Zlib(Box<Compress>),
    Zstd(Box<ZEnc<'static>>),
}

impl Deflater {
    pub fn new(c: Compression) -> Result<Deflater> {
        Ok(match c {
            Compression::None => Deflater::None,
            Compression::Zlib => Deflater::Zlib(Box::new(Compress::new(flate2::Compression::new(6), true))),
            Compression::Zstd => Deflater::Zstd(Box::new(ZEnc::new(3)?)),
        })
    }

    pub fn push(&mut self, plain: &[u8]) -> Result<Vec<u8>> {
        match self {
            Deflater::None => Ok(plain.to_vec()),
            Deflater::Zlib(z) => {
                let mut out = Vec::with_capacity(plain.len() / 2 + 64);
                let mut chunk = vec![0u8; 64 * 1024];
                let mut used = 0;
                loop {
                    let (i0, o0) = (z.total_in(), z.total_out());
                    z.compress(&plain[used..], &mut chunk, FlushCompress::Sync)?;
                    used += (z.total_in() - i0) as usize;
                    let n = (z.total_out() - o0) as usize;
                    out.extend_from_slice(&chunk[..n]);
                    if used >= plain.len() && n < chunk.len() && out.ends_with(&ZLIB_SUFFIX) {
                        break;
                    }
                }
                Ok(out)
            }
            Deflater::Zstd(e) => {
                let mut out = Vec::with_capacity(plain.len() / 2 + 64);
                let mut chunk = vec![0u8; 64 * 1024];
                let mut input = InBuffer::around(plain);
                while input.pos() < plain.len() {
                    let mut ob = OutBuffer::around(&mut chunk[..]);
                    e.run(&mut input, &mut ob)?;
                    let n = ob.pos();
                    out.extend_from_slice(&chunk[..n]);
                }
                loop {
                    let mut ob = OutBuffer::around(&mut chunk[..]);
                    let left = e.flush(&mut ob)?;
                    let n = ob.pos();
                    out.extend_from_slice(&chunk[..n]);
                    if left == 0 {
                        break;
                    }
                }
                Ok(out)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(c: Compression) {
        let mut def = Deflater::new(c).unwrap();
        let mut inf = Inflater::new(c).unwrap();
        let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        for msg in [b"hello".to_vec(), big, b"{\"op\":11}".to_vec(), Vec::new()] {
            let frame = def.push(&msg).unwrap();
            assert_eq!(inf.push(&frame).unwrap().unwrap(), msg);
        }
    }

    #[test]
    fn zlib_stream_roundtrip() {
        roundtrip(Compression::Zlib);
    }

    #[test]
    fn zstd_stream_roundtrip() {
        roundtrip(Compression::Zstd);
    }

    #[test]
    fn zlib_waits_for_whole_message() {
        let mut def = Deflater::new(Compression::Zlib).unwrap();
        let mut inf = Inflater::new(Compression::Zlib).unwrap();
        let frame = def.push(b"split across two frames").unwrap();
        let (a, b) = frame.split_at(frame.len() / 2);
        assert!(inf.push(a).unwrap().is_none());
        assert_eq!(inf.push(b).unwrap().unwrap(), b"split across two frames");
    }

    #[test]
    fn parses_query() {
        let c = Codec::from_query("encoding=etf&v=9&compress=zstd-stream").unwrap();
        assert_eq!(c, Codec { encoding: Encoding::Etf, compression: Compression::Zstd });
        assert!(Codec::from_query("encoding=xml").is_err());
    }
}
