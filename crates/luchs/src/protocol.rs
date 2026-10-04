//! Framed helper wire contract. See docs/helper-protocol.md.
use std::io::{self, Read};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_CONTROL_BYTES: usize = 128 * 1024;
pub const MAX_HEADER_BYTES: usize = 4096;
pub const FRAME_TAG: u8 = 1;
pub const ACK_TAG: u8 = 2;
pub const STATE_TAG: u8 = 3;

#[derive(Debug, Deserialize, Serialize)]
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

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AckOutcome {
    Executed,
    Unsupported,
    Failed,
}

#[derive(Debug, Deserialize)]
pub struct Ack {
    pub id: u64,
    pub outcome: AckOutcome,
    pub detail: Option<String>,
}

#[derive(Debug)]
pub enum Record {
    Frame(Frame),
    Ack(Ack),
    State(Map<String, Value>),
}

pub fn validate_size(width: u32, height: u32) -> Result<(), String> {
    let bytes = (u64::from(width) * u64::from(height)).checked_mul(4);
    if width == 0 || height == 0 || bytes.is_none_or(|bytes| bytes > MAX_FRAME_BYTES as u64) {
        return Err("size must be positive and fit in 64 MiB of RGBA pixels".into());
    }
    Ok(())
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn read_u32(reader: &mut impl Read) -> io::Result<usize> {
    let mut bytes = [0; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes) as usize)
}

/// Validate sizes before allocating or reading their payloads. EOF is clean
/// only between records, never inside an envelope, header or pixel buffer.
pub fn read_record(reader: &mut impl Read) -> io::Result<Option<Record>> {
    let mut first = [0];
    loop {
        match reader.read(&mut first) {
            Ok(0) => return Ok(None),
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    let mut rest = [0; 3];
    reader.read_exact(&mut rest)?;
    let len = u32::from_le_bytes([first[0], rest[0], rest[1], rest[2]]) as usize;
    if len == 0 || len > 1 + 4 + MAX_HEADER_BYTES + MAX_FRAME_BYTES {
        return Err(invalid("invalid helper record length"));
    }
    let mut tag = [0];
    reader.read_exact(&mut tag)?;
    match tag[0] {
        FRAME_TAG => {
            if len < 5 {
                return Err(invalid("short frame record"));
            }
            let header_len = read_u32(reader)?;
            if header_len == 0 || header_len > MAX_HEADER_BYTES || header_len > len - 5 {
                return Err(invalid("invalid frame header length"));
            }
            let mut json = vec![0; header_len];
            reader.read_exact(&mut json)?;
            let header: Header = serde_json::from_slice(&json)?;
            let row_bytes = u64::from(header.width) * 4;
            let pixel_len = u64::from(header.stride) * u64::from(header.height);
            if header.format != "rgba8"
                || header.width == 0
                || header.height == 0
                || u64::from(header.stride) < row_bytes
                || pixel_len != header.len as u64
                || header.len > MAX_FRAME_BYTES
                || header.len != len - 5 - header_len
            {
                return Err(invalid(
                    "invalid RGBA frame dimensions, stride, format, or length",
                ));
            }
            let mut pixels = vec![0; header.len];
            reader.read_exact(&mut pixels)?;
            Ok(Some(Record::Frame(Frame { header, pixels })))
        }
        ACK_TAG | STATE_TAG => {
            if len > MAX_CONTROL_BYTES {
                return Err(invalid("oversized control record"));
            }
            let mut json = vec![0; len - 1];
            reader.read_exact(&mut json)?;
            Ok(Some(if tag[0] == ACK_TAG {
                Record::Ack(serde_json::from_slice(&json)?)
            } else {
                Record::State(serde_json::from_slice(&json)?)
            }))
        }
        _ => Err(invalid("unknown helper record tag")),
    }
}

pub fn encode_command(id: u64, kind: &str) -> io::Result<Vec<u8>> {
    if kind.len() > MAX_CONTROL_BYTES {
        return Err(invalid("oversized command"));
    }
    if kind.is_empty() {
        return Err(invalid("empty command type"));
    }
    let json = serde_json::to_vec(&serde_json::json!({"id": id, "type": kind}))?;
    if json.len() > MAX_CONTROL_BYTES {
        return Err(invalid("oversized command"));
    }
    let mut bytes = Vec::with_capacity(4 + json.len());
    bytes.extend_from_slice(&(json.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&json);
    Ok(bytes)
}
