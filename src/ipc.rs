//! `rustybar msg …`: commands for the running bar over a Unix socket, for scripts and for
//! AeroSpace's bindings (`rustybar msg trigger aerospace_windows`).

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use crate::runtime;
use crate::widgets::Event;

/// In the per-user temporary folder, which launchd agents and shells share.
pub fn socket_path() -> PathBuf {
    temp_dir().join(format!("rustybar{}.sock", crate::config::instance()))
}

/// The settings window's own, beside the bar's: a second `rustybar settings` hands its page to the
/// one already open (see `settings::run`).
pub fn settings_socket_path() -> PathBuf {
    temp_dir().join(format!("rustybar{}-settings.sock", crate::config::instance()))
}

fn temp_dir() -> PathBuf {
    let mut buf = vec![0u8; 1024];
    let n = unsafe { libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, buf.as_mut_ptr().cast(), buf.len()) };
    let dir = if n > 0 && (n as usize) <= buf.len() {
        String::from_utf8_lossy(&buf[..n as usize - 1]).to_string()
    } else {
        std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp/".into())
    };
    PathBuf::from(dir)
}

pub fn serve() {
    let path = socket_path();
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            crate::log(&format!("ipc: can't listen on {}: {e}", path.display()));
            return;
        }
    };
    runtime::spawn("ipc", move || {
        for stream in listener.incoming().flatten() {
            runtime::spawn("ipc-client", move || handle(stream));
        }
    });
}

fn handle(mut stream: UnixStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut line = String::new();
    if BufReader::new(&stream).read_line(&mut line).is_err() {
        return;
    }
    let words: Vec<String> = line.split_whitespace().map(String::from).collect();
    let (tx, rx) = mpsc::channel::<String>();
    runtime::post(move |app| {
        let reply = match words.first().map(String::as_str) {
            Some("ping") => "pong".to_string(),
            Some("reload") => {
                app.reload();
                "ok".into()
            }
            Some("redraw") => {
                app.mark_dirty();
                "ok".into()
            }
            Some("query") => app.describe(),
            Some("trigger") if words.len() > 1 => {
                let args = words[2..]
                    .iter()
                    .filter_map(|kv| kv.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
                    .collect();
                app.broadcast(&Event::Trigger { name: words[1].clone(), args });
                "ok".into()
            }
            Some("popup") if words.len() > 1 => {
                app.dispatch(crate::ui::Action::Popup { widget: words[1].clone() }, None, None);
                "ok".into()
            }
            Some("peek") if words.len() > 1 => {
                app.toggle_peek(&words[1]);
                "ok".into()
            }
            Some("settings") => {
                crate::settings::launch(words.get(1).map(String::as_str));
                "ok".into()
            }
            Some("quit") => {
                runtime::after(0.1, |app| app.quit());
                "ok".into()
            }
            _ => "usage: rustybar msg ping | reload | redraw | query | trigger <event> [key=value…] | popup <widget> | peek <widget> | settings [page] | quit".into(),
        };
        let _ = tx.send(reply);
    });
    let reply = rx.recv_timeout(Duration::from_secs(5)).unwrap_or_else(|_| "timeout".into());
    let _ = stream.write_all(reply.as_bytes());
}

/// Sends `words` to the running bar and returns its answer.
pub fn send(words: &[String]) -> Result<String, String> {
    let mut stream = UnixStream::connect(socket_path()).map_err(|_| "RustyBar is not running".to_string())?;
    stream.set_read_timeout(Some(Duration::from_secs(6))).ok();
    stream.write_all(format!("{}\n", words.join(" ")).as_bytes()).map_err(|e| e.to_string())?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply).map_err(|e| e.to_string())?;
    Ok(reply)
}
