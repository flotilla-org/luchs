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
import json, struct, sys, os, time, socket, array, mmap
transport = socket.socket(fileno=int(os.environ["LUCHS_HELPER_FD"]))
wire = transport.makefile("rwb", buffering=0)
allocation, layout = None, None

def exact(n, rights=None):
    data = b''
    while len(data) < n:
        chunk, ancillary, flags, _ = transport.recvmsg(n - len(data), socket.CMSG_SPACE(8 * 4))
        assert not flags & socket.MSG_CTRUNC
        for level, kind, value in ancillary:
            assert level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS and rights is not None
            fds = array.array('i'); fds.frombytes(value)
            rights.extend(fds)
        if not chunk: raise EOFError()
        data += chunk
    return data

def raw_command():
    global allocation, layout
    while True:
        rights = []
        n = struct.unpack('<I', exact(4, rights))[0]
        assert 0 < n <= 128 * 1024
        cmd = json.loads(exact(n, rights))
        if cmd['type'] == 'arena_release':
            assert not rights and cmd['generation'] == layout['generation']
            allocation.close()
            allocation, layout = None, None
            control(2, {'id':cmd['id'], 'outcome':'executed', 'generation':cmd['generation']})
            continue
        if cmd['type'] != 'arena':
            assert not rights
            return cmd
        assert len(rights) == 1
        replacement = mmap.mmap(rights[0], cmd['layout']['map_len'])
        os.close(rights[0])
        if allocation is not None: allocation.close()
        allocation, layout = replacement, cmd['layout']
        control(2, {'id':cmd['id'], 'outcome':'executed', 'generation':layout['generation']})

def command():
    while True:
        cmd = raw_command()
        if cmd['type'] != 'draw': return cmd
        ack(cmd, capture={'published':False, 'snapshot_ns':100, 'publish_ns':0})

def control(tag, obj):
    body = bytes([tag]) + json.dumps(obj).encode()
    wire.write(struct.pack('<I', len(body)) + body)
    wire.flush()

def ack(cmd, outcome='executed', detail=None, capture=None, pixels=None):
    obj = {'id': cmd['id'], 'outcome': outcome}
    if cmd['type'] == 'draw':
        obj.update(generation=cmd['generation'], slot=cmd['slot'])
        if capture and capture['published']:
            assert cmd['generation'] == layout['generation']
            assert cmd['arena_scope'] == layout['arena_scope']
            count = cmd['stride'] * cmd['height']
            if pixels is None: pixels = b'rgba' * (count // 4)
            assert len(pixels) == count
            offset = cmd['slot'] * layout['slot_capacity']
            allocation[offset:offset+count] = pixels
            obj['frame'] = {'format':'bgra8', 'width':cmd['width'], 'height':cmd['height'], 'stride':cmd['stride'], 'len':count}
    if detail is not None: obj['detail'] = detail
    if capture is not None: obj['capture'] = capture
    control(2, obj)

def frame():
    cmd = raw_command()
    assert cmd['type'] == 'draw'
    ack(cmd, capture={'published':True, 'snapshot_ns':100, 'publish_ns':100})
"#;
