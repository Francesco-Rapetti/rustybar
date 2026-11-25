//! Google Meet, for `now_playing`, through RustyBar's browser extension (`extension/` in the repo,
//! `Contents/Resources/extension` in the app).
//!
//! The extension's page script tells its worker when the tab's call starts, changes and ends; the
//! worker keeps a WebSocket to the bar open while a tab is in a call, and passes the bar's clicks
//! back. Here, the other end: a server on 127.0.0.1:`PORT`, started once for the process, that
//! takes only the extension (the `Origin` of its id, fixed by the key in its manifest). Each line
//! has a thread that waits for its next message, so nothing runs between them; the worker sends
//! one every 20 s to stay on.
//!
//! Which browser is at the other end: the process that has the other end of the socket, found
//! among every process's sockets as `lsof` does (`peer`), then the app it belongs to: its icon,
//! its name and the app to bring forward.

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Deserialize;

use super::Handle;
use super::call::Control;
use super::now_playing::NowPlaying;

pub const PORT: u16 = 47823;
/// The extension's origin: its id comes from the key in its manifest.
const ORIGIN: &str = "chrome-extension://palkoaepeohmdiehbgmiglbndcebomlk";

/// A message of the extension.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Said {
    /// A tab's call: `call` false when it ended, or the tab closed.
    Call {
        tab: i64,
        #[serde(default)]
        call: bool,
        mic: Option<bool>,
        camera: Option<bool>,
        hand: Option<bool>,
        /// Before joining, with Join (`join`) while it's there.
        #[serde(default)]
        lobby: bool,
        #[serde(default)]
        join: bool,
        #[serde(default)]
        name: String,
        #[serde(default)]
        code: String,
    },
    /// What a tab's page has (see `msg trigger call_dump`).
    Dump { tab: i64, text: String },
    /// The extension's version, first on each line: in the log.
    Hello { version: String },
    #[serde(other)]
    Other,
}

/// The browser at the other end of a line: its app's pid and bundle.
pub type Browser = Option<(i32, String)>;

struct Bridge {
    started: bool,
    handles: Vec<Handle<NowPlaying>>,
    /// The lines open, to write to.
    lines: Vec<(u64, TcpStream)>,
}

static BRIDGE: Mutex<Bridge> = Mutex::new(Bridge { started: false, handles: Vec::new(), lines: Vec::new() });
static LINES: AtomicU64 = AtomicU64::new(0);

fn bridge() -> std::sync::MutexGuard<'static, Bridge> {
    BRIDGE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Tells `handle` what the extension says, from now on: starts the server the first time, and
/// asks the lines already open for their calls again (a reload).
pub fn serve(handle: Handle<NowPlaying>) {
    let mut b = bridge();
    b.handles.retain(|h| h.alive());
    b.handles.push(handle);
    if !b.started {
        b.started = true;
        std::thread::spawn(listen);
    }
    let lines: Vec<u64> = b.lines.iter().map(|(id, _)| *id).collect();
    drop(b);
    for id in lines {
        say(id, &serde_json::json!({ "type": "resend" }));
    }
}

/// Presses a control of the call in `tab`.
pub fn press(line: u64, tab: i64, control: Control) {
    say(line, &serde_json::json!({ "type": "press", "tab": tab, "control": control.key() }));
}

/// Brings `tab` forward in its window, and the window in its browser.
pub fn show(line: u64, tab: i64) {
    say(line, &serde_json::json!({ "type": "show", "tab": tab }));
}

/// Asks every tab in a call for what its page has (see `Said::Dump`).
pub fn dump() {
    let lines: Vec<u64> = bridge().lines.iter().map(|(id, _)| *id).collect();
    for id in lines {
        say(id, &serde_json::json!({ "type": "dump" }));
    }
}

/// Has every line's extension load again from its folder (`msg trigger meet_reload`): after a
/// change of its files with the same version.
pub fn reload() {
    let lines: Vec<u64> = bridge().lines.iter().map(|(id, _)| *id).collect();
    for id in lines {
        say(id, &serde_json::json!({ "type": "reload" }));
    }
}

