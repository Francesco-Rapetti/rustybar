//! The bridge between threads and the main thread, timers and background commands.
//! The app state lives on the main thread; other threads send it closures through GCD.

use std::io::Read;
use std::process::{Command, Stdio};
use std::ptr::NonNull;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchTime};
use objc2::rc::Retained;
use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSString, NSTimer, NSURL};

use crate::app::{self, App};

/// Runs `f` with the app on the main thread, from any thread.
pub fn post(f: impl FnOnce(&mut App) + Send + 'static) {
    DispatchQueue::main().exec_async(move || app::with(f));
}

/// A value that may cross threads only to come back to the main thread it was made on. Used for
/// main-thread closures scheduled through GCD's main queue.
struct MainOnly<T>(T);
unsafe impl<T> Send for MainOnly<T> {}

/// Runs `f` on the main thread after `secs`, from the main thread.
pub fn after(secs: f64, f: impl FnOnce(&mut App) + 'static) {
    let f = MainOnly(f);
    let when = DispatchTime::try_from(Duration::from_secs_f64(secs.max(0.0))).unwrap_or(DispatchTime::NOW);
    let _ = DispatchQueue::main().after(when, move || {
        let f = f;
        app::with(f.0);
    });
}

/// A repeating timer on the main run loop, stopped when dropped. It has a tolerance of a tenth of
/// its interval, so macOS can fire it together with other timers and let the CPU sleep longer.
pub struct Timer(Retained<NSTimer>);

impl Timer {
    pub fn every(secs: f64, f: impl Fn(&mut App) + 'static) -> Timer {
        let f = std::rc::Rc::new(f);
        let block = RcBlock::new(move |_: NonNull<NSTimer>| {
            let f = f.clone();
            app::with(move |a| f(a))
        });
        let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(secs, true, &block) };
        timer.setTolerance(secs * 0.1);
        Timer(timer)
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        self.0.invalidate();
    }
}

/// Starts a named thread; a panic in it is logged and doesn't take the app down.
pub fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    let name = name.to_string();
    let label = name.clone();
    let _ = std::thread::Builder::new().name(name).spawn(move || {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).is_err() {
            crate::log(&format!("thread {label} panicked"));
        }
    });
}

#[derive(Debug, Clone, Default)]
pub struct Output {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn ok(&self) -> bool {
        self.status == Some(0)
    }
}

/// The PATH of a login shell, so that commands find what the user installed (Homebrew, nvm,
/// ~/.local/bin…) even when RustyBar was started by launchd with the system PATH.
pub fn user_path() -> &'static str {
    static PATH: OnceLock<String> = OnceLock::new();
    PATH.get_or_init(|| {
        let mut parts: Vec<String> = std::env::var("PATH").unwrap_or_default().split(':').map(String::from).collect();
        let home = crate::config::home();
        for extra in [
            "/opt/homebrew/bin".to_string(),
            "/opt/homebrew/sbin".to_string(),
            "/usr/local/bin".to_string(),
            home.join(".local/bin").display().to_string(),
            home.join(".cargo/bin").display().to_string(),
            "/usr/bin".to_string(),
            "/bin".to_string(),
            "/usr/sbin".to_string(),
            "/sbin".to_string(),
        ] {
            if !parts.contains(&extra) {
                parts.push(extra);
            }
        }
        parts.retain(|p| !p.is_empty());
        parts.join(":")
    })
}

/// Finds `name` in the user's PATH.
pub fn which(name: &str) -> Option<String> {
    if name.contains('/') {
        return Some(name.to_string());
    }
    user_path().split(':').map(|d| format!("{d}/{name}")).find(|p| std::path::Path::new(p).is_file())
}

/// Runs a command to completion, killing it after `timeout`. For worker threads only.
pub fn run(program: &str, args: &[&str], stdin: Option<&str>, timeout: Duration) -> Output {
    let Some(path) = which(program) else {
        return Output { status: None, stdout: String::new(), stderr: format!("{program}: not found") };
    };
    let mut cmd = Command::new(path);
    cmd.args(args)
        .env("PATH", user_path())
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Output { status: None, stdout: String::new(), stderr: e.to_string() },
    };
    if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
        use std::io::Write;
        let _ = pipe.write_all(input.as_bytes());
    }
    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    // Read the pipes on their own threads, so a chatty command can't fill a pipe and hang
    let out_t = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(p) = out_pipe.as_mut() {
            let _ = p.read_to_string(&mut s);
        }
        s
    });
    let err_t = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(p) = err_pipe.as_mut() {
            let _ = p.read_to_string(&mut s);
        }
        s
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s.code(),
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(15)),
            Err(_) => break None,
        }
    };
    Output { status, stdout: out_t.join().unwrap_or_default(), stderr: err_t.join().unwrap_or_default() }
}

/// Starts a command and forgets it (a click that focuses a window, opens an app…).
pub fn fire(program: &str, args: &[&str]) {
    let Some(path) = which(program) else {
        crate::log(&format!("{program}: not found"));
        return;
    };
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    spawn("fire", move || {
        let child = Command::new(path)
            .args(&args)
            .env("PATH", user_path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(mut c) = child {
            let _ = c.wait();
        }
    });
}

/// `/bin/sh -c` in the background.
pub fn shell(command: &str) {
    fire("/bin/sh", &["-c", command]);
}

/// Opens a URL (or a file path) with its app, without starting a process of our own.
pub fn open(target: &str) {
    let ws = NSWorkspace::sharedWorkspace();
    let s = NSString::from_str(target);
    let url = if target.starts_with('/') || target.starts_with('~') {
        let expanded = target.replacen('~', &crate::config::home().display().to_string(), 1);
        Some(NSURL::fileURLWithPath(&NSString::from_str(&expanded)))
    } else {
        NSURL::URLWithString(&s)
    };
    match url {
        Some(u) => {
            ws.openURL(&u);
        }
        None => crate::log(&format!("not a URL: {target}")),
    }
}

/// Opens an app by bundle id.
pub fn open_app(bundle_id: &str) {
    let ws = NSWorkspace::sharedWorkspace();
    if let Some(url) = ws.URLForApplicationWithBundleIdentifier(&NSString::from_str(bundle_id)) {
        ws.openURL(&url);
    }
}
