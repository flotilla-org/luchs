#![allow(dead_code)]

use jackstay::{
    handshake::{self, Connected, Selected},
    input::Mode,
    local::Stream,
};
use jackstay_producer::{AFFORDANCES, INPUT, VIDEO};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::{Child, Command},
};

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

/// The interpreter the fake helpers run under. Windows installs have no
/// `python3` executable (only a Store alias), so use `python` there.
pub const PYTHON: &str = if cfg!(windows) { "python" } else { "python3" };

/// A fake helper running [`PYTHON_PROTOCOL`] followed by `script`.
pub fn python(script: &str) -> Command {
    let mut command = Command::new(PYTHON);
    command.args(["-c", &format!("{PYTHON_PROTOCOL}{script}")]);
    command
}

/// A Python statement writing raw bytes to the helper transport.
pub fn raw_write(bytes: &[u8]) -> String {
    format!(
        "wire.write(bytes.fromhex('{}')); wire.flush()\n",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    )
}

/// A Python statement recording this helper's PID in `path`.
pub fn record_pid(path: &Path) -> String {
    format!(
        "open({}, 'w').write(str(os.getpid()))\n",
        serde_json::to_string(&path.to_string_lossy()).unwrap()
    )
}

/// Write [`PYTHON_PROTOCOL`] and `body` as an executable `--helper` in `dir`.
/// Windows cannot execute a script directly, so a `.cmd` launcher runs it;
/// the helper transport accepts the interpreter it starts (see the helper job).
pub fn helper_script(dir: &Path, body: &str) -> PathBuf {
    let source = format!("#!/usr/bin/env python3\n{PYTHON_PROTOCOL}\n{body}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("helper");
        std::fs::write(&script, source).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        script
    }
    #[cfg(windows)]
    {
        let script = dir.join("helper.py");
        std::fs::write(&script, source).unwrap();
        let launcher = dir.join("helper.cmd");
        std::fs::write(
            &launcher,
            format!("@\"{PYTHON}\" \"{}\" %*\r\n", script.display()),
        )
        .unwrap();
        launcher
    }
}

/// Connect to a source by the address `Source::bind` returned.
pub fn connect(path: &str) -> Stream {
    #[cfg(unix)]
    {
        Stream::connect(path).unwrap()
    }
    #[cfg(windows)]
    {
        jackstay::local::connect(&endpoint(path))
            .unwrap()
            .into_stream()
    }
}

/// The endpoint a rendered address names (`\\.\pipe\jackstay.<SID>.<name>`).
#[cfg(windows)]
pub fn endpoint(path: &str) -> jackstay::local::Endpoint {
    let (_, name) = path
        .strip_prefix(r"\\.\pipe\jackstay.")
        .and_then(|rest| rest.split_once('.'))
        .expect("a user-scoped Jackstay pipe name");
    jackstay::local::Endpoint::new(
        jackstay::local::Scope::User,
        name,
        jackstay::local::Transport::LocalStream,
    )
    .unwrap()
}

/// Whether a stopped source removed its endpoint. An empty address (the CLI
/// failed before announcing one) names nothing.
pub fn endpoint_gone(path: &str) -> bool {
    if path.is_empty() {
        return true;
    }
    #[cfg(unix)]
    {
        !Path::new(path).exists()
    }
    // Probing a pipe by path would open an instance; try to connect instead.
    #[cfg(windows)]
    {
        jackstay::local::connect(&endpoint(path)).is_err()
    }
}

/// Stop a `luchs` CLI process. Unix sends `SIGTERM` for an orderly stop.
/// Windows has no stop request for it yet (#43), so it is terminated.
pub fn stop(child: &mut Child) {
    #[cfg(unix)]
    // SAFETY: this is the caller's own child process.
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    #[cfg(windows)]
    child.kill().unwrap();
}

/// Whether the process with this PID has exited and been reaped.
pub fn gone(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: signal zero only probes whether the test child still exists.
        let probed = unsafe { libc::kill(pid as i32, 0) };
        probed == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::{
            Foundation::WAIT_OBJECT_0,
            System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
        };
        // SAFETY: scalar arguments; a non-null result is a fresh owned handle.
        let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if raw.is_null() {
            return true;
        }
        // SAFETY: as above.
        let process = unsafe { OwnedHandle::from_raw_handle(raw) };
        // SAFETY: live handle; a zero timeout only polls.
        unsafe { WaitForSingleObject(process.as_raw_handle(), 0) == WAIT_OBJECT_0 }
    }
}