/// The versions of the extension already asked to load again, this run: one that comes back
/// the same was loaded from another folder, and isn't asked again.
static ASKED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The line's extension says its `version`: older than the one in the app, it loads again from
/// its folder (the app's, or this repository's, which the app's comes from), once.
fn update(id: u64, version: &str) {
    let Some(shipped) = shipped().filter(|s| older(version, s)) else { return };
    let mut asked = ASKED.lock().unwrap_or_else(|e| e.into_inner());
    if asked.iter().any(|v| v == version) {
        crate::log(&format!("meet: the extension is still {version}, the app has {shipped}: load the app's in the browser (see the README)"));
        return;
    }
    asked.push(version.to_string());
    drop(asked);
    crate::log(&format!("meet: the extension {version} loads {shipped} again"));
    say(id, &serde_json::json!({ "type": "reload" }));
}

/// The version of the extension in the app (`Contents/Resources/extension`), when RustyBar runs
/// from one.
fn shipped() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let manifest = exe.parent()?.parent()?.join("Resources/extension/manifest.json");
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(manifest).ok()?).ok()?;
    json.get("version")?.as_str().map(String::from)
}

/// Whether the version `a` comes before `b`, part by part: `1.9` before `1.10`.
fn older(a: &str, b: &str) -> bool {
    let parts = |v: &str| v.split('.').map(|p| p.parse::<u32>().unwrap_or(0)).collect::<Vec<_>>();
    let (a, b) = (parts(a), parts(b));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x < y;
        }
    }
    false
}

/// Writes `message` to the line `id`: a few hundred bytes, which the socket takes at once.
fn say(id: u64, message: &serde_json::Value) {
    let stream = bridge().lines.iter().find(|(i, _)| *i == id).and_then(|(_, s)| s.try_clone().ok());
    if let Some(mut stream) = stream {
        let _ = stream.write_all(&frame(1, message.to_string().as_bytes()));
    }
}

fn tell(f: impl Fn(&mut NowPlaying, &mut super::Cx) + Send + Clone + 'static) {
    let handles = {
        let mut b = bridge();
        b.handles.retain(|h| h.alive());
        b.handles.clone()
    };
    for h in handles {
        let f = f.clone();
        h.send(move |w, cx| f(w, cx));
    }
}

fn listen() {
    let listener = match TcpListener::bind(("127.0.0.1", PORT)) {
        Ok(l) => l,
        Err(e) => {
            // Another bar has it (one for a test, with its own config)
            crate::log(&format!("meet: no extension: 127.0.0.1:{PORT} {e}"));
            return;
        }
    };
    for stream in listener.incoming().flatten() {
        std::thread::spawn(move || line(stream));
    }
}

/// One line, from the extension's handshake to its end.
fn line(mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
    let key = match handshake(&mut stream) {
        Ok(key) => key,
        Err(origin) => {
            crate::log(&format!("meet: refused a line from {}", origin.as_deref().unwrap_or("no origin")));
            let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            return;
        }
    };
    let accept = format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n", accept_key(&key));
    if stream.write_all(accept.as_bytes()).is_err() {
        return;
    }
    let _ = stream.set_read_timeout(None);
    let browser: Browser = stream.peer_addr().ok().and_then(|a| peer(a.port())).and_then(super::call::app_of);
    let from = browser.as_ref().map_or("an app it can't tell", |(_, bundle)| bundle.as_str()).to_string();
    crate::log(&format!("meet: the extension is on, from {from}"));
    let id = LINES.fetch_add(1, Ordering::Relaxed) + 1;
    match stream.try_clone() {
        Ok(s) => bridge().lines.push((id, s)),
        Err(_) => return,
    }
    let mut message = Vec::new();
    while let Ok((fin, opcode, data)) = read_frame(&mut stream) {
        match opcode {
            // Text, and what follows it
            0 | 1 => {
                message.extend_from_slice(&data);
                if !fin {
                    continue;
                }
                if let Ok(said) = serde_json::from_slice::<Said>(&message) {
                    if let Said::Hello { version } = &said {
                        crate::log(&format!("meet: the extension {version}, from {from}"));
                        update(id, version);
                    }
                    let browser = browser.clone();
                    let said = std::sync::Arc::new(said);
                    tell(move |w, cx| w.meet_said(id, &browser, &said, cx));
                }
                message.clear();
            }
            8 => break,
            9 => {
                let _ = stream.write_all(&frame(10, &data));
            }
            _ => {}
        }
    }
    bridge().lines.retain(|(i, _)| *i != id);
    crate::log(&format!("meet: the extension's line from {from} closed"));
    tell(move |w, cx| w.meet_closed(id, cx));
}

