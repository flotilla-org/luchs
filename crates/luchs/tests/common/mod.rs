#![allow(dead_code)]

use serde_json::{Value, json};

pub fn frame(width: u32, pixels: &[u8]) -> Vec<u8> {
    let header = serde_json::to_vec(&json!({
        "format": "rgba8", "width": width, "height": 1,
        "stride": width * 4, "len": pixels.len(),
    }))
    .unwrap();
    let mut body = vec![1];
    body.extend_from_slice(&(header.len() as u32).to_le_bytes());
    body.extend_from_slice(&header);
    body.extend_from_slice(pixels);
    envelope(&body)
}

pub fn envelope(body: &[u8]) -> Vec<u8> {
    let mut bytes = (body.len() as u32).to_le_bytes().to_vec();
    bytes.extend_from_slice(body);
    bytes
}

pub fn control(tag: u8, json: Value) -> Vec<u8> {
    let mut body = vec![tag];
    body.extend_from_slice(&serde_json::to_vec(&json).unwrap());
    envelope(&body)
}

pub fn printf(bytes: &[u8]) -> String {
    format!(
        "printf '{}'",
        bytes
            .iter()
            .map(|b| format!("\\{b:03o}"))
            .collect::<String>()
    )
}

pub const PYTHON_PROTOCOL: &str = r#"
import json, struct, sys, os, time

def exact(n):
    data = b''
    while len(data) < n:
        chunk = sys.stdin.buffer.read(n - len(data))
        if not chunk:
            raise EOFError()
        data += chunk
    return data

def command():
    n = struct.unpack('<I', exact(4))[0]
    assert 0 < n <= 128 * 1024
    return json.loads(exact(n))

def control(tag, obj):
    body = bytes([tag]) + json.dumps(obj).encode()
    sys.stdout.buffer.write(struct.pack('<I', len(body)) + body)
    sys.stdout.buffer.flush()

def ack(cmd, outcome='executed', detail=None):
    obj = {'id': cmd['id'], 'outcome': outcome}
    if detail is not None: obj['detail'] = detail
    control(2, obj)

def frame():
    header = json.dumps({'format':'rgba8', 'width':1, 'height':1, 'stride':4, 'len':4}).encode()
    body = b'\x01' + struct.pack('<I', len(header)) + header + b'rgba'
    sys.stdout.buffer.write(struct.pack('<I', len(body)) + body)
    sys.stdout.buffer.flush()
"#;
