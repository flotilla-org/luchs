//! The existing Swift helper's stdout: one header line, then exactly len bytes.
use std::io::{self, BufRead, Read};

use serde::Deserialize;

pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;
const MAX_HEADER_BYTES: u64 = 4096;

#[derive(Debug, Deserialize)]
pub struct Header {
    pub format: String,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub len: usize,
}

#[derive(Debug)]
pub struct Frame {
    pub header: Header,
    pub pixels: Vec<u8>,
}

pub fn validate_size(width: u32, height: u32) -> Result<(), String> {
    let bytes = (u64::from(width) * u64::from(height)).checked_mul(4);
    if width == 0 || height == 0 || bytes.is_none_or(|bytes| bytes > MAX_FRAME_BYTES as u64) {
        return Err("size must be positive and fit in 64 MiB of RGBA pixels".into());
    }
    Ok(())
}

pub fn read_frame(reader: &mut impl BufRead) -> io::Result<Option<Frame>> {
    let invalid = |message| io::Error::new(io::ErrorKind::InvalidData, message);
    let mut line = Vec::new();
    reader
        .take(MAX_HEADER_BYTES + 1)
        .read_until(b'\n', &mut line)?;
    if line.is_empty() {
        return Ok(None);
    }
    if line.len() > MAX_HEADER_BYTES as usize || line.last() != Some(&b'\n') {
        return Err(invalid("oversized or unterminated frame header"));
    }
    let json = line
        .strip_prefix(b"LUCHS_RAW_FRAME ")
        .ok_or_else(|| invalid("missing LUCHS_RAW_FRAME prefix"))?;
    let header: Header = serde_json::from_slice(json)?;
    let row_bytes = u64::from(header.width) * 4;
    let len = u64::from(header.stride) * u64::from(header.height);
    if header.format != "rgba8"
        || header.width == 0
        || header.height == 0
        || u64::from(header.stride) < row_bytes
        || len != header.len as u64
        || header.len > MAX_FRAME_BYTES
    {
        return Err(invalid(
            "invalid RGBA frame dimensions, stride, format, or length",
        ));
    }
    let mut pixels = vec![0; header.len];
    reader.read_exact(&mut pixels)?;
    Ok(Some(Frame { header, pixels }))
}