/// The HTTP request that opens a line: its `Sec-WebSocket-Key`, if it's a WebSocket from the
/// extension; else its `Origin`, for the log.
fn handshake(stream: &mut TcpStream) -> Result<String, Option<String>> {
    let mut request = Vec::new();
    let mut byte = [0u8; 1];
    while !request.ends_with(b"\r\n\r\n") {
        if request.len() > 8192 || stream.read(&mut byte).map_or(true, |n| n == 0) {
            return Err(None);
        }
        request.push(byte[0]);
    }
    let request = String::from_utf8(request).map_err(|_| None)?;
    let header = |name: &str| {
        request.lines().skip(1).find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim().eq_ignore_ascii_case(name).then(|| v.trim().to_string())
        })
    };
    let websocket = header("Upgrade").is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
    let origin = header("Origin");
    match header("Sec-WebSocket-Key") {
        Some(key) if request.starts_with("GET ") && websocket && origin.as_deref() == Some(ORIGIN) => Ok(key),
        _ => Err(origin),
    }
}

#[link(name = "System", kind = "dylib")]
unsafe extern "C" {
    fn CC_SHA1(data: *const std::ffi::c_void, len: u32, md: *mut u8) -> *mut u8;
}

/// What the server answers to `key` (RFC 6455): the SHA-1 of it and a fixed GUID, in base64.
fn accept_key(key: &str) -> String {
    let text = format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    let mut digest = [0u8; 20];
    unsafe { CC_SHA1(text.as_ptr().cast(), text.len() as u32, digest.as_mut_ptr()) };
    base64(&digest)
}

fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { ABC[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// A frame from the extension: whether it's the last of its message, its opcode and its data
/// (unmasked). At most 1 MB.
fn read_frame(r: &mut impl Read) -> io::Result<(bool, u8, Vec<u8>)> {
    let mut head = [0u8; 2];
    r.read_exact(&mut head)?;
    let mut len = (head[1] & 0x7f) as u64;
    if len == 126 {
        let mut b = [0u8; 2];
        r.read_exact(&mut b)?;
        len = u16::from_be_bytes(b) as u64;
    } else if len == 127 {
        let mut b = [0u8; 8];
        r.read_exact(&mut b)?;
        len = u64::from_be_bytes(b);
    }
    if len > 1 << 20 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too big"));
    }
    let mut mask = [0u8; 4];
    if head[1] & 0x80 != 0 {
        r.read_exact(&mut mask)?;
    }
    let mut data = vec![0u8; len as usize];
    r.read_exact(&mut data)?;
    for (i, b) in data.iter_mut().enumerate() {
        *b ^= mask[i % 4];
    }
    Ok((head[0] & 0x80 != 0, head[0] & 0x0f, data))
}

/// A frame to the extension, whole and unmasked, as a server sends them.
fn frame(opcode: u8, data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x80 | opcode];
    match data.len() {
        n if n < 126 => out.push(n as u8),
        n if n <= u16::MAX as usize => {
            out.push(126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    out.extend_from_slice(data);
    out
}

// Where `struct socket_fdinfo` (sys/proc_info.h) keeps what `peer` needs: its size, the kind of
// socket, and the ports of a TCP one (local, then foreign; in network order).
const SOCKET_INFO: usize = 792;
const SOCKET_KIND: usize = 256;
const FOREIGN_PORT: usize = 264;
const LOCAL_PORT: usize = 268;
const SOCKINFO_TCP: i32 = 2;
const PROC_PIDFDSOCKETINFO: i32 = 3;

/// The process with the other end of a line to the server, from `port`: the one with a TCP socket
/// from `port` to `PORT`. A browser's network process: a helper of its app. Some ms, once a line.
fn peer(port: u16) -> Option<i32> {
    let me = std::process::id() as i32;
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let mut pids = vec![0i32; count.max(0) as usize + 64];
    let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), (pids.len() * 4) as i32) };
    let mut info = [0u8; SOCKET_INFO];
    for &pid in pids.iter().take(count.max(0) as usize).filter(|&&p| p > 0 && p != me) {
        let size = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
        if size <= 0 {
            continue;
        }
        let each = size_of::<libc::proc_fdinfo>();
        let mut fds = vec![libc::proc_fdinfo { proc_fd: 0, proc_fdtype: 0 }; size as usize / each + 16];
        let size = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, fds.as_mut_ptr().cast(), (fds.len() * each) as i32) };
        for fd in fds.iter().take(size.max(0) as usize / each) {
            if fd.proc_fdtype != libc::PROX_FDTYPE_SOCKET as u32 {
                continue;
            }
            let got = unsafe { libc::proc_pidfdinfo(pid, fd.proc_fd, PROC_PIDFDSOCKETINFO, info.as_mut_ptr().cast(), SOCKET_INFO as i32) };
            if got != SOCKET_INFO as i32 {
                continue;
            }
            let int = |at: usize| i32::from_ne_bytes([info[at], info[at + 1], info[at + 2], info[at + 3]]);
            let port_at = |at: usize| u16::from_be(int(at) as u16);
            if int(SOCKET_KIND) == SOCKINFO_TCP && port_at(LOCAL_PORT) == port && port_at(FOREIGN_PORT) == PORT {
                return Some(pid);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_handshake_answers_as_rfc_6455_says() {
        // The example of the RFC
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b"a"), "YQ==");
    }

    #[test]
    fn frames_go_both_ways() {
        // A masked frame, as a browser sends one
        let mask = [1u8, 2, 3, 4];
        let text = b"{\"type\":\"ping\"}";
        let mut sent = vec![0x81, 0x80 | text.len() as u8];
        sent.extend_from_slice(&mask);
        sent.extend(text.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        let (fin, opcode, data) = read_frame(&mut &sent[..]).unwrap();
        assert_eq!((fin, opcode, data.as_slice()), (true, 1, &text[..]));
        let long = vec![b'x'; 300];
        let out = frame(1, &long);
        assert_eq!(&out[..4], &[0x81, 126, 1, 44]);
        assert_eq!(read_frame(&mut &out[..]).unwrap().2, long);
    }

    #[test]
    fn a_line_finds_the_process_at_its_other_end() {
        // A child at the other end, as a browser would be: `nc` to a listener on `PORT`, if free
        let Ok(listener) = TcpListener::bind(("127.0.0.1", PORT)) else { return };
        let mut child = std::process::Command::new("/usr/bin/nc").args(["127.0.0.1", &PORT.to_string()]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).spawn().unwrap();
        let (_server, from) = listener.accept().unwrap();
        let found = peer(from.port());
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(found, Some(child.id() as i32));
    }

    #[test]
    fn an_older_extension_loads_the_new_one() {
        assert!(older("1.3", "1.4"));
        assert!(older("1.9", "1.10"));
        assert!(older("1", "1.0.1"));
        assert!(!older("1.4", "1.4"));
        assert!(!older("1.10", "1.9"));
    }

    #[test]
    fn the_extension_says_what_the_bar_reads() {
        let said: Said = serde_json::from_str(r#"{"type":"call","tab":7,"call":true,"mic":false,"camera":true,"hand":null,"name":"","code":"abc-defg-hij"}"#).unwrap();
        assert!(matches!(said, Said::Call { tab: 7, call: true, mic: Some(false), camera: Some(true), hand: None, lobby: false, .. }));
        // Before joining, from 1.5
        let said: Said = serde_json::from_str(r#"{"type":"call","tab":7,"call":true,"lobby":true,"join":true,"mic":true,"camera":false,"hand":null,"name":"","code":"abc-defg-hij"}"#).unwrap();
        assert!(matches!(said, Said::Call { lobby: true, join: true, mic: Some(true), .. }));
        assert!(matches!(serde_json::from_str::<Said>(r#"{"type":"ping"}"#).unwrap(), Said::Other));
    }
}
