//! The LaunchAgent that starts RustyBar at login and again if it crashes (not after `rustybar msg
//! quit`, which exits cleanly). Its output goes to ~/Library/Logs/rustybar.log.

use std::path::PathBuf;
use std::process::Command;

pub const LABEL: &str = "io.github.francesco-rapetti.rustybar";

fn plist_path() -> PathBuf {
    crate::config::home().join(format!("Library/LaunchAgents/{LABEL}.plist"))
}

pub fn log_path() -> PathBuf {
    crate::config::home().join("Library/Logs/rustybar.log")
}

fn domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn launchctl(args: &[&str]) -> bool {
    Command::new("/bin/launchctl").args(args).stderr(std::process::Stdio::null()).status().is_ok_and(|s| s.success())
}

/// Writes the LaunchAgent for this binary (inside RustyBar.app, so macOS asks its permissions for
/// RustyBar) and starts it.
pub fn install() -> Result<String, String> {
    let exe = std::env::current_exe().and_then(|p| p.canonicalize()).map_err(|e| e.to_string())?;
    let exe = exe.display().to_string();
    let log = log_path().display().to_string();
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>ThrottleInterval</key>
  <integer>5</integer>
  <key>ProcessType</key>
  <string>Interactive</string>
  <key>LimitLoadToSessionType</key>
  <string>Aqua</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
  <key>StandardOutPath</key>
  <string>{log}</string>
</dict>
</plist>
"#,
        exe = escape(&exe),
        log = escape(&log),
    );
    let path = plist_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, plist).map_err(|e| e.to_string())?;
    let target = format!("{}/{LABEL}", domain());
    launchctl(&["bootout", &target]);
    if !launchctl(&["bootstrap", &domain(), &path.display().to_string()]) {
        return Err(format!("launchctl couldn't load {}", path.display()));
    }
    Ok(format!("installed {} (starts {exe})", path.display()))
}

/// Stops RustyBar and removes the LaunchAgent.
pub fn uninstall() -> Result<String, String> {
    let path = plist_path();
    launchctl(&["bootout", &format!("{}/{LABEL}", domain())]);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(format!("removed {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok("not installed".into()),
        Err(e) => Err(e.to_string()),
    }
}
