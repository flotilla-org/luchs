#![allow(dead_code)]

use serde_json::{Value, json};

pub fn frame(width: u32, pixels: &[u8]) -> Vec<u8> {
    let header = serde_json::to_vec(&json!({
        "format": "bgra8", "width": width, "height": 1,
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

pub fn socket_write(bytes: &[u8]) -> String {
    // dash only accepts single-digit descriptors in shell redirections. Use
    // Python's fd API so concurrent tests can inherit any descriptor number.
    format!(
        "python3 -c 'import os; w=os.fdopen(int(os.environ[\"LUCHS_HELPER_FD\"]), \"wb\", closefd=False); w.write(bytes.fromhex(\"{}\")); w.flush()'",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    )
}

pub const PYTHON_PROTOCOL: &str = r#"
import json, struct, sys, os, time, socket
transport = socket.socket(fileno=int(os.environ["LUCHS_HELPER_FD"]))
wire = transport.makefile("rwb", buffering=0)

def exact(n):
    data = b''
    while len(data) < n:
        chunk = wire.read(n - len(data))
        if not chunk:
            raise EOFError()
        data += chunk
    return data

def raw_command():
    n = struct.unpack('<I', exact(4))[0]
    assert 0 < n <= 128 * 1024
    return json.loads(exact(n))

def command():
    while True:
        cmd = raw_command()
        if cmd['type'] != 'capture': return cmd
        ack(cmd, capture={'published':False, 'snapshot_ns':100, 'publish_ns':0})

def control(tag, obj):
    body = bytes([tag]) + json.dumps(obj).encode()
    wire.write(struct.pack('<I', len(body)) + body)
    wire.flush()

def ack(cmd, outcome='executed', detail=None, capture=None):
    obj = {'id': cmd['id'], 'outcome': outcome}
    if detail is not None: obj['detail'] = detail
    if capture is not None: obj['capture'] = capture
    control(2, obj)

def frame():
    header = json.dumps({'format':'bgra8', 'width':1, 'height':1, 'stride':4, 'len':4}).encode()
    body = b'\x01' + struct.pack('<I', len(header)) + header + b'rgba'
    wire.write(struct.pack('<I', len(body)) + body)
    wire.flush()
"#;
