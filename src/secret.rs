//! Secrets in the login keychain, through `security`: items it makes it reads back without asking,
//! while RustyBar itself would have to ask again after every build signed ad hoc. One process per
//! read or write, never at rest: the widgets keep what they read.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

/// The password of the item of `service` (and `account`, if given).
pub fn get(service: &str, account: Option<&str>) -> Option<String> {
    let mut args = vec!["find-generic-password", "-s", service];
    if let Some(a) = account {
        args.extend(["-a", a]);
    }
    args.push("-w");
    let out = Command::new("/usr/bin/security").args(&args).stderr(Stdio::null()).output().ok()?;
    let secret = String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_string();
    (out.status.success() && !secret.is_empty()).then_some(secret)
}

/// Adds or replaces the item. The command goes on stdin, never in the arguments, which any process
/// can read.
pub fn set(service: &str, account: &str, secret: &str) -> bool {
    let quote = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let Ok(mut child) = Command::new("/usr/bin/security").arg("-i").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() else { return false };
    if let Some(stdin) = child.stdin.as_mut() {
        let _ = writeln!(stdin, "add-generic-password -U -s \"{}\" -a \"{}\" -w \"{}\"", quote(service), quote(account), quote(secret));
    }
    drop(child.stdin.take());
    child.wait().is_ok_and(|s| s.success())
}

/// `rustybar secret set <name>`: the secret from stdin, e.g. `pbpaste | rustybar secret set
/// home-assistant`. The prompt of `security -w` would take at most 128 characters, fewer than a
/// token has.
pub fn command(args: &[String]) -> Result<String, String> {
    let names = ["home-assistant"];
    match (args.first().map(String::as_str), args.get(1)) {
        (Some("set"), Some(name)) if names.contains(&name.as_str()) => {
            let mut secret = String::new();
            std::io::stdin().read_to_string(&mut secret).map_err(|e| e.to_string())?;
            let secret = secret.trim();
            if secret.is_empty() {
                return Err("nothing on stdin: e.g. pbpaste | rustybar secret set home-assistant".into());
            }
            let user = std::env::var("USER").unwrap_or_else(|_| "rustybar".into());
            if !set(&format!("rustybar-{name}"), &user, secret) {
                return Err("security couldn't save it in the login keychain".into());
            }
            // The running bar connects again with it
            let _ = crate::ipc::send(&["trigger".to_string(), format!("{}_token", name.replace('-', "_"))]);
            Ok(format!("saved in the login keychain as rustybar-{name}"))
        }
        _ => Err(format!("usage: rustybar secret set <{}> (the secret on stdin)", names.join("|"))),
    }
}
