//! RustyBar: a light, animated, scriptable status bar for macOS.

mod app;
mod cache;
mod config;
mod display;
mod gfx;
mod i18n;
mod iokit;
mod ipc;
mod lua;
mod mock;
mod net;
mod runtime;
mod secret;
mod service;
mod surface;
mod theme;
mod time;
mod ui;
mod widgets;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate};
use objc2_foundation::{NSNotification, NSObject, NSObjectProtocol};

pub fn log(msg: &str) {
    let now = unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
    };
    eprintln!("[rustybar {now}] {msg}");
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            app::start(self.mtm());
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn will_terminate(&self, _notification: &NSNotification) {
            app::with(|app| app.cache.save());
        }
    }
);

const USAGE: &str = "usage:
  rustybar                 start the bar
  rustybar msg <command>   talk to the running bar: reload, query, trigger <event> [k=v…], popup <widget>, quit
  rustybar check           check the config and the Lua widgets
  rustybar service install start RustyBar at login, and again if it crashes (a LaunchAgent)
  rustybar service uninstall
  rustybar secret set home-assistant   save the Home Assistant token, read from stdin, in the keychain
  rustybar --version";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("msg") => {
            match ipc::send(&args[1..]) {
                Ok(reply) => println!("{}", reply.trim_end()),
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
            return;
        }
        Some("check") => {
            match config::load() {
                Ok(c) => {
                    let theme = theme::Theme::from_config(&c.theme);
                    match lua::Host::load(&c, &theme) {
                        Ok((_, widgets)) => println!("ok: {} Lua widget(s)", widgets.len()),
                        Err(e) => {
                            eprintln!("{e}");
                            std::process::exit(1);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
            return;
        }
        Some("service") => {
            let result = match args.get(1).map(String::as_str) {
                Some("install") => service::install(),
                Some("uninstall") => service::uninstall(),
                _ => Err(USAGE.to_string()),
            };
            match result {
                Ok(msg) => println!("{msg}"),
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
            return;
        }
        Some("secret") => {
            match secret::command(&args[1..]) {
                Ok(msg) => println!("{msg}"),
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
            return;
        }
        Some("--version") | Some("-V") => {
            println!("rustybar {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Some("--help") | Some("-h") => {
            println!("{USAGE}");
            return;
        }
        // AppKit's own defaults, e.g. `-AppleLanguages '(en)'`: the bar starts with them
        Some(flag) if flag.starts_with("-Apple") || flag.starts_with("-NS") => {}
        Some(other) => {
            eprintln!("unknown command {other}\n{USAGE}");
            std::process::exit(2);
        }
        None => {}
    }
    if ipc::send(&["ping".to_string()]).is_ok_and(|r| r.trim() == "pong") {
        // Not a failure, or launchd would start it again and again
        eprintln!("RustyBar is already running");
        std::process::exit(0);
    }

    let mtm = MainThreadMarker::new().expect("main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let delegate: Retained<Delegate> = unsafe { msg_send![Delegate::alloc(mtm), init] };
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.run();
}