/// The fake helpers' side of docs/helper-protocol.md, for either transport:
/// the Unix socketpair with `SCM_RIGHTS`, or the Windows named pipe with the
/// payload handle named in-band.
pub const PYTHON_PROTOCOL: &str = r#"
import json, struct, sys, os, time, mmap
if os.name == 'nt':
    import ctypes
    from ctypes import wintypes
    wire = open(os.environ["LUCHS_HELPER_PIPE"], "r+b", buffering=0)
    transport = wire
    kernel32 = ctypes.WinDLL('kernel32', use_last_error=True)
    kernel32.MapViewOfFile.restype = ctypes.c_void_p
    kernel32.MapViewOfFile.argtypes = (wintypes.HANDLE, wintypes.DWORD, wintypes.DWORD, wintypes.DWORD, ctypes.c_size_t)
    kernel32.UnmapViewOfFile.argtypes = (ctypes.c_void_p,)
    kernel32.CloseHandle.argtypes = (wintypes.HANDLE,)

    class View:
        # A writable view of the section handle Rust duplicated into this process.
        def __init__(self, handle, size):
            self.address = kernel32.MapViewOfFile(handle, 0x6, 0, 0, size)  # FILE_MAP_READ | FILE_MAP_WRITE
            error = ctypes.get_last_error()
            kernel32.CloseHandle(handle)
            assert self.address, error
            self.size = size
        def __setitem__(self, span, data):
            assert 0 <= span.start <= span.stop <= self.size and span.stop - span.start == len(data)
            ctypes.memmove(self.address + span.start, bytes(data), len(data))
        def close(self):
            kernel32.UnmapViewOfFile(self.address)

    def receive(n, rights):
        try: return wire.read(n)
        except OSError: return b''  # a disconnected pipe is EOF

    def adopt(cmd, rights):
        assert not rights
        return View(cmd['handle'], cmd['layout']['map_len'])

    import msvcrt
    kernel32.PeekNamedPipe.argtypes = (wintypes.HANDLE, ctypes.c_void_p, wintypes.DWORD, ctypes.c_void_p, ctypes.POINTER(wintypes.DWORD), ctypes.c_void_p)
    def readable(timeout):
        # select() cannot wait on a pipe; poll it. A broken pipe reads as EOF.
        deadline = time.monotonic() + timeout
        while True:
            available = wintypes.DWORD()
            if not kernel32.PeekNamedPipe(msvcrt.get_osfhandle(wire.fileno()), None, 0, None, ctypes.byref(available), None) or available.value:
                return True
            if time.monotonic() >= deadline: return False
            time.sleep(0.001)
else:
    import socket, array, select
    transport = socket.socket(fileno=int(os.environ["LUCHS_HELPER_FD"]))
    wire = transport.makefile("rwb", buffering=0)

    def readable(timeout):
        return bool(select.select([transport], [], [], timeout)[0])

    def receive(n, rights):
        chunk, ancillary, flags, _ = transport.recvmsg(n, socket.CMSG_SPACE(8 * 4))
        assert not flags & socket.MSG_CTRUNC
        for level, kind, value in ancillary:
            assert level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS and rights is not None
            fds = array.array('i'); fds.frombytes(value)
            rights.extend(fds)
        return chunk

    def adopt(cmd, rights):
        assert len(rights) == 1 and 'handle' not in cmd
        mapping = mmap.mmap(rights[0], cmd['layout']['map_len'])
        os.close(rights[0])
        return mapping
allocation, layout = None, None

def exact(n, rights=None):
    data = b''
    while len(data) < n:
        chunk = receive(n - len(data), rights)
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
            assert not rights and 'handle' not in cmd and cmd['generation'] == layout['generation']
            allocation.close()
            allocation, layout = None, None
            control(2, {'id':cmd['id'], 'outcome':'executed', 'generation':cmd['generation']})
            continue
        if cmd['type'] != 'arena':
            assert not rights and 'handle' not in cmd
            return cmd
        replacement = adopt(cmd, rights)
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

/// Whether a consumer asks for a track, and whether the handshake must admit it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Want {
    No,
    Optional,
    Required,
}

/// Selects Luchs's video track, plus input and affordances as asked.
pub fn connect_tracks(stream: Stream, input: Want, mode: Mode, affordances: Want) -> Connected {
    let mut selection = vec![Selected {
        id: VIDEO,
        required: true,
        write: false,
    }];
    for (id, want, write) in [(INPUT, input, true), (AFFORDANCES, affordances, false)] {
        if want != Want::No {
            selection.push(Selected {
                id,
                required: want == Want::Required,
                write,
            });
        }
    }
    handshake::connect(stream)
        .unwrap()
        .select(&selection, mode)
        .unwrap()
}
