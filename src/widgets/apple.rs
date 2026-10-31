//! The Apple menu, which the bar covers: the apple opens its commands, with the same texts and
//! shortcuts. They run in-process (Apple events to loginwindow, the sleep of IOKit, the lock of
//! login.framework), so a click starts no process.
//! Options: `icon` (an SF Symbol, "apple.logo" by default). With made-up data (see `mock`) the
//! name is the `user` of `apple.json` and the commands do nothing.

use serde::Deserialize;

use super::{Cx, Rx, Widget};
use crate::config::Options;
use crate::gfx::Weight;
use crate::i18n::t;
use crate::runtime;
use crate::ui::Node;

pub struct Apple {
    icon: String,
    user: String,
    mock: bool,
}

/// Made-up data (see `mock`).
#[derive(Default, Deserialize)]
#[serde(default)]
struct Mock {
    user: String,
}

impl Apple {
    pub fn new(options: &Options) -> Apple {
        let mock = crate::mock::load::<Mock>("apple");
        Apple {
            icon: options.str("icon").unwrap_or("apple.logo").to_string(),
            mock: mock.is_some(),
            user: mock.map(|m| m.user).unwrap_or_else(|| objc2_foundation::NSFullUserName().to_string()),
        }
    }
}

impl Widget for Apple {
    fn bar(&self, rx: &Rx) -> Option<Node> {
        let th = rx.theme;
        // No pill: the logo stands on the desktop, as in the menu bar
        Some(
            Node::row(vec![Node::symbol(&self.icon, 16.0, Weight::Semibold, th.text)])
                .key(rx.name)
                .height(th.pill_height)
                .padding(0.0, 6.0, 0.0, 6.0)
                .radius(th.pill_radius)
                .click(rx.toggle_popup()),
        )
    }

    fn popup(&self, rx: &Rx) -> Option<Node> {
        let th = rx.theme;
        let row = |name: &str, text: String, shortcut: Option<&str>| {
            let right = shortcut.map(|s| vec![Node::text(s, th.font_with(Weight::Medium, 12.0), th.subtext)]).unwrap_or_default();
            rx.row_button(vec![Node::text(text, th.font(), th.text)], right, rx.action(name)).key(name)
        };
        let log_out = format!("{} {}…", t("Esegui il logout da", "Log Out"), self.user);
        Some(
            rx.popup(
                212.0,
                vec![
                    row("about", t("Informazioni su questo Mac", "About This Mac").into(), None),
                    rx.divider().key("d1"),
                    row("force_quit", t("Uscita forzata…", "Force Quit…").into(), Some("⌥⌘⎋")),
                    rx.divider().key("d2"),
                    row("sleep", t("Standby", "Sleep").into(), None),
                    row("restart", t("Riavvia…", "Restart…").into(), None),
                    row("shut_down", t("Spegni…", "Shut Down…").into(), None),
                    rx.divider().key("d3"),
                    row("lock", t("Blocca schermo", "Lock Screen").into(), Some("⌃⌘Q")),
                    row("log_out", log_out, Some("⇧⌘Q")),
                ],
            )
            .gap(0.0)
            .padding(4.0, 4.0, 4.0, 4.0),
        )
    }

    fn action(&mut self, name: &str, _value: Option<f64>, cx: &mut Cx) {
        // As in a menu: the popup closes, then the command runs
        cx.close_popup();
        if self.mock {
            return;
        }
        match name {
            "about" => runtime::open("/System/Library/CoreServices/Applications/About This Mac.app"),
            "force_quit" => loginwindow(*b"apwn"),
            "restart" => loginwindow(*b"rrst"),
            "shut_down" => loginwindow(*b"rsdn"),
            "log_out" => loginwindow(*b"logo"),
            // After the popup has faded, or the screen sleeps with it on
            "sleep" => runtime::after(0.3, |_| sleep_now()),
            "lock" => runtime::after(0.3, |_| lock_screen()),
            _ => {}
        }
    }

    crate::any_widget!();
}

/// Sends loginwindow the Apple event the Apple menu sends: it shows the Force Quit window or asks
/// to confirm a restart, a shut down or a log out.
fn loginwindow(id: [u8; 4]) {
    use objc2_foundation::{NSAppleEventDescriptor, NSString};
    let code = |b: [u8; 4]| u32::from_be_bytes(b);
    runtime::spawn("loginwindow", move || unsafe {
        let target = NSAppleEventDescriptor::descriptorWithBundleIdentifier(&NSString::from_str("com.apple.loginwindow"));
        // The constructor is behind CoreServices in objc2-foundation: call it directly
        let event: objc2::rc::Retained<NSAppleEventDescriptor> = objc2::msg_send![
            <NSAppleEventDescriptor as objc2::ClassType>::class(),
            appleEventWithEventClass: code(*b"aevt"),
            eventID: code(id),
            targetDescriptor: &*target,
            returnID: -1i16, // kAutoGenerateReturnID
            transactionID: 0i32 // kAnyTransactionID
        ];
        if let Err(e) = event.sendEventWithOptions_timeout_error(objc2_foundation::NSAppleEventSendOptions::NoReply, 5.0) {
            crate::log(&format!("loginwindow: {}", e.localizedDescription()));
        }
    });
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOPMSleepSystem(port: u32) -> i32;
    fn IOPMFindPowerManagement(master: u32) -> u32;
    fn IOServiceClose(connect: u32) -> i32;
}

fn sleep_now() {
    unsafe {
        let port = IOPMFindPowerManagement(0);
        if port != 0 {
            IOPMSleepSystem(port);
            IOServiceClose(port);
        }
    }
}

/// Lock Screen of the Apple menu, a function of a private framework.
fn lock_screen() {
    unsafe {
        let lib = libc::dlopen(c"/System/Library/PrivateFrameworks/login.framework/login".as_ptr(), libc::RTLD_NOW);
        if lib.is_null() {
            return;
        }
        let f = libc::dlsym(lib, c"SACLockScreenImmediate".as_ptr());
        if !f.is_null() {
            let f: extern "C" fn() -> i32 = std::mem::transmute(f);
            f();
        }
    }
}
